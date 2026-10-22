//! Control-stream plumbing shared by both roles: a writer task that owns the
//! `SendStream`, a reader task that owns the `RecvStream` and dispatches
//! frames, the listing producer, and session persistence.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use iroh::endpoint::{RecvStream, SendStream};
use irsync_proto::chunk::FileDigest;
use irsync_proto::v1::{
    control::Msg, file_entry::Kind, Control, FileEntry, Hash, ListingEnd, ListingEntry, PathError,
    PlanEntry, SessionPlan, SessionProgress,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::engine::Ctx;
use super::{IndexScope, SessionRecord};
use crate::filter::FilterSet;
use crate::fs::paths::RelPath;
use crate::hasher::Hasher;
use crate::index::IndexHandle;
use crate::net::frame::{read_msg, write_msg};
use crate::plan::Side;
use crate::scan::{walk, ScanItem, ScanSummary, WalkOpts};

/// How long to wait for the peer to take our last bytes before giving up.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// Session rows are written at most this often (plus a final write).
const PERSIST_EVERY: Duration = Duration::from_secs(2);
const WALK_THREADS: usize = 4;

/// What a role's main loop reacts to: inbound control frames, transport
/// trouble, and completions of the tasks the role spawned.
pub(super) enum Ev {
    /// A control frame other than a listing entry.
    Ctrl(Msg),
    /// The peer started streaming a listing this side does not consume
    /// (sent once, at its first entry).
    PeerListing,
    /// The peer finished its control stream.
    PeerEof,
    PeerFailed(String),
    WriterFailed(String),
    /// Role-specific task completion.
    Recv(super::receiver::Done),
    Send(super::sender_role::Done),
}

/// Handle for queueing control frames to the writer task.
#[derive(Clone)]
pub(super) struct Out {
    tx: mpsc::UnboundedSender<Option<Control>>,
}

impl Out {
    pub fn send(&self, msg: Msg) {
        // A closed writer surfaces as `Ev::WriterFailed`; nothing to do here.
        let _ = self.tx.send(Some(Control { msg: Some(msg) }));
    }

    /// Ask the writer to finish the stream after everything queued so far.
    pub fn finish(&self) {
        let _ = self.tx.send(None);
    }
}

/// Spawns the writer: frames go out in queue order; `finish` ends the stream
/// and waits (bounded) until the peer has the bytes.
pub(super) fn spawn_writer(
    mut send: SendStream,
    events: mpsc::UnboundedSender<Ev>,
) -> (Out, JoinHandle<()>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Option<Control>>();
    let task = tokio::spawn(async move {
        while let Some(Some(c)) = rx.recv().await {
            if let Err(e) = write_msg(&mut send, &c).await {
                let _ = events.send(Ev::WriterFailed(e.to_string()));
                return;
            }
        }
        if send.finish().is_ok() {
            let _ = tokio::time::timeout(DRAIN_TIMEOUT, send.stopped()).await;
        }
    });
    (Out { tx }, task)
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

/// Spawns the reader: listing entries go to `listing` (dropped when `None`),
/// which is closed at `ListingEnd`; every other frame goes to `events`.
pub(super) fn spawn_reader(
    mut recv: RecvStream,
    mut listing: Option<mpsc::UnboundedSender<Side>>,
    events: mpsc::UnboundedSender<Ev>,
    bus: super::EventBus,
    activity: Arc<std::sync::atomic::AtomicU64>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut announced = false;
        // Receiver role: what the peer lists, reported at its `ListingEnd`.
        let mut seen = crate::scan::ListStats::default();
        loop {
            let frame = read_msg::<Control, _>(&mut recv).await;
            activity.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            match frame {
                Ok(Some(Control { msg: Some(msg) })) => match msg {
                    Msg::Listing(ListingEntry { entry: Some(e) }) => match &listing {
                        Some(tx) => {
                            seen.count(&e);
                            let _ = tx.send(side_of(e));
                        }
                        None if !announced => {
                            announced = true;
                            let _ = events.send(Ev::PeerListing);
                        }
                        None => {}
                    },
                    Msg::Listing(ListingEntry { entry: None }) => {
                        tracing::warn!("listing frame without entry; ignored");
                    }
                    // Already counted as activity above; nothing else to do.
                    Msg::Heartbeat(_) => {}
                    m @ Msg::ListingEnd(_) => {
                        if listing.is_some() {
                            bus.emit(crate::session::Event::Total {
                                files: seen.reg,
                                bytes: seen.total_size,
                            });
                        }
                        listing = None;
                        let _ = events.send(Ev::Ctrl(m));
                    }
                    m => {
                        let _ = events.send(Ev::Ctrl(m));
                    }
                },
                Ok(Some(Control { msg: None })) => {
                    tracing::warn!("empty control frame; ignored");
                }
                Ok(None) => {
                    let _ = events.send(Ev::PeerEof);
                    return;
                }
                Err(e) => {
                    let _ = events.send(Ev::PeerFailed(e.to_string()));
                    return;
                }
            }
        }
    })
}

/// irsync's own temporary files (`.irsync.<name>.part` / `.tmp`) never appear
/// in a listing, and a receiver refuses them in a peer's.
pub(super) fn is_own_temp(path: &RelPath) -> bool {
    path.components()
        .last()
        .is_some_and(crate::recv::is_temp_name)
}

/// Paths of regular files this side has listed: the only paths it will send.
pub(super) type Listed = Arc<Mutex<HashSet<RelPath>>>;

pub(super) struct ListingSpec {
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
    /// Where to report scan progress (the sender's walk only).
    pub bus: Option<super::EventBus>,
    /// Test hook: extra entries to list right after an entry.
    #[cfg(feature = "test-hooks")]
    pub inject: Option<super::ListingHook>,
}

/// Walks `spec.root` and streams the listing to the peer (with cached hashes
/// only; nothing is read), then `ListingEnd`. Each entry also goes to `local`
/// (the receiver's planner) and regular files are recorded in `listed`.
pub(super) fn spawn_listing(
    spec: ListingSpec,
    out: Out,
    local: Option<mpsc::UnboundedSender<Side>>,
    listed: Option<Listed>,
) -> JoinHandle<ScanSummary> {
    let (mut rx, summary) = walk(
        spec.root.clone(),
        spec.filters.clone(),
        WalkOpts {
            follow_root_symlink: true,
            threads: WALK_THREADS,
        },
    );
    tokio::task::spawn_blocking(move || {
        let under_skip = |p: &RelPath| {
            spec.skip.as_ref().is_some_and(|s| {
                let (p, s) = (p.as_bytes(), s.as_bytes());
                p.starts_with(s) && (p.len() == s.len() || p[s.len()] == b'/')
            })
        };
        let mut count = 0u64;
        let mut stats = crate::scan::ListStats::default();
        // `-H`: the first listed name of each (dev, inode) with nlink > 1.
        let mut first_names: HashMap<(u64, u64), Vec<u8>> = HashMap::new();
        while let Some(item) = rx.blocking_recv() {
            if is_own_temp(&item.path) || under_skip(&item.path) {
                continue;
            }
            let ScanItem {
                path, mut entry, ..
            } = item.clone();
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
                match spec.hasher.cached(&keyed) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!(?path, error = %e, "index lookup failed");
                        None
                    }
                }
            } else {
                None
            };
            if let Some(d) = &digest {
                entry.whole_hash = Some(Hash {
                    bytes: d.hash.to_vec(),
                });
            }
            if entry.kind() == Kind::Regular {
                if let Some(l) = &listed {
                    l.lock().unwrap_or_else(|e| e.into_inner()).insert(path);
                }
            }
            out.send(Msg::Listing(ListingEntry {
                entry: Some(entry.clone()),
            }));
            #[cfg(feature = "test-hooks")]
            if let Some(inject) = &spec.inject {
                for e in inject(&entry) {
                    out.send(Msg::Listing(ListingEntry { entry: Some(e) }));
                }
            }
            stats.count(&entry);
            if let Some(tx) = &local {
                let _ = tx.send(Side { entry, digest });
            }
            count += 1;
            if count.is_multiple_of(1000) {
                if let Some(b) = &spec.bus {
                    b.emit(super::Event::Scanned(count));
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
                files: stats.reg,
                bytes: stats.total_size,
            });
        }
        for (p, e) in &summary.errors {
            tracing::warn!(path = ?p, error = %e, "walk error");
        }
        out.send(Msg::ListingEnd(ListingEnd {
            incomplete: summary.incomplete,
            count,
        }));
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
        Msg::Plan(_) => "PlanItem",
        Msg::ChunkListPart(_) => "ChunkListPart",
        Msg::Need(_) => "NeedChunks",
        Msg::Ack(_) => "FileAck",
        Msg::Done(_) => "SessionDone",
        Msg::Error(_) => "Error",
        Msg::Itemized(_) => "Itemized",
        Msg::Heartbeat(_) => "Heartbeat",
    }
}

pub(super) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
