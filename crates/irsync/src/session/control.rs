//! Control-stream plumbing shared by both roles: a writer task that owns the
//! `SendStream`, a reader task that owns the `RecvStream` and dispatches
//! frames, the listing producer (whose frames go out on the listing stream,
//! see [`super::listing_stream`]), and session persistence.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::endpoint::{RecvStream, SendStream};
use irsync_proto::chunk::FileDigest;
use irsync_proto::v1::{
    control::Msg, file_entry::Kind, listing_frame::Frame, Control, FileEntry, Hash, ListingEnd,
    PathError, PlacedStat, PlanEntry, SessionPlan, SessionProgress, SourceStat,
};
use prost::Message as _;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::Instrument;

use super::engine::{Ctx, PeerEvents};
use super::limits::{Backlog, ByteBudget, OUT_BUDGET, PEER_EVENTS};
use super::listing_stream::{ListingOut, PeerScan};
use super::single::SingleFile;
use super::{IndexScope, SessionRecord};
use crate::filter::{FilterSet, Verdict};
use crate::fs::paths::RelPath;
use crate::hasher::Hasher;
use crate::index::IndexHandle;
use crate::net::frame::{read_msg_tapped, write_msg_tapped};
use crate::net::tap::StreamTap;
use crate::plan::Side;
use crate::scan::{walk_below, ScanEvent, ScanItem, ScanSummary, WalkOpts, WALK_THREADS};
use crate::telemetry::Phase;

/// How long to wait for the peer to take our last bytes before giving up.
pub(super) const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// Session rows are written at most this often (plus a final write).
const PERSIST_EVERY: Duration = Duration::from_secs(2);

/// What a role's main loop reacts to: inbound control frames, transport
/// trouble, and completions of the tasks the role spawned.
pub(super) enum Ev {
    /// A control frame.
    Ctrl(Msg),
    /// The peer started its listing (sent once, at its first listing frame).
    PeerListing,
    /// The peer's listing ended (its last listing frame).
    PeerListingEnd(ListingEnd),
    /// The peer finished its control stream.
    PeerEof,
    /// A stream from the peer failed (the message names the stream).
    PeerFailed(String),
    /// A stream to the peer failed (the message names the stream).
    WriterFailed(String),
    /// The peer broke a rule of the protocol.
    ProtocolError(String),
    /// Role-specific task completion.
    Recv(super::receiver::Done),
    Send(super::sender::Done),
}

/// Handle the stream readers post peer events with: a frame waits while the
/// main loop has [`PEER_EVENTS`] queued, and so does the reader's stream.
#[derive(Clone)]
pub(super) struct PeerTx {
    tx: mpsc::Sender<Ev>,
    #[cfg(feature = "test-hooks")]
    peak: Option<Arc<std::sync::atomic::AtomicUsize>>,
    #[cfg(feature = "test-hooks")]
    stop_after_done: bool,
}

impl PeerTx {
    /// A queue of peer events and its handle (see [`PEER_EVENTS`]).
    #[cfg_attr(not(feature = "test-hooks"), allow(unused_variables))]
    pub fn channel(hooks: &super::SideHooks) -> (PeerTx, PeerEvents) {
        let (tx, rx) = mpsc::channel(PEER_EVENTS);
        let tx = PeerTx {
            tx,
            #[cfg(feature = "test-hooks")]
            peak: hooks.peer_events_peak.clone(),
            #[cfg(feature = "test-hooks")]
            stop_after_done: hooks.stop_reading_after_done,
        };
        (tx, rx)
    }

    /// Queues `ev`, waiting while the queue is full. Once the main loop is
    /// gone the event is dropped, so a reader reads on until it is aborted.
    pub async fn send(&self, ev: Ev) {
        #[cfg(feature = "test-hooks")]
        let done = self.stop_after_done && matches!(ev, Ev::Ctrl(Msg::Done(_)));
        let _ = self.tx.send(ev).await;
        #[cfg(feature = "test-hooks")]
        if done {
            std::future::pending::<()>().await;
        }
        #[cfg(feature = "test-hooks")]
        if let Some(p) = &self.peak {
            p.fetch_max(
                self.tx.max_capacity() - self.tx.capacity(),
                std::sync::atomic::Ordering::SeqCst,
            );
        }
    }
}

/// A frame on its way to the writer; `paced` is the bytes it holds of the
/// writer's [`ByteBudget`] (zero for `Out::send`), given back once written,
/// and `len` its size in the writer's [`Backlog`].
struct Queued {
    frame: Control,
    paced: usize,
    len: usize,
}

/// Handle for queueing control frames to the writer task.
#[derive(Clone)]
pub(super) struct Out {
    tx: mpsc::UnboundedSender<Option<Queued>>,
    budget: Arc<ByteBudget>,
    /// Every byte queued and not yet written, paced or not.
    backlog: Arc<Backlog>,
    /// Where the role learns that the backlog went past its ceiling.
    events: mpsc::UnboundedSender<Ev>,
    /// Set once `SessionDone` is queued: it is the last frame of the
    /// stream, and whatever comes after it is dropped (see [`Out::queue`]).
    done: Arc<Mutex<bool>>,
    #[cfg(feature = "test-hooks")]
    peak: Option<Arc<std::sync::atomic::AtomicUsize>>,
}

/// A frame that holds its bytes of the writer's budget and is not queued
/// yet (see [`Out::reserve`]); dropped unsent, it gives them back.
pub(super) struct Paced {
    out: Out,
    frame: Option<Queued>,
}

impl Paced {
    /// Queues the frame behind everything queued so far.
    pub fn send(mut self) {
        if let Some(q) = self.frame.take() {
            let n = q.paced;
            if !self.out.queue(q) {
                self.out.budget.release(n);
            }
        }
    }
}

impl Drop for Paced {
    fn drop(&mut self) {
        if let Some(q) = self.frame.take() {
            self.out.budget.release(q.paced);
        }
    }
}

impl Out {
    /// Queues `msg` without waiting: for main loops, which must never wait
    /// on the network (see the invariant in `run_role`).
    pub fn send(&self, msg: Msg) {
        let frame = Control { msg: Some(msg) };
        let len = frame.encoded_len();
        // A closed writer surfaces as `Ev::WriterFailed`; nothing to do here.
        self.queue(Queued {
            frame,
            paced: 0,
            len,
        });
    }

    /// Hands `q` to the writer, counting it in the backlog; false if the
    /// writer is gone. The backlog past its ceiling (see
    /// `limits::out_ceiling`) means the peer is not reading what an honest
    /// peer reads: the role is told, once, and fails the session. Nothing
    /// here waits, so the main loops may queue as before.
    ///
    /// `SessionDone` ends the stream: a frame queued after it is dropped
    /// (false). The peer stops reading there, and the tasks beside the role
    /// (the heartbeat, the walk) do not know that the role has finished.
    /// The lock keeps a frame from another thread from slipping in behind
    /// `SessionDone`.
    fn queue(&self, q: Queued) -> bool {
        let mut done = self.done.lock().unwrap_or_else(|e| e.into_inner());
        if *done {
            return false;
        }
        *done = matches!(q.frame.msg, Some(Msg::Done(_)));
        let len = q.len;
        if self.backlog.add(len) {
            let _ = self.events.send(Ev::ProtocolError(format!(
                "the peer is not reading its control stream: {} bytes wait for it, \
                 above the ceiling of {}",
                self.backlog.bytes(),
                self.backlog.ceiling()
            )));
        }
        if self.tx.send(Some(q)).is_err() {
            self.backlog.sub(len);
            return false;
        }
        true
    }

    /// Queues `msg` once the writer has room for it under [`OUT_BUDGET`]
    /// bytes of paced frames not yet written, so a slow link stalls the
    /// caller instead of growing the queue. Only for spawned tasks that may
    /// wait on the peer, never a main loop. Frames from `send` and
    /// `send_paced` share one queue, so their order is kept.
    pub async fn send_paced(&self, msg: Msg) {
        self.reserve(msg).await.send();
    }

    /// Waits for room for `msg` as [`Out::send_paced`] does, without
    /// queueing it yet, so a caller can check after the wait that the frame
    /// is still wanted. Once the writer is gone it returns at once.
    pub async fn reserve(&self, msg: Msg) -> Paced {
        let frame = Control { msg: Some(msg) };
        let n = frame.encoded_len();
        let gone = tokio::select! {
            () = self.budget.acquire(n) => false,
            () = self.tx.closed() => true,
        };
        #[cfg(feature = "test-hooks")]
        if let Some(p) = &self.peak {
            p.fetch_max(self.budget.held(), std::sync::atomic::Ordering::SeqCst);
        }
        Paced {
            out: self.clone(),
            frame: (!gone).then_some(Queued {
                frame,
                paced: n,
                len: n,
            }),
        }
    }

    /// Ask the writer to finish the stream after everything queued so far.
    pub fn finish(&self) {
        let _ = self.tx.send(None);
    }
}

/// Spawns the writer: frames go out in queue order, and a paced frame's
/// bytes return to the budget once written; `finish` ends the stream and
/// waits (bounded) until the peer has the bytes. `ceiling` bounds the
/// bytes queued and not yet written (see `limits::out_ceiling`).
pub(super) fn spawn_writer(
    mut send: SendStream,
    events: mpsc::UnboundedSender<Ev>,
    hooks: &super::SideHooks,
    ceiling: usize,
    tap: StreamTap,
) -> (Out, JoinHandle<()>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Option<Queued>>();
    let budget = Arc::new(ByteBudget::new(OUT_BUDGET));
    let backlog = Arc::new(Backlog::new(ceiling));
    let tamper = hooks.tamper();
    let (written, unqueued, writer_events) = (budget.clone(), backlog.clone(), events.clone());
    let task = tokio::spawn(
        async move {
            while let Some(Some(q)) = rx.recv().await {
                for msg in q.frame.msg.into_iter().flat_map(|m| tamper.frames(m)) {
                    let c = Control { msg: Some(msg) };
                    if let Err(e) = write_msg_tapped(&mut send, &c, &tap).await {
                        let _ =
                            writer_events.send(Ev::WriterFailed(format!("control stream: {e}")));
                        return;
                    }
                }
                written.release(q.paced);
                unqueued.sub(q.len);
            }
            if send.finish().is_ok() {
                tap.finished();
                let _ = tokio::time::timeout(DRAIN_TIMEOUT, send.stopped()).await;
            }
        }
        .instrument(tracing::Span::current()),
    );
    let out = Out {
        tx,
        budget,
        backlog,
        events,
        done: Arc::default(),
        #[cfg(feature = "test-hooks")]
        peak: hooks.out_peak.clone(),
    };
    (out, task)
}

/// A listing entry's hash as a hash-only digest (no chunk list), which is all
/// the planner compares.
pub(super) fn side_of(entry: irsync_proto::v1::FileEntry) -> Side {
    let digest = entry.whole_hash.as_ref().and_then(|h| {
        let hash: [u8; 32] = h.bytes.clone().try_into().ok()?;
        Some(FileDigest {
            size: entry.size,
            hash,
            chunks: Vec::new(),
        })
    });
    Side { entry, digest }
}

/// One item of a listing as the receiver's planner consumes it.
// A `Side` is large (a whole `FileEntry`); boxing it would cost an allocation
// per listed entry for a channel that holds items briefly.
#[allow(clippy::large_enum_variant)]
pub(super) enum Listed {
    Side(Side),
    /// Entries at or below this path are missing from the listing (see
    /// `ScanEvent::Gap`); it is at its place in path order.
    Gap(RelPath),
}

/// Spawns the reader: every control frame but heartbeats goes to `events`,
/// and a full queue stops the reading. The peer's listing has a stream of
/// its own; `scan` is what its reader has seen. A heartbeat that reports
/// data received on this connection sets `progress`.
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_reader(
    recv: RecvStream,
    events: PeerTx,
    bus: super::EventBus,
    activity: Arc<std::sync::atomic::AtomicU64>,
    progress: Arc<std::sync::atomic::AtomicBool>,
    scan: Arc<PeerScan>,
    tap: StreamTap,
) -> JoinHandle<()> {
    tokio::spawn(
        read_control(recv, events, bus, activity, progress, scan, tap)
            .instrument(tracing::Span::current()),
    )
}

/// A reader that bumps `activity` on every read that brings bytes, so a
/// large frame still arriving over a slow link counts as progress for the
/// idle watchdog, not only once it is complete.
struct Counted<R> {
    inner: R,
    activity: Arc<std::sync::atomic::AtomicU64>,
}

impl<R: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for Counted<R> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let res = std::pin::Pin::new(&mut self.inner).poll_read(cx, buf);
        if buf.filled().len() > before {
            self.activity
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        res
    }
}

/// The reader's loop; see [`spawn_reader`].
async fn read_control<R: tokio::io::AsyncRead + Unpin>(
    recv: R,
    events: PeerTx,
    bus: super::EventBus,
    activity: Arc<std::sync::atomic::AtomicU64>,
    progress: Arc<std::sync::atomic::AtomicBool>,
    scan: Arc<PeerScan>,
    tap: StreamTap,
) {
    let mut recv = Counted {
        inner: recv,
        activity,
    };
    loop {
        // Activity is counted per read, in `Counted`.
        let frame = read_msg_tapped::<Control, _>(&mut recv, &tap).await;
        match frame {
            Ok(Some(Control { msg: Some(msg) })) => match msg {
                // Counted as activity by `Counted`; it also carries the peer's
                // progress this side cannot otherwise see.
                Msg::Heartbeat(h) => {
                    use crate::session::{Event, Party};
                    // After the peer's `ListingEnd` its walk position is stale.
                    let walking = h.walking.filter(|_| !scan.ended());
                    if let Some(walking) = walking {
                        // Empty bytes are the root.
                        if let Ok(at) = RelPath::new(&walking) {
                            let seen = scan.seen();
                            bus.emit(Event::Scanned {
                                party: Party::Peer,
                                entries: seen.files(),
                                bytes: seen.total_size,
                                at: Some(at),
                            });
                        }
                    }
                    // Only a receiver sends these, so this side is the
                    // source.
                    if let Some(listed) = &h.listed {
                        bus.emit_listed(
                            Party::Local,
                            &crate::scan::ListStats::from_wire(listed),
                            h.listed_settled,
                        );
                    }
                    // Only a receiver sends these: data it took in on this
                    // connection is progress for this side, the sender,
                    // which otherwise counts only finished sends and files.
                    if h.received_bytes > 0 {
                        progress.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    if h.skipped_bytes > 0 {
                        bus.emit(Event::PeerSkipped(h.skipped_bytes));
                    }
                    if h.deleted_count > 0 || h.delete_total > 0 {
                        bus.emit(Event::Delete {
                            party: Party::Peer,
                            done: h.deleted_count,
                            total: (h.delete_total > 0).then_some(h.delete_total),
                        });
                    }
                }
                m => {
                    events.send(Ev::Ctrl(m)).await;
                }
            },
            Ok(Some(Control { msg: None })) => {
                tracing::warn!("empty control frame; ignored");
            }
            Ok(None) => {
                events.send(Ev::PeerEof).await;
                return;
            }
            // Not a frame of ours (undecodable, or longer than any frame):
            // the peer broke the protocol, which no reconnect mends. Read
            // no further, but keep the stream, as for a listing frame above.
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                events
                    .send(Ev::ProtocolError(format!("control stream: {e}")))
                    .await;
                std::future::pending::<()>().await;
            }
            Err(e) => {
                events
                    .send(Ev::PeerFailed(format!("control stream: {e}")))
                    .await;
                return;
            }
        }
    }
}

/// irsync's own temporary files (`.irsync.<name>.part` / `.tmp`) never appear
/// in a listing, and a receiver refuses them in a peer's.
pub(super) fn is_own_temp(path: &RelPath) -> bool {
    path.components()
        .last()
        .is_some_and(crate::fs::temp_name::is_temp_name)
}

/// Paths of regular files this side has listed, the only paths it will
/// send, with the walk's stat of each for the digest's quick check.
pub(super) type ListedFiles = Arc<Mutex<HashMap<RelPath, crate::hasher::Freshness>>>;

pub(super) struct ListingSpec {
    /// The `list` phase; it ends with the walk.
    pub phase: Phase,
    pub root: PathBuf,
    /// The directory `root` lies below, at `scope.prefix`: the export's
    /// directory on a daemon. The walk goes down from here without
    /// following a symlink (see [`walk_below`]).
    pub base: PathBuf,
    pub filters: Arc<FilterSet>,
    pub hasher: Arc<Hasher>,
    /// Maps listing paths to index keys for the cached-hash lookup.
    pub scope: IndexScope,
    /// Entries at or below this path are left out (a relative `--partial-dir`).
    pub skip: Option<RelPath>,
    /// `-H` (sender): later names of a multiply-linked regular file are
    /// listed as `HARDLINK`s to the first one.
    pub hard_links: bool,
    /// `-r`: list the whole tree; off, only the top-level entries, and
    /// directories among them only with `dirs` (`-d`).
    pub recursive: bool,
    pub dirs: bool,
    /// Where to report scan progress (the sender's walk only).
    pub bus: Option<super::EventBus>,
    /// Receiver: where to note regular files without a fresh index row.
    pub cold: Option<Arc<crate::warm::Cold>>,
    /// Where the walk is, for this side's heartbeats; the listing also sends
    /// one before its first entry and at every `Scanned`.
    pub busy: Option<Arc<super::Busy>>,
    /// Bumped per listed entry, so heartbeats go out while only this side walks.
    pub activity: Option<Arc<std::sync::atomic::AtomicU64>>,
    /// Times the walk.
    pub meter: Option<Arc<crate::telemetry::meter::Meter>>,
    /// A single-file session: list only the root and this one file (under
    /// its wire name), never walk the root.
    pub single: Option<SingleFile>,
    /// Test hook: extra entries to list right after an entry.
    #[cfg(feature = "test-hooks")]
    pub inject: Option<super::ListingHook>,
    /// Test hook: close this connection once the fault's count of listing
    /// frames is reached.
    #[cfg(feature = "test-hooks")]
    pub drop_after: Option<(Arc<super::PartsFault>, iroh::endpoint::Connection)>,
    /// Test hook: queue only this many listing frames, then none (no
    /// `ListingEnd` either), as a lister that stops short.
    #[cfg(feature = "test-hooks")]
    pub cut_after: Option<usize>,
}

/// Walks `spec.root` and streams the listing to the peer on `listing` (with
/// cached hashes only; nothing is read), then `ListingEnd`, and ends the
/// stream; heartbeats go to `out`. Each entry also goes to `local` (the
/// receiver's planner) and regular files are recorded in `listed`. A full
/// `listing` or `local` queue stalls the walk.
pub(super) fn spawn_listing(
    spec: ListingSpec,
    out: Out,
    listing: ListingOut,
    local: Option<mpsc::Sender<Listed>>,
    listed: Option<ListedFiles>,
) -> JoinHandle<ScanSummary> {
    // A single file is listed on the listing thread itself: at most two
    // events, which the channel always has room for.
    let mut single = None;
    let (mut rx, summary) = match &spec.single {
        Some(s) => {
            let (tx, rx) = mpsc::channel(2);
            let (stx, srx) = tokio::sync::oneshot::channel();
            single = Some((s.clone(), tx, stx));
            (rx, srx)
        }
        None => walk_below(
            spec.base.clone(),
            spec.scope.prefix.clone(),
            spec.filters.clone(),
            WalkOpts {
                follow_root_symlink: true,
                threads: WALK_THREADS,
                max_depth: (!spec.recursive).then_some(1),
            },
            spec.meter.clone(),
        ),
    };
    // Waits on `listing` and `local` for the whole walk: a thread of its
    // own, not one of the blocking pool's (see `scan::spawn_thread`).
    crate::scan::spawn_thread("irsync-listing", move || {
        let _in_list = spec.phase.span().clone().entered();
        if let Some((s, tx, stx)) = single {
            let _ = stx.send(list_single(
                spec.root.clone(),
                &spec.base,
                &spec.scope.prefix,
                &s,
                &spec.filters,
                &tx,
            ));
        }
        let under_skip = |p: &RelPath| {
            spec.skip.as_ref().is_some_and(|s| {
                let (p, s) = (p.as_bytes(), s.as_bytes());
                p.starts_with(s) && (p.len() == s.len() || p[s.len()] == b'/')
            })
        };
        let mut count = 0u64;
        // Entries sent, injected ones included.
        let mut sent = 0u64;
        let mut stats = crate::scan::ListStats::default();
        let mut names = crate::fs::ids::IdNames::new();
        // `-H`: the first listed name of each (dev, inode) with nlink > 1.
        let mut first_names: HashMap<(u64, u64), Vec<u8>> = HashMap::new();
        let mut last_emit = std::time::Instant::now();
        // The directory last published to `spec.busy.walking`.
        let mut walking = RelPath::root();
        #[cfg(feature = "test-hooks")]
        let queued = std::cell::Cell::new(0usize);
        let send = |frame: Frame| {
            #[cfg(feature = "test-hooks")]
            if let Some(n) = spec.cut_after {
                if queued.get() >= n {
                    return;
                }
                queued.set(queued.get() + 1);
            }
            listing.send_blocking(frame);
            #[cfg(feature = "test-hooks")]
            if let Some((fault, conn)) = &spec.drop_after {
                if fault.queued() {
                    tracing::warn!("fault injection: dropping the connection mid-listing");
                    conn.close(99u32.into(), b"fault");
                }
            }
        };
        if let Some(busy) = &spec.busy {
            *busy.walking.lock().unwrap_or_else(|e| e.into_inner()) = Some(walking.clone());
            out.send(Msg::Heartbeat(busy.heartbeat()));
        }
        while let Some(ev) = rx.blocking_recv() {
            let item = match ev {
                ScanEvent::Entry(item) => item,
                ScanEvent::Gap { path, message } => {
                    send(Frame::Error(PathError {
                        path: path.as_bytes().to_vec(),
                        message,
                    }));
                    if let Some(tx) = &local {
                        let _ = tx.blocking_send(Listed::Gap(path));
                    }
                    if let Some(a) = &spec.activity {
                        a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    continue;
                }
            };
            if is_own_temp(&item.path) || under_skip(&item.path) {
                continue;
            }
            if !spec.recursive
                && !spec.dirs
                && item.entry.kind() == Kind::Directory
                && !item.path.as_bytes().is_empty()
            {
                tracing::info!(path = ?item.path, "skipping directory (no -r)");
                continue;
            }
            let ScanItem {
                path, mut entry, ..
            } = item.clone();
            names.attach(&mut entry);
            if spec.hard_links && entry.kind() == Kind::Regular && entry.nlink > 1 {
                match first_names.entry((entry.dev, entry.inode)) {
                    std::collections::hash_map::Entry::Occupied(first) => {
                        // Transferred once, under the first name; never requested here.
                        entry.kind = Kind::Hardlink as i32;
                        entry.link_target = first.get().clone();
                    }
                    std::collections::hash_map::Entry::Vacant(v) => {
                        v.insert(entry.path.clone());
                    }
                }
            }
            let digest = if entry.kind() == Kind::Regular {
                // The index knows a single file by this side's name.
                let local_path = match &spec.single {
                    Some(s) => s.local_of(&item.path),
                    None => &item.path,
                };
                let keyed = ScanItem {
                    path: spec.scope.key(local_path),
                    ..item.clone()
                };
                let d = match spec.hasher.cached(&keyed) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!(?path, error = %e, "index lookup failed");
                        None
                    }
                };
                if d.is_none() {
                    if let Some(cold) = &spec.cold {
                        cold.record(entry.size, keyed.path);
                    }
                }
                d
            } else {
                None
            };
            if let Some(d) = &digest {
                entry.whole_hash = Some(Hash {
                    bytes: d.hash.to_vec(),
                });
            }
            let due = (spec.bus.is_some() || spec.busy.is_some())
                && ((count + 1).is_multiple_of(1000)
                    || last_emit.elapsed() >= std::time::Duration::from_millis(250));
            let at = due.then(|| path.parent());
            if let Some(busy) = &spec.busy {
                let dir = path.parent();
                if dir != walking {
                    walking = dir;
                    *busy.walking.lock().unwrap_or_else(|e| e.into_inner()) = Some(walking.clone());
                }
            }
            if entry.kind() == Kind::Regular {
                if let Some(l) = &listed {
                    l.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(path, crate::hasher::Freshness::of(&item));
                }
            }
            send(Frame::Entry(wire_entry(entry.clone())));
            sent += 1;
            #[cfg(feature = "test-hooks")]
            if let Some(inject) = &spec.inject {
                for e in inject(&entry) {
                    send(Frame::Entry(wire_entry(e)));
                    sent += 1;
                }
            }
            stats.count(&entry);
            if let Some(tx) = &local {
                let _ = tx.blocking_send(Listed::Side(Side { entry, digest }));
            }
            count += 1;
            if let Some(a) = &spec.activity {
                a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            if let Some(at) = at {
                last_emit = std::time::Instant::now();
                if let Some(busy) = &spec.busy {
                    out.send(Msg::Heartbeat(busy.heartbeat()));
                }
                if let Some(b) = &spec.bus {
                    b.emit(super::Event::Scanned {
                        party: super::Party::Local,
                        entries: count,
                        bytes: stats.total_size,
                        at: Some(at),
                    });
                }
            }
        }
        let mut summary = summary.blocking_recv().unwrap_or_else(|_| ScanSummary {
            incomplete: true,
            ..ScanSummary::default()
        });
        summary.listed = stats;
        if let Some(b) = &spec.bus {
            b.emit(crate::session::Event::Total {
                party: crate::session::Party::Local,
                files: stats.reg,
                bytes: stats.total_size,
            });
        }
        for (p, e) in &summary.errors {
            tracing::warn!(path = ?p, error = %e, "walk error");
        }
        if let Some(busy) = &spec.busy {
            *busy.walking.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
        send(Frame::End(ListingEnd {
            incomplete: summary.incomplete,
            count: sent,
        }));
        listing.finish();
        summary
    })
}

/// The listing of a single-file session, sent on `tx` (which has room for
/// both events): the root, then the file `local` under its `wire` name when
/// `filters` admit it. A missing file lists nothing; a file that cannot be
/// stat'ed is a gap. As a walk does, a root that is a symlink to a
/// directory is listed as that directory.
///
/// A root at `prefix` below `base` (a session below an export's directory)
/// is looked at from `base` with no symlink followed, as the walk does: a
/// directory on the way that was swapped for a symlink is a gap.
fn list_single(
    mut root: PathBuf,
    base: &std::path::Path,
    prefix: &RelPath,
    single: &SingleFile,
    filters: &FilterSet,
    tx: &mpsc::Sender<ScanEvent>,
) -> ScanSummary {
    use crate::fs::meta::{lstat_at, lstat_entry_with_ctime};
    let anchored = !prefix.as_bytes().is_empty();
    let mut sum = ScanSummary::default();
    let gap = |sum: &mut ScanSummary, path: RelPath, message: String| {
        sum.incomplete = true;
        sum.errors.push((path.clone(), message.clone()));
        let _ = tx.try_send(ScanEvent::Gap { path, message });
    };
    if !anchored && std::fs::metadata(&root).is_ok_and(|m| m.is_dir()) {
        if let Ok(real) = std::fs::canonicalize(&root) {
            root = real;
        }
    }
    let lstat = |rel: &RelPath| -> io::Result<(FileEntry, (i64, u32))> {
        if !anchored {
            return lstat_entry_with_ctime(&root, rel);
        }
        let (dir, name) =
            crate::fs::safe::open_parent_beneath(base, &super::record::join_rel(prefix, rel))?;
        lstat_at(&dir, &name, rel)
    };
    match lstat(&RelPath::root()) {
        Ok((entry, ctime)) => {
            // As in a walk: a root below a prefix that is not a directory
            // (a symlink put there meanwhile, a file) is listed as what it
            // is, and has nothing below it.
            let walkable = !anchored || entry.kind() == Kind::Directory;
            sum.entries += 1;
            let _ = tx.try_send(ScanEvent::Entry(ScanItem {
                path: RelPath::root(),
                entry,
                ctime,
            }));
            if !walkable {
                return sum;
            }
        }
        Err(e) => {
            gap(&mut sum, RelPath::root(), e.to_string());
            return sum;
        }
    }
    match lstat(&single.local) {
        Ok((mut entry, ctime)) => {
            let is_dir = entry.kind() == Kind::Directory;
            if filters.matches(&single.local, is_dir) == Verdict::Exclude {
                return sum;
            }
            entry.path = single.wire.as_bytes().to_vec();
            sum.entries += 1;
            let _ = tx.try_send(ScanEvent::Entry(ScanItem {
                path: single.wire.clone(),
                entry,
                ctime,
            }));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => gap(&mut sum, single.wire.clone(), e.to_string()),
    }
    sum
}

/// Keeps the session row in the index current, writing at most every 2 s.
/// A resumed session continues from its stored record; a finished session's
/// row is deleted, so the next run with the same key starts fresh.
pub(super) struct Persister {
    index: IndexHandle,
    rec: SessionRecord,
    /// Bytes counted by earlier connections of this session.
    base_bytes: u64,
    /// Position of each file id in `rec.plan.entries`.
    planned: HashMap<u64, usize>,
    done: HashSet<u64>,
    /// Position of each path in `rec.progress.placed`.
    placed_at: HashMap<Vec<u8>, usize>,
    dirty: bool,
    last: Instant,
}

impl Persister {
    /// Continues the session's stored record (if any) and writes the row right away.
    pub async fn start(ctx: &Ctx) -> Persister {
        let now = crate::index::now_secs();
        let mut rec = ctx.record.clone().unwrap_or_else(|| SessionRecord {
            id: ctx.session_id,
            key: ctx.key.clone(),
            plan: SessionPlan::default(),
            progress: SessionProgress::default(),
            created_at: now,
            updated_at: now,
        });
        rec.id = ctx.session_id;
        rec.key = ctx.key.clone();
        if let Some(r) = &ctx.remote_id {
            rec.plan.remote_id = r.clone();
        }
        let planned = rec
            .plan
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.file_id, i))
            .collect();
        let done = rec.progress.done_file_ids.iter().copied().collect();
        let placed_at = rec
            .progress
            .placed
            .iter()
            .enumerate()
            .map(|(i, p)| (p.path.clone(), i))
            .collect();
        let mut p = Persister {
            index: ctx.index().clone(),
            base_bytes: rec.progress.bytes_transferred,
            rec,
            planned,
            done,
            placed_at,
            dirty: true,
            last: Instant::now(),
        };
        p.flush().await;
        p
    }

    /// The plan as stored when this connection started (plus what was added since).
    pub fn stored_plan(&self) -> &SessionPlan {
        &self.rec.plan
    }

    pub fn is_done(&self, file_id: u64) -> bool {
        self.done.contains(&file_id)
    }

    /// Records a planned file (replacing an earlier entry with the same id).
    pub fn plan(&mut self, e: PlanEntry) {
        match self.planned.get(&e.file_id) {
            Some(&i) => self.rec.plan.entries[i] = e,
            None => {
                self.planned.insert(e.file_id, self.rec.plan.entries.len());
                self.rec.plan.entries.push(e);
            }
        }
        self.dirty = true;
    }

    /// Marks the plan complete (receiver), with what a resumed session needs
    /// to finish without the listing exchange.
    pub fn complete(
        &mut self,
        dirs: Vec<FileEntry>,
        errors: Vec<PathError>,
        skipped: u64,
        peer_listing_incomplete: bool,
        listed: &crate::scan::ListStats,
        created: u64,
    ) {
        let p = &mut self.rec.plan;
        p.plan_complete = true;
        p.dirs = dirs;
        p.errors = errors;
        p.skipped = skipped;
        p.peer_listing_incomplete = peer_listing_incomplete;
        p.listed = Some(listed.to_wire());
        p.created = created;
        self.dirty = true;
    }

    /// The created count changed after the plan was complete (a file
    /// planned again).
    pub fn created(&mut self, created: u64) {
        if self.rec.plan.created != created {
            self.rec.plan.created = created;
            self.dirty = true;
        }
    }

    /// Deletions made by every connection of this session so far.
    pub fn deleted(&self) -> u64 {
        self.rec.progress.deleted_count
    }

    pub fn add_deleted(&mut self, n: u64) {
        if n > 0 {
            self.rec.progress.deleted_count += n;
            self.dirty = true;
        }
    }

    /// Receiver, `-H`: the stat a placed file was hashed under, for the
    /// link check (see `PlacedStat`); a path placed again replaces it.
    pub fn placed(&mut self, path: &RelPath, stat: SourceStat) {
        let placed = &mut self.rec.progress.placed;
        match self.placed_at.get(path.as_bytes()) {
            Some(&i) => placed[i].stat = Some(stat),
            None => {
                self.placed_at
                    .insert(path.as_bytes().to_vec(), placed.len());
                placed.push(PlacedStat {
                    path: path.as_bytes().to_vec(),
                    stat: Some(stat),
                });
            }
        }
        self.dirty = true;
    }

    /// What [`Persister::placed`] recorded, this connection and earlier ones.
    pub fn placed_stats(&self) -> &[PlacedStat] {
        &self.rec.progress.placed
    }

    pub fn done(&mut self, file_id: u64) {
        if self.done.insert(file_id) {
            self.rec.progress.done_file_ids.push(file_id);
            self.dirty = true;
        }
    }

    /// This connection's byte count so far.
    pub fn bytes(&mut self, conn_total: u64) {
        let total = self.base_bytes + conn_total;
        if self.rec.progress.bytes_transferred != total {
            self.rec.progress.bytes_transferred = total;
            self.dirty = true;
        }
    }

    /// The session finished: its row is no longer needed.
    pub async fn finish(&mut self) {
        let (index, id) = (self.index.clone(), self.rec.id);
        let res = tokio::task::spawn_blocking(move || {
            index
                .lock()
                .map_err(|_| io::Error::other("index poisoned"))?
                .delete_session(&id)
                .map_err(io::Error::other)
        })
        .await
        .map_err(io::Error::other)
        .and_then(|r| r);
        if let Err(e) = res {
            tracing::warn!(error = %e, "removing the finished session failed");
        }
        self.dirty = false;
    }

    pub async fn maybe_flush(&mut self) {
        if self.dirty && self.last.elapsed() >= PERSIST_EVERY {
            self.flush().await;
        }
    }

    pub async fn flush(&mut self) {
        self.rec.updated_at = crate::index::now_secs();
        let row = self.rec.to_row();
        let index = self.index.clone();
        let res = tokio::task::spawn_blocking(move || {
            index
                .lock()
                .map_err(|_| io::Error::other("index poisoned"))?
                .save_session(&row)
                .map_err(io::Error::other)
        })
        .await
        .map_err(io::Error::other)
        .and_then(|r| r);
        match res {
            Ok(Some(evicted)) => {
                tracing::info!(evicted = ?hex(&evicted), "replaced older session with the same key")
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "persisting session failed"),
        }
        self.dirty = false;
        self.last = Instant::now();
    }
}

/// A walk's problems as per-path errors; an incomplete listing always counts.
pub(super) fn walk_errors(s: &ScanSummary) -> Vec<(RelPath, String)> {
    let mut v: Vec<(RelPath, String)> = s
        .errors
        .iter()
        .map(|(p, e)| (p.clone(), format!("walk: {e}")))
        .collect();
    if s.incomplete && v.is_empty() {
        v.push((RelPath::root(), "listing incomplete".into()));
    }
    v
}

/// At most this many per-path errors travel in `SessionDone`.
pub(super) const MAX_DONE_ERRORS: usize = 1000;

/// Errors for `SessionDone`: the first [`MAX_DONE_ERRORS`], then one line
/// counting the rest, so the frame stays far below the frame limit.
pub(super) fn capped_errors(errors: &[(RelPath, String)]) -> Vec<PathError> {
    let mut v: Vec<PathError> = errors
        .iter()
        .take(MAX_DONE_ERRORS)
        .map(|(p, m)| PathError {
            path: p.as_bytes().to_vec(),
            message: m.clone(),
        })
        .collect();
    if errors.len() > MAX_DONE_ERRORS {
        v.push(PathError {
            path: Vec::new(),
            message: format!("and {} more errors", errors.len() - MAX_DONE_ERRORS),
        });
    }
    v
}

/// `e` as it goes on the wire: without the device and inode, which are
/// this side's own and which the peer never reads.
pub(super) fn wire_entry(mut e: FileEntry) -> FileEntry {
    e.dev = 0;
    e.inode = 0;
    e
}

pub(super) fn msg_name(m: &Msg) -> &'static str {
    match m {
        Msg::Plan(_) => "PlanItem",
        Msg::ChunkListStart(_) => "ChunkListStart",
        Msg::ChunkListMore(_) => "ChunkListMore",
        Msg::WithdrawChunkList(_) => "WithdrawChunkList",
        Msg::Need(_) => "NeedChunks",
        Msg::ChunkBasis(_) => "ChunkBasis",
        Msg::Ack(_) => "FileAck",
        Msg::SourceError(_) => "SourceError",
        Msg::Done(_) => "SessionDone",
        Msg::Error(_) => "Error",
        Msg::Itemized(_) => "Itemized",
        Msg::Heartbeat(_) => "Heartbeat",
        Msg::ListRoom(_) => "ListRoom",
        Msg::MayPlace(_) => "MayPlace",
        Msg::PlaceVerdict(_) => "PlaceVerdict",
        Msg::Replan(_) => "Replan",
        Msg::Settle(_) => "Settle",
        Msg::Settled(_) => "Settled",
        Msg::CheckLink(_) => "CheckLink",
        Msg::CheckLinksEnd(_) => "CheckLinksEnd",
        Msg::Unlinked(_) => "Unlinked",
        Msg::LinksChecked(_) => "LinksChecked",
    }
}

pub(super) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the control reader makes of `bytes`: its first peer event.
    async fn first_event(bytes: &'static [u8]) -> Ev {
        let (tx, mut rx) = PeerTx::channel(&super::super::SideHooks::default());
        let reader = tokio::spawn(read_control(
            bytes,
            tx,
            super::super::EventBus::default(),
            Arc::default(),
            Arc::default(),
            Arc::default(),
            StreamTap::off(),
        ));
        let ev = rx.recv().await.expect("an event");
        reader.abort();
        ev
    }

    /// `SessionDone` is the last frame of a control stream. The heartbeat
    /// task and the walk run beside the role and may still have a
    /// `Heartbeat` to send; the peer stops reading at `SessionDone`, so it
    /// is dropped, and so is a paced frame, whose bytes go back to the
    /// budget.
    #[tokio::test]
    async fn nothing_is_queued_after_session_done() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Option<Queued>>();
        let (events, _events_rx) = mpsc::unbounded_channel();
        let out = Out {
            tx,
            budget: Arc::new(ByteBudget::new(OUT_BUDGET)),
            backlog: Arc::new(Backlog::new(usize::MAX)),
            events,
            done: Arc::default(),
            #[cfg(feature = "test-hooks")]
            peak: None,
        };
        let heartbeat = || Msg::Heartbeat(irsync_proto::v1::Heartbeat::default());
        out.send(heartbeat());
        out.send(Msg::Done(irsync_proto::v1::SessionDone::default()));
        let queued = out.backlog.bytes();
        out.send(heartbeat());
        out.clone().send_paced(heartbeat()).await;
        out.finish();

        let mut names = Vec::new();
        while let Some(Some(q)) = rx.recv().await {
            names.push(msg_name(q.frame.msg.as_ref().unwrap()));
        }
        assert_eq!(names, ["Heartbeat", "SessionDone"]);
        assert_eq!(
            out.backlog.bytes(),
            queued,
            "a dropped frame is not counted"
        );
        assert_eq!(
            out.budget.held(),
            0,
            "a dropped paced frame gives its bytes back"
        );
    }

    /// Bytes that are not a frame of ours break the protocol; they are not
    /// a failed stream, which a client would retry.
    #[tokio::test]
    async fn an_undecodable_control_frame_is_a_protocol_error() {
        // Field 1 with wire type 7, which does not exist.
        for bytes in [&[0u8, 0, 0, 1, 0x0f][..], &[0xff, 0xff, 0xff, 0xff][..]] {
            match first_event(bytes).await {
                Ev::ProtocolError(e) => assert!(e.contains("control stream"), "{e}"),
                Ev::PeerFailed(e) => panic!("a transport error: {e}"),
                _ => panic!("another event"),
            }
        }
    }

    /// A large frame still arriving is progress: the idle watchdog must not
    /// see a peer as silent while its `SessionDone` crosses a slow link.
    #[tokio::test]
    async fn bytes_of_an_unfinished_frame_count_as_activity() {
        use tokio::io::AsyncWriteExt;
        let (mut w, r) = tokio::io::duplex(1 << 16);
        let (tx, _rx) = PeerTx::channel(&super::super::SideHooks::default());
        let activity: Arc<std::sync::atomic::AtomicU64> = Arc::default();
        let reader = tokio::spawn(read_control(
            r,
            tx,
            super::super::EventBus::default(),
            activity.clone(),
            Arc::default(),
            Arc::default(),
            StreamTap::off(),
        ));
        // The length prefix of a 400 KB frame and the first part of its body.
        let mut prefix = Vec::new();
        crate::net::frame::encode_prefix(400_000, &mut prefix);
        w.write_all(&prefix).await.unwrap();
        w.write_all(&[0u8; 1000]).await.unwrap();
        let moved = async {
            while activity.load(std::sync::atomic::Ordering::Relaxed) == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        };
        let seen = tokio::time::timeout(std::time::Duration::from_secs(2), moved).await;
        reader.abort();
        assert!(seen.is_ok(), "no activity while a frame was arriving");
    }

    /// A receiver's heartbeat that reports data taken in on this
    /// connection is progress for the sender; one that does not is not.
    #[tokio::test]
    async fn a_heartbeat_with_received_bytes_counts_as_progress() {
        use irsync_proto::v1::Heartbeat;
        for (received_bytes, expected) in [(0, false), (4096, true)] {
            let (mut w, r) = tokio::io::duplex(1 << 16);
            let (tx, _rx) = PeerTx::channel(&super::super::SideHooks::default());
            let progress: Arc<std::sync::atomic::AtomicBool> = Arc::default();
            let reader = tokio::spawn(read_control(
                r,
                tx,
                super::super::EventBus::default(),
                Arc::default(),
                progress.clone(),
                Arc::default(),
                StreamTap::off(),
            ));
            let hb = Control {
                msg: Some(Msg::Heartbeat(Heartbeat {
                    received_bytes,
                    ..Heartbeat::default()
                })),
            };
            crate::net::frame::write_msg(&mut w, &hb).await.unwrap();
            // The reader ends at the end of the stream, after the heartbeat.
            drop(w);
            reader.await.unwrap();
            assert_eq!(
                progress.load(std::sync::atomic::Ordering::Relaxed),
                expected,
                "received_bytes {received_bytes}"
            );
        }
    }

    #[test]
    fn session_done_errors_are_capped() {
        let errs = |n: usize| -> Vec<(RelPath, String)> {
            (0..n)
                .map(|i| {
                    (
                        RelPath::new(format!("f{i}").as_bytes()).unwrap(),
                        "x".into(),
                    )
                })
                .collect()
        };
        assert_eq!(capped_errors(&errs(3)).len(), 3);
        assert_eq!(capped_errors(&errs(MAX_DONE_ERRORS)).len(), MAX_DONE_ERRORS);
        let v = capped_errors(&errs(MAX_DONE_ERRORS + 42));
        assert_eq!(v.len(), MAX_DONE_ERRORS + 1);
        let last = v.last().unwrap();
        assert!(last.path.is_empty());
        assert_eq!(last.message, "and 42 more errors");
    }

    /// The listing of a single file below an export's directory looks at
    /// nothing through a symlink put where the session root was.
    #[test]
    fn a_single_file_is_not_listed_through_a_swapped_directory() {
        let export = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(export.path().join("d")).unwrap();
        std::fs::write(export.path().join("d/f"), b"in").unwrap();
        std::fs::write(outside.path().join("f"), b"outside").unwrap();
        let rel = |s: &str| RelPath::new(s.as_bytes()).unwrap();
        let single = SingleFile {
            wire: rel("f"),
            local: rel("f"),
        };
        let filters = FilterSet::new(vec![]);
        let list = || {
            let (tx, mut rx) = mpsc::channel(2);
            let sum = list_single(
                export.path().join("d"),
                export.path(),
                &rel("d"),
                &single,
                &filters,
                &tx,
            );
            drop(tx);
            let mut got = Vec::new();
            while let Ok(ev) = rx.try_recv() {
                if let ScanEvent::Entry(item) = ev {
                    got.push((item.path, item.entry.kind(), item.entry.size));
                }
            }
            (sum, got)
        };
        let (sum, got) = list();
        assert!(!sum.incomplete);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, RelPath::root());
        assert_eq!(got[0].1, Kind::Directory);
        assert_eq!(got[1], (rel("f"), Kind::Regular, 2));

        std::fs::remove_dir_all(export.path().join("d")).unwrap();
        crate::platform::testutil::symlink(outside.path(), &export.path().join("d")).unwrap();
        // The link is listed as a link; the file behind it is not looked at.
        let (_, got) = list();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!((&got[0].0, got[0].1), (&RelPath::root(), Kind::Symlink));
    }
}
