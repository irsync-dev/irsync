//! Control-stream plumbing shared by both roles: a writer task that owns the
//! `SendStream`, a reader task that owns the `RecvStream` and dispatches
//! frames, the listing producer, and session persistence.

use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use iroh::endpoint::{RecvStream, SendStream};
use irsync_proto::chunk::FileDigest;
use irsync_proto::v1::{
    control::Msg, file_entry::Kind, Control, Hash, ListingEnd, ListingEntry, PlanEntry,
    SessionPlan, SessionProgress,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::SessionRecord;
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
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match read_msg::<Control, _>(&mut recv).await {
                Ok(Some(Control { msg: Some(msg) })) => match msg {
                    Msg::Listing(ListingEntry { entry: Some(e) }) => {
                        if let Some(tx) = &listing {
                            let _ = tx.send(side_of(e));
                        }
                    }
                    Msg::Listing(ListingEntry { entry: None }) => {
                        tracing::warn!("listing frame without entry; ignored");
                    }
                    m @ Msg::ListingEnd(_) => {
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
/// in a listing.
fn is_own_temp(path: &RelPath) -> bool {
    path.components().last().is_some_and(|n| {
        n.starts_with(b".irsync.") && (n.ends_with(b".part") || n.ends_with(b".tmp"))
    })
}

/// Paths of regular files this side has listed: the only paths it will send.
pub(super) type Listed = Arc<Mutex<HashSet<RelPath>>>;

pub(super) struct ListingSpec {
    pub root: PathBuf,
    pub filters: Arc<FilterSet>,
    pub hasher: Arc<Hasher>,
    /// Entries at or below this path are left out (a relative `--partial-dir`).
    pub skip: Option<RelPath>,
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
        while let Some(item) = rx.blocking_recv() {
            if is_own_temp(&item.path) || under_skip(&item.path) {
                continue;
            }
            let ScanItem {
                path, mut entry, ..
            } = item.clone();
            let digest = if entry.kind() == Kind::Regular {
                match spec.hasher.cached(&item) {
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
            if let Some(tx) = &local {
                let _ = tx.send(Side { entry, digest });
            }
            count += 1;
        }
        let summary = summary.blocking_recv().unwrap_or_else(|_| ScanSummary {
            incomplete: true,
            ..ScanSummary::default()
        });
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
pub(super) struct Persister {
    index: IndexHandle,
    rec: SessionRecord,
    dirty: bool,
    last: Instant,
}

impl Persister {
    /// Writes the initial row (empty plan) right away.
    pub async fn start(index: IndexHandle, id: [u8; 16], key: Vec<u8>) -> Persister {
        let now = now_secs();
        let mut p = Persister {
            index,
            rec: SessionRecord {
                id,
                key,
                plan: SessionPlan::default(),
                progress: SessionProgress::default(),
                created_at: now,
                updated_at: now,
            },
            dirty: true,
            last: Instant::now(),
        };
        p.flush().await;
        p
    }

    pub fn plan(&mut self, e: PlanEntry) {
        self.rec.plan.entries.push(e);
        self.dirty = true;
    }

    pub fn done(&mut self, file_id: u64) {
        self.rec.progress.done_file_ids.push(file_id);
        self.dirty = true;
    }

    pub fn bytes(&mut self, total: u64) {
        if self.rec.progress.bytes_transferred != total {
            self.rec.progress.bytes_transferred = total;
            self.dirty = true;
        }
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

pub(super) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
