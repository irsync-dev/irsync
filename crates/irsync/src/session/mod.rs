//! Sync sessions: the session key, the persisted record, the local side of a
//! session, and its outcome. The protocol roles live in [`engine`].

mod control;
pub mod engine;
mod limits;
mod listing_stream;
mod parts;
mod planner;
mod receiver;
pub mod reconnect;
mod recv_io;
mod sender_role;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::Connection;
use iroh::EndpointId;
use irsync_proto::v1::{Hello, SessionPlan, SessionProgress};
use prost::Message;

use crate::cli::location::Location;
use crate::cli::{DeleteMode, Direction, FilterRule, SyncOptions};
use crate::filter::FilterSet;
use crate::fs::paths::RelPath;
use crate::hasher::Hasher;
use crate::index::{IndexHandle, SessionRow};
use crate::telemetry::meter::{Meter, Usage};

/// Files hashed concurrently by a session's hasher.
pub const HASH_WORKERS: usize = 4;

/// Identity of a session: BLAKE3 over a canonical byte string of everything
/// that changes what the session does (daemon, both locations, and the
/// semantics-affecting flags). Equal tuples give equal keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionKey(Vec<u8>);

/// Appends one length-prefixed field to the canonical encoding.
fn field(h: &mut blake3::Hasher, tag: &[u8], v: &[u8]) {
    h.update(&(tag.len() as u64).to_le_bytes());
    h.update(tag);
    h.update(&(v.len() as u64).to_le_bytes());
    h.update(v);
}

fn location_bytes(l: &Location) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    let mut v = Vec::new();
    match l {
        Location::Local {
            path,
            contents_only,
        } => {
            let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
            v.extend_from_slice(b"local\0");
            v.push(*contents_only as u8);
            v.extend_from_slice(canon.as_os_str().as_bytes());
        }
        // The host string is an alias; the daemon id passed alongside identifies it.
        Location::Remote {
            path,
            contents_only,
            ..
        } => {
            v.extend_from_slice(b"remote\0");
            v.push(*contents_only as u8);
            v.extend_from_slice(path);
        }
    }
    v
}

impl SessionKey {
    pub fn new(
        daemon: EndpointId,
        src: &Location,
        dst: &Location,
        opts: &SyncOptions,
    ) -> SessionKey {
        let mut h = blake3::Hasher::new();
        field(&mut h, b"irsync-session-key", b"v1");
        field(&mut h, b"daemon", daemon.as_bytes());
        field(&mut h, b"src", &location_bytes(src));
        field(&mut h, b"dst", &location_bytes(dst));
        let p = &opts.preserve;
        let bits = [
            p.recursive,
            p.dirs,
            p.links,
            p.perms,
            p.times,
            p.group,
            p.owner,
            p.devices,
            p.specials,
            p.hard_links,
            p.sparse,
        ]
        .iter()
        .enumerate()
        .fold(0u16, |acc, (i, &b)| acc | ((b as u16) << i));
        field(&mut h, b"preserve", &bits.to_le_bytes());
        field(&mut h, b"checksum", &[opts.checksum as u8]);
        field(&mut h, b"update", &[opts.update_only as u8]);
        field(&mut h, b"inplace", &[opts.inplace as u8]);
        field(&mut h, b"append", &[opts.append as u8]);
        let delete = match opts.delete {
            DeleteMode::Off => 0u8,
            DeleteMode::Before => 1,
            DeleteMode::During => 2,
            DeleteMode::After => 3,
        };
        field(&mut h, b"delete", &[delete, opts.delete_excluded as u8]);
        // A dry run's record must never be resumed by a real run (or back).
        field(&mut h, b"dry-run", &[opts.dry_run as u8]);
        for r in &opts.filters {
            use std::os::unix::ffi::OsStrExt;
            match r {
                FilterRule::Include(s) => field(&mut h, b"include", s.as_bytes()),
                FilterRule::Exclude(s) => field(&mut h, b"exclude", s.as_bytes()),
                FilterRule::IncludeFrom(p) => {
                    field(&mut h, b"include-from", p.as_os_str().as_bytes())
                }
                FilterRule::ExcludeFrom(p) => {
                    field(&mut h, b"exclude-from", p.as_os_str().as_bytes())
                }
            }
        }
        SessionKey(h.finalize().as_bytes().to_vec())
    }

    /// The daemon's key for a session: the client's identity plus the
    /// resolved place (`export` and `rel`, so every spelling of it resumes
    /// the same session) and the flags.
    pub fn for_daemon(
        client: EndpointId,
        export: &str,
        rel: &RelPath,
        hello: &Hello,
    ) -> SessionKey {
        let mut h = blake3::Hasher::new();
        field(&mut h, b"irsync-daemon-session-key", b"v1");
        field(&mut h, b"client", client.as_bytes());
        field(&mut h, b"export", export.as_bytes());
        field(&mut h, b"path", rel.as_bytes());
        field(
            &mut h,
            b"flags",
            &[hello.direction as u8, hello.contents_only as u8],
        );
        field(
            &mut h,
            b"receiver",
            &[
                hello.delete_mode as u8,
                hello.delete_excluded as u8,
                hello.dry_run as u8,
                hello.update_only as u8,
                hello.checksum as u8,
                hello.inplace as u8,
                hello.append as u8,
            ],
        );
        SessionKey(h.finalize().as_bytes().to_vec())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

/// A session as persisted in the index: plan and progress are protobuf
/// messages (`proto/irsync/v1/session.proto`) stored as opaque blobs.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRecord {
    pub id: [u8; 16],
    pub key: Vec<u8>,
    pub plan: SessionPlan,
    pub progress: SessionProgress,
    pub created_at: i64,
    pub updated_at: i64,
}

impl SessionRecord {
    pub fn to_row(&self) -> SessionRow {
        SessionRow {
            id: self.id,
            key: self.key.clone(),
            plan: self.plan.encode_to_vec(),
            progress: self.progress.encode_to_vec(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }

    pub fn from_row(row: &SessionRow) -> Result<SessionRecord, prost::DecodeError> {
        Ok(SessionRecord {
            id: row.id,
            key: row.key.clone(),
            plan: SessionPlan::decode(&row.plan[..])?,
            progress: SessionProgress::decode(&row.progress[..])?,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

/// The index a session uses, and where the session root sits below the root
/// the index was opened for. Index rows are always relative to that index
/// root (the export root on the daemon), so `key` maps session-relative paths
/// to index keys and `strip` maps index keys back.
#[derive(Clone)]
pub struct IndexScope {
    pub handle: IndexHandle,
    pub prefix: RelPath,
}

impl IndexScope {
    pub fn new(handle: IndexHandle, prefix: RelPath) -> IndexScope {
        IndexScope { handle, prefix }
    }

    /// The index key for a session-relative path.
    pub fn key(&self, rel: &RelPath) -> RelPath {
        join_rel(&self.prefix, rel)
    }

    /// The session-relative path of an index key, if it lies inside the session root.
    pub fn strip(&self, key: &RelPath) -> Option<RelPath> {
        let (k, p) = (key.as_bytes(), self.prefix.as_bytes());
        if p.is_empty() {
            return Some(key.clone());
        }
        match k.strip_prefix(p) {
            Some([]) => Some(RelPath::root()),
            Some([b'/', rest @ ..]) => RelPath::new(rest).ok(),
            _ => None,
        }
    }
}

/// `a/b` for two canonical relative paths (either may be the root).
fn join_rel(a: &RelPath, b: &RelPath) -> RelPath {
    match (a.as_bytes(), b.as_bytes()) {
        ([], _) => b.clone(),
        (_, []) => a.clone(),
        (x, y) => {
            let mut v = x.to_vec();
            v.push(b'/');
            v.extend_from_slice(y);
            RelPath::new(&v).expect("joining canonical paths stays canonical")
        }
    }
}

/// Close code for a connection whose session was taken over by a newer
/// connection resuming it.
pub const CLOSE_SUPERSEDED: u32 = 3;
/// How long a resuming connection waits for the session's previous host to stop.
const TAKEOVER_TIMEOUT: Duration = Duration::from_secs(30);

struct Hosted {
    conn: Connection,
    lock: Arc<tokio::sync::Mutex<()>>,
}

/// The sessions a daemon is hosting. A session id runs on at most one
/// connection: a resuming connection closes the previous one and waits until
/// its role has stopped (and persisted) before loading the session.
#[derive(Default)]
pub struct SessionRegistry {
    map: std::sync::Mutex<HashMap<[u8; 16], Hosted>>,
    /// Every id hosted so far, in order (test hook).
    #[cfg(feature = "test-hooks")]
    pub hosted: Arc<std::sync::Mutex<Vec<[u8; 16]>>>,
}

/// Holding this means the session runs on this connection only.
pub struct SessionClaim {
    registry: Arc<SessionRegistry>,
    id: [u8; 16],
    conn_id: usize,
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

impl Drop for SessionClaim {
    fn drop(&mut self) {
        let mut map = self.registry.map.lock().unwrap_or_else(|e| e.into_inner());
        if map
            .get(&self.id)
            .is_some_and(|h| h.conn.stable_id() == self.conn_id)
        {
            map.remove(&self.id);
        }
    }
}

impl SessionRegistry {
    /// Makes `conn` the host of session `id`, closing any earlier host and
    /// waiting (bounded) until it has let go.
    pub async fn claim(
        self: &Arc<Self>,
        id: [u8; 16],
        conn: &Connection,
    ) -> anyhow::Result<SessionClaim> {
        let (previous, lock) = {
            let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
            let lock = map.get(&id).map(|h| h.lock.clone()).unwrap_or_default();
            let previous = map.insert(
                id,
                Hosted {
                    conn: conn.clone(),
                    lock: lock.clone(),
                },
            );
            (previous.map(|h| h.conn), lock)
        };
        if let Some(old) = previous {
            tracing::info!(session = %control::hex(&id), "taking over the session from an older connection");
            old.close(CLOSE_SUPERSEDED.into(), b"superseded");
        }
        let guard = tokio::time::timeout(TAKEOVER_TIMEOUT, lock.lock_owned())
            .await
            .map_err(|_| anyhow::anyhow!("the session's previous connection did not stop"))?;
        #[cfg(feature = "test-hooks")]
        self.hosted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(id);
        Ok(SessionClaim {
            registry: self.clone(),
            id,
            conn_id: conn.stable_id(),
            _guard: guard,
        })
    }
}

/// Test hook: closes the connection (code 99) once the daemon's data bytes
/// (received, as receiver; written, as sender) cross a limit — exactly once.
#[cfg(feature = "test-hooks")]
#[derive(Debug, Default)]
pub struct FaultInjector {
    limit: std::sync::atomic::AtomicU64,
    armed: std::sync::atomic::AtomicBool,
    fired: std::sync::atomic::AtomicBool,
    after_ack: std::sync::atomic::AtomicBool,
    stall: std::sync::atomic::AtomicBool,
}

#[cfg(feature = "test-hooks")]
impl FaultInjector {
    pub fn drop_after_bytes(&self, n: u64) {
        use std::sync::atomic::Ordering::SeqCst;
        self.limit.store(n, SeqCst);
        self.armed.store(true, SeqCst);
    }

    /// Every session from now on: close the connection as soon as the
    /// client has the `HelloAck` (its first frame after it arrives), before
    /// any file data moves. Repeatedly, not once.
    pub fn drop_after_ack(&self) {
        self.after_ack
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn drops_after_ack(&self) -> bool {
        self.after_ack.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The next session only: after `HelloAck`, send and read nothing more
    /// (the connection stays open) until the client gives up on it.
    pub fn stall_after_ack_once(&self) {
        self.stall.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn take_stall(&self) -> bool {
        self.stall.swap(false, std::sync::atomic::Ordering::SeqCst)
    }

    pub fn fired(&self) -> bool {
        self.fired.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Called with a connection's running byte count.
    pub(crate) fn observe(&self, total: u64, conn: &Connection) {
        use std::sync::atomic::Ordering::SeqCst;
        if self.armed.load(SeqCst)
            && total >= self.limit.load(SeqCst)
            && !self.fired.swap(true, SeqCst)
        {
            tracing::warn!(total, "fault injection: dropping the connection");
            conn.close(99u32.into(), b"fault");
        }
    }
}

/// What the daemon shares with every session it serves.
#[derive(Clone, Default)]
pub struct DaemonSessions {
    pub registry: Arc<SessionRegistry>,
    /// How long an authenticated session may go without activity (default
    /// [`engine::DAEMON_IDLE_TIMEOUT`]; zero disables the limit).
    pub idle_timeout: Option<Duration>,
    #[cfg(feature = "test-hooks")]
    pub fault: Option<Arc<FaultInjector>>,
    /// `NeedChunks` re-requests sent by daemon receivers (test hook).
    #[cfg(feature = "test-hooks")]
    pub rerequests: Arc<std::sync::atomic::AtomicU64>,
    /// The event log of every daemon receiver (test hook).
    #[cfg(feature = "test-hooks")]
    pub events: EventLog,
    /// Every daemon receiver's finalize takes this much longer (test hook,
    /// settable after the daemon is built).
    #[cfg(feature = "test-hooks")]
    pub slow_finalize: Arc<std::sync::Mutex<Option<Duration>>>,
    /// Streams a daemon sender runs at once, instead of the default (test
    /// hook; zero keeps the default). Bounded, a byte-count fault lands with
    /// some files complete and others still to come.
    #[cfg(feature = "test-hooks")]
    pub sender_parallel: Arc<std::sync::atomic::AtomicUsize>,
    /// Hooks for every daemon session (test hook, settable after the daemon
    /// is built).
    #[cfg(feature = "test-hooks")]
    pub daemon_hooks: Arc<std::sync::Mutex<DaemonHooks>>,
}

/// The hooks a test sets on a daemon's side of a session (see [`SideHooks`]
/// for each).
#[cfg(feature = "test-hooks")]
#[derive(Clone, Default)]
pub struct DaemonHooks {
    pub slow_plan: Option<Duration>,
    pub max_inflight: Option<usize>,
    pub listing_peak: Option<Arc<std::sync::atomic::AtomicUsize>>,
    pub listing_waits: Option<Arc<std::sync::atomic::AtomicU64>>,
    pub peer_events_peak: Option<Arc<std::sync::atomic::AtomicUsize>>,
    pub slow_peer_events: Option<Duration>,
    pub out_peak: Option<Arc<std::sync::atomic::AtomicUsize>>,
    pub basis_budget: Option<usize>,
    pub basis_peak: Option<Arc<std::sync::atomic::AtomicUsize>>,
    pub list_budget: Option<u64>,
    pub list_peak: Option<Arc<std::sync::atomic::AtomicU64>>,
}

/// What a receiver did, in order: `delete:<path>` per deletion and
/// `plan:<path>` per requested file (test hook).
pub type EventLog = Arc<std::sync::Mutex<Vec<String>>>;

/// Called by the sender role after the first chunk of a path went out, before
/// the rest; lets a test change the file mid-transfer.
#[cfg(feature = "test-hooks")]
pub type FirstChunkHook =
    Arc<dyn Fn(&RelPath) -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

/// Test hook: runs at a point in a file's handling (see [`SideHooks`]).
#[cfg(feature = "test-hooks")]
pub type PathHook = Arc<dyn Fn(&RelPath) -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

/// Which side of the session an event describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Party {
    Local,
    Peer,
}

/// Something worth telling the user about, emitted as a session runs.
/// Events are best-effort: a slow reporter loses some (`Lagged`), and the
/// final [`Outcome`] is authoritative for totals.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A data stream for this file is open (both roles); its size.
    FileStart(RelPath, u64),
    /// A hasher started reading this file; its size.
    FileHashing(RelPath, u64),
    /// The hasher finished reading this file.
    FileHashed(RelPath),
    /// Literal bytes moved on a data stream for this file.
    FileBytes(RelPath, u64),
    /// Bytes of this file that need no transfer (matched basis chunks,
    /// local copies). Absolute: the latest value replaces the one before
    /// (the sender re-reports it as more of the file is requested).
    FileMatched(RelPath, u64),
    /// The sender finished all data streams for this file; the receiver's
    /// ack is still outstanding.
    FileSent(RelPath),
    /// A file was placed (receiver) or acknowledged (sender); its size.
    FileDone(RelPath, u64),
    /// The planner found this regular file up to date; its size.
    Skipped(RelPath, u64),
    /// Bytes of source files the peer's planner skipped so far (cumulative).
    PeerSkipped(u64),
    /// File data bytes moved on a data stream (aggregate, as before).
    Bytes(u64),
    /// Listing progress for one side.
    Scanned {
        party: Party,
        entries: u64,
        bytes: u64,
        at: Option<RelPath>,
    },
    /// One side's listing is complete: regular files and their total size.
    Total {
        party: Party,
        files: u64,
        bytes: u64,
    },
    /// Deletion pass progress for one side.
    Delete {
        party: Party,
        done: u64,
        total: Option<u64>,
    },
    /// A new session (one source) starts: its 0-based index and the count.
    SessionStart {
        index: usize,
        of: usize,
    },
    /// A session (one source) ended, well or not: where its time went.
    SessionEnd(std::sync::Arc<crate::cli::progress::summary::SessionSummary>),
    Conn(reconnect::ConnState),
    Deleted(RelPath),
    /// `--dry-run`: this path would be deleted.
    WouldDelete(RelPath),
    /// rsync's `-i` code for an item (see [`crate::plan::itemize`]).
    Itemized(RelPath, String),
    /// A hard link `path` to `target` was made.
    Hardlinked(RelPath, RelPath),
    /// This file failed; the message is the one the [`Outcome`] records.
    Failed(RelPath, String),
}

/// The channel events travel on. Sending never blocks; with nobody
/// listening it is a cheap no-op.
#[derive(Clone)]
pub struct EventBus(tokio::sync::broadcast::Sender<Event>);

impl EventBus {
    pub fn new(capacity: usize) -> EventBus {
        EventBus(tokio::sync::broadcast::channel(capacity).0)
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Event> {
        self.0.subscribe()
    }

    pub fn emit(&self, ev: Event) {
        if self.0.receiver_count() > 0 {
            let _ = self.0.send(ev);
        }
    }
}

impl Default for EventBus {
    fn default() -> Self {
        EventBus::new(4096)
    }
}

/// This side's progress the peer cannot see, carried on `Heartbeat`.
#[derive(Default)]
pub struct Busy {
    /// The directory this side's listing walk is in, while it walks.
    pub walking: std::sync::Mutex<Option<RelPath>>,
    /// Bytes of source files this side's planner found up to date.
    pub skipped_bytes: std::sync::atomic::AtomicU64,
    /// Paths this side's deletion pass removed so far.
    pub deleted: std::sync::atomic::AtomicU64,
    /// The deletion pass's candidate count once known, else 0.
    pub delete_total: std::sync::atomic::AtomicU64,
}

impl Busy {
    /// The frame that tells the peer where this side is.
    pub fn heartbeat(&self) -> irsync_proto::v1::Heartbeat {
        use std::sync::atomic::Ordering::Relaxed;
        let walking = self
            .walking
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|p| p.as_bytes().to_vec());
        irsync_proto::v1::Heartbeat {
            walking,
            skipped_bytes: self.skipped_bytes.load(Relaxed),
            deleted: self.deleted.load(Relaxed),
            delete_total: self.delete_total.load(Relaxed),
        }
    }
}

/// Test-only hooks (plus the event bus); the hooks are empty in normal builds.
#[derive(Clone, Default)]
pub struct SideHooks {
    /// Progress events for the user interface.
    pub bus: EventBus,
    #[cfg(feature = "test-hooks")]
    pub on_first_chunk_sent: Option<FirstChunkHook>,
    /// Cuts data streams short (see [`crate::send::TruncateHook`]).
    #[cfg(feature = "test-hooks")]
    pub truncate_streams: Option<Arc<crate::send::TruncateHook>>,
    /// Receives the receiver role's events (see [`EventLog`]).
    #[cfg(feature = "test-hooks")]
    pub events: Option<EventLog>,
    /// The sender role lists these extra entries after an entry (crafts a
    /// hostile listing).
    #[cfg(feature = "test-hooks")]
    pub inject_listing: Option<ListingHook>,
    /// Receiver: cap on files in flight instead of the default 4096.
    #[cfg(feature = "test-hooks")]
    pub max_inflight: Option<usize>,
    /// Receiver: each file's planning takes this much longer (a slow
    /// planner, so the peer's listing backs up).
    #[cfg(feature = "test-hooks")]
    pub slow_plan: Option<std::time::Duration>,
    /// Receiver: the most peer-listing entries it has had queued for the
    /// planner at once.
    #[cfg(feature = "test-hooks")]
    pub listing_peak: Option<Arc<std::sync::atomic::AtomicUsize>>,
    /// Lister: counts the times the listing queue was full when the walk
    /// had a frame to queue (the walk waited for the link or the peer).
    #[cfg(feature = "test-hooks")]
    pub listing_waits: Option<Arc<std::sync::atomic::AtomicU64>>,
    /// Both roles: the most peer events (control frames, listing signals)
    /// the stream readers have had queued for the main loop at once.
    #[cfg(feature = "test-hooks")]
    pub peer_events_peak: Option<Arc<std::sync::atomic::AtomicUsize>>,
    /// Both roles: the main loop takes this much longer per peer event (a
    /// slow main loop, so the peer's frames back up).
    #[cfg(feature = "test-hooks")]
    pub slow_peer_events: Option<std::time::Duration>,
    /// Both roles: the most bytes of paced control frames (see
    /// `Out::send_paced`) queued for the control stream's writer at once.
    #[cfg(feature = "test-hooks")]
    pub out_peak: Option<Arc<std::sync::atomic::AtomicUsize>>,
    /// Both roles: bytes of chunk bases (see `limits::BASIS_BUDGET`) the
    /// session holds at once, instead of the default.
    #[cfg(feature = "test-hooks")]
    pub basis_budget: Option<usize>,
    /// Sender: the most bytes of chunk bases it has held at once.
    #[cfg(feature = "test-hooks")]
    pub basis_peak: Option<Arc<std::sync::atomic::AtomicUsize>>,
    /// Both roles: chunk records of room for lists (see
    /// `limits::LIST_BUDGET`) each connection starts with, instead of the
    /// default.
    #[cfg(feature = "test-hooks")]
    pub list_budget: Option<u64>,
    /// Receiver: the most chunk records of lists it has held at once.
    #[cfg(feature = "test-hooks")]
    pub list_peak: Option<Arc<std::sync::atomic::AtomicU64>>,
    /// Receiver: each `Done::Planned` reaches the main loop this much later
    /// than the planner posts it (a completion hidden behind another
    /// producer's half-written slot), still before the planner finishes.
    #[cfg(feature = "test-hooks")]
    pub delay_planned: Option<std::time::Duration>,
    /// Receiver: the most files it has had in flight at once.
    #[cfg(feature = "test-hooks")]
    pub inflight_peak: Option<Arc<std::sync::atomic::AtomicUsize>>,
    /// Receiver: every finalize takes this much longer (slow storage).
    #[cfg(feature = "test-hooks")]
    pub slow_finalize: Option<std::time::Duration>,
    /// Receiver: every local chunk copy takes this much longer (slow disk).
    #[cfg(feature = "test-hooks")]
    pub slow_local_copy: Option<std::time::Duration>,
    /// Receiver: runs once a `DELTA` file's content has proved unchanged,
    /// before the destination is checked to still be the file that was
    /// hashed (a test replaces it meanwhile).
    #[cfg(feature = "test-hooks")]
    pub before_identical_check: Option<PathHook>,
    /// Sender: records per `ChunkListPart` instead of the default.
    #[cfg(feature = "test-hooks")]
    pub chunk_list_part_records: Option<usize>,
    /// Sender: close the connection once this many parts have been queued
    /// (once per hook instance, across reconnects).
    #[cfg(feature = "test-hooks")]
    pub drop_after_parts: Option<Arc<PartsFault>>,
    /// Sender: close the connection once this many listing frames have been
    /// queued (once per hook instance, across reconnects).
    #[cfg(feature = "test-hooks")]
    pub drop_after_listing: Option<Arc<PartsFault>>,
    /// Both roles: every outgoing frame of the control and listing streams
    /// passes through this; the frames it returns go out instead (a test
    /// drops, duplicates, alters or injects frames).
    #[cfg(feature = "test-hooks")]
    pub tamper_frames: Option<FrameHook>,
    /// Receiver: counts the `NeedChunks` re-requests it sends.
    #[cfg(feature = "test-hooks")]
    pub rerequests: Option<Arc<std::sync::atomic::AtomicU64>>,
    /// Receiver: runs once a `DELTA` file's chunk bases have been sent,
    /// before the request for those chunks goes out (a test changes the
    /// old file meanwhile).
    #[cfg(feature = "test-hooks")]
    pub after_bases: Option<PathHook>,
    /// Receiver: runs when a file's chunk list is about to be opened,
    /// before the old file is looked at (a test changes it meanwhile).
    #[cfg(feature = "test-hooks")]
    pub before_open: Option<PathHook>,
}

/// Test hook: a connection drop after a number of frames of one kind
/// (chunk-list parts, listing frames).
#[cfg(feature = "test-hooks")]
#[derive(Debug, Default)]
pub struct PartsFault {
    after: usize,
    sent: std::sync::atomic::AtomicUsize,
    fired: std::sync::atomic::AtomicBool,
}

#[cfg(feature = "test-hooks")]
impl PartsFault {
    pub fn after(n: usize) -> Arc<PartsFault> {
        Arc::new(PartsFault {
            after: n,
            ..PartsFault::default()
        })
    }

    pub fn fired(&self) -> bool {
        self.fired.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Counts one queued frame; true when the connection is to be dropped now.
    pub(crate) fn queued(&self) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        self.sent.fetch_add(1, SeqCst) + 1 >= self.after && !self.fired.swap(true, SeqCst)
    }
}

/// The stream a frame goes out on (see [`SideHooks::tamper_frames`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Control,
    Listing,
}

/// Test hook: the frames to send in place of one outgoing frame (see
/// [`SideHooks`]).
#[cfg(feature = "test-hooks")]
pub type FrameHook = Arc<
    dyn Fn(StreamKind, irsync_proto::v1::control::Msg) -> Vec<irsync_proto::v1::control::Msg>
        + Send
        + Sync,
>;

/// [`SideHooks::tamper_frames`] as the stream writers carry it; empty (and
/// a no-op) without test hooks.
#[derive(Clone, Default)]
pub(crate) struct Tamper {
    #[cfg(feature = "test-hooks")]
    hook: Option<FrameHook>,
}

impl Tamper {
    /// The frames to send in place of `msg` on a `kind` stream.
    #[cfg(feature = "test-hooks")]
    pub(crate) fn frames(
        &self,
        kind: StreamKind,
        msg: irsync_proto::v1::control::Msg,
    ) -> impl Iterator<Item = irsync_proto::v1::control::Msg> {
        match &self.hook {
            Some(h) => h(kind, msg),
            None => vec![msg],
        }
        .into_iter()
    }

    #[cfg(not(feature = "test-hooks"))]
    pub(crate) fn frames(
        &self,
        _kind: StreamKind,
        msg: irsync_proto::v1::control::Msg,
    ) -> impl Iterator<Item = irsync_proto::v1::control::Msg> {
        std::iter::once(msg)
    }
}

/// Test hook: entries to list right after `entry` (see [`SideHooks`]).
#[cfg(feature = "test-hooks")]
pub type ListingHook =
    Arc<dyn Fn(&irsync_proto::v1::FileEntry) -> Vec<irsync_proto::v1::FileEntry> + Send + Sync>;

impl SideHooks {
    /// The `tamper_frames` hook for the stream writers.
    pub(crate) fn tamper(&self) -> Tamper {
        Tamper {
            #[cfg(feature = "test-hooks")]
            hook: self.tamper_frames.clone(),
        }
    }

    /// Records one receiver event (test hook; a no-op otherwise).
    #[cfg_attr(not(feature = "test-hooks"), allow(unused_variables))]
    pub(crate) fn event(&self, what: &str, path: &RelPath) {
        #[cfg(feature = "test-hooks")]
        if let Some(log) = &self.events {
            log.lock().unwrap_or_else(|e| e.into_inner()).push(format!(
                "{what}:{}",
                String::from_utf8_lossy(path.as_bytes())
            ));
        }
    }
}

impl std::fmt::Debug for SideHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SideHooks")
    }
}

/// This process's end of a session: the directory it reads or writes, with
/// its index and hasher. `filters` only shape the walk when this side sends;
/// the client's filters are not transmitted to the daemon in v1.
#[derive(Clone)]
pub struct LocalSide {
    pub root: PathBuf,
    /// rsync's trailing-slash rule as typed (part of the session key; the
    /// root itself is already resolved, see [`resolve_roots`]).
    pub contents_only: bool,
    pub index: IndexHandle,
    pub hasher: Arc<Hasher>,
    pub filters: Arc<FilterSet>,
    pub hooks: SideHooks,
}

impl LocalSide {
    pub fn new(
        root: PathBuf,
        contents_only: bool,
        index: IndexHandle,
        filters: Arc<FilterSet>,
    ) -> LocalSide {
        let hasher = Arc::new(Hasher::new(index.clone(), root.clone(), HASH_WORKERS));
        LocalSide {
            root,
            contents_only,
            index,
            hasher,
            filters,
            hooks: SideHooks::default(),
        }
    }
}

/// The start of a timed wait, when `m` is recording (it reads no clock otherwise).
pub(crate) fn wait_start(m: &Meter) -> Option<std::time::Instant> {
    m.is_enabled().then(std::time::Instant::now)
}

/// Records how long a role waited on its hasher.
pub(crate) fn record_hash_wait(m: &Meter, since: Option<std::time::Instant>) {
    if let Some(t) = since {
        metrics::histogram!("irsync_hash_wait_seconds", m.labels())
            .record(t.elapsed().as_secs_f64());
    }
}

/// Records a transferred file: its size, and the time from its first request to its DONE.
pub(crate) fn record_file(m: &Meter, since: Option<std::time::Instant>, size: u64) {
    let Some(t) = since else { return };
    metrics::histogram!("irsync_file_transfer_seconds", m.labels())
        .record(t.elapsed().as_secs_f64());
    metrics::histogram!("irsync_file_size_bytes", m.labels()).record(size as f64);
}

/// What a session did, from one side's point of view.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub session_id: [u8; 16],
    /// The daemon resumed an existing session (`HelloAck.resumed`).
    pub resumed: bool,
    /// Files placed (receiver) or acknowledged as placed (sender) on this connection.
    pub files_transferred: u64,
    /// File data bytes that crossed the network on data streams.
    pub bytes_transferred: u64,
    /// Regular files found up to date (known on the receiving side only).
    pub files_skipped: u64,
    /// Files this side's hashers had to read during the session.
    pub files_hashed: u64,
    pub errors: Vec<(RelPath, String)>,
    /// Source entries by kind and their total size (`--stats`).
    pub listed: crate::scan::ListStats,
    /// Items created new at the destination (files, dirs, links, specials).
    pub created: u64,
    /// Sum of the sizes of the regular files transferred.
    pub transferred_size: u64,
    /// Bytes this side's connection sent and received (QUIC level).
    pub bytes_sent: u64,
    pub bytes_received: u64,
    /// Paths the receiver deleted (`--delete*`), deepest first. A pushing
    /// client gets at most 1000 of them from the daemon.
    pub deleted: Vec<RelPath>,
    /// `--dry-run`: paths the receiver would have deleted.
    pub would_delete: Vec<RelPath>,
    /// Deletions stopped at `--max-delete` with candidates left.
    pub max_delete_reached: bool,
    /// What this side spent its time and bytes on.
    pub usage: Usage,
    /// Times the client dialled again after losing the connection.
    pub reconnects: u32,
    /// How the last connection was made (`Direct` or `Relay`), if known.
    pub conn: Option<reconnect::ConnState>,
    /// Per-file records the role still held when it finished (should be 0).
    #[cfg(feature = "test-hooks")]
    pub retained_files: usize,
}

impl Outcome {
    /// Adds another session's counts (several sources in one run).
    pub fn absorb(&mut self, o: Outcome) {
        self.files_transferred += o.files_transferred;
        self.bytes_transferred += o.bytes_transferred;
        self.files_skipped += o.files_skipped;
        self.files_hashed += o.files_hashed;
        self.listed.add(&o.listed);
        self.created += o.created;
        self.transferred_size += o.transferred_size;
        self.bytes_sent += o.bytes_sent;
        self.bytes_received += o.bytes_received;
        self.errors.extend(o.errors);
        self.deleted.extend(o.deleted);
        self.would_delete.extend(o.would_delete);
        self.max_delete_reached |= o.max_delete_reached;
        self.usage.add(&o.usage);
    }

    /// rsync's convention: 0 ok, 23 partial transfer due to error, 25
    /// deletions stopped at `--max-delete` (which takes precedence).
    pub fn exit_code(&self) -> i32 {
        if self.max_delete_reached {
            25
        } else if self.errors.is_empty() {
            0
        } else {
            23
        }
    }
}

/// Applies rsync's trailing-slash rule. On a push, `src` without a slash
/// copies the directory itself: its basename is returned as `Hello.name`
/// for the daemon to join after resolving `remote`. On a pull, the last
/// component of `remote` names the local directory. The remote path is
/// never changed here: the daemon resolves it.
pub fn resolve_roots(
    direction: Direction,
    local: &Path,
    local_contents_only: bool,
    remote: &[u8],
    remote_contents_only: bool,
) -> (PathBuf, Vec<u8>) {
    use std::os::unix::ffi::OsStrExt;
    match direction {
        Direction::ClientSends => {
            let name = (!local_contents_only)
                .then(|| local.file_name())
                .flatten()
                .map(|n| n.as_bytes().to_vec())
                .unwrap_or_default();
            (local.to_path_buf(), name)
        }
        Direction::DaemonSends => {
            let last = remote
                .rsplit(|&b| b == b'/')
                .find(|c| !c.is_empty() && *c != b"." && *c != b"~");
            let root = match last {
                Some(name) if !remote_contents_only => {
                    local.join(std::ffi::OsStr::from_bytes(name))
                }
                _ => local.to_path_buf(),
            };
            (root, Vec::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_slash_rule_puts_the_basename_in_name() {
        use Direction::*;
        let p = Path::new("/l/photos");
        let r = |d, l: &Path, lco, rem: &[u8], rco| resolve_roots(d, l, lco, rem, rco);
        assert_eq!(
            r(ClientSends, p, false, b"", false),
            (p.into(), b"photos".to_vec())
        );
        assert_eq!(r(ClientSends, p, true, b"", false), (p.into(), vec![]));
        assert_eq!(
            r(ClientSends, p, false, b"/srv/x", false),
            (p.into(), b"photos".to_vec())
        );
        // Pull: the remote's last component names the local directory; `name` stays empty.
        let l = Path::new("/l");
        assert_eq!(
            r(DaemonSends, l, false, b"/srv/photos", false),
            (PathBuf::from("/l/photos"), vec![])
        );
        assert_eq!(
            r(DaemonSends, l, false, b"/srv/photos", true),
            (PathBuf::from("/l"), vec![])
        );
        assert_eq!(
            r(DaemonSends, l, false, b"", false),
            (PathBuf::from("/l"), vec![])
        );
        assert_eq!(
            r(DaemonSends, l, false, b"/", false),
            (PathBuf::from("/l"), vec![])
        );
        assert_eq!(
            r(DaemonSends, l, false, b"photos", false),
            (PathBuf::from("/l/photos"), vec![])
        );
    }

    #[test]
    fn daemon_session_key_is_by_resolved_place() {
        let c = iroh::SecretKey::from_bytes(&[1; 32]).public();
        let rel = RelPath::new(b"2024").unwrap();
        let h = |path: &[u8]| Hello {
            path: path.to_vec(),
            ..Hello::default()
        };
        assert_eq!(
            SessionKey::for_daemon(c, "photos", &rel, &h(b"photos/2024")),
            SessionKey::for_daemon(c, "photos", &rel, &h(b"/srv/photos/2024"))
        );
        assert_ne!(
            SessionKey::for_daemon(c, "photos", &rel, &h(b"photos/2024")),
            SessionKey::for_daemon(c, "srv", &rel, &h(b"photos/2024"))
        );
    }

    #[test]
    fn index_scope_maps_session_paths_to_index_keys() {
        let ix = crate::index::Index::open(
            tempfile::tempdir().unwrap().path(),
            tempfile::tempdir().unwrap().path(),
        )
        .unwrap()
        .into_handle();
        let r = |s: &[u8]| RelPath::new(s).unwrap();
        let s = IndexScope::new(ix.clone(), r(b"sub/d"));
        assert_eq!(s.key(&r(b"f")), r(b"sub/d/f"));
        assert_eq!(s.key(&RelPath::root()), r(b"sub/d"));
        assert_eq!(s.strip(&r(b"sub/d/x/y")), Some(r(b"x/y")));
        assert_eq!(s.strip(&r(b"sub/d")), Some(RelPath::root()));
        assert_eq!(s.strip(&r(b"sub/dd/x")), None);
        assert_eq!(s.strip(&r(b"other")), None);
        let root = IndexScope::new(ix, RelPath::root());
        assert_eq!(root.key(&r(b"f")), r(b"f"));
        assert_eq!(root.strip(&r(b"f")), Some(r(b"f")));
    }

    #[test]
    fn session_record_roundtrips_through_row() {
        use irsync_proto::v1::{plan_item::Action, PlanEntry};
        let rec = SessionRecord {
            id: [7; 16],
            key: vec![1, 2, 3],
            plan: SessionPlan {
                entries: vec![PlanEntry {
                    file_id: 1,
                    path: b"a/\xff".to_vec(),
                    action: Action::Delta as i32,
                    size: 9,
                    whole_hash: None,
                    entry: None,
                    append_from: 0,
                }],
                plan_complete: true,
                remote_id: vec![4; 32],
                ..SessionPlan::default()
            },
            progress: SessionProgress {
                done_file_ids: vec![1],
                bytes_transferred: 9,
                deleted: 3,
            },
            created_at: 1,
            updated_at: 2,
        };
        assert_eq!(SessionRecord::from_row(&rec.to_row()).unwrap(), rec);
    }

    #[test]
    fn session_key_depends_on_semantic_flags_only() {
        let args = |extra: &[&str]| {
            let mut v = vec!["irsync", "-a"];
            v.extend_from_slice(extra);
            v.extend_from_slice(&["src/", "h:m/"]);
            SyncOptions::from_args(&crate::cli::Args::try_parse_from(v).unwrap())
        };
        let id = iroh::SecretKey::from_bytes(&[3; 32]).public();
        let src = Location::parse("/nonexistent/src/").unwrap();
        let dst = Location::parse("h:m/").unwrap();
        let k = |o: &SyncOptions| SessionKey::new(id, &src, &dst, o);
        let base = k(&args(&[]));
        assert_eq!(base, k(&args(&["-v", "--progress"])));
        assert_ne!(base, k(&args(&["-c"])));
        assert_ne!(base, k(&args(&["--delete"])));
        assert_ne!(base, k(&args(&["-n"])));
        assert_ne!(base, k(&args(&["--exclude", "*.o"])));
        let other = Location::parse("h:m/sub").unwrap();
        assert_ne!(base, SessionKey::new(id, &src, &other, &args(&[])));
    }
}
