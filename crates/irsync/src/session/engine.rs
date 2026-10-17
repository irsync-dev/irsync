//! Session entry points for both ends of a connection.
//!
//! The client opens the control stream and sends `Hello`; the daemon answers
//! `HelloAck`. From then on both ends speak `Control` frames on that one
//! bidirectional stream: each streams its listing while reading the peer's,
//! then the **receiver role** (the planner, rsync-generator style) drives the
//! transfer with `PlanItem`/`NeedChunks`/`FileAck` and the **sender role**
//! answers with `ChunkList`s and data on unidirectional streams. Which end
//! plays which role follows `Hello.direction`.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use irsync_proto::v1::{error::Code, hello::Direction, Error, FileEntry, Hello, HelloAck};
use irsync_proto::PROTOCOL_VERSION;
use prost::Message;
use rand_core::{OsRng, RngCore};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

use super::control::{self, Ev};
use super::{receiver, sender_role, LocalSide, Outcome, SessionKey, SideHooks, HASH_WORKERS};
use crate::cli::location::Location;
use crate::cli::{Preserve, SyncOptions};
use crate::filter::FilterSet;
use crate::fs::meta::ApplyOpts;
use crate::fs::paths::RelPath;
use crate::hasher::Hasher;
use crate::index::IndexHandle;
use crate::net::frame::{write_msg, MAX_FRAME};
use crate::send::DEFAULT_PARALLEL;

/// What a role needs to know about the session options.
#[derive(Debug, Clone)]
pub(super) struct RoleOpts {
    pub preserve: Preserve,
    pub checksum: bool,
    pub update_only: bool,
    pub rename_detection: bool,
    pub partial_dir: Option<PathBuf>,
    pub parallel: usize,
    pub bwlimit: Option<u64>,
}

impl RoleOpts {
    fn client(o: &SyncOptions) -> RoleOpts {
        RoleOpts {
            preserve: o.preserve,
            checksum: o.checksum,
            update_only: o.update_only,
            rename_detection: !o.no_rename_detection,
            partial_dir: o.partial_dir.clone(),
            parallel: o.parallel.map_or(DEFAULT_PARALLEL, |p| p.max(1) as usize),
            bwlimit: o.bwlimit,
        }
    }

    /// v1: options not negotiated. The client's flags never reach the daemon,
    /// so the daemon preserves everything it can and trusts the quick check.
    fn daemon() -> RoleOpts {
        RoleOpts {
            preserve: Preserve {
                recursive: true,
                links: true,
                perms: true,
                times: true,
                group: true,
                owner: true,
                devices: true,
                hard_links: false,
                sparse: false,
            },
            checksum: false,
            update_only: false,
            rename_detection: true,
            partial_dir: None,
            parallel: DEFAULT_PARALLEL,
            bwlimit: None,
        }
    }
}

/// Everything a role works with.
pub(super) struct Ctx {
    pub conn: Connection,
    pub session_id: [u8; 16],
    pub key: Vec<u8>,
    pub root: PathBuf,
    pub index: IndexHandle,
    pub hasher: Arc<Hasher>,
    /// A second hasher that always re-reads (used after a file changed).
    pub forced: Arc<Hasher>,
    pub filters: Arc<FilterSet>,
    pub opts: RoleOpts,
    pub privs: Privs,
    #[cfg_attr(not(feature = "test-hooks"), allow(dead_code))]
    pub hooks: SideHooks,
}

impl Ctx {
    /// Absolute path of `rel` under the session root.
    pub fn full(&self, rel: &RelPath) -> PathBuf {
        if rel.as_bytes().is_empty() {
            self.root.clone()
        } else {
            self.root.join(rel.to_os())
        }
    }

    pub fn files_hashed(&self) -> u64 {
        use std::sync::atomic::Ordering::Relaxed;
        self.hasher.stats().files_hashed.load(Relaxed)
            + self.forced.stats().files_hashed.load(Relaxed)
    }

    /// Which metadata to apply for `e` on this side.
    pub fn apply_opts(&self, e: &FileEntry) -> ApplyOpts {
        let p = &self.opts.preserve;
        ApplyOpts {
            perms: p.perms,
            times: p.times,
            owner: p.owner && self.privs.root,
            group: p.group && self.privs.can_chgrp(e.gid),
        }
    }
}

/// Ownership changes this process may make: any as root, otherwise only to
/// groups it belongs to (rsync applies owner only when privileged).
#[derive(Debug, Clone)]
pub(super) struct Privs {
    root: bool,
    groups: HashSet<u32>,
}

impl Privs {
    fn current() -> Privs {
        use nix::unistd::{getegid, geteuid, getgroups};
        let mut groups: HashSet<u32> = getgroups()
            .map(|g| g.into_iter().map(|g| g.as_raw()).collect())
            .unwrap_or_default();
        groups.insert(getegid().as_raw());
        Privs {
            root: geteuid().is_root(),
            groups,
        }
    }

    fn can_chgrp(&self, gid: u32) -> bool {
        self.root || self.groups.contains(&gid)
    }
}

/// Reads one raw frame body (the caller decides what it is).
async fn read_frame(recv: &mut RecvStream) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    AsyncReadExt::read_exact(recv, &mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "oversize frame"));
    }
    let mut body = vec![0u8; len];
    AsyncReadExt::read_exact(recv, &mut body).await?;
    Ok(body)
}

/// The daemon answers `Hello` with `HelloAck`, or with `Error` when it
/// refuses. The two differ in the wire type of field 1, so at most one
/// decodes as a well-formed ack.
async fn read_hello_reply(recv: &mut RecvStream) -> anyhow::Result<HelloAck> {
    let body = read_frame(recv).await.context("reading HelloAck")?;
    if let Ok(ack) = HelloAck::decode(&body[..]) {
        if ack.session_id.len() == 16 {
            return Ok(ack);
        }
    }
    match Error::decode(&body[..]) {
        Ok(e) => bail!(
            "daemon refused the session ({:?}): {}",
            Code::try_from(e.code).unwrap_or(Code::Unknown),
            e.message
        ),
        Err(_) => bail!("malformed reply to Hello"),
    }
}

fn remote_location(conn: &Connection, hello: &Hello) -> anyhow::Result<Location> {
    Ok(Location::Remote {
        host: conn.remote_id().to_string(),
        module: hello.module.clone(),
        path: RelPath::new(&hello.path).map_err(|_| anyhow::anyhow!("invalid remote path"))?,
        contents_only: hello.contents_only,
    })
}

/// Runs a whole session as the client over `conn`: sends `hello`, then plays
/// the sender role (`CLIENT_SENDS`) or the receiver role (`DAEMON_SENDS`) on
/// `local`. The root and remote path must already follow rsync's
/// trailing-slash rule (see [`super::resolve_roots`]).
pub async fn run_client(
    conn: Connection,
    hello: Hello,
    local: LocalSide,
    opts: &SyncOptions,
) -> anyhow::Result<Outcome> {
    let direction = Direction::try_from(hello.direction).context("bad direction in Hello")?;
    let local_loc = Location::Local {
        path: local.root.clone(),
        contents_only: local.contents_only,
    };
    let remote_loc = remote_location(&conn, &hello)?;
    let (src, dst) = match direction {
        Direction::ClientSends => (&local_loc, &remote_loc),
        Direction::DaemonSends => (&remote_loc, &local_loc),
    };
    let key = SessionKey::new(conn.remote_id(), src, dst, opts).into_bytes();

    let (mut send, mut recv) = conn.open_bi().await.context("opening control stream")?;
    write_msg(&mut send, &hello)
        .await
        .context("sending Hello")?;
    let ack = read_hello_reply(&mut recv).await?;
    let session_id: [u8; 16] = ack.session_id.as_slice().try_into()?;
    tracing::info!(session = %control::hex(&session_id), ?direction, "session started");

    let sends = direction == Direction::ClientSends;
    if !sends {
        std::fs::create_dir_all(&local.root)
            .with_context(|| format!("creating {}", local.root.display()))?;
    }
    let forced = Arc::new(Hasher::new(local.index.clone(), local.root.clone(), 1).with_force(true));
    let ctx = Ctx {
        conn,
        session_id,
        key,
        root: local.root,
        index: local.index,
        hasher: local.hasher,
        forced,
        filters: local.filters,
        opts: RoleOpts::client(opts),
        privs: Privs::current(),
        hooks: local.hooks,
    };
    run_role(Arc::new(ctx), send, recv, sends).await
}

/// Checks the session root below the module root: every existing component
/// must be a real directory (never a symlink, which could lead outside the
/// module); missing ones are created when `create` (receiving) is set.
fn prepare_root(module_root: &Path, rel: &RelPath, create: bool) -> io::Result<PathBuf> {
    let mut p = module_root.to_path_buf();
    let check = |p: &Path| -> io::Result<bool> {
        match std::fs::symlink_metadata(p) {
            Ok(m) if m.is_dir() => Ok(true),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a directory", p.display()),
            )),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    };
    if !check(&p)? {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "module root is missing",
        ));
    }
    for c in rel.components() {
        p.push(<std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(c));
        if !check(&p)? {
            if !create {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{} does not exist", p.display()),
                ));
            }
            std::fs::create_dir(&p)?;
        }
    }
    Ok(p)
}

/// Serves one session on the daemon after `Hello` was validated: replies
/// `HelloAck` and plays the role opposite to the client's.
pub async fn serve_daemon(
    conn: Connection,
    mut send: SendStream,
    recv: RecvStream,
    module_root: PathBuf,
    hello: Hello,
    index: IndexHandle,
) -> anyhow::Result<Outcome> {
    let direction = Direction::try_from(hello.direction).context("bad direction")?;
    // The daemon sends when the client receives.
    let sends = direction == Direction::DaemonSends;
    let rel = RelPath::new(&hello.path).map_err(|_| anyhow::anyhow!("invalid path"))?;
    let root = match prepare_root(&module_root, &rel, !sends) {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("bad session path: {e}");
            write_msg(
                &mut send,
                &Error {
                    code: Code::BadPath as i32,
                    message: msg.clone(),
                },
            )
            .await?;
            send.finish()?;
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), send.stopped()).await;
            bail!(msg);
        }
    };
    // Task 17 will validate resumption; for now a client-chosen id is used as is.
    let session_id: [u8; 16] = match <[u8; 16]>::try_from(hello.session_id.as_slice()) {
        Ok(id) => id,
        Err(_) => {
            let mut id = [0u8; 16];
            OsRng.fill_bytes(&mut id);
            id
        }
    };
    write_msg(
        &mut send,
        &HelloAck {
            session_id: session_id.to_vec(),
            resumed: false,
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await?;
    tracing::info!(
        session = %control::hex(&session_id),
        peer = %conn.remote_id().fmt_short(),
        module = %hello.module,
        ?direction,
        "session started"
    );
    let key = SessionKey::for_daemon(conn.remote_id(), &hello).into_bytes();
    let hasher = Arc::new(Hasher::new(index.clone(), root.clone(), HASH_WORKERS));
    let forced = Arc::new(Hasher::new(index.clone(), root.clone(), 1).with_force(true));
    let ctx = Ctx {
        conn,
        session_id,
        key,
        root,
        index,
        hasher,
        forced,
        // v1: the client's filters are not transmitted; the daemon walks everything.
        filters: Arc::new(FilterSet::new(vec![])),
        opts: RoleOpts::daemon(),
        privs: Privs::current(),
        hooks: SideHooks::default(),
    };
    run_role(Arc::new(ctx), send, recv, sends).await
}

/// Wires the control stream to the chosen role and reports a fatal error to
/// the peer before giving up.
async fn run_role(
    ctx: Arc<Ctx>,
    send: SendStream,
    recv: RecvStream,
    sends: bool,
) -> anyhow::Result<Outcome> {
    let (ev_tx, ev_rx) = mpsc::unbounded_channel();
    let (out, writer) = control::spawn_writer(send, ev_tx.clone());
    let (listing_tx, listing_rx) = mpsc::unbounded_channel();
    let reader = control::spawn_reader(recv, (!sends).then_some(listing_tx), ev_tx.clone());
    let res = if sends {
        sender_role::run(ctx.clone(), out.clone(), ev_tx, ev_rx).await
    } else {
        receiver::run(ctx.clone(), out.clone(), ev_tx, ev_rx, listing_rx).await
    };
    match res {
        Ok(o) => {
            let _ = writer.await;
            reader.abort();
            tracing::info!(
                files = o.files_transferred,
                bytes = o.bytes_transferred,
                errors = o.errors.len(),
                "session finished"
            );
            Ok(o)
        }
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "session failed");
            out.send(irsync_proto::v1::control::Msg::Error(Error {
                code: Code::Internal as i32,
                message: format!("{e:#}"),
            }));
            out.finish();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), writer).await;
            reader.abort();
            Err(e)
        }
    }
}

/// Runs blocking filesystem work off the async threads.
pub(super) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(io::Error::other)?
}

/// The `Ev` channel type both roles receive on.
pub(super) type Events = mpsc::UnboundedReceiver<Ev>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_root_never_passes_through_a_symlink() {
        let m = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), m.path().join("link")).unwrap();
        let rel = |s: &[u8]| RelPath::new(s).unwrap();
        assert!(prepare_root(m.path(), &rel(b"link"), true).is_err());
        assert!(prepare_root(m.path(), &rel(b"link/sub"), true).is_err());
        assert!(!outside.path().join("sub").exists());
        // Missing components are created only when receiving.
        assert!(prepare_root(m.path(), &rel(b"new/dir"), false).is_err());
        let p = prepare_root(m.path(), &rel(b"new/dir"), true).unwrap();
        assert_eq!(p, m.path().join("new/dir"));
        assert!(p.is_dir());
        assert_eq!(
            prepare_root(m.path(), &RelPath::root(), false).unwrap(),
            m.path()
        );
    }
}
