//! Control-stream plumbing shared by both roles: a writer task that owns the
//! `SendStream`, a reader task that owns the `RecvStream` and dispatches
//! frames, the listing producer (whose frames go out on the listing stream,
//! see [`super::listing_stream`]), and session persistence.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iroh::endpoint::{RecvStream, SendStream};
use irsync_proto::chunk::FileDigest;
use irsync_proto::v1::{
    control::Msg, file_entry::Kind, Control, FileEntry, Hash, ListingEnd, ListingEntry, PathError,
    PlanEntry, SessionPlan, SessionProgress,
};
use prost::Message as _;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::Instrument;

use super::engine::{Ctx, PeerEvents};
use super::limits::{Backlog, ByteBudget, OUT_BUDGET, PEER_EVENTS};
use super::listing_stream::{ListingOut, PeerScan};
use super::{IndexScope, SessionRecord, StreamKind};
use crate::filter::FilterSet;
use crate::fs::paths::RelPath;
use crate::hasher::Hasher;
use crate::index::IndexHandle;
use crate::net::frame::{read_msg, write_msg};
use crate::plan::Side;
use crate::scan::{walk, ScanEvent, ScanItem, ScanSummary, WalkOpts, WALK_THREADS};
use crate::telemetry::Phase;

/// How long to wait for the peer to take our last bytes before giving up.
pub(super) const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// Session rows are written at most this often (plus a final write).
const PERSIST_EVERY: Duration = Duration::from_secs(2);

/// What a role's main loop reacts to: inbound control frames, transport
/// trouble, and completions of the tasks the role spawned.
pub(super) enum Ev {
    /// A control frame, or the peer's `ListingEnd` (from the listing stream).
    Ctrl(Msg),
    /// The peer started its listing (sent once, at its first listing frame).
    PeerListing,
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
    Send(super::sender_role::Done),
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
    fn queue(&self, q: Queued) -> bool {
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
) -> (Out, JoinHandle<()>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Option<Queued>>();
    let budget = Arc::new(ByteBudget::new(OUT_BUDGET));
    let backlog = Arc::new(Backlog::new(ceiling));
    let tamper = hooks.tamper();
    let (written, unqueued, writer_events) = (budget.clone(), backlog.clone(), events.clone());
    let task = tokio::spawn(
        async move {
            while let Some(Some(q)) = rx.recv().await {
                for msg in q
                    .frame
                    .msg
                    .into_iter()
                    .flat_map(|m| tamper.frames(StreamKind::Control, m))
                {
                    let c = Control { msg: Some(msg) };
                    if let Err(e) = write_msg(&mut send, &c).await {
                        let _ =
                            writer_events.send(Ev::WriterFailed(format!("control stream: {e}")));
                        return;
                    }
                }
                written.release(q.paced);
                unqueued.sub(q.len);
            }
            if send.finish().is_ok() {
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
/// its own; `scan` is what its reader has seen.
pub(super) fn spawn_reader(
    recv: RecvStream,
    events: PeerTx,
    bus: super::EventBus,
    activity: Arc<std::sync::atomic::AtomicU64>,
    scan: Arc<PeerScan>,
) -> JoinHandle<()> {
    tokio::spawn(
        read_control(recv, events, bus, activity, scan).instrument(tracing::Span::current()),
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
    scan: Arc<PeerScan>,
) {
    let mut recv = Counted {
        inner: recv,
        activity,
    };
    loop {
        // Activity is counted per read, in `Counted`.
        let frame = read_msg::<Control, _>(&mut recv).await;
        match frame {
            Ok(Some(Control { msg: Some(msg) })) => match msg {
                Msg::Listing(_) | Msg::ListingError(_) | Msg::ListingEnd(_) => {
                    // Read on: dropping the stream would stop the peer's
                    // writer, and that transport error could reach the peer
                    // before our `Error` saying why.
                    events
                        .send(Ev::ProtocolError(
                            "listing frame on the control stream".into(),
                        ))
                        .await;
                }
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
                    if h.skipped_bytes > 0 {
                        bus.emit(Event::PeerSkipped(h.skipped_bytes));
                    }
                    if h.deleted > 0 || h.delete_total > 0 {
                        bus.emit(Event::Delete {
                            party: Party::Peer,
                            done: h.deleted,
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
        .is_some_and(crate::recv::is_temp_name)
}

/// Paths of regular files this side has listed, the only paths it will
/// send, with the walk's stat of each for the digest's quick check.
pub(super) type ListedFiles = Arc<Mutex<HashMap<RelPath, crate::hasher::Freshness>>>;

pub(super) struct ListingSpec {
    /// The `list` phase; it ends with the walk.
    pub phase: Phase,
    pub root: PathBuf,
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
    let (mut rx, summary) = walk(
        spec.root.clone(),
        spec.filters.clone(),
        WalkOpts {
            follow_root_symlink: true,
            threads: WALK_THREADS,
            max_depth: (!spec.recursive).then_some(1),
        },
        spec.meter.clone(),
    );
    // Waits on `listing` and `local` for the whole walk: a thread of its
    // own, not one of the blocking pool's (see `scan::spawn_thread`).
    crate::scan::spawn_thread("irsync-listing", move || {
        let _in_list = spec.phase.span().clone().entered();
        let under_skip = |p: &RelPath| {
            spec.skip.as_ref().is_some_and(|s| {
                let (p, s) = (p.as_bytes(), s.as_bytes());
                p.starts_with(s) && (p.len() == s.len() || p[s.len()] == b'/')
            })
        };
        let mut count = 0u64;
        // `ListingEntry` frames sent, injected ones included.
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
        let send = |msg: Msg| {
            #[cfg(feature = "test-hooks")]
            if let Some(n) = spec.cut_after {
                if queued.get() >= n {
                    return;
                }
                queued.set(queued.get() + 1);
            }
            listing.send_blocking(msg);
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
                    send(Msg::ListingError(PathError {
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
                let keyed = ScanItem {
                    path: spec.scope.key(&item.path),
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
            send(Msg::Listing(ListingEntry {
                entry: Some(entry.clone()),
            }));
            sent += 1;
            #[cfg(feature = "test-hooks")]
            if let Some(inject) = &spec.inject {
                for e in inject(&entry) {
                    send(Msg::Listing(ListingEntry { entry: Some(e) }));
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
        send(Msg::ListingEnd(ListingEnd {
            incomplete: summary.incomplete,
            count: sent,
        }));
        listing.finish();
        summary
    })
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
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
    dirty: bool,
    last: Instant,
}

impl Persister {
    /// Continues the session's stored record (if any) and writes the row right away.
    pub async fn start(ctx: &Ctx) -> Persister {
        let now = now_secs();
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
        let mut p = Persister {
            index: ctx.index().clone(),
            base_bytes: rec.progress.bytes_transferred,
            rec,
            planned,
            done,
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
    ) {
        let p = &mut self.rec.plan;
        p.plan_complete = true;
        p.dirs = dirs;
        p.errors = errors;
        p.skipped = skipped;
        p.peer_listing_incomplete = peer_listing_incomplete;
        self.dirty = true;
    }

    /// Deletions made by every connection of this session so far.
    pub fn deleted(&self) -> u64 {
        self.rec.progress.deleted
    }

    pub fn add_deleted(&mut self, n: u64) {
        if n > 0 {
            self.rec.progress.deleted += n;
            self.dirty = true;
        }
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
        self.rec.updated_at = now_secs();
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

pub(super) fn msg_name(m: &Msg) -> &'static str {
    match m {
        Msg::Listing(_) => "ListingEntry",
        Msg::ListingEnd(_) => "ListingEnd",
        Msg::ListingError(_) => "ListingError",
        Msg::Plan(_) => "PlanItem",
        Msg::ChunkListPart(_) => "ChunkListPart",
        Msg::Need(_) => "NeedChunks",
        Msg::ChunkBasis(_) => "ChunkBasis",
        Msg::Ack(_) => "FileAck",
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
        ));
        let ev = rx.recv().await.expect("an event");
        reader.abort();
        ev
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
        ));
        // The length prefix of a 400 KB frame and the first part of its body.
        w.write_all(&400_000u32.to_be_bytes()).await.unwrap();
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
}
