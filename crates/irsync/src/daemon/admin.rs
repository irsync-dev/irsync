//! The admin service on the daemon's side. An admin session is the
//! control stream of a connection whose Hello says ADMIN: the client sends
//! changes, which wait until its COMMIT applies them all or nothing under
//! the config lock (with the engine in [`crate::manage::admin`]), and reads,
//! answered at once from the saved file. The client's `admin` right is
//! checked again at every commit and every read.
//!
//! The admin messages go past the stream's tap: a wire trace or capture
//! shows the Hello and the end of the stream, not the config they carry.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::EndpointId;
use irsync_proto::config as pb;
use irsync_proto::v1::{
    admin_reply, admin_request, error::Code, read, AdminReply, AdminRequest, Change, Error, Nack,
};
use prost::Message;

use super::Shared;
use crate::authz::{lock_config_waiting, Access, Snapshot};
use crate::config::clients::Clients;
use crate::config::file::Config;
use crate::manage::admin::{self as engine, Committer, NOT_ALLOWED};
use crate::manage::{load_message, ManageError};
use crate::net::frame::{read_msg_max, write_msg, FrameTooLarge};
use crate::net::tap::StreamTap;

/// The most changes one transaction holds. A transaction that passes it
/// is dropped: the changes after it are discarded, and its COMMIT is
/// answered NACK.
pub const ADMIN_MAX_CHANGES: usize = 100;

/// The largest admin request frame the daemon reads (64 KiB). A longer one
/// ends the session.
pub const ADMIN_MAX_REQUEST: usize = 64 * 1024;

/// The most bytes of changes one transaction holds (1 MiB); past it the
/// transaction is dropped as with [`ADMIN_MAX_CHANGES`].
pub const ADMIN_MAX_PENDING: usize = 1024 * 1024;

/// How long an admin session may wait for the client's next request
/// before the daemon closes it.
pub const ADMIN_IDLE: Duration = Duration::from_secs(600);

/// The NACK reason for a commit that could not get the config lock in
/// [`crate::authz::LOCK_WAIT`].
const BUSY: &str = "busy: the config is locked, try again";

/// The changes of the transaction a session is building.
#[derive(Default)]
struct Pending {
    changes: Vec<Change>,
    bytes: usize,
    /// Why the transaction was dropped: its COMMIT gets this as NACK 0.
    dropped: Option<String>,
}

impl Pending {
    /// Adds `c`, or drops the transaction when `c` would pass a limit.
    /// A dropped transaction discards every change until its COMMIT.
    fn add(&mut self, c: Change) {
        if self.dropped.is_some() {
            return;
        }
        let size = c.encoded_len();
        let reason = if self.changes.len() >= ADMIN_MAX_CHANGES {
            format!("too many changes in one transaction (at most {ADMIN_MAX_CHANGES})")
        } else if self.bytes + size > ADMIN_MAX_PENDING {
            format!(
                "the transaction is too large (at most {} MiB)",
                ADMIN_MAX_PENDING / (1024 * 1024)
            )
        } else {
            self.bytes += size;
            self.changes.push(c);
            return;
        };
        self.changes = Vec::new();
        self.bytes = 0;
        self.dropped = Some(reason);
    }

    /// The transaction to commit, or why it was dropped; the next one
    /// starts empty.
    fn take(&mut self) -> Result<Vec<Change>, String> {
        let p = std::mem::take(self);
        match p.dropped {
            Some(reason) => Err(reason),
            None => Ok(p.changes),
        }
    }
}

/// Serves the admin session on the control stream `send`/`recv` of `conn`,
/// opened by `id` with an ADMIN Hello whose version was checked; `snap`
/// is the connection's snapshot and `local` the daemon's own id. A key
/// without `admin` in `snap` gets the plain refusal. Ends when the client
/// closes the stream or stays idle for [`ADMIN_IDLE`]; pending changes are
/// dropped then.
#[allow(clippy::too_many_arguments)]
pub(super) async fn serve_admin(
    shared: Arc<Shared>,
    snap: &Snapshot,
    conn: Connection,
    local: EndpointId,
    id: EndpointId,
    mut send: SendStream,
    mut recv: RecvStream,
    tap: StreamTap,
) -> Result<()> {
    // A daemon whose grants are not from a file has nothing to administer.
    let path = shared.authz.config_path().map(Path::to_path_buf);
    let Some(path) = path.filter(|_| snap.admin(&id).is_some()) else {
        let who = label_in(shared.authz.config_path(), id).await;
        tracing::warn!("admin: {who}: session refused");
        return refuse(&conn, &mut send, &tap).await;
    };
    let mut pending = Pending::default();
    let reason = loop {
        let read = read_msg_max(&mut recv, ADMIN_MAX_REQUEST);
        let req = match tokio::time::timeout(ADMIN_IDLE, read).await {
            Err(_) => break &b"idle"[..],
            Ok(Ok(None)) => break b"done",
            Ok(Ok(Some(AdminRequest { msg: Some(m) }))) => m,
            Ok(Ok(Some(AdminRequest { msg: None }))) => {
                return bad_request(&conn, &mut send, &tap, &path, id).await;
            }
            Ok(Err(e)) if FrameTooLarge::of(&e).is_some() => {
                let who = label_in(Some(&path), id).await;
                tracing::warn!("admin: {who}: request too large, session refused");
                let message = format!(
                    "request too large (at most {} KiB)",
                    ADMIN_MAX_REQUEST / 1024
                );
                return close_with(&conn, &mut send, &tap, Code::Unknown, &message).await;
            }
            Ok(Err(e)) => {
                conn.close(2u32.into(), b"bad request");
                return Err(anyhow::Error::from(e).context("read admin request"));
            }
        };
        let reply = match req {
            admin_request::Msg::Change(c) => {
                pending.add(c);
                continue;
            }
            admin_request::Msg::Commit(_) => match pending.take() {
                Ok(changes) => commit(&shared, &path, id, changes).await,
                Err(reason) => {
                    let who = label_in(Some(&path), id).await;
                    tracing::warn!("admin: {who}: transaction refused");
                    admin_reply::Msg::Nack(Nack { change: 0, reason })
                }
            },
            admin_request::Msg::Read(r) => match r.what {
                None => return bad_request(&conn, &mut send, &tap, &path, id).await,
                Some(what) => {
                    let (p, home) = (path.clone(), shared.home.clone());
                    let answered = tokio::task::spawn_blocking(move || {
                        answer(&p, home.as_deref(), local, &id, what)
                    })
                    .await
                    .unwrap_or_else(|e| {
                        tracing::error!("admin: the read failed: {e}");
                        None
                    });
                    match answered {
                        Some(reply) => reply,
                        None => {
                            let who = label_in(Some(&path), id).await;
                            tracing::warn!("admin: {who}: read refused");
                            return refuse(&conn, &mut send, &tap).await;
                        }
                    }
                }
            },
        };
        write_msg(&mut send, &AdminReply { msg: Some(reply) }).await?;
    };
    send.finish()?;
    tap.finished();
    let _ = tokio::time::timeout(Duration::from_secs(5), send.stopped()).await;
    conn.close(0u32.into(), reason);
    Ok(())
}

/// Applies `changes` to the config file `path` for `id`, all or nothing,
/// and writes the audit lines. No changes: ACK, with nothing written or
/// locked, to a client that has `admin = "rw"` in the saved file.
async fn commit(
    shared: &Arc<Shared>,
    path: &Path,
    id: EndpointId,
    changes: Vec<Change>,
) -> admin_reply::Msg {
    let (path, home) = (path.to_path_buf(), shared.home.clone());
    let done = tokio::task::spawn_blocking(move || {
        let (result, who) = commit_blocking(&path, home.as_deref(), id, &changes);
        (result, who, changes)
    })
    .await;
    let (result, who, changes) = match done {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("admin: {}: transaction refused", id.fmt_short());
            tracing::error!("admin: the commit failed: {e}");
            return admin_reply::Msg::Nack(Nack {
                change: 0,
                reason: "internal error".to_string(),
            });
        }
    };
    match result {
        Ok(results) => {
            for c in &changes {
                tracing::info!("admin: {who} {}", engine::summary(c));
            }
            admin_reply::Msg::Ack(irsync_proto::v1::Ack { results })
        }
        Err(nack) => {
            tracing::warn!("admin: {who}: transaction refused");
            admin_reply::Msg::Nack(nack)
        }
    }
}

/// [`commit`]'s work. First the committer's right, from the file as
/// saved (no lock needed to read it): a client without `admin = "rw"` is
/// told `not allowed`, never `busy`. Then, with changes, under the lock,
/// waited for up to [`crate::authz::LOCK_WAIT`]: load, apply (which checks
/// the right again), and save only when every change applied. Also
/// returns the committer's label for the audit lines. A file that cannot
/// be locked or read is refused as [`NOT_ALLOWED`]: nothing is known yet
/// about the committer's right.
fn commit_blocking(
    path: &Path,
    home: Option<&Path>,
    id: EndpointId,
    changes: &[Change],
) -> (Result<Vec<String>, Nack>, String) {
    let whole = |reason: &str| Nack {
        change: 0,
        reason: reason.to_string(),
    };
    let Some(saved) = read_config(path) else {
        return (Err(whole(NOT_ALLOWED)), id.fmt_short().to_string());
    };
    let who = label(&saved, path, &id);
    if engine::admin_of(&saved, path, &id) != Some(Access::ReadWrite) {
        return (Err(whole(NOT_ALLOWED)), who);
    }
    if changes.is_empty() {
        return (Ok(Vec::new()), who);
    }
    let mut lock = match lock_config_waiting(path) {
        Ok(lock) => lock,
        Err(ManageError::Locked { .. }) => return (Err(whole(BUSY)), who),
        Err(e) => {
            cannot_read(path, &e);
            return (Err(whole(NOT_ALLOWED)), who);
        }
    };
    let mut msg = match lock.load() {
        Ok(msg) => msg,
        Err(e) => {
            cannot_read(path, &e);
            return (Err(whole(NOT_ALLOWED)), who);
        }
    };
    let who = label(&msg, path, &id);
    let result = match engine::apply(&mut msg, path, home, &Committer { id }, changes) {
        // The committer has `admin = "rw"` by now: it may read why.
        Ok(results) => lock
            .save(&mut msg)
            .map(|()| results)
            .map_err(|e| whole(&e.to_string())),
        Err(nack) => Err(nack),
    };
    (result, who)
}

/// The reply to a read by `id`, from the config file as saved now; `None`
/// when `id` has no `admin` right in it (or it cannot be read). `home` is
/// the daemon's home, which a `~` path of an export is under.
fn answer(
    path: &Path,
    home: Option<&Path>,
    local: EndpointId,
    id: &EndpointId,
    what: read::What,
) -> Option<admin_reply::Msg> {
    let msg = read_config(path)?;
    engine::admin_of(&msg, path, id)?;
    Some(match what {
        read::What::View(_) => {
            let config = Config::from_message(&msg, path)
                .map_err(|e| cannot_read(path, &e))
                .ok()?;
            let allow_all_paths = config.settings.daemon.allow_all_paths;
            admin_reply::Msg::View(engine::view(&msg, local, allow_all_paths, home))
        }
        read::What::Config(c) => match engine::config_text(&msg, &c.key) {
            Ok(text) => admin_reply::Msg::Config(text),
            Err(message) => admin_reply::Msg::Refused(Error {
                code: Code::Unknown as i32,
                message,
            }),
        },
    })
}

/// How the audit lines name `id`: the name of its client entry in
/// `msg` (one with `admin = "rw"` first), else its short id.
fn label(msg: &pb::Config, path: &Path, id: &EndpointId) -> String {
    let Ok(clients) = Clients::from_message(&msg.clients, path) else {
        return id.fmt_short().to_string();
    };
    let mine = || clients.entries.iter().filter(|(_, c)| c.client_id == *id);
    mine()
        .find(|(_, c)| c.admin == Some(Access::ReadWrite))
        .or_else(|| mine().next())
        .map_or_else(|| id.fmt_short().to_string(), |(name, _)| name.clone())
}

/// [`label`] from the config file at `path` as it is now, read off the
/// async task.
async fn label_in(path: Option<&Path>, id: EndpointId) -> String {
    let short = || id.fmt_short().to_string();
    let Some(p) = path.map(Path::to_path_buf) else {
        return short();
    };
    tokio::task::spawn_blocking(move || match load_message(&p) {
        Ok(msg) => label(&msg, &p, &id),
        Err(_) => id.fmt_short().to_string(),
    })
    .await
    .unwrap_or_else(|_| short())
}

/// The config file at `path` as saved now; `None`, with the error
/// logged, when it cannot be read.
fn read_config(path: &Path) -> Option<pb::Config> {
    load_message(path).map_err(|e| cannot_read(path, &e)).ok()
}

/// Logs why the config file cannot be read or locked. The client is
/// told only `not allowed`: its right is not known then.
fn cannot_read(path: &Path, e: &dyn std::fmt::Display) {
    tracing::error!(path = %path.display(), "admin: cannot read the config: {e}");
}

/// The plain refusal, which says nothing about the server; closes the
/// session.
async fn refuse(conn: &Connection, send: &mut SendStream, tap: &StreamTap) -> Result<()> {
    close_with(conn, send, tap, Code::Unauthorized, NOT_ALLOWED).await
}

/// The reply to a request with nothing in it; closes the session.
async fn bad_request(
    conn: &Connection,
    send: &mut SendStream,
    tap: &StreamTap,
    path: &Path,
    id: EndpointId,
) -> Result<()> {
    let who = label_in(Some(path), id).await;
    tracing::warn!("admin: {who}: bad request");
    close_with(conn, send, tap, Code::Unknown, "bad request").await
}

async fn close_with(
    conn: &Connection,
    send: &mut SendStream,
    tap: &StreamTap,
    code: Code,
    message: &str,
) -> Result<()> {
    let refused = admin_reply::Msg::Refused(Error {
        code: code as i32,
        message: message.to_string(),
    });
    write_msg(send, &AdminReply { msg: Some(refused) }).await?;
    send.finish()?;
    tap.finished();
    // Wait (bounded) for the peer to read it before closing the connection.
    let _ = tokio::time::timeout(Duration::from_secs(5), send.stopped()).await;
    conn.close(2u32.into(), b"rejected");
    Ok(())
}
