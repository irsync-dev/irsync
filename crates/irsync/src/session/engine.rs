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
use iroh::EndpointId;
use irsync_proto::v1::{error::Code, hello::Direction, Error, FileEntry, Hello, HelloAck};
use irsync_proto::PROTOCOL_VERSION;
use prost::Message;
use rand_core::{OsRng, RngCore};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

use super::control::{self, Ev};
use super::{
    receiver, sender_role, DaemonSessions, IndexScope, LocalSide, Outcome, SessionKey,
    SessionRecord, SideHooks, HASH_WORKERS,
};
use crate::cli::location::Location;
use crate::cli::{Preserve, SyncOptions};
use crate::filter::FilterSet;
use crate::fs::meta::ApplyOpts;
use crate::fs::paths::RelPath;
use crate::hasher::Hasher;
use crate::index::IndexHandle;
use crate::net::frame::{write_msg, MAX_FRAME};
use crate::send::DEFAULT_PARALLEL;

/// The daemon refused the session: its `Error` reply to `Hello`.
#[derive(Debug, Clone)]
pub struct SessionRefused {
    pub code: Code,
    pub message: String,
}

impl std::fmt::Display for SessionRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "daemon refused the session ({:?}): {}",
            self.code, self.message
        )
    }
}

impl std::error::Error for SessionRefused {}

/// The connection or the control stream failed underneath the session: the
/// session can continue on a new connection.
#[derive(Debug, Clone)]
pub struct TransportError(pub String);

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TransportError {}

/// The peer gave up on the session and said why (`Error` on the control stream).
#[derive(Debug, Clone)]
pub struct PeerError(pub String);

impl std::fmt::Display for PeerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "peer error: {}", self.0)
    }
}

impl std::error::Error for PeerError {}

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
    /// This connection continues an existing session (`HelloAck.resumed`).
    pub resumed: bool,
    /// This side's stored record of the session, when resumed and found.
    pub record: Option<SessionRecord>,
    /// Daemon: the client's EndpointId, stored with the session.
    pub remote_id: Option<Vec<u8>>,
    #[cfg(feature = "test-hooks")]
    pub fault: Option<Arc<super::FaultInjector>>,
    /// The session root: listing paths are relative to it.
    pub root: PathBuf,
    /// The root the index was opened for (`root` = `base` + `scope.prefix`).
    /// The hashers, `Sender` and `FileWriter` work on `base` + index keys.
    pub base: PathBuf,
    pub scope: IndexScope,
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

    /// The index key of a session-relative path.
    pub fn key(&self, rel: &RelPath) -> RelPath {
        self.scope.key(rel)
    }

    /// Absolute path of an index key.
    pub fn full_key(&self, key: &RelPath) -> PathBuf {
        if key.as_bytes().is_empty() {
            self.base.clone()
        } else {
            self.base.join(key.to_os())
        }
    }

    pub fn index(&self) -> &IndexHandle {
        &self.scope.handle
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
    let body = read_frame(recv)
        .await
        .map_err(|e| TransportError(format!("reading HelloAck: {e}")))?;
    if let Ok(ack) = HelloAck::decode(&body[..]) {
        if ack.session_id.len() == 16 {
            return Ok(ack);
        }
    }
    match Error::decode(&body[..]) {
        Ok(e) => Err(SessionRefused {
            code: Code::try_from(e.code).unwrap_or(Code::Unknown),
            message: e.message,
        }
        .into()),
        Err(_) => bail!("malformed reply to Hello"),
    }
}

fn remote_location(daemon: EndpointId, hello: &Hello) -> anyhow::Result<Location> {
    Ok(Location::Remote {
        host: daemon.to_string(),
        module: hello.module.clone(),
        path: RelPath::new(&hello.path).map_err(|_| anyhow::anyhow!("invalid remote path"))?,
        contents_only: hello.contents_only,
    })
}

/// The client's [`SessionKey`] for a session with `daemon` described by
/// `hello` over `local` (canonicalizes the local path, so off the async threads).
pub async fn client_session_key(
    daemon: EndpointId,
    hello: &Hello,
    local: &LocalSide,
    opts: &SyncOptions,
) -> anyhow::Result<Vec<u8>> {
    let direction = Direction::try_from(hello.direction).context("bad direction in Hello")?;
    let local_loc = Location::Local {
        path: local.root.clone(),
        contents_only: local.contents_only,
    };
    let remote_loc = remote_location(daemon, hello)?;
    let (src, dst) = match direction {
        Direction::ClientSends => (local_loc, remote_loc),
        Direction::DaemonSends => (remote_loc, local_loc),
    };
    let o = opts.clone();
    Ok(
        tokio::task::spawn_blocking(move || SessionKey::new(daemon, &src, &dst, &o).into_bytes())
            .await?,
    )
}

/// This side's stored record of session `id`, if it is there and belongs to `key`.
async fn load_record(index: &IndexHandle, id: [u8; 16], key: &[u8]) -> Option<SessionRecord> {
    let index = index.clone();
    let row = blocking(move || {
        index
            .lock()
            .map_err(|_| io::Error::other("index poisoned"))?
            .load_session_by_id(&id)
            .map_err(io::Error::other)
    })
    .await;
    match row {
        Ok(Some(row)) if row.key == key => match SessionRecord::from_row(&row) {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::warn!(error = %e, "stored session is unreadable; starting it over");
                None
            }
        },
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(error = %e, "loading the stored session failed");
            None
        }
    }
}

/// Runs a whole session as the client over `conn`: sends `hello`, then plays
/// the sender role (`CLIENT_SENDS`) or the receiver role (`DAEMON_SENDS`) on
/// `local`. The root and remote path must already follow rsync's
/// trailing-slash rule (see [`super::resolve_roots`]). A non-empty
/// `hello.session_id` asks the daemon to resume that session.
pub async fn run_client(
    conn: Connection,
    hello: Hello,
    local: LocalSide,
    opts: &SyncOptions,
) -> anyhow::Result<Outcome> {
    run_client_inner(conn, hello, local, opts, &mut None).await
}

/// [`run_client`], also reporting the session id as soon as the daemon has
/// acknowledged it (so a failed run can be resumed).
pub(super) async fn run_client_inner(
    conn: Connection,
    hello: Hello,
    local: LocalSide,
    opts: &SyncOptions,
    acked: &mut Option<[u8; 16]>,
) -> anyhow::Result<Outcome> {
    let direction = Direction::try_from(hello.direction).context("bad direction in Hello")?;
    let key = client_session_key(conn.remote_id(), &hello, &local, opts).await?;
    warn_ignored_options(direction, opts);

    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|e| TransportError(format!("opening control stream: {e}")))?;
    write_msg(&mut send, &hello)
        .await
        .map_err(|e| TransportError(format!("sending Hello: {e}")))?;
    let ack = read_hello_reply(&mut recv).await?;
    let session_id: [u8; 16] = ack.session_id.as_slice().try_into()?;
    *acked = Some(session_id);
    tracing::info!(
        session = %control::hex(&session_id),
        resumed = ack.resumed,
        ?direction,
        "session started"
    );
    let record = if ack.resumed {
        load_record(&local.index, session_id, &key).await
    } else {
        None
    };

    let sends = direction == Direction::ClientSends;
    if !sends {
        let root = local.root.clone();
        blocking(move || std::fs::create_dir_all(&root))
            .await
            .with_context(|| format!("creating {}", local.root.display()))?;
    }
    let forced = Arc::new(Hasher::new(local.index.clone(), local.root.clone(), 1).with_force(true));
    let ctx = Ctx {
        conn,
        session_id,
        key,
        resumed: ack.resumed,
        record,
        remote_id: None,
        #[cfg(feature = "test-hooks")]
        fault: None,
        base: local.root.clone(),
        root: local.root,
        scope: IndexScope::new(local.index, RelPath::root()),
        hasher: local.hasher,
        forced,
        filters: local.filters,
        opts: RoleOpts::client(opts),
        privs: Privs::current(),
        hooks: local.hooks,
    };
    run_role(Arc::new(ctx), send, recv, sends).await
}

/// v1 does not negotiate options: say so once when the user set some that
/// only the daemon side could honour.
fn warn_ignored_options(direction: Direction, o: &SyncOptions) {
    let ignored: Vec<&str> = match direction {
        Direction::DaemonSends => [(!o.filters.is_empty(), "--include/--exclude")]
            .into_iter()
            .filter_map(|(set, name)| set.then_some(name))
            .collect(),
        Direction::ClientSends => [
            (o.checksum, "--checksum"),
            (o.update_only, "--update"),
            (o.partial_dir.is_some(), "--partial-dir"),
        ]
        .into_iter()
        .filter_map(|(set, name)| set.then_some(name))
        .collect(),
    };
    if !ignored.is_empty() {
        tracing::warn!(
            options = %ignored.join(", "),
            "ignored: the daemon side applies its own defaults in this protocol version"
        );
    }
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
    shared: DaemonSessions,
) -> anyhow::Result<Outcome> {
    let direction = Direction::try_from(hello.direction).context("bad direction")?;
    // The daemon sends when the client receives.
    let sends = direction == Direction::DaemonSends;
    let rel = RelPath::new(&hello.path).map_err(|_| anyhow::anyhow!("invalid path"))?;
    let prepared = {
        let (m, r) = (module_root.clone(), rel.clone());
        blocking(move || prepare_root(&m, &r, !sends)).await
    };
    let root = match prepared {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("bad session path: {e}");
            return refuse(&conn, &mut send, Code::BadPath, msg).await;
        }
    };
    let remote = conn.remote_id();
    let key = SessionKey::for_daemon(remote, &hello).into_bytes();
    let (session_id, record, _claim) = if hello.session_id.is_empty() {
        let mut id = [0u8; 16];
        OsRng.fill_bytes(&mut id);
        let claim = shared.registry.claim(id, &conn).await?;
        (id, None, claim)
    } else {
        // Only the key that created a session may resume it, and only for
        // the same module, path and direction.
        let owned = |rec: &SessionRecord| rec.plan.remote_id == remote.as_bytes();
        let id = <[u8; 16]>::try_from(hello.session_id.as_slice()).ok();
        let found = match id {
            Some(id) => load_record(&index, id, &key).await.filter(owned),
            None => None,
        };
        let Some(id) = id.filter(|_| found.is_some()) else {
            let msg = "unknown session".to_string();
            return refuse(&conn, &mut send, Code::BadSession, msg).await;
        };
        // Stop an older connection still hosting it, then reload what it saved.
        let claim = shared.registry.claim(id, &conn).await?;
        let Some(rec) = load_record(&index, id, &key).await.filter(owned) else {
            let msg = "session finished".to_string();
            return refuse(&conn, &mut send, Code::BadSession, msg).await;
        };
        (id, Some(rec), claim)
    };
    let resumed = record.is_some();
    write_msg(
        &mut send,
        &HelloAck {
            session_id: session_id.to_vec(),
            resumed,
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await?;
    tracing::info!(
        session = %control::hex(&session_id),
        peer = %remote.fmt_short(),
        module = %hello.module,
        ?direction,
        resumed,
        "session started"
    );
    // The module's index is opened for the module root: hash and key paths
    // from there, so rows stay module-relative whatever the session path.
    let hasher = Arc::new(Hasher::new(
        index.clone(),
        module_root.clone(),
        HASH_WORKERS,
    ));
    let forced = Arc::new(Hasher::new(index.clone(), module_root.clone(), 1).with_force(true));
    let ctx = Ctx {
        conn,
        session_id,
        key,
        resumed,
        record,
        remote_id: Some(remote.as_bytes().to_vec()),
        #[cfg(feature = "test-hooks")]
        fault: shared.fault.clone(),
        root,
        base: module_root,
        scope: IndexScope::new(index, rel),
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

/// Answers `Hello` with an `Error`, waits (bounded) until the client has it,
/// and closes the connection.
async fn refuse(
    conn: &Connection,
    send: &mut SendStream,
    code: Code,
    msg: String,
) -> anyhow::Result<Outcome> {
    tracing::info!(peer = %conn.remote_id().fmt_short(), ?code, %msg, "refusing session");
    write_msg(
        send,
        &Error {
            code: code as i32,
            message: msg.clone(),
        },
    )
    .await?;
    send.finish()?;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), send.stopped()).await;
    conn.close(2u32.into(), b"rejected");
    bail!(msg);
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
