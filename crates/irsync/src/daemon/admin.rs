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

use super::Shared;
use crate::authz::{lock_config_waiting, Access, Snapshot};
use crate::config::clients::Clients;
use crate::config::file::Config;
use crate::manage::admin::{self as engine, Committer, NOT_ALLOWED};
use crate::manage::{load_message, ManageError};
use crate::net::frame::{read_msg, write_msg};
use crate::net::tap::StreamTap;

/// The most changes one transaction holds; one more drops it.
pub const ADMIN_MAX_CHANGES: usize = 100;

/// How long an admin session may wait for the client's next request
/// before the daemon closes it.
pub const ADMIN_IDLE: Duration = Duration::from_secs(600);

/// The NACK reason for a commit that could not get the config lock in
/// [`crate::authz::LOCK_WAIT`].
const BUSY: &str = "busy: the config is locked, try again";

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
        let who = label_in(shared.authz.config_path(), &id);
        tracing::warn!("admin: {who}: session refused");
        return refuse(&conn, &mut send, &tap).await;
    };
    let mut pending: Vec<Change> = Vec::new();
    let reason = loop {
        let req = match tokio::time::timeout(ADMIN_IDLE, read_msg(&mut recv)).await {
            Err(_) => break &b"idle"[..],
            Ok(Ok(None)) => break b"done",
            Ok(Ok(Some(AdminRequest { msg: Some(m) }))) => m,
            Ok(Ok(Some(AdminRequest { msg: None }))) => {
                return bad_request(&conn, &mut send, &tap, &path, &id).await;
            }
            Ok(Err(e)) => {
                conn.close(2u32.into(), b"bad request");
                return Err(anyhow::Error::from(e).context("read admin request"));
            }
        };
        let reply = match req {
            admin_request::Msg::Change(c) => {
                if pending.len() < ADMIN_MAX_CHANGES {
                    pending.push(c);
                    continue;
                }
                pending.clear();
                tracing::warn!("admin: {}: transaction refused", label_in(Some(&path), &id));
                admin_reply::Msg::Nack(Nack {
                    change: 0,
                    reason: format!(
                        "too many changes in one transaction (at most {ADMIN_MAX_CHANGES})"
                    ),
                })
            }
            admin_request::Msg::Commit(_) => {
                let changes = std::mem::take(&mut pending);
                commit(&shared, &path, id, changes).await
            }
            admin_request::Msg::Read(r) => match r.what {
                None => return bad_request(&conn, &mut send, &tap, &path, &id).await,
                Some(what) => match answer(&path, local, &id, what) {
                    Some(reply) => reply,
                    None => {
                        tracing::warn!("admin: {}: read refused", label_in(Some(&path), &id));
                        return refuse(&conn, &mut send, &tap).await;
                    }
                },
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
    if changes.is_empty() {
        let rw = read_config(path)
            .is_some_and(|msg| engine::admin_of(&msg, path, &id) == Some(Access::ReadWrite));
        if rw {
            return admin_reply::Msg::Ack(irsync_proto::v1::Ack::default());
        }
        tracing::warn!("admin: {}: transaction refused", label_in(Some(path), &id));
        return admin_reply::Msg::Nack(Nack {
            change: 0,
            reason: NOT_ALLOWED.to_string(),
        });
    }
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

/// [`commit`]'s work under the lock, waited for up to
/// [`crate::authz::LOCK_WAIT`]: load, apply, and save only when every
/// change applied. Also returns the committer's label for the audit
/// lines. A file that cannot be locked or read is refused as
/// [`NOT_ALLOWED`]: nothing is known yet about the committer's right.
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
    let mut lock = match lock_config_waiting(path) {
        Ok(lock) => lock,
        Err(ManageError::Locked { .. }) => return (Err(whole(BUSY)), label_in(Some(path), &id)),
        Err(e) => {
            cannot_read(path, &e);
            return (Err(whole(NOT_ALLOWED)), id.fmt_short().to_string());
        }
    };
    let mut msg = match lock.load() {
        Ok(msg) => msg,
        Err(e) => {
            cannot_read(path, &e);
            return (Err(whole(NOT_ALLOWED)), id.fmt_short().to_string());
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
/// when `id` has no `admin` right in it (or it cannot be read).
fn answer(
    path: &Path,
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
            admin_reply::Msg::View(engine::view(&msg, local, allow_all_paths))
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

/// [`label`] from the config file at `path` as it is now.
fn label_in(path: Option<&Path>, id: &EndpointId) -> String {
    if let Some(p) = path {
        if let Ok(msg) = load_message(p) {
            return label(&msg, p, id);
        }
    }
    id.fmt_short().to_string()
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
    id: &EndpointId,
) -> Result<()> {
    tracing::warn!("admin: {}: bad request", label_in(Some(path), id));
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
