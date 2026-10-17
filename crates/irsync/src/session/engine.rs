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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::EndpointId;
use irsync_proto::v1::hello::{DeleteMode as WireDeleteMode, Direction};
use irsync_proto::v1::{error::Code, Error, FileEntry, Hello, HelloAck};
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
use crate::cli::{DeleteMode, Preserve, SyncOptions};
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
    /// Receiver only: when (and whether) to delete extraneous paths.
    pub delete: DeleteMode,
    pub delete_excluded: bool,
    pub max_delete: Option<u64>,
    /// Receiver only: decide and report, change nothing.
    pub dry_run: bool,
    /// Delete even when a listing was incomplete.
    pub ignore_errors: bool,
    /// `-i`: a receiving daemon sends `Itemized` frames.
    pub itemize: bool,
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
            delete: o.delete,
            delete_excluded: o.delete_excluded,
            max_delete: o.max_delete,
            dry_run: o.dry_run,
            ignore_errors: o.ignore_errors,
            itemize: o.itemize,
        }
    }

    /// v1: transfer options are not negotiated, so the daemon preserves
    /// everything it can. Only the receiver-side options carried in `Hello`
    /// (deletion, dry run, `-u`, `-c`, itemize) are honoured, and only when
    /// the daemon `receives` (the client pushes, which already requires
    /// write access to a writable module).
    fn daemon(hello: &Hello, receives: bool) -> RoleOpts {
        let delete = match hello.delete_mode() {
            _ if !receives => DeleteMode::Off,
            WireDeleteMode::DeleteOff => DeleteMode::Off,
            WireDeleteMode::DeleteBefore => DeleteMode::Before,
            WireDeleteMode::DeleteDuring => DeleteMode::During,
            WireDeleteMode::DeleteAfter => DeleteMode::After,
        };
        RoleOpts {
            preserve: Preserve {
                recursive: true,
                links: true,
                perms: true,
                times: true,
                group: true,
                owner: true,
                devices: true,
                hard_links: true,
                sparse: true,
            },
            checksum: receives && hello.checksum,
            update_only: receives && hello.update_only,
            rename_detection: true,
            partial_dir: None,
            parallel: DEFAULT_PARALLEL,
            bwlimit: None,
            delete,
            delete_excluded: receives && hello.delete_excluded,
            max_delete: hello.max_delete,
            dry_run: receives && hello.dry_run,
            ignore_errors: hello.ignore_errors,
            itemize: hello.itemize,
        }
    }
}

/// Puts the client's receiver-side options into `Hello` (Ruling 19): the
/// daemon honours them when it receives.
pub fn set_hello_options(hello: &mut Hello, o: &SyncOptions) {
    hello.delete_mode = match o.delete {
        DeleteMode::Off => WireDeleteMode::DeleteOff,
        DeleteMode::Before => WireDeleteMode::DeleteBefore,
        DeleteMode::During => WireDeleteMode::DeleteDuring,
        DeleteMode::After => WireDeleteMode::DeleteAfter,
    } as i32;
    hello.delete_excluded = o.delete_excluded;
    hello.max_delete = o.max_delete;
    hello.dry_run = o.dry_run;
    hello.ignore_errors = o.ignore_errors;
    // `-n -v` lists what a push would transfer: only the daemon knows.
    hello.itemize = o.itemize || (o.dry_run && o.verbose > 0);
    hello.update_only = o.update_only;
    hello.checksum = o.checksum;
}

/// Option combinations this protocol version cannot honour safely.
///
/// Filters are not sent to the daemon, so a receiving daemon would see every
/// destination path the client's filters exclude as extraneous and delete
/// it: with `--delete` that breaks rsync's rule that excluded files are
/// protected. Only `--delete-excluded` (which deletes them anyway) is allowed.
///
/// `--partial-dir` names a directory on the receiving side: on a push that
/// is the daemon's filesystem, which a client must not choose.
pub fn check_options(direction: Direction, o: &SyncOptions) -> anyhow::Result<()> {
    if direction == Direction::ClientSends && o.partial_dir.is_some() {
        bail!(
            "--partial-dir is not supported when pushing: it would name a directory on the \
             daemon's side"
        );
    }
    if direction == Direction::ClientSends
        && o.delete != DeleteMode::Off
        && !o.filters.is_empty()
        && !o.delete_excluded
    {
        bail!(
            "--delete with --include/--exclude is not supported when pushing: the daemon \
             does not see the filters and would delete excluded files; add \
             --delete-excluded to delete them deliberately"
        );
    }
    Ok(())
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
    /// Set once file data crossed or a file was acknowledged DONE.
    pub progress: Arc<AtomicBool>,
    /// Bumped for every control frame received, every data chunk moved, and
    /// local progress (hashing, chunk writes, finalize, deletions).
    pub activity: Arc<AtomicU64>,
    /// Drop the connection after this long without activity.
    pub idle_timeout: Option<Duration>,
    #[cfg(feature = "test-hooks")]
    pub fault: Option<Arc<super::FaultInjector>>,
    /// Counts `NeedChunks` re-requests (test hook).
    #[cfg(feature = "test-hooks")]
    pub rerequests: Option<Arc<std::sync::atomic::AtomicU64>>,
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
    run_client_inner(conn, hello, local, opts, &mut Attempt::default()).await
}

/// What one connection of a client session achieved, known even when it failed.
#[derive(Debug, Default)]
pub(super) struct Attempt {
    /// The session id, once the daemon acknowledged it (so a failed run can be resumed).
    pub acked: Option<[u8; 16]>,
    /// Set once file data crossed or a file was acknowledged DONE.
    pub progress: Arc<AtomicBool>,
}

/// [`run_client`], also reporting what the connection achieved in `attempt`.
pub(super) async fn run_client_inner(
    conn: Connection,
    hello: Hello,
    local: LocalSide,
    opts: &SyncOptions,
    attempt: &mut Attempt,
) -> anyhow::Result<Outcome> {
    let mut hello = hello;
    let direction = Direction::try_from(hello.direction).context("bad direction in Hello")?;
    check_options(direction, opts)?;
    set_hello_options(&mut hello, opts);
    let key = client_session_key(conn.remote_id(), &hello, &local, opts).await?;
    warn_ignored_options(direction, opts);

    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|e| TransportError(format!("opening control stream: {e}")))?;
    write_msg(&mut send, &hello)
        .await
        .map_err(|e| TransportError(format!("sending Hello: {e}")))?;
    let ack = match opts.timeout.filter(|t| !t.is_zero()) {
        Some(t) => tokio::time::timeout(t, read_hello_reply(&mut recv))
            .await
            .map_err(|_| TransportError(format!("no reply to Hello within {} s", t.as_secs())))??,
        None => read_hello_reply(&mut recv).await?,
    };
    let session_id: [u8; 16] = ack.session_id.as_slice().try_into()?;
    attempt.acked = Some(session_id);
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
    if !sends && !opts.dry_run {
        let root = local.root.clone();
        blocking(move || std::fs::create_dir_all(&root))
            .await
            .with_context(|| format!("creating {}", local.root.display()))?;
    }
    // Local work (hashing, writing, finalizing) bumps the same marker the
    // inactivity watchdog reads, as received and sent traffic does.
    let activity = local.hasher.activity().clone();
    let forced = Arc::new(
        Hasher::new(local.index.clone(), local.root.clone(), 1)
            .with_force(true)
            .with_activity(activity.clone()),
    );
    let ctx = Ctx {
        conn,
        session_id,
        key,
        resumed: ack.resumed,
        record,
        remote_id: None,
        progress: attempt.progress.clone(),
        activity,
        idle_timeout: opts.timeout.filter(|t| !t.is_zero()),
        #[cfg(feature = "test-hooks")]
        fault: None,
        #[cfg(feature = "test-hooks")]
        rerequests: None,
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

/// v1 does not send filters: say so once when a pull sets some that only
/// the daemon's (sending) walk could honour.
fn warn_ignored_options(direction: Direction, o: &SyncOptions) {
    if direction == Direction::DaemonSends && !o.filters.is_empty() {
        tracing::warn!(
            options = "--include/--exclude",
            "ignored: the daemon side applies its own defaults in this protocol version"
        );
    }
}

/// What [`prepare_root`] does with missing components of the session root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Missing {
    /// Sending: the root must exist.
    Refuse,
    /// Receiving: create them.
    Create,
    /// Receiving on a dry run: leave them missing (the receiver lists nothing).
    Allow,
}

/// Checks the session root below the module root: every existing component
/// must be a real directory (never a symlink, which could lead outside the
/// module); missing ones are handled as `missing` says.
fn prepare_root(module_root: &Path, rel: &RelPath, missing: Missing) -> io::Result<PathBuf> {
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
            match missing {
                Missing::Refuse => {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("{} does not exist", p.display()),
                    ))
                }
                Missing::Create => std::fs::create_dir(&p)?,
                Missing::Allow => {
                    // Nothing below a missing component can exist.
                    p = module_root.to_path_buf();
                    p.push(rel.to_os());
                    return Ok(p);
                }
            }
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
    let missing = match (sends, hello.dry_run) {
        (true, _) => Missing::Refuse,
        (false, false) => Missing::Create,
        (false, true) => Missing::Allow,
    };
    let prepared = {
        let (m, r) = (module_root.clone(), rel.clone());
        blocking(move || prepare_root(&m, &r, missing)).await
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
    #[cfg(feature = "test-hooks")]
    if shared.fault.as_ref().is_some_and(|f| f.drops_after_ack()) {
        let mut recv = recv;
        let mut byte = [0u8; 1];
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            AsyncReadExt::read(&mut recv, &mut byte),
        )
        .await;
        tracing::warn!("fault injection: dropping the connection after HelloAck");
        conn.close(99u32.into(), b"fault");
        bail!("fault injection");
    }
    #[cfg(feature = "test-hooks")]
    if shared.fault.as_ref().is_some_and(|f| f.take_stall()) {
        tracing::warn!("fault injection: stalling after HelloAck");
        conn.closed().await;
        bail!("fault injection: stalled");
    }
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
    // Local work (hashing, writing, finalizing) bumps the same marker the
    // inactivity watchdog reads, as received and sent traffic does.
    let activity = Arc::new(AtomicU64::new(0));
    let hasher = Arc::new(
        Hasher::new(index.clone(), module_root.clone(), HASH_WORKERS)
            .with_activity(activity.clone()),
    );
    let forced = Arc::new(
        Hasher::new(index.clone(), module_root.clone(), 1)
            .with_force(true)
            .with_activity(activity.clone()),
    );
    let ctx = Ctx {
        conn,
        session_id,
        key,
        resumed,
        record,
        remote_id: Some(remote.as_bytes().to_vec()),
        progress: Arc::default(),
        activity,
        idle_timeout: Some(shared.idle_timeout.unwrap_or(DAEMON_IDLE_TIMEOUT))
            .filter(|t| !t.is_zero()),
        #[cfg(feature = "test-hooks")]
        fault: shared.fault.clone(),
        #[cfg(feature = "test-hooks")]
        rerequests: Some(shared.rerequests.clone()),
        root,
        base: module_root,
        scope: IndexScope::new(index, rel),
        hasher,
        forced,
        // v1: the client's filters are not transmitted; the daemon walks everything.
        filters: Arc::new(FilterSet::new(vec![])),
        opts: RoleOpts::daemon(&hello, !sends),
        privs: Privs::current(),
        hooks: SideHooks {
            #[cfg(feature = "test-hooks")]
            events: Some(shared.events.clone()),
            ..SideHooks::default()
        },
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
    let reader = control::spawn_reader(
        recv,
        (!sends).then_some(listing_tx),
        ev_tx.clone(),
        ctx.hooks.bus.clone(),
        ctx.activity.clone(),
    );
    let idle = Arc::new(AtomicBool::new(false));
    let watchdog = ctx
        .idle_timeout
        .map(|t| tokio::spawn(watch_idle(ctx.clone(), t, idle.clone())));
    let res = if sends {
        sender_role::run(ctx.clone(), out.clone(), ev_tx, ev_rx).await
    } else {
        receiver::run(ctx.clone(), out.clone(), ev_tx, ev_rx, listing_rx).await
    };
    if let Some(w) = watchdog {
        w.abort();
    }
    // The role failed because the watchdog closed the connection: a
    // transport failure (the client reconnects and resumes).
    let res = match res {
        Err(e) if idle.load(Ordering::SeqCst) => {
            let secs = ctx.idle_timeout.unwrap_or_default().as_secs();
            Err(anyhow::Error::new(TransportError(format!(
                "no activity for {secs} s: {e:#}"
            ))))
        }
        r => r,
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

/// How long a daemon session may go without activity before the daemon
/// drops the connection (the client can resume), unless the daemon config
/// sets `idle_timeout`.
pub const DAEMON_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Close code for a connection dropped for inactivity.
const CLOSE_IDLE: u32 = 4;

/// Closes the connection once `ctx.activity` has not moved for `timeout`
/// (no control frame received, no data chunk moved, no local progress such
/// as hashing, writing, finalizing or deleting), and sets `fired`.
async fn watch_idle(ctx: Arc<Ctx>, timeout: Duration, fired: Arc<AtomicBool>) {
    let tick = (timeout / 4).clamp(Duration::from_millis(50), Duration::from_secs(1));
    let mut seen = ctx.activity.load(Ordering::Relaxed);
    let mut since = tokio::time::Instant::now();
    loop {
        tokio::time::sleep(tick).await;
        let now = ctx.activity.load(Ordering::Relaxed);
        if now != seen {
            (seen, since) = (now, tokio::time::Instant::now());
        } else if since.elapsed() >= timeout {
            tracing::warn!(
                secs = timeout.as_secs(),
                "no activity; dropping the connection"
            );
            fired.store(true, Ordering::SeqCst);
            ctx.conn.close(CLOSE_IDLE.into(), b"idle");
            return;
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
        assert!(prepare_root(m.path(), &rel(b"link"), Missing::Create).is_err());
        assert!(prepare_root(m.path(), &rel(b"link/sub"), Missing::Create).is_err());
        assert!(prepare_root(m.path(), &rel(b"link/sub"), Missing::Allow).is_err());
        assert!(!outside.path().join("sub").exists());
        // Missing components are created only when receiving, and not on a dry run.
        assert!(prepare_root(m.path(), &rel(b"new/dir"), Missing::Refuse).is_err());
        let p = prepare_root(m.path(), &rel(b"new/dir"), Missing::Allow).unwrap();
        assert_eq!(p, m.path().join("new/dir"));
        assert!(!m.path().join("new").exists());
        let p = prepare_root(m.path(), &rel(b"new/dir"), Missing::Create).unwrap();
        assert_eq!(p, m.path().join("new/dir"));
        assert!(p.is_dir());
        assert_eq!(
            prepare_root(m.path(), &RelPath::root(), Missing::Refuse).unwrap(),
            m.path()
        );
    }
}
