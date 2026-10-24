//! The receiver role: walks its own tree, merge-joins it with the sender's
//! listing and decides per path (rsync-generator style). Directories,
//! symlinks, specials and metadata are applied locally; regular files are
//! requested with `PlanItem`, opened as verified `.part` writers when their
//! chunk list arrives, filled from local copies and data streams, and
//! acknowledged only after `finalize` has verified and renamed them.
//!
//! Deletions (`--delete*`) run here too: before the transfers (asked for by
//! the planner once both listings have ended) or after them, before the
//! directory metadata pass. A dry run plans and reports but changes nothing.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use irsync_proto::chunk::FileDigest;
use irsync_proto::v1::{
    control::Msg, file_ack::Status, plan_item::Action, ChunkListPart, FileAck, FileEntry, Itemized,
    ListingEnd, NeedChunks, PathError, PlanEntry, SessionDone,
};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::task::TaskTracker;

use super::control::{
    capped_errors, msg_name, spawn_listing, walk_errors, Ev, ListingSpec, Out, Persister,
};
use super::engine::{blocking, Ctx, Events, PeerError, TransportError};
use super::parts::{assemble, Assembly};
use super::planner::{
    inflight_cap, parent_of, plan, Deferred, Linked, Planned, PlannerReport, Prior,
};
use super::recv_io::{
    accept_streams, copy_local_chunks, finalize, offer_bases, open_writer, sender_idx,
    settle_identical, Fill, Opened, Route, Routes,
};
use super::Outcome;
use crate::cli::DeleteMode;
use crate::delete::DeletePass;
use crate::filter::FilterSet;
use crate::fs::meta::apply_metadata;
use crate::fs::paths::RelPath;
use crate::plan::Side;
use crate::recv::{FileWriter, FinalizeError};
use recv_delete::{capped_paths, MAX_DONE_PATHS};

#[path = "recv_delete.rs"]
mod recv_delete;
#[path = "recv_links.rs"]
mod recv_links;
#[path = "recv_resume.rs"]
mod recv_resume;

/// How long to wait for the sender to close its end after `SessionDone`.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);
/// A file is given up after this many whole-file hash mismatches.
const MAX_MISMATCHES: u32 = 2;
/// Re-requests per file and connection for chunks its data streams did not deliver.
const MAX_REREQUESTS: u32 = 3;
/// How long a failed session waits for its own file tasks to stop.
const TASK_DRAIN: Duration = Duration::from_secs(10);

/// Completions of the receiver's tasks, delivered to its main loop.
pub(super) enum Done {
    Planned(Box<Planned>),
    /// A hard link to place once every regular file is placed.
    Linked(Box<Linked>),
    PlannerFinished(Box<PlannerReport>),
    /// `--delete-before`: every candidate is known; delete once both
    /// listings have ended and reply with what was deleted.
    DeleteBefore(Box<DeletePass>, oneshot::Sender<Vec<RelPath>>),
    Opened {
        file_id: u64,
        gen: u32,
        result: io::Result<Opened>,
    },
    /// Every chunk of the file is verified on disk.
    Ready {
        file_id: u64,
        gen: u32,
        writer: FileWriter,
    },
    Finalized {
        file_id: u64,
        gen: u32,
        result: Result<(), FinalizeError>,
    },
    /// Writing a received chunk failed for a reason other than verification.
    StreamFailed {
        file_id: u64,
        gen: u32,
        msg: String,
    },
    /// Every data stream for the requested chunks has ended, and some
    /// chunks are still missing.
    Drained {
        file_id: u64,
        gen: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum St {
    /// `PlanItem` sent (or a retry requested); waiting for a chunk list.
    Requested,
    Opening,
    Receiving,
    Finalizing,
}

struct RFile {
    path: RelPath,
    entry: FileEntry,
    action: Action,
    /// `DELTA`: the planner's early hash of the old file, handed to the
    /// first `open_writer` so it waits instead of reading the file again,
    /// and the itemize line, emitted once the chunk list is resolved.
    deferred: Deferred,
    /// The destination already held the content: settled in place, not placed.
    identical: bool,
    /// Bumped for every new chunk list and on failure; stale task results
    /// and data streams carry an older value and are ignored.
    gen: u32,
    mismatches: u32,
    /// `NeedChunks` re-sent for chunks the data streams did not deliver.
    rerequests: u32,
    st: St,
    /// A chunk list that arrived while the `.part` was busy (opening or finalizing).
    pending: Option<Arc<FileDigest>>,
    /// A chunk list arriving in parts, until its last part.
    assembly: Option<Assembly>,
    /// The sender gave up on this file while we were finalizing it.
    abandoned: Option<String>,
    /// The current chunk list as native records (set once opened).
    digest: Option<Arc<FileDigest>>,
    /// The planner's in-flight slot (none for a resumed plan), released
    /// when the file leaves `files`.
    _slot: Option<tokio::sync::OwnedSemaphorePermit>,
}

/// Itemizes a file from the receiver: to the sending client when this is
/// a daemon asked for `-i`, and on the local bus.
fn emit_itemized(ctx: &Ctx, out: &Out, path: &RelPath, code: String) {
    if ctx.opts.itemize && ctx.remote_id.is_some() {
        out.send(Msg::Itemized(Itemized {
            path: path.as_bytes().to_vec(),
            code: code.clone(),
        }));
    }
    ctx.hooks
        .bus
        .emit(super::Event::Itemized(path.clone(), code));
}

struct Receiver {
    ctx: Arc<Ctx>,
    out: Out,
    events: mpsc::UnboundedSender<Ev>,
    routes: Routes,
    files: HashMap<u64, RFile>,
    /// Files in flight (entries in `files`). A file leaves `files` when it is
    /// placed or has failed, so no chunk list outlives its file.
    active: usize,
    report: Option<PlannerReport>,
    peer_listing_done: bool,
    peer_listing_incomplete: bool,
    /// This side's walk (absent when a resumed session reuses its plan).
    listing: Option<JoinHandle<crate::scan::ScanSummary>>,
    /// Problems of this side's walk, known once it ended.
    walk_errors: Vec<(RelPath, String)>,
    /// This side's walk was incomplete (known once it ended).
    local_listing_incomplete: bool,
    /// A `--delete-before` pass waiting for the sender's `ListingEnd`.
    pending_before: Option<(DeletePass, oneshot::Sender<Vec<RelPath>>)>,
    deleted: Vec<RelPath>,
    would_delete: Vec<RelPath>,
    max_delete_reached: bool,
    /// The stored plan says every path is planned.
    plan_complete: bool,
    files_done: u64,
    /// Summed size of the files placed.
    transferred_size: u64,
    errors: Vec<(RelPath, String)>,
    persist: Persister,
    /// Open and finalize tasks, waited for when the session fails.
    tasks: TaskTracker,
    /// Hard links waiting for the final pass.
    links: Vec<Linked>,
    /// Files that failed this session (never linked to).
    failed: HashSet<RelPath>,
    /// A resumed plan's files not yet requested (see `pump`).
    queued: VecDeque<(u64, RelPath, FileEntry, Action, u64)>,
    /// At most this many files in flight.
    inflight_cap: usize,
}

impl Receiver {
    fn complete(&self) -> bool {
        self.report.is_some()
            && self.peer_listing_done
            && self.active == 0
            && self.queued.is_empty()
    }

    /// Once the sender's listing has ended and every path is planned, the
    /// plan is stored as complete: a resumed session then needs no listings.
    async fn maybe_mark_complete(&mut self) {
        if self.plan_complete || !self.peer_listing_done {
            return;
        }
        if self.report.is_none() {
            return;
        }
        self.await_own_walk().await;
        let Some(report) = &self.report else {
            return;
        };
        let dirs = report
            .dirs
            .iter()
            .filter(|d| d.apply || report.touched.contains(&d.path))
            .map(|d| FileEntry {
                path: d.path.as_bytes().to_vec(),
                ..d.entry.clone()
            })
            .collect();
        let errors: Vec<(RelPath, String)> = self
            .walk_errors
            .iter()
            .chain(&report.errors)
            .cloned()
            .collect();
        let errors = errors
            .iter()
            .map(|(p, m)| PathError {
                path: p.as_bytes().to_vec(),
                message: m.clone(),
            })
            .collect();
        self.persist
            .complete(dirs, errors, report.skipped, self.peer_listing_incomplete);
        self.plan_complete = true;
    }

    /// Waits for this side's walk to end and records its problems.
    async fn await_own_walk(&mut self) {
        if let Some(listing) = self.listing.take() {
            match listing.await {
                Ok(summary) => {
                    self.walk_errors = walk_errors(&summary);
                    self.local_listing_incomplete = summary.incomplete;
                }
                Err(e) => {
                    self.walk_errors = vec![(RelPath::root(), format!("listing failed: {e}"))];
                    self.local_listing_incomplete = true;
                }
            }
        }
    }

    /// A file requested from the sender, now in flight.
    fn register(
        &mut self,
        file_id: u64,
        path: RelPath,
        entry: FileEntry,
        action: Action,
        deferred: Deferred,
        slot: Option<tokio::sync::OwnedSemaphorePermit>,
    ) {
        self.active += 1;
        #[cfg(feature = "test-hooks")]
        if let Some(peak) = &self.ctx.hooks.inflight_peak {
            peak.fetch_max(self.active, Ordering::SeqCst);
        }
        self.files.insert(
            file_id,
            RFile {
                path,
                entry,
                action,
                deferred,
                identical: false,
                gen: 0,
                mismatches: 0,
                rerequests: 0,
                st: St::Requested,
                pending: None,
                assembly: None,
                abandoned: None,
                digest: None,
                _slot: slot,
            },
        );
    }

    /// Every data stream for the file has ended with chunks still missing:
    /// ask again for those (bounded), or give the file up.
    async fn on_drained(&mut self, file_id: u64, gen: u32) {
        let Some(f) = self.files.get_mut(&file_id) else {
            return;
        };
        if f.gen != gen || f.st != St::Receiving {
            return;
        }
        let route = self
            .routes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&file_id)
            .cloned();
        let Some(route) = route.filter(|r| r.gen == gen) else {
            return;
        };
        let missing = match route.slot.lock().await.as_ref() {
            Some(w) => w.missing_chunks(),
            None => return,
        };
        if missing.is_empty() || route.pending.load(Ordering::SeqCst) != 0 {
            return;
        }
        if f.rerequests >= MAX_REREQUESTS {
            let msg = "incomplete after retries".to_string();
            self.fail(file_id, Some(Status::WriteError), msg).await;
            return;
        }
        f.rerequests += 1;
        #[cfg(feature = "test-hooks")]
        if let Some(c) = &self.ctx.rerequests {
            c.fetch_add(1, Ordering::SeqCst);
        }
        tracing::info!(path = ?f.path, missing = missing.len(), "re-requesting chunks");
        route.pending.fetch_add(missing.len(), Ordering::SeqCst);
        self.out.send(Msg::Need(NeedChunks {
            file_id,
            chunk_idx: missing
                .into_iter()
                .map(|i| sender_idx(route.idx_base, i))
                .collect(),
        }));
    }

    fn ack(&self, file_id: u64, status: Status, message: String) {
        self.out.send(Msg::Ack(FileAck {
            file_id,
            status: status as i32,
            message,
        }));
    }

    /// Takes the file's writer out of the routing table (it is dropped, which
    /// persists its progress for a later writer to adopt).
    async fn retire_writer(&self, file_id: u64) {
        let route = self
            .routes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&file_id);
        if let Some(r) = route {
            drop(r.slot.lock().await.take());
            if let Some(g) = &r.gate {
                g.close();
            }
        }
    }

    /// Marks the file failed; `ack` tells the sender why (none if the sender
    /// itself gave up).
    async fn fail(&mut self, file_id: u64, ack: Option<Status>, msg: String) {
        self.retire_writer(file_id).await;
        // Finished files leave the table; late events for them are ignored.
        let Some(f) = self.files.remove(&file_id) else {
            return;
        };
        self.active -= 1;
        tracing::warn!(path = ?f.path, error = %msg, "file failed");
        self.failed.insert(f.path.clone());
        self.errors.push((f.path, msg.clone()));
        if let Some(status) = ack {
            self.ack(file_id, status, msg);
        }
    }

    /// One part of a chunk list; the complete list goes to `on_chunk_list`.
    /// A malformed sequence fails the file, like a list that does not tile it.
    async fn on_chunk_list_part(&mut self, p: ChunkListPart) {
        let file_id = p.file_id;
        let Some(f) = self.files.get_mut(&file_id) else {
            tracing::debug!(file_id, "chunk list part for a file not in flight");
            return;
        };
        self.ctx.hooks.event("part", &f.path);
        match assemble(&mut f.assembly, p) {
            Ok(Some(d)) => self.on_chunk_list(file_id, Arc::new(d)).await,
            Ok(None) => {}
            Err(e) => {
                let msg = format!("chunk list in parts: {e}");
                self.fail(file_id, Some(Status::WriteError), msg).await;
            }
        }
    }

    async fn on_chunk_list(&mut self, file_id: u64, digest: Arc<FileDigest>) {
        let Some(f) = self.files.get_mut(&file_id) else {
            tracing::debug!(file_id, "chunk list for a file not in flight");
            return;
        };
        f.assembly = None;
        match f.st {
            St::Opening | St::Finalizing => {
                f.pending = Some(digest);
                return;
            }
            St::Requested | St::Receiving => {}
        }
        f.gen += 1;
        f.st = St::Opening;
        self.ctx.hooks.event("chunklist", &f.path);
        let (gen, path, action) = (f.gen, f.path.clone(), f.action);
        #[cfg(feature = "test-hooks")]
        if let Some(h) = &self.ctx.hooks.before_open {
            h(&path).await;
        }
        let prehash = f.deferred.prehash.take();
        let sparse = f.entry.sparse || self.ctx.opts.preserve.sparse;
        // A new chunk list replaces the previous writer; it must be gone
        // before the next one opens the same `.part`.
        self.retire_writer(file_id).await;
        let (ctx, events) = (self.ctx.clone(), self.events.clone());
        self.tasks.spawn(async move {
            let result = open_writer(ctx, path, action, prehash, digest, sparse).await;
            let _ = events.send(Ev::Recv(Done::Opened {
                file_id,
                gen,
                result,
            }));
        });
    }

    fn spawn_finalize(&self, file_id: u64, gen: u32, w: FileWriter, digest: Arc<FileDigest>) {
        let f = &self.files[&file_id];
        let (ctx, events, path, entry) = (
            self.ctx.clone(),
            self.events.clone(),
            f.path.clone(),
            f.entry.clone(),
        );
        self.tasks.spawn(async move {
            let result = finalize(ctx, w, path, entry, digest).await;
            let _ = events.send(Ev::Recv(Done::Finalized {
                file_id,
                gen,
                result,
            }));
        });
    }

    async fn on_done(&mut self, d: Done) {
        match d {
            Done::Linked(l) => self.on_linked(*l),
            Done::Planned(p) => {
                self.persist.plan(PlanEntry {
                    file_id: p.file_id,
                    path: p.path.as_bytes().to_vec(),
                    action: p.action as i32,
                    size: p.entry.size,
                    whole_hash: p.entry.whole_hash.clone(),
                    entry: Some(p.entry.clone()),
                    append_from: p.append_from,
                });
                let p = *p;
                self.register(
                    p.file_id,
                    p.path,
                    p.entry,
                    p.action,
                    p.deferred,
                    Some(p.slot),
                );
            }
            Done::PlannerFinished(r) => {
                self.report = Some(*r);
                self.maybe_mark_complete().await;
            }
            Done::DeleteBefore(pass, reply) => {
                self.pending_before = Some((*pass, reply));
                self.maybe_delete_before().await;
            }
            Done::Drained { file_id, gen } => self.on_drained(file_id, gen).await,
            Done::Opened {
                file_id,
                gen,
                result,
            } => self.on_opened(file_id, gen, result).await,
            Done::Ready {
                file_id,
                gen,
                writer,
            } => {
                let Some(f) = self.files.get_mut(&file_id) else {
                    return;
                };
                if f.gen != gen || f.st != St::Receiving {
                    return;
                }
                f.st = St::Finalizing;
                let digest = f.digest.clone();
                self.routes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&file_id);
                match digest {
                    Some(d) => self.spawn_finalize(file_id, gen, writer, d),
                    None => {
                        let msg = "internal: no digest for a ready file".to_string();
                        self.fail(file_id, Some(Status::WriteError), msg).await;
                    }
                }
            }
            Done::Finalized {
                file_id,
                gen,
                result,
            } => self.on_finalized(file_id, gen, result).await,
            Done::StreamFailed { file_id, gen, msg } => {
                let current = self
                    .files
                    .get(&file_id)
                    .is_some_and(|f| f.gen == gen && f.st == St::Receiving);
                if current {
                    self.fail(file_id, Some(Status::WriteError), msg).await;
                }
            }
        }
    }

    async fn on_opened(&mut self, file_id: u64, gen: u32, result: io::Result<Opened>) {
        let Some(f) = self.files.get_mut(&file_id) else {
            return;
        };
        if f.gen != gen || f.st != St::Opening {
            return;
        }
        if let Some(cl) = f.pending.take() {
            // Superseded while opening: drop this writer, open the newer list.
            drop(result);
            f.st = St::Requested;
            self.on_chunk_list(file_id, cl).await;
            return;
        }
        let o = match result {
            Ok(o) => o,
            Err(e) => {
                let msg = if e.kind() == io::ErrorKind::WouldBlock {
                    // Another session writes the same destination; a later
                    // run (or this session's resume) retries it.
                    crate::recv::PART_BUSY.to_string()
                } else {
                    format!("opening partial file: {e}")
                };
                self.fail(file_id, Some(Status::WriteError), msg).await;
                return;
            }
        };
        let fill = match o {
            Opened::Identical { file, digest } => {
                f.digest = Some(digest.clone());
                f.identical = true;
                f.st = St::Finalizing;
                self.ctx
                    .hooks
                    .bus
                    .emit(super::Event::Skipped(f.path.clone(), f.entry.size));
                self.ctx.hooks.event("identical", &f.path);
                if let Some(code) = f.deferred.itemize.take().and_then(|i| i.meta) {
                    emit_itemized(&self.ctx, &self.out, &f.path, code);
                }
                // The sender has nothing to deliver; it waits for the ack.
                self.out.send(Msg::Need(NeedChunks {
                    file_id,
                    chunk_idx: Vec::new(),
                }));
                let (ctx, events, path, entry) = (
                    self.ctx.clone(),
                    self.events.clone(),
                    f.path.clone(),
                    f.entry.clone(),
                );
                self.tasks.spawn(async move {
                    let result = settle_identical(ctx, file, path, entry, digest).await;
                    let _ = events.send(Ev::Recv(Done::Finalized {
                        file_id,
                        gen,
                        result,
                    }));
                });
                return;
            }
            Opened::Writer(fill) => *fill,
        };
        f.digest = Some(fill.digest.clone());
        if let Some(i) = f.deferred.itemize.take() {
            emit_itemized(&self.ctx, &self.out, &f.path, i.planned);
        }
        self.ctx
            .hooks
            .bus
            .emit(super::Event::FileStart(f.path.clone(), f.entry.size));
        let Fill {
            writer,
            remote,
            local,
            digest,
            bases,
            old,
            gate,
            idx_base,
        } = fill;
        // Everything not requested from the sender (local copies, adopted
        // chunks) needs no transfer.
        let needed: u64 = remote
            .iter()
            .filter_map(|&i| digest.chunks.get(i as usize))
            .map(|c| c.len as u64)
            .sum();
        self.ctx.hooks.bus.emit(super::Event::FileMatched(
            f.path.clone(),
            digest.size.saturating_sub(needed),
        ));
        self.ctx.hooks.event("need", &f.path);
        if remote.is_empty() && local.is_empty() {
            f.st = St::Finalizing;
            // Nothing needed: every chunk was adopted from earlier progress.
            self.out.send(Msg::Need(NeedChunks {
                file_id,
                chunk_idx: Vec::new(),
            }));
            self.spawn_finalize(file_id, gen, writer, digest);
            return;
        }
        f.st = St::Receiving;
        let route = Route {
            gen,
            slot: Arc::new(tokio::sync::Mutex::new(Some(writer))),
            digest: digest.clone(),
            pending: Arc::new(AtomicUsize::new(
                remote.len() + usize::from(!local.is_empty()),
            )),
            path: f.path.clone(),
            old,
            gate,
            idx_base,
        };
        // Routed before the request goes out, so no data stream can miss it.
        self.routes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(file_id, route.clone());
        // The request for the chunks without a basis first (empty means
        // "nothing needed for now"); the chunks with one are requested by
        // the basis task once their signatures are out; the local copies
        // run while the sender delivers. Every remote chunk is already
        // counted in `pending`, whichever request carries it.
        // In place, writes are ordered, so the file gets one ascending
        // request (after its bases): its streams then open in order and
        // the one the cursor needs is never behind the others.
        let (raw, request) = match (&route.gate, &route.old) {
            (Some(_), Some(_)) => (None, remote),
            _ => {
                let based: HashSet<u32> = bases.iter().map(|b| b.0).collect();
                let (based, raw): (Vec<u32>, Vec<u32>) =
                    remote.into_iter().partition(|i| based.contains(i));
                (Some(raw), based)
            }
        };
        if let Some(raw) = raw {
            self.out.send(Msg::Need(NeedChunks {
                file_id,
                chunk_idx: raw.into_iter().map(|i| sender_idx(idx_base, i)).collect(),
            }));
        }
        if !request.is_empty() {
            match route.old.clone() {
                Some(old) => {
                    let (ctx, out, r) = (self.ctx.clone(), self.out.clone(), route.clone());
                    self.tasks
                        .spawn(offer_bases(ctx, out, r, old, bases, request, file_id));
                }
                None => self.out.send(Msg::Need(NeedChunks {
                    file_id,
                    chunk_idx: request
                        .into_iter()
                        .map(|i| sender_idx(idx_base, i))
                        .collect(),
                })),
            }
        }
        if !local.is_empty() {
            let (ctx, events) = (self.ctx.clone(), self.events.clone());
            self.tasks
                .spawn(copy_local_chunks(ctx, route, local, events, file_id));
        }
    }

    async fn on_finalized(&mut self, file_id: u64, gen: u32, result: Result<(), FinalizeError>) {
        // (`identical` files arrive here from `settle_identical`.)
        let Some(f) = self.files.get_mut(&file_id) else {
            return;
        };
        if f.gen != gen || f.st != St::Finalizing {
            return;
        }
        let abandoned = f.abandoned.take();
        match result {
            Ok(()) => {
                let (path, size) = (f.path.clone(), f.entry.size);
                // Settled in place is a metadata-only update, not a transfer.
                if !f.identical {
                    self.transferred_size += size;
                    self.ctx.hooks.bus.emit(super::Event::FileDone(path, size));
                    self.files_done += 1;
                }
                // Placed: acknowledged DONE even if the sender gave up meanwhile
                // (it withdraws its error on this ack).
                self.files.remove(&file_id);
                self.active -= 1;
                self.persist.done(file_id);
                self.ctx.progress.store(true, Ordering::Relaxed);
                self.ack(file_id, Status::Done, String::new());
            }
            Err(e) if abandoned.is_some() => {
                f.st = St::Requested;
                let msg = format!("sender gave up: {}; {e}", abandoned.unwrap_or_default());
                self.fail(file_id, None, msg).await;
            }
            Err(FinalizeError::HashMismatch { .. }) => {
                f.mismatches += 1;
                f.st = St::Requested;
                let (mismatches, path, pending) = (f.mismatches, f.path.clone(), f.pending.take());
                tracing::info!(?path, mismatches, "whole-file hash mismatch");
                self.ack(
                    file_id,
                    Status::HashMismatch,
                    "whole-file hash mismatch".into(),
                );
                if mismatches >= MAX_MISMATCHES {
                    let msg = "whole-file hash mismatch after retry".to_string();
                    self.fail(file_id, None, msg).await;
                } else if let Some(cl) = pending {
                    // The partial stays; the fresh chunk list adopts it.
                    self.on_chunk_list(file_id, cl).await;
                }
            }
            Err(e) => {
                f.st = St::Requested;
                self.fail(file_id, Some(Status::WriteError), e.to_string())
                    .await;
            }
        }
    }

    async fn on_source_error(&mut self, file_id: u64, msg: String) {
        let Some(f) = self.files.get_mut(&file_id) else {
            return;
        };
        match f.st {
            // Finalize decides: a verified file is still placed.
            St::Finalizing => f.abandoned = Some(msg),
            _ => self.fail(file_id, None, format!("sender: {msg}")).await,
        }
    }

    async fn handle(&mut self, ev: Ev) -> anyhow::Result<()> {
        match ev {
            Ev::Recv(d) => self.on_done(d).await,
            Ev::Ctrl(Msg::ChunkListPart(p)) => self.on_chunk_list_part(p).await,
            Ev::Ctrl(Msg::Ack(a)) if a.status == Status::SourceError as i32 => {
                self.on_source_error(a.file_id, a.message).await
            }
            Ev::Ctrl(Msg::ListingEnd(e)) => {
                if e.incomplete {
                    tracing::warn!("the sender's listing is incomplete");
                }
                self.peer_listing_incomplete = e.incomplete;
                self.peer_listing_done = true;
                self.maybe_delete_before().await;
                self.maybe_mark_complete().await;
            }
            Ev::Ctrl(Msg::Error(e)) => return Err(PeerError(e.message).into()),
            Ev::Ctrl(m) => anyhow::bail!(
                "protocol error: unexpected {} from the sender",
                msg_name(&m)
            ),
            Ev::PeerEof => {
                return Err(TransportError(
                    "the sender closed the control stream before the session ended".into(),
                )
                .into())
            }
            Ev::PeerFailed(e) | Ev::WriterFailed(e) => {
                return Err(TransportError(format!("control stream: {e}")).into())
            }
            Ev::PeerListing | Ev::Send(_) => {}
        }
        Ok(())
    }

    /// The session failed: drop every open writer (persisting its progress
    /// for the next connection), let in-flight file tasks end, and record
    /// the files they placed meanwhile as done.
    async fn shut_down(&mut self, events: &mut Events) {
        let ids: Vec<u64> = self
            .routes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .copied()
            .collect();
        for id in ids {
            self.retire_writer(id).await;
        }
        self.tasks.close();
        if tokio::time::timeout(TASK_DRAIN, self.tasks.wait())
            .await
            .is_err()
        {
            tracing::warn!("file tasks still running after the session failed");
        }
        while let Ok(ev) = events.try_recv() {
            if let Ev::Recv(Done::Finalized {
                file_id,
                gen,
                result: Ok(()),
            }) = ev
            {
                let current = self
                    .files
                    .get(&file_id)
                    .is_some_and(|f| f.gen == gen && f.st == St::Finalizing);
                if current {
                    self.persist.done(file_id);
                }
            }
        }
    }

    /// Applies directory metadata deepest first, after all their contents.
    async fn directory_pass(&mut self, report: &mut PlannerReport) {
        let dirs = std::mem::take(&mut report.dirs);
        let touched = std::mem::take(&mut report.touched);
        let ctx = self.ctx.clone();
        let errors = blocking(move || {
            let mut errors = Vec::new();
            for d in dirs.iter().rev() {
                if !(d.apply || touched.contains(&d.path)) {
                    continue;
                }
                let apply = ctx.apply_opts(&d.entry);
                if let Err(e) = apply_metadata(&ctx.full(&d.path), &d.entry, &apply) {
                    errors.push((d.path.clone(), format!("directory metadata: {e}")));
                }
            }
            Ok(errors)
        })
        .await
        .unwrap_or_else(|e| vec![(RelPath::root(), e.to_string())]);
        self.errors.extend(errors);
    }
}

/// `--partial-dir` inside the root, as a path to leave out of the listing.
fn partial_dir_rel(pd: Option<&Path>) -> Option<RelPath> {
    let pd = pd.filter(|p| p.is_relative())?;
    RelPath::new(pd.as_os_str().as_bytes()).ok()
}

/// Runs the receiver role to completion: `SessionDone` is sent once the
/// sender's listing has ended, every path is planned and every requested
/// file is acknowledged (DONE only for verified, placed files).
///
/// A resumed session whose stored plan is complete skips both listings and
/// requests the files not yet placed; one with an incomplete plan lists and
/// plans afresh, keeping the stored file ids and skipping placed files.
pub(super) async fn run(
    ctx: Arc<Ctx>,
    out: Out,
    events_tx: mpsc::UnboundedSender<Ev>,
    mut events: Events,
    peer_listing: mpsc::UnboundedReceiver<Side>,
) -> anyhow::Result<Outcome> {
    let hashed_before = ctx.files_hashed();
    let persist = Persister::start(&ctx).await;
    let stored = persist.stored_plan().clone();
    let routes: Routes = Arc::default();
    let bytes = Arc::new(AtomicU64::new(0));
    let acceptor = tokio::spawn(accept_streams(
        ctx.clone(),
        routes.clone(),
        events_tx.clone(),
        bytes.clone(),
    ));
    let mut r = Receiver {
        ctx: ctx.clone(),
        out: out.clone(),
        events: events_tx.clone(),
        routes,
        files: HashMap::new(),
        active: 0,
        report: None,
        peer_listing_done: false,
        peer_listing_incomplete: false,
        listing: None,
        walk_errors: Vec::new(),
        local_listing_incomplete: false,
        pending_before: None,
        deleted: Vec::new(),
        would_delete: Vec::new(),
        max_delete_reached: false,
        plan_complete: false,
        files_done: 0,
        transferred_size: 0,
        errors: Vec::new(),
        persist,
        tasks: TaskTracker::new(),
        links: Vec::new(),
        failed: HashSet::new(),
        queued: VecDeque::new(),
        inflight_cap: inflight_cap(&ctx),
    };
    let mut planner_abort = None;
    // A stored plan holds no deletion candidates, and a dry run stores
    // nothing worth resuming: both list afresh.
    let reuse_plan = ctx.opts.delete == DeleteMode::Off && !ctx.opts.dry_run;
    if ctx.resumed && stored.plan_complete && reuse_plan {
        r.resume_plan(stored).await;
    } else {
        let prior = Prior::new(&stored, |id| r.persist.is_done(id));
        let (local_tx, local_rx) = mpsc::unbounded_channel();
        let root = ctx.root.clone();
        let root_missing = blocking(move || Ok(std::fs::symlink_metadata(&root).is_err()))
            .await
            .unwrap_or(false);
        if ctx.opts.dry_run && root_missing {
            // A dry run does not create the root: there is nothing to list.
            drop(local_tx);
            out.send(Msg::ListingEnd(ListingEnd {
                incomplete: false,
                count: 0,
            }));
        } else {
            r.listing = Some(spawn_listing(
                ListingSpec {
                    root: ctx.root.clone(),
                    // Everything this side has: filters shape the sender's
                    // walk, and the planner applies them for deletions.
                    filters: Arc::new(FilterSet::new(vec![])),
                    hasher: ctx.hasher.clone(),
                    scope: ctx.scope.clone(),
                    skip: partial_dir_rel(ctx.opts.partial_dir.as_deref()),
                    hard_links: false,
                    bus: Some(ctx.hooks.bus.clone()),
                    cold: Some(ctx.cold.clone()),
                    #[cfg(feature = "test-hooks")]
                    inject: None,
                },
                out.clone(),
                Some(local_tx),
                None,
            ));
        }
        let planner = tokio::spawn(plan(
            ctx.clone(),
            out.clone(),
            events_tx.clone(),
            peer_listing,
            local_rx,
            prior,
        ));
        planner_abort = Some(planner.abort_handle());
        // A planner that dies without reporting must not leave us waiting.
        let events = events_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = planner.await {
                if e.is_panic() {
                    let _ = events.send(Ev::WriterFailed("planner panicked".into()));
                }
            }
        });
    }
    drop(events_tx);
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    let res: anyhow::Result<()> = loop {
        if r.complete() {
            break Ok(());
        }
        tokio::select! {
            ev = events.recv() => match ev {
                Some(ev) => {
                    if let Err(e) = r.handle(ev).await { break Err(e) }
                    r.pump();
                }
                None => break Err(anyhow::anyhow!("event channel closed")),
            },
            _ = tick.tick() => {
                r.persist.bytes(bytes.load(Ordering::Relaxed));
                r.persist.maybe_flush().await;
            }
        }
    };
    if let Err(e) = res {
        acceptor.abort();
        if let Some(p) = planner_abort {
            p.abort();
        }
        if let Some(l) = r.listing.take() {
            l.abort();
        }
        r.shut_down(&mut events).await;
        r.persist.bytes(bytes.load(Ordering::Relaxed));
        r.persist.flush().await;
        return Err(e);
    }
    if !ctx.opts.dry_run {
        r.link_pass().await;
    }
    let mut report = r.report.take().unwrap_or_default();
    // `--delete-after` (and `--delete-during`): after every file is placed,
    // before the directory pass so directories that lost entries get their
    // metadata (mtime) applied again.
    if let Some(pass) = report.deletes.take() {
        for p in r.run_deletes(pass).await {
            report.touched.insert(parent_of(&p));
        }
    }
    if !ctx.opts.dry_run {
        r.directory_pass(&mut report).await;
    }
    let bytes_total = bytes.load(Ordering::Relaxed);
    // Every problem this side recorded: its own walk, the planner (mkdir,
    // symlink, special, metadata, refused paths), files and directories.
    // The peer folds them into its outcome, so a push reports them too.
    let mut errors = std::mem::take(&mut r.walk_errors);
    errors.append(&mut report.errors);
    errors.append(&mut r.errors);
    out.send(Msg::Done(SessionDone {
        files: r.files_done,
        bytes: bytes_total,
        errors: capped_errors(&errors),
        deleted: capped_paths(&r.deleted),
        would_delete: capped_paths(&r.would_delete),
        max_delete_reached: r.max_delete_reached,
        created: report.created,
        transferred_size: r.transferred_size,
        deleted_omitted: r.deleted.len().saturating_sub(MAX_DONE_PATHS) as u64,
        would_delete_omitted: r.would_delete.len().saturating_sub(MAX_DONE_PATHS) as u64,
    }));
    // Finished: a later run with the same key starts a new session.
    r.persist.finish().await;
    if r.peer_listing_incomplete {
        errors.push((RelPath::root(), "the sender's listing is incomplete".into()));
    }
    out.finish();
    // Let the sender read SessionDone and close its side.
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, async {
        while let Some(ev) = events.recv().await {
            if matches!(ev, Ev::PeerEof | Ev::PeerFailed(_)) {
                break;
            }
        }
    })
    .await;
    acceptor.abort();
    Ok(Outcome {
        session_id: ctx.session_id,
        resumed: ctx.resumed,
        files_transferred: r.files_done,
        bytes_transferred: bytes_total,
        files_skipped: report.skipped,
        listed: report.listed,
        created: report.created,
        transferred_size: r.transferred_size,
        bytes_sent: ctx.conn.stats().udp_tx.bytes,
        bytes_received: ctx.conn.stats().udp_rx.bytes,
        files_hashed: ctx.files_hashed() - hashed_before,
        errors,
        deleted: r.deleted,
        would_delete: r.would_delete,
        max_delete_reached: r.max_delete_reached,
        #[cfg(feature = "test-hooks")]
        retained_files: r.files.len(),
    })
}
