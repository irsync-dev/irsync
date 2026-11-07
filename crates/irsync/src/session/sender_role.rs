//! The sender role: walks and streams its listing, then answers the
//! receiver: `PlanItem` → digest (lookahead) → chunk list in parts; `NeedChunks` →
//! data streams. A file counts as done only at `FileAck DONE`. A file that
//! changed while being sent, or whose whole-file hash did not match at the
//! receiver, is re-stat'ed, re-digested and offered once more.
//!
//! Per-file state lives only while a file is in flight: it is dropped at its
//! final ack or when the sender gives up on it.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::bail;
use irsync_proto::chunk::FileDigest;
use irsync_proto::v1::{
    control::Msg, file_ack::Status, file_entry::Kind, ChunkBasis, ChunkListPart, FileAck,
    NeedChunks, PlanEntry, PlanItem, SessionDone,
};
use tokio::sync::{mpsc, OwnedSemaphorePermit};
use tokio::task::JoinHandle;
use tracing::Instrument;

use super::control::msg_name;
use super::control::{
    is_own_temp, spawn_listing, walk_errors, Ev, ListedFiles, ListingSpec, Out, Persister,
};
use super::engine::{blocking, Ctx, Events, PeerError, PeerEvents, TransferPhase, TransportError};
use super::limits::{basis_budget, basis_wire_len, list_budget};
use super::listing_stream::ListingOut;
use super::parts::{records_at_most, split, ListCredit, PART_RECORDS};
use super::planner::MAX_INFLIGHT;
use super::Outcome;
use crate::delta::Basis;
use crate::filter::{FilterSet, Verdict};
use crate::fs::meta::lstat_entry_with_ctime;
use crate::fs::paths::RelPath;
use crate::hasher::Freshness;
use crate::scan::{ScanItem, ScanSummary};
use crate::send::{SendProgress, Sender, DEFAULT_SPLIT_THRESHOLD};
use crate::telemetry::meter::Counter;
use crate::telemetry::PhaseKind;
use irsync_proto::v1::plan_item::Action;

/// Re-offers per file after a change or a hash mismatch.
pub(super) const MAX_RETRIES: u32 = 1;
/// How long to wait for in-flight data streams after `SessionDone`.
const DRAIN_SENDS: Duration = Duration::from_secs(10);

/// Completions of the sender's tasks, delivered to its main loop.
pub(super) enum Done {
    Digested {
        file_id: u64,
        gen: u32,
        /// With the permits the digest took (see `ListCredit`).
        result: io::Result<(FileDigest, OwnedSemaphorePermit)>,
    },
    Sent {
        file_id: u64,
        gen: u32,
        /// The task's id in [`SendTasks`].
        task: u64,
        /// Bytes sent on success.
        result: io::Result<u64>,
    },
}

/// The send tasks running. A file has at most one (see `SFile::pending`),
/// and it is aborted when the file leaves `files` (`remove_file`): a
/// receiver that acknowledges a file early cannot leave its task behind,
/// so there are never more than files in flight.
#[derive(Default)]
struct SendTasks {
    next: u64,
    /// By task id: how to abort it, and the bytes it has put on the wire.
    live: HashMap<u64, (tokio::task::AbortHandle, Arc<std::sync::atomic::AtomicU64>)>,
}

impl SendTasks {
    /// Spawns the task `make` builds from its id and its byte counter.
    fn start<F>(&mut self, make: impl FnOnce(u64, Arc<std::sync::atomic::AtomicU64>) -> F) -> u64
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let id = self.next;
        self.next += 1;
        let sent = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let task = tokio::spawn(make(id, sent.clone()));
        self.live.insert(id, (task.abort_handle(), sent));
        id
    }

    /// The task reported back. False if it no longer counts: it was
    /// aborted (with its file) after it reported, and its bytes were
    /// counted then.
    fn finished(&mut self, id: u64) -> bool {
        self.live.remove(&id).is_some()
    }

    /// Aborts the task; returns the bytes it had put on the wire.
    fn abort(&mut self, id: u64) -> u64 {
        self.live.remove(&id).map_or(0, |(task, sent)| {
            task.abort();
            sent.load(std::sync::atomic::Ordering::SeqCst)
        })
    }

    fn len(&self) -> usize {
        self.live.len()
    }
}

/// A file's chunks requested while its send task runs (see `SFile::pending`).
#[derive(Default)]
struct Pending {
    idxs: BTreeSet<u32>,
    /// Bases for some of `idxs`. They left the basis budget when the
    /// request that brought them arrived (the receiver counts them free
    /// once it has sent it), so pending bases sit outside the budget,
    /// bounded by the file's chunk count.
    bases: HashMap<u32, Basis>,
}

struct SFile {
    path: RelPath,
    /// Bumped on every re-offer; older task results are ignored.
    gen: u32,
    retries: u32,
    /// `APPEND`: only the tail from here is digested and sent.
    append_from: Option<u64>,
    /// The digest behind the chunk list last sent (none while re-digesting).
    digest: Option<Arc<FileDigest>>,
    /// Bases the receiver offered for chunks it is about to request, by
    /// chunk index, with their size against the basis budget; each is used
    /// for one request and dropped.
    bases: HashMap<u32, (Basis, usize)>,
    /// The summed size of `bases`.
    bases_held: usize,
    /// Chunk indices the receiver asked for so far under this digest: the
    /// rest of the file is matched (`FileMatched`). A request may come in
    /// several parts (chunks without a basis first, then those with one).
    requested: HashSet<u32>,
    /// Send tasks of this generation started and not yet finished;
    /// `FileSent` when the last one finishes with nothing pending.
    streams: u32,
    /// The file's send task, while it runs (of this generation or an older
    /// one): there is at most one, see `pending`.
    task: Option<u64>,
    /// The file's digest task, while it runs: aborted with the file (see
    /// `remove_file`), or when a retry starts another.
    digesting: Option<tokio::task::AbortHandle>,
    /// Chunks requested while the send task runs, sent once it is done.
    pending: Option<Pending>,
    /// When the receiver first asked for data; for the transfer histogram.
    started: Option<std::time::Instant>,
    #[cfg(feature = "test-hooks")]
    hooked: bool,
    /// The `file` span (debug level); its `hash` and `send` spans hang off it.
    span: tracing::Span,
}

/// Whether this side walks and streams its listing.
enum Listing {
    Running(JoinHandle<ScanSummary>),
    /// Resumed: the receiver's first frame decides. A listing from it means
    /// it plans afresh (we list too); a `PlanItem` means it resumes its
    /// stored plan. The two travel on different streams, but in a fresh
    /// listing `PeerListing` still comes first: the receiver's first
    /// `PlanItem` needs our entries, and we start walking only at
    /// `PeerListing`. Holds the listing stream until then.
    Undecided(ListingOut),
    /// Resumed from the receiver's stored plan: no walk; every requested
    /// path is checked the way the walk would have admitted it.
    FromPlan,
}

struct SenderRole {
    ctx: Arc<Ctx>,
    out: Out,
    events: mpsc::UnboundedSender<Ev>,
    sender: Arc<Sender>,
    listed: ListedFiles,
    listing: Listing,
    /// Summed size of the files acknowledged placed.
    transferred_size: u64,
    /// Items the receiver reported creating.
    created: u64,
    /// Files in flight (no final ack yet, not given up). Leave through
    /// [`SenderRole::remove_file`].
    files: HashMap<u64, SFile>,
    /// Bytes of chunk bases held for all files (see `limits::BASIS_BUDGET`).
    basis_held: usize,
    /// More than this fails the session.
    basis_budget: usize,
    /// Chunk lists waiting for room the receiver grants, and that room.
    /// Fresh for every connection, as the receiver's budget is.
    lists: ListCredit,
    /// The room a connection starts with: a longer list can never be sent.
    list_budget: u64,
    /// Every file id ever planned (duplicate detection).
    seen: HashSet<u64>,
    /// Files given up with `SOURCE_ERROR`, by id, with their path. If
    /// the receiver still places one (it had every chunk verified when our
    /// give-up arrived), its DONE withdraws the error.
    abandoned: HashMap<u64, RelPath>,
    files_done: u64,
    bytes: u64,
    /// Send tasks not yet reported back, one per file in flight at most.
    tasks: SendTasks,
    errors: Vec<(RelPath, String)>,
    persist: Persister,
    transfer: TransferPhase,
    /// What the receiver deleted (or would have, on a dry run).
    deleted: Vec<RelPath>,
    would_delete: Vec<RelPath>,
    max_delete_reached: bool,
}

/// Records per `ChunkListPart` (a test hook may shrink it).
fn part_records(ctx: &Ctx) -> usize {
    #[cfg(feature = "test-hooks")]
    if let Some(n) = ctx.hooks.chunk_list_part_records {
        return n;
    }
    let _ = ctx;
    PART_RECORDS
}

impl SenderRole {
    /// Leaves `Listing::Undecided`: walk and list (`fresh`), or resume from
    /// the receiver's stored plan with no walk.
    fn decide(&mut self, fresh: bool) {
        let Listing::Undecided(listing) = std::mem::replace(&mut self.listing, Listing::FromPlan)
        else {
            return;
        };
        if fresh {
            self.start_listing(listing);
        } else {
            tracing::info!("the receiver resumes its stored plan");
            // No listing: our half of the listing stream ends empty.
            listing.finish();
        }
    }

    fn start_listing(&mut self, listing: ListingOut) {
        tracing::debug!("listing the source");
        self.listing = Listing::Running(spawn_listing(
            ListingSpec {
                phase: self.ctx.phase(PhaseKind::List),
                root: self.ctx.root.clone(),
                filters: self.ctx.filters.clone(),
                hasher: self.ctx.hasher.clone(),
                scope: self.ctx.scope.clone(),
                skip: None,
                hard_links: self.ctx.opts.preserve.hard_links,
                recursive: self.ctx.opts.preserve.recursive,
                dirs: self.ctx.opts.preserve.dirs,
                bus: Some(self.ctx.hooks.bus.clone()),
                cold: None,
                busy: Some(self.ctx.busy.clone()),
                activity: Some(self.ctx.activity.clone()),
                meter: Some(self.ctx.meter.clone()),
                #[cfg(feature = "test-hooks")]
                inject: self.ctx.hooks.inject_listing.clone(),
                #[cfg(feature = "test-hooks")]
                drop_after: self
                    .ctx
                    .hooks
                    .drop_after_listing
                    .clone()
                    .map(|f| (f, self.ctx.conn.clone())),
                #[cfg(feature = "test-hooks")]
                cut_after: self.ctx.hooks.cut_listing_after,
            },
            self.out.clone(),
            listing,
            None,
            Some(self.listed.clone()),
        ));
    }

    /// `check`: admit the path only if the walk would have listed it as a
    /// regular file (a resumed plan has no listing of this connection).
    /// `walked`: this connection's walk stat of the file, which spares the
    /// re-stat; a retry passes none, so it describes the file as it is now.
    fn spawn_digest(
        &mut self,
        file_id: u64,
        gen: u32,
        path: RelPath,
        force: bool,
        check: bool,
        walked: Option<Freshness>,
    ) {
        let (ctx, sender, events) = (self.ctx.clone(), self.sender.clone(), self.events.clone());
        let append_from = self.files.get(&file_id).and_then(|f| f.append_from);
        let (ahead, most) = self.lists.ahead();
        let hash = self.files.get(&file_id).map_or_else(
            tracing::Span::none,
            |f| tracing::debug_span!(parent: &f.span, "hash"),
        );
        let task = tokio::spawn(
            async move {
                let result = async {
                    if check {
                        let (root, p, filters) =
                            (ctx.root.clone(), path.clone(), ctx.filters.clone());
                        if !blocking(move || Ok(walk_admits(&root, &p, &filters))).await? {
                            return Err(io::Error::new(
                                io::ErrorKind::PermissionDenied,
                                "not a listed regular file",
                            ));
                        }
                    }
                    // Hashing works on index keys under the index root.
                    let key = ctx.key(&path);
                    // The walk saw a regular file (the opens below refuse
                    // anything else now); without it, re-stat: the digest
                    // must describe the file as it is now.
                    let (size, item) = match walked {
                        Some(walked) => (walked.size, None),
                        None => {
                            let (base, rel) = (ctx.base.clone(), key.clone());
                            let (entry, ctime) =
                                blocking(move || lstat_entry_with_ctime(&base, &rel)).await?;
                            if entry.kind() != Kind::Regular {
                                return Err(io::Error::new(
                                    io::ErrorKind::InvalidInput,
                                    "no longer a regular file",
                                ));
                            }
                            let item = ScanItem {
                                path: key.clone(),
                                entry,
                                ctime,
                            };
                            (item.entry.size, Some(item))
                        }
                    };
                    // Only as far ahead of the room as it holds.
                    let size = size.saturating_sub(append_from.unwrap_or(0));
                    let held = ahead
                        .acquire_many_owned(records_at_most(size, most))
                        .await
                        .map_err(|_| io::Error::other("the session ended"))?;
                    let digest = if let Some(from) = append_from {
                        // Only the tail: the receiver trusts its prefix.
                        let full = ctx.full_key(&key);
                        blocking(move || tail_digest(&full, from)).await
                    } else {
                        let since = super::wait_start(&ctx.meter);
                        let digest = match (walked, item) {
                            (Some(walked), _) => sender.lookahead_digest_walked(key, walked).await,
                            (None, Some(item)) if force => ctx.forced.digest(&item).await,
                            (None, Some(item)) => sender.lookahead_digest(&item).await,
                            (None, None) => unreachable!("no walk without a re-stat"),
                        };
                        super::record_hash_wait(&ctx.meter, since);
                        digest
                    };
                    Ok((digest?, held))
                }
                .await;
                let _ = events.send(Ev::Send(Done::Digested {
                    file_id,
                    gen,
                    result,
                }));
            }
            .instrument(hash),
        );
        match self.files.get_mut(&file_id) {
            Some(f) => {
                if let Some(old) = f.digesting.replace(task.abort_handle()) {
                    old.abort();
                }
            }
            None => task.abort(),
        }
    }

    /// Takes a file out of `files`, with what it held of the basis budget.
    fn remove_file(&mut self, file_id: u64) -> Option<SFile> {
        let mut f = self.files.remove(&file_id)?;
        drop_bases(&mut self.basis_held, &mut f);
        // Done with or given up: its tasks go too. What it sent counts.
        if let Some(task) = f.task.take() {
            self.bytes += self.tasks.abort(task);
        }
        if let Some(task) = f.digesting.take() {
            task.abort();
        }
        self.lists.cancel(file_id);
        Some(f)
    }

    /// Gives up on the file and tells the receiver to stop waiting for it.
    fn source_error(&mut self, file_id: u64, msg: String) {
        let Some(f) = self.remove_file(file_id) else {
            return;
        };
        tracing::info!(path = ?f.path, error = %msg, "cannot send file");
        self.ctx
            .hooks
            .bus
            .emit(super::Event::Failed(f.path.clone(), msg.clone()));
        self.errors.push((f.path.clone(), msg.clone()));
        self.abandoned.insert(file_id, f.path);
        self.out.send(Msg::Ack(FileAck {
            file_id,
            status: Status::SourceError as i32,
            message: msg,
            matched: 0,
        }));
    }

    /// Offers the file again with a fresh stat and a forced re-read, or gives
    /// up once the retry is spent.
    fn retry(&mut self, file_id: u64, why: String) {
        let Some(f) = self.files.get_mut(&file_id) else {
            return;
        };
        if f.retries >= MAX_RETRIES {
            self.source_error(file_id, format!("{why} (after retry)"));
            return;
        }
        tracing::info!(path = ?f.path, reason = %why, "re-sending file");
        f.retries += 1;
        f.gen += 1;
        let listed = f.digest.take().is_some();
        drop_bases(&mut self.basis_held, f);
        f.requested.clear();
        f.streams = 0;
        // Requested under the old digest. (A task still running for it
        // stays the file's one task until it ends.)
        f.pending = None;
        let (gen, path) = (f.gen, f.path.clone());
        if listed && !self.lists.cancel(file_id) {
            // The old list went out, and the receiver holds it until a new
            // one replaces it. The new one may have to wait for room, which
            // the old one may be taking: restart the list now with an empty,
            // unfinished first part, so the receiver drops the old one and
            // frees its room. No room needed (it announces no records), so
            // it goes out at once.
            self.out.send(Msg::ChunkListPart(ChunkListPart {
                file_id,
                total: 0,
                first: 0,
                last: false,
                ..ChunkListPart::default()
            }));
        }
        self.spawn_digest(file_id, gen, path, true, false, None);
    }

    fn on_plan(&mut self, p: PlanItem) -> anyhow::Result<()> {
        let file_id = p.file_id;
        if !self.seen.insert(file_id) {
            bail!("protocol error: duplicate file id {file_id}");
        }
        // The receiver's planner holds a slot per file until it has queued
        // the file's final ack (see `on_finalized`), so an honest receiver
        // never has more in flight than the window.
        if self.files.len() >= MAX_INFLIGHT {
            bail!("protocol error: more than {MAX_INFLIGHT} files in flight");
        }
        let path = RelPath::new(&p.path).unwrap_or_default();
        // Only paths this side listed as regular files are ever read: a peer
        // cannot ask for something outside the walk (e.g. through a symlink).
        // With a listing of this connection, a receiver plans each listed
        // regular file once (a retry keeps its id and sends no new plan;
        // `-H` names after the first are linked on its side, never planned):
        // anything else is a protocol error, refused before the file costs
        // anything. Taking the path out of `listed` catches a second plan.
        // Resumed from the receiver's stored plan, nothing was listed: each
        // path is checked as the walk would have admitted it, and one gone
        // since is an honest source error.
        // The root is never sent as a file, even when it is one and was listed.
        let walked = match self.listing {
            _ if path.as_bytes().is_empty() => None,
            Listing::FromPlan => None,
            Listing::Running(_) | Listing::Undecided(_) => {
                let walked = self
                    .listed
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&path);
                match walked {
                    Some(w) => Some(w),
                    None => bail!(
                        "protocol error: PlanItem for file id {file_id} names a path this side \
                         did not list as a regular file, or one already planned"
                    ),
                }
            }
        };
        let append_from = (p.action == Action::Append as i32).then_some(p.append_from);
        let kind = if p.action == Action::Whole as i32 {
            "new"
        } else {
            "delta"
        };
        let span = tracing::debug_span!(
            parent: self.transfer.span(&self.ctx),
            "file",
            path = ?path,
            size = tracing::field::Empty,
            kind,
        );
        self.files.insert(
            file_id,
            SFile {
                path: path.clone(),
                gen: 0,
                retries: 0,
                append_from,
                digest: None,
                bases: HashMap::new(),
                bases_held: 0,
                requested: HashSet::new(),
                streams: 0,
                task: None,
                digesting: None,
                pending: None,
                started: None,
                #[cfg(feature = "test-hooks")]
                hooked: false,
                span,
            },
        );
        self.persist.plan(PlanEntry {
            file_id,
            path: p.path,
            action: p.action,
            ..PlanEntry::default()
        });
        match walked {
            Some(walked) => self.spawn_digest(file_id, 0, path, false, false, Some(walked)),
            None if path.as_bytes().is_empty() => {
                self.source_error(file_id, "not a listed regular file".into());
            }
            None => self.spawn_digest(file_id, 0, path, false, true, None),
        }
        Ok(())
    }

    /// The receiver's itemize code for a path, shown from the sender's side
    /// (`>` received becomes `<` sent).
    fn on_itemized(&mut self, i: irsync_proto::v1::Itemized) {
        let Ok(path) = RelPath::new(&i.path) else {
            return;
        };
        // `-n -v` lists the files that would be transferred.
        if self.ctx.opts.dry_run && i.code.starts_with('>') && i.code[1..].starts_with('f') {
            self.ctx
                .hooks
                .bus
                .emit(super::Event::WouldTransfer(path.clone()));
        }
        let code = match i.code.strip_prefix('>') {
            Some(rest) => format!("<{rest}"),
            None => i.code,
        };
        self.ctx.hooks.bus.emit(super::Event::Itemized(path, code));
    }

    fn on_digested(
        &mut self,
        file_id: u64,
        gen: u32,
        result: io::Result<(FileDigest, OwnedSemaphorePermit)>,
    ) {
        let Some(f) = self.files.get_mut(&file_id) else {
            return;
        };
        if f.gen != gen {
            return;
        }
        f.digesting = None;
        match result {
            Ok((d, _)) if d.chunks.len() as u64 > self.list_budget => {
                let msg = format!(
                    "chunk list of {} chunks, beyond the session's room of {}",
                    d.chunks.len(),
                    self.list_budget
                );
                self.source_error(file_id, msg);
            }
            Ok((d, held)) => {
                let with_hash = f.append_from.is_none();
                // Sent once the receiver has room for it (`pump_lists`).
                let parts = split(file_id, &d, part_records(&self.ctx), with_hash);
                self.lists.push(file_id, parts, Some(held));
                #[cfg(feature = "test-hooks")]
                if let Some(p) = &self.ctx.hooks.lists_ahead_peak {
                    p.fetch_max(self.lists.queued(), std::sync::atomic::Ordering::SeqCst);
                }
                f.span.record("size", d.size);
                f.digest = Some(Arc::new(d));
            }
            Err(e) => self.source_error(file_id, e.to_string()),
        }
    }

    /// Sends the chunk lists that fit in the room the receiver has granted.
    /// Runs on the main loop, so it never waits (see the deadlock invariant
    /// at `run_role`): the parts are queued with `Out::send`, not paced.
    /// They are bounded already, by the room granted.
    fn pump_lists(&mut self) {
        for p in self.lists.ready() {
            self.out.send(Msg::ChunkListPart(p));
            #[cfg(feature = "test-hooks")]
            if let Some(h) = &self.ctx.hooks.drop_after_parts {
                if h.queued() {
                    tracing::warn!("fault injection: dropping the connection between parts");
                    self.ctx.conn.close(99u32.into(), b"fault");
                }
            }
        }
    }

    /// A basis for one chunk of a file in flight: kept for the request
    /// that follows. A malformed or misplaced basis is dropped (the chunk
    /// then goes raw); it is never a protocol error. Holding more than the
    /// basis budget is: an honest receiver requests its bases in batches
    /// under the same budget (see `offer_bases`).
    fn on_basis(&mut self, b: ChunkBasis) -> anyhow::Result<()> {
        let Some(f) = self.files.get_mut(&b.file_id) else {
            return Ok(());
        };
        let Some(d) = &f.digest else {
            return Ok(());
        };
        let idx = b.chunk_idx;
        if (idx as usize) >= d.chunks.len()
            || (f.bases.len() >= d.chunks.len() && !f.bases.contains_key(&idx))
        {
            return Ok(());
        }
        let n = basis_wire_len(&b);
        match Basis::from_wire(b) {
            Ok(basis) => {
                let replaced = f.bases.insert(idx, (basis, n)).map_or(0, |(_, m)| m);
                f.bases_held = f.bases_held + n - replaced;
                self.basis_held = self.basis_held + n - replaced;
            }
            Err(e) => {
                tracing::debug!(path = ?f.path, idx, error = %e, "ignoring a chunk basis");
                return Ok(());
            }
        }
        #[cfg(feature = "test-hooks")]
        if let Some(p) = &self.ctx.hooks.basis_peak {
            p.fetch_max(self.basis_held, std::sync::atomic::Ordering::SeqCst);
        }
        // As the receiver's budget, a single basis larger than the budget
        // passes alone.
        if self.basis_held > self.basis_budget && self.basis_held > n {
            bail!("protocol error: chunk bases above the budget");
        }
        Ok(())
    }

    fn on_need(&mut self, n: NeedChunks) {
        let file_id = n.file_id;
        let Some(f) = self.files.get_mut(&file_id) else {
            tracing::debug!(file_id, "NeedChunks for a file no longer in flight");
            return;
        };
        // Any request, empty or not, takes the file's bases: those of its
        // chunks are used, the rest dropped. A basis serves only the
        // request that follows it; one left over is stale (its writer was
        // retired before requesting, and the receiver sent an empty
        // request so that we drop it before it reuses the bytes). They
        // leave the basis budget here, even if the request waits below:
        // the receiver counts them free once it has sent the request.
        let mut held = std::mem::take(&mut f.bases);
        self.basis_held -= std::mem::take(&mut f.bases_held);
        // The receiver may ask again for chunks its streams did not deliver.
        // Without a digest a fresh chunk list is on its way and will be
        // answered with a new request.
        let Some(digest) = f.digest.clone() else {
            return;
        };
        let idxs: Vec<u32> = n
            .chunk_idx
            .into_iter()
            .filter(|&i| (i as usize) < digest.chunks.len())
            .collect();
        // Everything not requested so far is matched. An empty request says
        // nothing (it may be "nothing for now", with chunks that have a
        // basis still to come), so it reports nothing: a file the receiver
        // needs nothing of is accounted for by its `FileDone`.
        if !idxs.is_empty() {
            f.requested.extend(idxs.iter().copied());
            let needed: u64 = f
                .requested
                .iter()
                .map(|&i| digest.chunks[i as usize].len as u64)
                .sum();
            // Shown as matched now; counted from the receiver's DONE, which
            // knows what it really reused.
            let matched = digest.size.saturating_sub(needed);
            if f.started.is_none() {
                f.started = super::wait_start(&self.ctx.meter);
            }
            self.ctx
                .hooks
                .bus
                .emit(super::Event::FileMatched(f.path.clone(), matched));
        }
        if idxs.is_empty() {
            return; // The receiver has everything; wait for its ack.
        }
        let bases: HashMap<u32, Basis> = idxs
            .iter()
            .filter_map(|i| held.remove_entry(i))
            .map(|(i, (b, _))| (i, b))
            .collect();
        drop(held);
        if f.task.is_some() {
            // At most one send task per file: a request that arrives while
            // one runs (a batch, a re-request, a repeat) joins the file's
            // pending request, sent when the running task is done. A chunk
            // is in it once (with the basis of its latest request), so it
            // never holds more than the file's chunks.
            let p = f.pending.get_or_insert_with(Pending::default);
            for i in &idxs {
                p.bases.remove(i);
            }
            p.idxs.extend(idxs);
            p.bases.extend(bases);
            return;
        }
        self.spawn_send(file_id, idxs, bases);
    }

    /// Starts the file's send task for `idxs` (the file is in flight, with
    /// a digest, and has no send task running).
    #[cfg_attr(not(feature = "test-hooks"), allow(unused_mut))]
    fn spawn_send(&mut self, file_id: u64, idxs: Vec<u32>, mut bases: HashMap<u32, Basis>) {
        let Some(f) = self.files.get_mut(&file_id) else {
            return;
        };
        let Some(digest) = f.digest.clone() else {
            return;
        };
        #[cfg(feature = "test-hooks")]
        let hook = if f.hooked {
            None
        } else {
            f.hooked = true;
            self.ctx.hooks.on_first_chunk_sent.clone()
        };
        let (gen, key, sender, events) = (
            f.gen,
            self.ctx.key(&f.path),
            self.sender.clone(),
            self.events.clone(),
        );
        #[cfg(feature = "test-hooks")]
        let path = f.path.clone();
        f.streams += 1;
        let send_span = tracing::debug_span!(parent: &f.span, "send");
        let task = self.tasks.start(|task, sent| {
            async move {
                #[cfg(feature = "test-hooks")]
                let result = match hook {
                    Some(h) if idxs.len() > 1 => {
                        let first: HashMap<u32, Basis> =
                            bases.remove_entry(&idxs[0]).into_iter().collect();
                        match sender
                            .send_chunks_counted(file_id, &key, &digest, &idxs[..1], first, &sent)
                            .await
                        {
                            Ok(n) => {
                                h(&path).await;
                                sender
                                    .send_chunks_counted(
                                        file_id,
                                        &key,
                                        &digest,
                                        &idxs[1..],
                                        bases,
                                        &sent,
                                    )
                                    .await
                                    .map(|m| n + m)
                            }
                            Err(e) => Err(e),
                        }
                    }
                    _ => {
                        sender
                            .send_chunks_counted(file_id, &key, &digest, &idxs, bases, &sent)
                            .await
                    }
                };
                #[cfg(not(feature = "test-hooks"))]
                let result = sender
                    .send_chunks_counted(file_id, &key, &digest, &idxs, bases, &sent)
                    .await;
                let _ = events.send(Ev::Send(Done::Sent {
                    file_id,
                    gen,
                    task,
                    result,
                }));
            }
            .instrument(send_span)
        });
        f.task = Some(task);
        #[cfg(feature = "test-hooks")]
        if let Some(p) = &self.ctx.hooks.send_tasks_peak {
            p.fetch_max(self.tasks.len(), std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn on_sent(&mut self, file_id: u64, gen: u32, task: u64, result: io::Result<u64>) {
        if !self.tasks.finished(task) {
            return; // Aborted with its file, its bytes counted then.
        }
        if let Some(f) = self.files.get_mut(&file_id) {
            // The file's only send task (of this generation or an older one).
            if f.task == Some(task) {
                f.task = None;
            }
            // The last send of the current generation: only the ack is left.
            // (A superseded send must not mark the re-offered file sent.)
            if f.gen == gen {
                f.streams = f.streams.saturating_sub(1);
                if f.streams == 0 && f.pending.is_none() && result.is_ok() {
                    self.ctx
                        .hooks
                        .bus
                        .emit(super::Event::FileSent(f.path.clone()));
                }
            }
        }
        match result {
            // Counted even if the file was acknowledged meanwhile.
            Ok(b) => {
                self.bytes += b;
                self.ctx
                    .progress
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            Err(e) if self.files.get(&file_id).is_some_and(|f| f.gen == gen) => {
                if e.kind() == io::ErrorKind::InvalidData {
                    // The file changed under us; never send stale data.
                    self.retry(file_id, e.to_string());
                } else {
                    self.source_error(file_id, e.to_string());
                }
            }
            Err(_) => {}
        }
        // What was requested while the task ran (a retry dropped it).
        if let Some(p) = self.files.get_mut(&file_id).and_then(|f| f.pending.take()) {
            self.spawn_send(file_id, p.idxs.into_iter().collect(), p.bases);
        }
    }

    fn on_ack(&mut self, a: FileAck) {
        let file_id = a.file_id;
        let status = Status::try_from(a.status);
        if !self.files.contains_key(&file_id) {
            // Withdraw our give-up if the receiver placed the file anyway.
            if status == Ok(Status::Done) {
                if let Some(path) = self.abandoned.remove(&file_id) {
                    self.ctx.meter.add(Counter::BytesMatched, a.matched);
                    tracing::info!(?path, "placed by the receiver after all");
                    if let Some(i) = self.errors.iter().rposition(|(p, _)| *p == path) {
                        self.errors.remove(i);
                    }
                    self.files_done += 1;
                    self.persist.done(file_id);
                }
            }
            return;
        }
        match status {
            Ok(Status::Done) => {
                self.ctx
                    .progress
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                self.ctx.meter.add(Counter::BytesMatched, a.matched);
                if let Some(f) = self.remove_file(file_id) {
                    let size = f.digest.as_ref().map_or(0, |d| d.size);
                    super::record_file(&self.ctx.meter, f.started, size);
                    self.transferred_size += size;
                    self.ctx
                        .hooks
                        .bus
                        .emit(super::Event::FileDone(f.path, size));
                }
                self.files_done += 1;
                self.persist.done(file_id);
            }
            Ok(Status::HashMismatch) => self.retry(file_id, "whole-file hash mismatch".into()),
            Ok(Status::WriteError) => {
                if let Some(f) = self.remove_file(file_id) {
                    let msg = format!("receiver: {}", a.message);
                    tracing::info!(path = ?f.path, error = %msg, "file failed");
                    self.ctx
                        .hooks
                        .bus
                        .emit(super::Event::Failed(f.path.clone(), msg.clone()));
                    self.errors.push((f.path, msg));
                }
            }
            Ok(Status::SourceError) | Err(_) => {
                tracing::warn!(file_id, status = a.status, "unexpected FileAck status");
            }
        }
    }

    /// Folds the receiver's own per-path errors in (those not already known
    /// from a file ack).
    fn on_session_done(&mut self, d: SessionDone) {
        tracing::debug!(files = d.files, bytes = d.bytes, "receiver done");
        for e in d.errors {
            let path = RelPath::new(&e.path).unwrap_or_default();
            if !self.errors.iter().any(|(p, _)| *p == path) {
                self.errors.push((path, format!("receiver: {}", e.message)));
            }
        }
        let paths = |v: &[Vec<u8>]| -> Vec<RelPath> {
            v.iter().filter_map(|p| RelPath::new(p).ok()).collect()
        };
        self.deleted = paths(&d.deleted);
        self.would_delete = paths(&d.would_delete);
        for p in &self.deleted {
            self.ctx.hooks.bus.emit(super::Event::Itemized(
                p.clone(),
                crate::plan::DELETING.into(),
            ));
            self.ctx.hooks.bus.emit(super::Event::Deleted(p.clone()));
        }
        for p in &self.would_delete {
            self.ctx
                .hooks
                .bus
                .emit(super::Event::WouldDelete(p.clone()));
        }
        self.created = d.created;
        // The receiver knows what it placed: a file whose content was
        // already there is acknowledged DONE but settled in place, a
        // metadata-only update rather than a transfer.
        self.files_done = d.files;
        self.transferred_size = d.transferred_size;
        self.max_delete_reached = d.max_delete_reached;
        if d.deleted_omitted + d.would_delete_omitted > 0 {
            tracing::info!(
                deleted = d.deleted_omitted,
                would_delete = d.would_delete_omitted,
                "the receiver reported more deleted paths than listed"
            );
        }
    }
}

/// Drops the file's bases, and their bytes from the session's count.
fn drop_bases(basis_held: &mut usize, f: &mut SFile) {
    f.bases.clear();
    *basis_held -= std::mem::take(&mut f.bases_held);
}

/// The chunk list of `full` from `from` on (an `APPEND` tail): chunked
/// from that offset, records at absolute offsets, no whole-file hash.
fn tail_digest(full: &std::path::Path, from: u64) -> io::Result<FileDigest> {
    use std::io::{Read, Seek, SeekFrom};
    // The open's type check already took the file's stat.
    let (mut file, st) = crate::fs::safe::open_regular_nofollow_stat(full)?;
    let size = st.st_size as u64;
    if size < from {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "shorter than the receiver's copy; not an append",
        ));
    }
    file.seek(SeekFrom::Start(from))?;
    let mut tail = irsync_proto::chunk::digest_reader((&mut file).take(size - from))?;
    for c in &mut tail.chunks {
        c.offset += from;
    }
    Ok(FileDigest {
        size,
        hash: [0; 32],
        chunks: tail.chunks,
    })
}

/// Whether the walk (from `root`, with `filters`) would list `path` as a
/// regular file: no component is a symlink or excluded, the file is regular
/// and is not one of irsync's own temporary files.
fn walk_admits(root: &Path, path: &RelPath, filters: &FilterSet) -> bool {
    if path.as_bytes().is_empty() || is_own_temp(path) {
        return false;
    }
    let comps: Vec<&[u8]> = path.components().collect();
    let (mut rel, mut full) = (Vec::new(), root.to_path_buf());
    for (i, c) in comps.iter().enumerate() {
        if i > 0 {
            rel.push(b'/');
        }
        rel.extend_from_slice(c);
        full.push(std::ffi::OsStr::from_bytes(c));
        let last = i + 1 == comps.len();
        let Ok(r) = RelPath::new(&rel) else {
            return false;
        };
        let Ok(md) = std::fs::symlink_metadata(&full) else {
            return false;
        };
        let kind_ok = if last { md.is_file() } else { md.is_dir() };
        if !kind_ok || filters.matches(&r, !last) == Verdict::Exclude {
            return false;
        }
    }
    true
}

/// Runs the sender role until the receiver's `SessionDone`.
pub(super) async fn run(
    ctx: Arc<Ctx>,
    out: Out,
    listing_out: ListingOut,
    events_tx: mpsc::UnboundedSender<Ev>,
    mut events: Events,
    mut peer: PeerEvents,
) -> anyhow::Result<Outcome> {
    let hashed_before = ctx.files_hashed();
    let root = ctx.root.clone();
    let root_ok = blocking(move || Ok(std::fs::metadata(&root).is_ok_and(|m| m.is_dir())))
        .await
        .unwrap_or(false);
    if !root_ok {
        bail!("source {} is not a directory", ctx.root.display());
    }
    let persist = Persister::start(&ctx).await;
    let sender = Arc::new(Sender::new(
        ctx.conn.clone(),
        ctx.session_id,
        ctx.base.clone(),
        ctx.hasher.clone(),
        ctx.opts.parallel,
        DEFAULT_SPLIT_THRESHOLD,
        ctx.opts.bwlimit,
    ));
    sender.set_meter(ctx.meter.clone());
    {
        let (bus, activity) = (ctx.hooks.bus.clone(), ctx.activity.clone());
        #[cfg(feature = "test-hooks")]
        let (fault, conn, wire) = (
            ctx.fault.clone(),
            ctx.conn.clone(),
            std::sync::atomic::AtomicU64::new(0),
        );
        sender.set_progress(Arc::new(move |path, p| {
            activity.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            match p {
                SendProgress::Opened { size } => {
                    bus.emit(super::Event::FileStart(path.clone(), size))
                }
                SendProgress::Sent(n) => {
                    bus.emit(super::Event::FileBytes(path.clone(), n));
                    bus.emit(super::Event::Bytes(n));
                    // The fault counts bytes as they go on the wire, chunk by
                    // chunk, like the receiver's; a per-send count would fire
                    // only after every parallel stream had already left.
                    #[cfg(feature = "test-hooks")]
                    if let Some(f) = &fault {
                        let total = wire.fetch_add(n, std::sync::atomic::Ordering::Relaxed) + n;
                        f.observe(total, &conn);
                    }
                }
            }
        }));
    }
    #[cfg(feature = "test-hooks")]
    if let Some(t) = ctx.hooks.truncate_streams.clone() {
        sender.set_truncate(t);
    }
    let mut s = SenderRole {
        ctx: ctx.clone(),
        out: out.clone(),
        events: events_tx,
        sender,
        listed: Arc::default(),
        listing: Listing::Undecided(listing_out),
        files: HashMap::new(),
        basis_held: 0,
        basis_budget: basis_budget(&ctx.hooks),
        lists: ListCredit::new(list_budget(&ctx.hooks)),
        list_budget: list_budget(&ctx.hooks),
        seen: HashSet::new(),
        abandoned: HashMap::new(),
        files_done: 0,
        bytes: 0,
        transferred_size: 0,
        created: 0,
        tasks: SendTasks::default(),
        errors: Vec::new(),
        persist,
        transfer: TransferPhase::default(),
        deleted: Vec::new(),
        would_delete: Vec::new(),
        max_delete_reached: false,
    };
    if !ctx.resumed {
        s.decide(true);
    }
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    let res: anyhow::Result<()> = loop {
        // Own completions before peer events (see `run_role`).
        let ev = tokio::select! {
            biased;
            _ = tick.tick() => {
                s.persist.bytes(s.bytes);
                s.persist.maybe_flush().await;
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
        if let Listing::Undecided(_) = s.listing {
            match &ev {
                Ev::PeerListing | Ev::Ctrl(Msg::ListingEnd(_)) => s.decide(true),
                Ev::Ctrl(Msg::Plan(_)) => s.decide(false),
                _ => {}
            }
        }
        let step = match ev {
            Ev::Send(Done::Digested {
                file_id,
                gen,
                result,
            }) => {
                s.on_digested(file_id, gen, result);
                Ok(())
            }
            Ev::Send(Done::Sent {
                file_id,
                gen,
                task,
                result,
            }) => {
                s.on_sent(file_id, gen, task, result);
                Ok(())
            }
            Ev::Ctrl(Msg::Plan(p)) => s.on_plan(p),
            Ev::Ctrl(Msg::Itemized(i)) => {
                s.on_itemized(i);
                Ok(())
            }
            Ev::Ctrl(Msg::ChunkBasis(b)) => s.on_basis(b),
            Ev::Ctrl(Msg::Need(n)) => {
                s.on_need(n);
                Ok(())
            }
            Ev::Ctrl(Msg::Ack(a)) => {
                s.on_ack(a);
                Ok(())
            }
            Ev::Ctrl(Msg::ListRoom(r)) => s
                .lists
                .grant(r.records)
                .map_err(|e| anyhow::anyhow!("protocol error: list room beyond the budget: {e}")),
            Ev::Ctrl(Msg::ListingEnd(e)) => {
                if e.incomplete {
                    let msg = "the receiver's listing is incomplete".to_string();
                    tracing::warn!("{msg}");
                    s.errors.push((RelPath::root(), msg));
                }
                Ok(())
            }
            Ev::Ctrl(Msg::Done(d)) => {
                s.on_session_done(d);
                break Ok(());
            }
            Ev::Ctrl(Msg::Error(e)) => Err(PeerError(e.message).into()),
            Ev::Ctrl(m) => Err(anyhow::anyhow!(
                "protocol error: unexpected {} from the receiver",
                msg_name(&m)
            )),
            Ev::PeerEof => Err(TransportError(
                "the receiver closed the control stream before the session ended".into(),
            )
            .into()),
            Ev::PeerFailed(e) | Ev::WriterFailed(e) => Err(TransportError(e).into()),
            Ev::ProtocolError(e) => Err(anyhow::anyhow!("protocol error: {e}")),
            Ev::PeerListing | Ev::Recv(_) => Ok(()),
        };
        if let Err(e) = step {
            break Err(e);
        }
        // Lists digested or room granted (or a file at the front given up).
        s.pump_lists();
    };
    s.transfer.end();
    if res.is_ok() {
        // The receiver can finish before our last streams report back (it
        // has the data; we wait for the transport's acknowledgement): collect
        // those results so the byte count is complete.
        let _ = tokio::time::timeout(DRAIN_SENDS, async {
            while s.tasks.len() > 0 {
                match events.recv().await {
                    Some(Ev::Send(Done::Sent {
                        file_id,
                        gen,
                        task,
                        result,
                    })) => s.on_sent(file_id, gen, task, result),
                    Some(_) => {}
                    None => break,
                }
            }
        })
        .await;
    }
    s.persist.bytes(s.bytes);
    let listing = std::mem::replace(&mut s.listing, Listing::FromPlan);
    if let Err(e) = res {
        s.persist.flush().await;
        if let Listing::Running(l) = listing {
            l.abort();
        }
        return Err(e);
    }
    // Finished: a later run with the same key starts a new session.
    s.persist.finish().await;
    out.finish();
    // The receiver cannot finish before our ListingEnd, so the walk is over.
    let mut listed = crate::scan::ListStats::default();
    let mut errors = match listing {
        Listing::Running(l) => match l.await {
            Ok(summary) => {
                listed = summary.listed;
                walk_errors(&summary)
            }
            Err(e) => vec![(RelPath::root(), format!("listing failed: {e}"))],
        },
        Listing::Undecided(_) | Listing::FromPlan => Vec::new(),
    };
    errors.append(&mut s.errors);
    Ok(Outcome {
        session_id: ctx.session_id,
        resumed: ctx.resumed,
        files_transferred: s.files_done,
        bytes_transferred: s.bytes,
        files_skipped: 0,
        listed,
        created: s.created,
        transferred_size: s.transferred_size,
        bytes_sent: ctx.conn.stats().udp_tx.bytes,
        bytes_received: ctx.conn.stats().udp_rx.bytes,
        files_hashed: ctx.files_hashed() - hashed_before,
        errors,
        deleted: s.deleted,
        would_delete: s.would_delete,
        max_delete_reached: s.max_delete_reached,
        usage: Default::default(),
        reconnects: 0,
        conn: None,
        #[cfg(feature = "test-hooks")]
        retained_files: s.files.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};

    /// Sets its flag when dropped: the task holding it has ended.
    struct Ended(Arc<AtomicBool>);

    impl Drop for Ended {
        fn drop(&mut self) {
            self.0.store(true, SeqCst);
        }
    }

    /// A task that has sent `bytes` and then waits forever, like a send
    /// waiting for the peer.
    fn stuck(
        tasks: &mut SendTasks,
        bytes: u64,
    ) -> (u64, Arc<AtomicBool>, Arc<tokio::sync::Notify>) {
        let (ended, started) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(tokio::sync::Notify::new()),
        );
        let (e, s) = (ended.clone(), started.clone());
        let id = tasks.start(move |_, sent: Arc<AtomicU64>| async move {
            let _ended = Ended(e);
            sent.fetch_add(bytes, SeqCst);
            s.notify_one();
            std::future::pending::<()>().await;
        });
        (id, ended, started)
    }

    /// A file's task ends with the file: aborting it stops it, gives its
    /// bytes, and a report it sent before is no longer counted.
    #[tokio::test]
    async fn an_aborted_send_task_ends_and_no_longer_counts() {
        let mut tasks = SendTasks::default();
        let (a, a_ended, a_started) = stuck(&mut tasks, 100);
        let (b, b_ended, b_started) = stuck(&mut tasks, 7);
        a_started.notified().await;
        b_started.notified().await;
        assert_eq!(tasks.len(), 2);

        assert_eq!(tasks.abort(a), 100, "the bytes it had sent");
        assert_eq!(tasks.len(), 1);
        for _ in 0..100 {
            if a_ended.load(SeqCst) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(a_ended.load(SeqCst), "the aborted task still runs");
        assert!(!b_ended.load(SeqCst));
        // A report from before the abort, read after it: not counted again.
        assert!(!tasks.finished(a));
        assert_eq!(tasks.abort(a), 0);

        assert!(tasks.finished(b));
        assert_eq!(tasks.len(), 0);
    }

    /// A sender role over a loopback connection, resumed from a plan (no
    /// listing), and what keeps it alive.
    async fn role() -> (SenderRole, crate::session::engine::TestCtx) {
        let t = crate::session::engine::test_ctx().await;
        let ctx = t.ctx.clone();
        let (send, _) = ctx.conn.open_bi().await.unwrap();
        let (events, _) = mpsc::unbounded_channel();
        let (out, _) =
            crate::session::control::spawn_writer(send, events.clone(), &ctx.hooks, usize::MAX);
        let sender = Arc::new(Sender::new(
            ctx.conn.clone(),
            ctx.session_id,
            ctx.base.clone(),
            ctx.hasher.clone(),
            1,
            DEFAULT_SPLIT_THRESHOLD,
            None,
        ));
        let s = SenderRole {
            ctx: ctx.clone(),
            out,
            events,
            sender,
            listed: Arc::default(),
            listing: Listing::FromPlan,
            files: HashMap::new(),
            basis_held: 0,
            basis_budget: 1 << 20,
            lists: ListCredit::new(1000),
            list_budget: 1000,
            seen: HashSet::new(),
            abandoned: HashMap::new(),
            files_done: 0,
            bytes: 0,
            transferred_size: 0,
            created: 0,
            tasks: SendTasks::default(),
            errors: Vec::new(),
            persist: Persister::start(&ctx).await,
            transfer: TransferPhase::default(),
            deleted: Vec::new(),
            would_delete: Vec::new(),
            max_delete_reached: false,
        };
        (s, t)
    }

    /// A file in flight at `path`, with `digest` as its chunk list sent.
    fn in_flight(path: &str, digest: Option<FileDigest>) -> SFile {
        SFile {
            path: RelPath::new(path.as_bytes()).unwrap(),
            gen: 0,
            retries: 0,
            append_from: None,
            digest: digest.map(Arc::new),
            bases: HashMap::new(),
            bases_held: 0,
            requested: HashSet::new(),
            streams: 0,
            task: None,
            digesting: None,
            pending: None,
            started: None,
            #[cfg(feature = "test-hooks")]
            hooked: false,
            span: tracing::Span::none(),
        }
    }

    /// A file the receiver acknowledges while its digest still runs: the
    /// digest task ends with the file, as its send task does.
    #[tokio::test]
    async fn a_file_acknowledged_early_takes_its_digest_task_with_it() {
        let (mut s, _t) = role().await;
        let (ended, started) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(tokio::sync::Notify::new()),
        );
        let task = tokio::spawn({
            let (e, s) = (ended.clone(), started.clone());
            async move {
                let _ended = Ended(e);
                s.notify_one();
                std::future::pending::<()>().await;
            }
        });
        started.notified().await;
        let mut f = in_flight("a", None);
        f.digesting = Some(task.abort_handle());
        s.files.insert(1, f);
        s.on_ack(FileAck {
            file_id: 1,
            status: Status::Done as i32,
            ..FileAck::default()
        });
        assert!(s.files.is_empty());
        for _ in 0..100 {
            if ended.load(SeqCst) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(ended.load(SeqCst), "the digest task still runs");
    }

    /// A receiver whose writer was retired after it offered bases, before
    /// it requested them, sends an empty request: the sender drops the
    /// file's bases, and their bytes leave the budget.
    #[tokio::test]
    async fn an_empty_need_drops_the_bases_offered() {
        let (mut s, _t) = role().await;
        let chunks = (0..4u32)
            .map(|i| irsync_proto::chunk::ChunkRecord {
                offset: u64::from(i) * 65536,
                len: 65536,
                hash: [0; 32],
            })
            .collect();
        let digest = FileDigest {
            size: 4 * 65536,
            hash: [0; 32],
            chunks,
        };
        s.files.insert(1, in_flight("a", Some(digest)));
        let block: Vec<u8> = (0..65536u32).map(|i| (i * 7919 % 251) as u8).collect();
        for idx in 0..2 {
            s.on_basis(Basis::sign(0, &block).to_wire(1, idx)).unwrap();
        }
        assert!(s.basis_held > 0, "no basis was held");
        assert_eq!(s.files[&1].bases.len(), 2);
        s.on_need(NeedChunks {
            file_id: 1,
            chunk_idx: Vec::new(),
        });
        assert_eq!(s.basis_held, 0);
        assert_eq!(s.files[&1].bases_held, 0);
        assert!(s.files[&1].bases.is_empty());
        assert_eq!(s.tasks.len(), 0, "an empty request sends nothing");
    }

    /// Files that come and go, each leaving a task that would never end on
    /// its own: none of them stays behind.
    #[tokio::test]
    async fn tasks_of_removed_files_do_not_accumulate() {
        let mut tasks = SendTasks::default();
        let mut ended = Vec::new();
        for _ in 0..1000 {
            let (id, e, started) = stuck(&mut tasks, 1);
            started.notified().await;
            // The file is acknowledged early, before its send is done.
            tasks.abort(id);
            ended.push(e);
            assert!(tasks.len() <= 1);
        }
        tokio::task::yield_now().await;
        for _ in 0..100 {
            if ended.iter().all(|e| e.load(SeqCst)) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            ended.iter().all(|e| e.load(SeqCst)),
            "aborted tasks still run"
        );
        assert_eq!(tasks.len(), 0);
    }
}
