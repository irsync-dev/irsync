//! The receiver role: walks its own tree, merge-joins it with the sender's
//! listing and decides per path (rsync-generator style). Directories,
//! symlinks, specials and metadata are applied locally; regular files are
//! requested with `PlanItem`, opened as verified `.part` writers when their
//! chunk list arrives, filled from local copies and data streams, and
//! verified; then the sender is asked whether the source is unchanged
//! (`MayPlace`), and only on its `PlaceVerdict` saying so is the file
//! renamed into place and acknowledged.
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
    ListRoom, ListingEnd, MayPlace, NeedChunks, PathError, PlaceVerdict, PlanEntry, PlanItem,
    SessionDone,
};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::task::TaskTracker;
use tracing::Instrument;

use super::control::{
    capped_errors, msg_name, spawn_listing, walk_errors, Ev, Listed, ListingSpec, Out, Persister,
};
use super::engine::{blocking, Ctx, Events, PeerError, PeerEvents, TransferPhase, TransportError};
use super::limits::{
    basis_budget, list_budget, need_piece, need_pieces, ByteBudget, LISTING_QUEUE,
};
use super::listing_stream::ListingOut;
use super::parts::{assemble, Assembly, ListBudget};
use super::planner::{
    inflight_cap, parent_of, plan, Deferred, Linked, Planned, PlannerReport, Prior,
};
use super::recv_io::{
    accept_streams, commit, copy_local_chunks, offer_bases, open_writer, sender_idx,
    settle_identical, verify, Fill, Opened, Route, Routes,
};
use super::Outcome;
use crate::cli::DeleteMode;
use crate::delete::DeletePass;
use crate::filter::FilterSet;
use crate::fs::meta::apply_metadata;
use crate::fs::paths::RelPath;
use crate::recv::{FileWriter, FinalizeError, Verified};
use crate::telemetry::meter::Counter;
use crate::telemetry::PhaseKind;
use recv_delete::capped_paths;

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
pub(super) const MAX_REREQUESTS: u32 = 3;
/// Chunk lists a sender starts per file and connection (first parts): the
/// first list, and per retry an empty part withdrawing the list so far and
/// the new list (see `SenderRole::retry`).
const MAX_LIST_STARTS: u32 = 1 + 2 * super::sender_role::MAX_RETRIES;
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
    /// `--delete-during`: the candidates the merge has left behind; delete
    /// now (`incomplete`: a listing has a gap so far) and reply with what
    /// was deleted.
    DeleteDuring {
        pass: Box<DeletePass>,
        incomplete: bool,
        reply: oneshot::Sender<Vec<RelPath>>,
    },
    Opened {
        file_id: u64,
        gen: u32,
        result: io::Result<Opened>,
    },
    /// Every chunk of the file is verified on disk.
    Ready {
        file_id: u64,
        gen: u32,
        writer: Box<FileWriter>,
    },
    /// The file's `.part` is verified (or failed to be).
    Verified {
        file_id: u64,
        gen: u32,
        /// Bytes the file's writer took from the sender.
        received: u64,
        result: Result<Box<Verified>, FinalizeError>,
    },
    /// The file is placed (or settled in place), or failed to be.
    Finalized {
        file_id: u64,
        gen: u32,
        /// Bytes the file's writer took from the sender (none when settled
        /// in place).
        received: u64,
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
    /// Verifying the `.part`, or (after the sender's verdict) placing it.
    Finalizing,
    /// Verified (or found identical); `MayPlace` sent, waiting for the
    /// sender's `PlaceVerdict`.
    Confirming,
}

/// What a file in [`St::Confirming`] holds until the sender's verdict.
enum Held {
    /// The verified `.part`, with the bytes its writer took from the sender.
    Writer(Box<Verified>, u64),
    /// The destination already holds the content: settled in place on
    /// the verdict.
    Identical(std::fs::File),
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
    /// Bytes the sender delivered to this file's earlier writers in this
    /// session (retired, or failing the whole-file hash): not matched,
    /// though the writer that places the file did not receive them.
    received_before: u64,
    /// `NeedChunks` re-sent for chunks the data streams did not deliver.
    rerequests: u32,
    st: St,
    /// When the file was requested; for the transfer histogram.
    started: Option<std::time::Instant>,
    /// A chunk list that arrived while the `.part` was busy (opening or finalizing).
    pending: Option<(Arc<FileDigest>, u32)>,
    /// Chunk lists the sender has started for this file on this connection
    /// (first parts, not counting the empty part that withdraws a list):
    /// the number of the latest. The sender counts the same.
    lists: u32,
    /// The number (see `lists`) of the list the current writer was opened
    /// for; `MayPlace` and `PlaceVerdict` carry it.
    list: u32,
    /// A chunk list arriving in parts, until its last part.
    assembly: Option<Assembly>,
    /// In [`St::Confirming`]: what to place on the sender's verdict.
    held: Option<Held>,
    /// The current chunk list as native records (set once opened).
    digest: Option<Arc<FileDigest>>,
    /// The planner's in-flight slot (none for a resumed plan), released
    /// when the file leaves `files`.
    _slot: Option<tokio::sync::OwnedSemaphorePermit>,
    /// The `file` span (debug level); `recv` and `place` hang off it.
    span: tracing::Span,
    /// The `recv` span, open while chunks are being received.
    recv: Option<tracing::Span>,
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
    /// A deletion pass was refused (reported once, however many passes follow).
    delete_refused: bool,
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
    transfer: TransferPhase,
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
    /// Bytes of chunk bases sent and not yet requested, across files (see
    /// `offer_bases`). Only the basis tasks wait on it, never the main loop.
    basis_budget: Arc<ByteBudget>,
    /// Chunk records of the files' lists held, against the room granted to
    /// the sender (see `parts::ListBudget`). Fresh for every connection, as
    /// the sender's room is.
    lists: ListBudget,
    /// Every file id requested on this connection, with the chunk lists the
    /// sender has started for it (first parts): a chunk list part for any
    /// other id is a protocol error, one for a file that has left `files`
    /// is late and ignored, and more than [`MAX_LIST_STARTS`] lists for a
    /// file are a protocol error (each would make us grant room again).
    planned: HashMap<u64, u32>,
    /// Records of list room freed and not yet granted back: sent as one
    /// `ListRoom` after each main-loop event (see `grant_room`).
    freed: u64,
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
        self.planned.entry(file_id).or_insert(0);
        #[cfg(feature = "test-hooks")]
        if let Some(peak) = &self.ctx.hooks.inflight_peak {
            peak.fetch_max(self.active, Ordering::SeqCst);
        }
        let span = tracing::debug_span!(
            parent: self.transfer.span(&self.ctx),
            "file",
            path = ?path,
            size = entry.size,
            kind = if action == Action::Whole { "new" } else { "delta" },
        );
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
                received_before: 0,
                rerequests: 0,
                st: St::Requested,
                started: super::wait_start(&self.ctx.meter),
                pending: None,
                lists: 0,
                list: 0,
                assembly: None,
                held: None,
                digest: None,
                _slot: slot,
                span,
                recv: None,
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
        // Held by others only for local disk work (see `Slot`).
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
        self.send_need(
            file_id,
            missing
                .into_iter()
                .map(|i| sender_idx(route.idx_base, i))
                .collect(),
        );
    }

    /// Requests chunks `idxs` (sender indices) of `file_id`, in pieces
    /// that fit a frame (see `limits::need_pieces`).
    fn send_need(&self, file_id: u64, idxs: Vec<u32>) {
        for n in need_pieces(file_id, idxs, need_piece(&self.ctx.hooks)) {
            self.out.send(Msg::Need(n));
        }
    }

    fn ack(&self, file_id: u64, status: Status, message: String) {
        self.out.send(Msg::Ack(FileAck {
            file_id,
            status: status as i32,
            message,
            matched: 0,
        }));
    }

    /// Takes the file's writer out of the routing table (it is dropped, which
    /// persists its progress for a later writer to adopt), keeping count of
    /// what the sender delivered to it.
    async fn retire_writer(&mut self, file_id: u64) {
        let route = self
            .routes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&file_id);
        if let Some(r) = route {
            // Held by others only for local disk work (see `Slot`).
            let received = r.slot.lock().await.take().map_or(0, |w| w.received());
            if let Some(f) = self.files.get_mut(&file_id) {
                f.received_before += received;
            }
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
        self.release_list(file_id);
        tracing::info!(path = ?f.path, error = %msg, "file failed");
        self.ctx
            .hooks
            .bus
            .emit(super::Event::Failed(f.path.clone(), msg.clone()));
        self.failed.insert(f.path.clone());
        self.errors.push((f.path.clone(), msg.clone()));
        if let Some(status) = ack {
            self.ack(file_id, status, msg);
        }
        // The planner's slot goes only after the ack (see `on_finalized`).
        drop(f);
    }

    /// Grants the sender room for `records` chunk records of lists, with
    /// the rest freed by this main-loop event (see `grant_room`).
    fn grant_lists(&mut self, records: u64) {
        self.freed += records;
    }

    /// Sends the room freed by the event just handled, in one `ListRoom`.
    /// Sent with `Out::send`, which never waits: the main loop must not
    /// wait on the peer (see the deadlock invariant at `run_role`).
    fn grant_room(&mut self) {
        let records = std::mem::take(&mut self.freed);
        if records > 0 {
            self.out.send(Msg::ListRoom(ListRoom { records }));
        }
    }

    /// The file has left `files`: its list's room goes back to the sender.
    fn release_list(&mut self, file_id: u64) {
        let n = self.lists.release(file_id);
        self.grant_lists(n);
    }

    /// One part of a chunk list; the complete list goes to `on_chunk_list`.
    /// A malformed sequence fails the file, like a list that does not tile
    /// it. A list beyond the room granted fails the session, and is checked
    /// first: before the part is looked at, and before any of it is held.
    async fn on_chunk_list_part(&mut self, p: ChunkListPart) -> anyhow::Result<()> {
        let file_id = p.file_id;
        let Some(starts) = self.planned.get_mut(&file_id) else {
            anyhow::bail!(
                "protocol error: chunk list part for file id {file_id}, which was never planned"
            );
        };
        let known = self.files.contains_key(&file_id);
        if p.first == 0 {
            *starts += 1;
            if *starts > MAX_LIST_STARTS {
                anyhow::bail!(
                    "protocol error: more than {MAX_LIST_STARTS} chunk lists for file id {file_id}"
                );
            }
            // The first part takes the list's whole announced total; the
            // later parts are covered by it (assembly refuses more). A new
            // list for the file replaces the old one, whose room is freed.
            let freed = self
                .lists
                .reserve(file_id, u64::from(p.total))
                .map_err(|e| {
                    anyhow::anyhow!("protocol error: chunk list beyond the granted room: {e}")
                })?;
            #[cfg(feature = "test-hooks")]
            if let Some(peak) = &self.ctx.hooks.list_peak {
                peak.fetch_max(self.lists.total(), Ordering::SeqCst);
            }
            // Late (the file failed or was placed): nothing is kept.
            let late = if known {
                0
            } else {
                self.lists.release(file_id)
            };
            self.grant_lists(freed + late);
        }
        let Some(f) = self.files.get_mut(&file_id) else {
            tracing::debug!(file_id, "chunk list part for a file not in flight");
            return Ok(());
        };
        self.ctx.hooks.event("part", &f.path);
        if p.first == 0 && (p.total > 0 || p.last) {
            f.lists += 1;
        }
        let n = f.lists;
        match assemble(&mut f.assembly, p) {
            Ok(Some(d)) => self.on_chunk_list(file_id, Arc::new(d), n).await,
            Ok(None) => {}
            Err(e) => {
                let msg = format!("chunk list in parts: {e}");
                self.fail(file_id, Some(Status::WriteError), msg).await;
            }
        }
        Ok(())
    }

    /// The complete chunk list number `n` (see `RFile::lists`).
    async fn on_chunk_list(&mut self, file_id: u64, digest: Arc<FileDigest>, n: u32) {
        let Some(f) = self.files.get_mut(&file_id) else {
            tracing::debug!(file_id, "chunk list for a file not in flight");
            return;
        };
        f.assembly = None;
        match f.st {
            St::Opening | St::Finalizing | St::Confirming => {
                f.pending = Some((digest, n));
                return;
            }
            St::Requested | St::Receiving => {}
        }
        f.gen += 1;
        f.list = n;
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

    /// Verifies the file's `.part`; `on_verified` asks the sender next.
    fn spawn_verify(&self, file_id: u64, gen: u32, w: FileWriter) {
        let f = &self.files[&file_id];
        let (ctx, events, path) = (self.ctx.clone(), self.events.clone(), f.path.clone());
        let place = tracing::debug_span!(parent: &f.span, "place");
        let received = w.received();
        self.tasks.spawn(
            async move {
                let result = verify(ctx, w, path).await.map(Box::new);
                let _ = events.send(Ev::Recv(Done::Verified {
                    file_id,
                    gen,
                    received,
                    result,
                }));
            }
            .instrument(place),
        );
    }

    /// Places what the file holds, now that the sender said its source is
    /// unchanged: renames the verified `.part`, or settles an identical
    /// destination in place.
    fn spawn_commit(&self, file_id: u64, gen: u32, held: Held, digest: Arc<FileDigest>) {
        let f = &self.files[&file_id];
        let (ctx, events, path, entry) = (
            self.ctx.clone(),
            self.events.clone(),
            f.path.clone(),
            f.entry.clone(),
        );
        let place = tracing::debug_span!(parent: &f.span, "place");
        self.tasks.spawn(
            async move {
                let (received, result) = match held {
                    Held::Writer(v, received) => {
                        (received, commit(ctx, *v, path, entry, digest).await)
                    }
                    Held::Identical(file) => {
                        (0, settle_identical(ctx, file, path, entry, digest).await)
                    }
                };
                let _ = events.send(Ev::Recv(Done::Finalized {
                    file_id,
                    gen,
                    received,
                    result,
                }));
            }
            .instrument(place),
        );
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
                    p.path.clone(),
                    p.entry,
                    p.action,
                    p.deferred,
                    Some(p.slot),
                );
                // Requested only now that it is registered: an answer can
                // never find the file unknown. Completions and peer frames
                // travel on separate channels, and a completion sent first
                // is not always received first (an mpsc slot still being
                // written hides the ones behind it). `Out::send` never waits:
                // a main loop must not wait on the peer, so this frame is not
                // paced. With a peer that reads, the planner's in-flight slots
                // bound how many queue; one that frees slots without reading
                // (acks for files it never read the plan of) is stopped by
                // the writer's ceiling (`limits::out_ceiling`).
                self.ctx.hooks.event("plan", &p.path);
                self.out.send(Msg::Plan(PlanItem {
                    file_id: p.file_id,
                    path: p.path.as_bytes().to_vec(),
                    action: p.action as i32,
                    append_from: p.append_from,
                }));
            }
            Done::PlannerFinished(r) => {
                self.report = Some(*r);
                self.maybe_mark_complete().await;
            }
            Done::DeleteBefore(pass, reply) => {
                self.pending_before = Some((*pass, reply));
                self.maybe_delete_before().await;
            }
            Done::DeleteDuring {
                pass,
                incomplete,
                reply,
            } => {
                let deleted = self.run_deletes(*pass, incomplete).await;
                let _ = reply.send(deleted);
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
                f.recv = None;
                self.routes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&file_id);
                self.spawn_verify(file_id, gen, *writer);
            }
            Done::Verified {
                file_id,
                gen,
                received,
                result,
            } => self.on_verified(file_id, gen, received, result).await,
            Done::Finalized {
                file_id,
                gen,
                received,
                result,
            } => self.on_finalized(file_id, gen, received, result).await,
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
        if let Some((cl, n)) = f.pending.take() {
            // Superseded while opening: drop this writer, open the newer list.
            drop(result);
            f.st = St::Requested;
            self.on_chunk_list(file_id, cl, n).await;
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
                f.digest = Some(digest);
                f.identical = true;
                self.ctx
                    .hooks
                    .bus
                    .emit(super::Event::Skipped(f.path.clone(), f.entry.size));
                self.ctx
                    .busy
                    .skipped_bytes
                    .fetch_add(f.entry.size, Ordering::Relaxed);
                self.ctx.hooks.event("identical", &f.path);
                if let Some(code) = f.deferred.itemize.take().and_then(|i| i.meta) {
                    emit_itemized(&self.ctx, &self.out, &f.path, code);
                }
                // The sender has nothing to deliver; settled in place once
                // it says the source is unchanged.
                self.out.send(Msg::Need(NeedChunks {
                    file_id,
                    chunk_idx: Vec::new(),
                }));
                self.confirm(file_id, Held::Identical(file));
                return;
            }
            Opened::Writer(fill) => *fill,
        };
        if fill.writer.is_open() {
            self.ctx.hooks.event("open", &f.path);
        }
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
        // chunks) needs no transfer: shown as matched now, counted once the
        // file is placed (a local copy may still fail and be requested).
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
            self.spawn_verify(file_id, gen, writer);
            return;
        }
        f.st = St::Receiving;
        f.recv = Some(tracing::debug_span!(parent: &f.span, "recv"));
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
        let alone = remote.is_empty();
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
            self.send_need(
                file_id,
                raw.into_iter().map(|i| sender_idx(idx_base, i)).collect(),
            );
        }
        if !request.is_empty() {
            match route.old.clone() {
                Some(old) => {
                    let (ctx, out, r) = (self.ctx.clone(), self.out.clone(), route.clone());
                    let budget = self.basis_budget.clone();
                    self.tasks.spawn(offer_bases(
                        ctx, out, r, old, bases, request, file_id, budget,
                    ));
                }
                None => self.send_need(
                    file_id,
                    request
                        .into_iter()
                        .map(|i| sender_idx(idx_base, i))
                        .collect(),
                ),
            }
        }
        if !local.is_empty() {
            let (ctx, events) = (self.ctx.clone(), self.events.clone());
            self.tasks
                .spawn(copy_local_chunks(ctx, route, local, alone, events, file_id));
        }
    }

    /// Holds what is to be placed and asks the sender whether its source
    /// is unchanged; [`Receiver::on_verdict`] goes on.
    fn confirm(&mut self, file_id: u64, held: Held) {
        let Some(f) = self.files.get_mut(&file_id) else {
            return;
        };
        f.st = St::Confirming;
        f.held = Some(held);
        self.ctx.hooks.event("mayplace", &f.path);
        self.out.send(Msg::MayPlace(MayPlace {
            file_id,
            gen: f.list,
        }));
    }

    /// Drops what a file held for placing (its source changed, or a newer
    /// chunk list replaces it) and goes on with the newer list, if one came.
    async fn drop_held(&mut self, file_id: u64, held: Option<Held>) {
        if let Some(Held::Writer(v, received)) = held {
            v.discard().await;
            if let Some(f) = self.files.get_mut(&file_id) {
                f.received_before += received;
            }
        }
        let Some(f) = self.files.get_mut(&file_id) else {
            return;
        };
        f.st = St::Requested;
        if let Some((cl, n)) = f.pending.take() {
            self.on_chunk_list(file_id, cl, n).await;
        }
    }

    async fn on_verified(
        &mut self,
        file_id: u64,
        gen: u32,
        received: u64,
        result: Result<Box<Verified>, FinalizeError>,
    ) {
        let Some(f) = self.files.get_mut(&file_id) else {
            return;
        };
        if f.gen != gen || f.st != St::Finalizing {
            return;
        }
        match result {
            // The sender has started a newer list meanwhile (it retried
            // the file): this one is stale, not worth asking about.
            Ok(v) if f.pending.is_some() || f.assembly.is_some() => {
                self.drop_held(file_id, Some(Held::Writer(v, received)))
                    .await;
            }
            Ok(v) => self.confirm(file_id, Held::Writer(v, received)),
            Err(FinalizeError::HashMismatch { .. }) => {
                f.mismatches += 1;
                f.received_before += received;
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
                } else if let Some((cl, n)) = pending {
                    // The partial stays; the fresh chunk list adopts it.
                    self.on_chunk_list(file_id, cl, n).await;
                }
            }
            Err(e) => {
                f.st = St::Requested;
                self.fail(file_id, Some(Status::WriteError), e.to_string())
                    .await;
            }
        }
    }

    /// The sender's answer to `MayPlace`: place the file, or (its source
    /// changed) drop it and wait for the new chunk list the sender sends,
    /// or for its give-up. Not a whole-file mismatch: it is not counted
    /// as one.
    async fn on_verdict(&mut self, v: PlaceVerdict) {
        let file_id = v.file_id;
        let Some(f) = self.files.get_mut(&file_id) else {
            tracing::debug!(file_id, "place verdict for a file not in flight");
            return;
        };
        if f.st != St::Confirming || v.gen != f.list {
            tracing::debug!(file_id, "place verdict for a file not waiting for one");
            return;
        }
        let held = f.held.take();
        // A newer list started before the verdict (the sender sent it
        // first): what we hold is stale, whatever the verdict says.
        if v.changed || f.pending.is_some() || f.assembly.is_some() {
            tracing::info!(path = ?f.path, "the source changed; not placing it");
            self.drop_held(file_id, held).await;
            return;
        }
        let (Some(held), Some(digest)) = (held, f.digest.clone()) else {
            let msg = "internal: nothing held to place".to_string();
            self.fail(file_id, Some(Status::WriteError), msg).await;
            return;
        };
        f.st = St::Finalizing;
        let gen = f.gen;
        self.spawn_commit(file_id, gen, held, digest);
    }

    async fn on_finalized(
        &mut self,
        file_id: u64,
        gen: u32,
        received: u64,
        result: Result<(), FinalizeError>,
    ) {
        // (`identical` files arrive here from `settle_identical`.)
        let Some(f) = self.files.get_mut(&file_id) else {
            return;
        };
        if f.gen != gen || f.st != St::Finalizing {
            return;
        }
        match result {
            Ok(()) => {
                let (path, size) = (f.path.clone(), f.entry.size);
                // Settled in place is a metadata-only update, not a transfer.
                if !f.identical {
                    super::record_file(&self.ctx.meter, f.started, size);
                    self.transferred_size += size;
                    self.ctx.hooks.bus.emit(super::Event::FileDone(path, size));
                    self.files_done += 1;
                }
                // What this session did not take from the sender: local
                // copies, earlier sessions' progress, or all of a file
                // settled in place. Bytes an earlier writer of this session
                // received were taken from the sender all the same.
                let matched = size.saturating_sub(f.received_before + received);
                self.ctx.meter.add(Counter::BytesMatched, matched);
                // Placed (the sender said its source was unchanged, so it
                // has not given up on it). The ack is queued before the
                // file (and with it the planner's slot) is dropped: the
                // planner's next `Plan` then follows the ack, and the
                // sender never sees more files in flight than the window.
                let placed = self.files.remove(&file_id);
                self.active -= 1;
                self.release_list(file_id);
                self.persist.done(file_id);
                self.ctx.progress.store(true, Ordering::Relaxed);
                self.out.send(Msg::Ack(FileAck {
                    file_id,
                    status: Status::Done as i32,
                    message: String::new(),
                    matched,
                }));
                drop(placed);
            }
            Err(e) => {
                f.st = St::Requested;
                self.fail(file_id, Some(Status::WriteError), e.to_string())
                    .await;
            }
        }
    }

    /// The sender gave up on the file. Nothing of it is placed: a file is
    /// placed only on the sender's word that its source is unchanged.
    async fn on_source_error(&mut self, file_id: u64, msg: String) {
        self.fail(file_id, None, format!("sender: {msg}")).await;
    }

    async fn handle(&mut self, ev: Ev) -> anyhow::Result<()> {
        match ev {
            Ev::Recv(d) => self.on_done(d).await,
            Ev::Ctrl(Msg::ChunkListPart(p)) => self.on_chunk_list_part(p).await?,
            Ev::Ctrl(Msg::PlaceVerdict(v)) => self.on_verdict(v).await,
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
            Ev::PeerFailed(e) | Ev::WriterFailed(e) => return Err(TransportError(e).into()),
            Ev::ProtocolError(e) => anyhow::bail!("protocol error: {e}"),
            Ev::PeerListing | Ev::Send(_) => {}
        }
        Ok(())
    }

    /// The session failed: drop every open writer (persisting its progress
    /// for the next connection), let in-flight file tasks end, and record
    /// the files they placed meanwhile as done. A file verified and
    /// waiting for its verdict is not done: it is dropped like any writer,
    /// and the next connection asks again.
    async fn shut_down(&mut self, events: &mut Events) {
        for f in self.files.values_mut() {
            drop(f.held.take());
        }
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
                ..
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
    listing_out: ListingOut,
    events_tx: mpsc::UnboundedSender<Ev>,
    mut events: Events,
    mut peer: PeerEvents,
    peer_listing: mpsc::Receiver<Listed>,
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
        delete_refused: false,
        deleted: Vec::new(),
        would_delete: Vec::new(),
        max_delete_reached: false,
        plan_complete: false,
        files_done: 0,
        transferred_size: 0,
        errors: Vec::new(),
        persist,
        transfer: TransferPhase::default(),
        tasks: TaskTracker::new(),
        links: Vec::new(),
        failed: HashSet::new(),
        queued: VecDeque::new(),
        inflight_cap: inflight_cap(&ctx),
        basis_budget: Arc::new(ByteBudget::new(basis_budget(&ctx.hooks))),
        lists: ListBudget::new(list_budget(&ctx.hooks)),
        planned: HashMap::new(),
        freed: 0,
    };
    let mut planner_abort = None;
    // A stored plan holds no deletion candidates, and a dry run stores
    // nothing worth resuming: both list afresh.
    let reuse_plan = ctx.opts.delete == DeleteMode::Off && !ctx.opts.dry_run;
    if ctx.resumed && stored.plan_complete && reuse_plan {
        // No listing: this side's half of the listing stream ends empty.
        listing_out.finish();
        // Nor one from the sender: it ends its half empty once our first
        // `PlanItem` arrives. Let go of the planner's queue first, so the
        // listing reader takes that empty half as expected rather than as
        // a listing that never came (see `listing_stream::read_listing`).
        drop(peer_listing);
        r.resume_plan(stored).await;
    } else {
        let prior = Prior::new(&stored, |id| r.persist.is_done(id));
        let (local_tx, local_rx) = mpsc::channel(LISTING_QUEUE);
        let root = ctx.root.clone();
        let root_missing = blocking(move || Ok(std::fs::symlink_metadata(&root).is_err()))
            .await
            .unwrap_or(false);
        if ctx.opts.dry_run && root_missing {
            // A dry run does not create the root: there is nothing to list.
            drop(local_tx);
            listing_out.send_first(Msg::ListingEnd(ListingEnd {
                incomplete: false,
                count: 0,
            }));
            listing_out.finish();
        } else {
            r.listing = Some(spawn_listing(
                ListingSpec {
                    phase: ctx.phase(PhaseKind::List),
                    root: ctx.root.clone(),
                    // Everything this side has: filters shape the sender's
                    // walk, and the planner applies them for deletions.
                    filters: Arc::new(FilterSet::new(vec![])),
                    hasher: ctx.hasher.clone(),
                    scope: ctx.scope.clone(),
                    skip: partial_dir_rel(ctx.opts.partial_dir.as_deref()),
                    hard_links: false,
                    recursive: ctx.opts.preserve.recursive,
                    dirs: ctx.opts.preserve.dirs,
                    bus: Some(ctx.hooks.bus.clone()),
                    cold: Some(ctx.cold.clone()),
                    busy: Some(ctx.busy.clone()),
                    activity: Some(ctx.activity.clone()),
                    meter: Some(ctx.meter.clone()),
                    #[cfg(feature = "test-hooks")]
                    inject: ctx.hooks.inject_listing.clone(),
                    #[cfg(feature = "test-hooks")]
                    drop_after: None,
                    #[cfg(feature = "test-hooks")]
                    cut_after: None,
                },
                out.clone(),
                listing_out,
                Some(local_tx),
                None,
            ));
        }
        let planner = tokio::spawn(
            plan(
                ctx.clone(),
                out.clone(),
                events_tx.clone(),
                peer_listing,
                local_rx,
                prior,
            )
            .instrument(tracing::Span::current()),
        );
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
        // Own completions before peer events (see `run_role`).
        let ev = tokio::select! {
            biased;
            _ = tick.tick() => {
                r.persist.bytes(bytes.load(Ordering::Relaxed));
                r.persist.maybe_flush().await;
                continue;
            }
            Some(ev) = events.recv() => ev,
            Some(ev) = peer.recv() => {
                #[cfg(feature = "test-hooks")]
                if let Some(d) = ctx.hooks.slow_peer_events {
                    tokio::time::sleep(d).await;
                }
                ev
            }
            else => break Err(anyhow::anyhow!("event channel closed")),
        };
        if let Err(e) = r.handle(ev).await {
            break Err(e);
        }
        r.grant_room();
        r.pump();
    };
    r.transfer.end();
    if let Err(e) = res {
        acceptor.abort();
        if let Some(p) = planner_abort {
            p.abort();
        }
        if let Some(l) = r.listing.take() {
            l.abort();
        }
        // Nothing the peer sends matters now: a reader waiting on a full
        // queue reads on (and drops) instead of stalling its stream.
        drop(peer);
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
        r.await_own_walk().await;
        let incomplete = r.peer_listing_incomplete || r.local_listing_incomplete;
        for p in r.run_deletes(pass, incomplete).await {
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
    // The final skipped and deletion counts, even for a session shorter
    // than one heartbeat interval.
    out.send(Msg::Heartbeat(ctx.busy.heartbeat()));
    let (deleted, deleted_omitted) = capped_paths(&r.deleted);
    let (would_delete, would_delete_omitted) = capped_paths(&r.would_delete);
    out.send(Msg::Done(SessionDone {
        files: r.files_done,
        bytes: bytes_total,
        errors: capped_errors(&errors),
        deleted,
        would_delete,
        max_delete_reached: r.max_delete_reached,
        created: report.created,
        transferred_size: r.transferred_size,
        deleted_omitted,
        would_delete_omitted,
    }));
    // Finished: a later run with the same key starts a new session.
    r.persist.finish().await;
    if r.peer_listing_incomplete {
        errors.push((RelPath::root(), "the sender's listing is incomplete".into()));
    }
    out.finish();
    // Let the sender read SessionDone and close its side.
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, async {
        while let Some(ev) = peer.recv().await {
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
        deleted_omitted: 0,
        would_delete: r.would_delete,
        would_delete_omitted: 0,
        max_delete_reached: r.max_delete_reached,
        usage: Default::default(),
        reconnects: 0,
        conn: None,
        #[cfg(feature = "test-hooks")]
        retained_files: r.files.len(),
    })
}
