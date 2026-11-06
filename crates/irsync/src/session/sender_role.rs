//! The sender role: walks and streams its listing, then answers the
//! receiver: `PlanItem` → digest (lookahead) → chunk list in parts; `NeedChunks` →
//! data streams. A file counts as done only at `FileAck DONE`. A file that
//! changed while being sent, or whose whole-file hash did not match at the
//! receiver, is re-stat'ed, re-digested and offered once more.
//!
//! Per-file state lives only while a file is in flight: it is dropped at its
//! final ack or when the sender gives up on it.

use std::collections::{HashMap, HashSet};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::bail;
use irsync_proto::chunk::FileDigest;
use irsync_proto::v1::{
    control::Msg, file_ack::Status, file_entry::Kind, ChunkBasis, FileAck, NeedChunks, PlanEntry,
    PlanItem, SessionDone,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::Instrument;

use super::control::msg_name;
use super::control::{
    is_own_temp, spawn_listing, walk_errors, Ev, ListedFiles, ListingSpec, Out, Persister,
};
use super::engine::{blocking, Ctx, Events, PeerError, TransferPhase, TransportError};
use super::parts::{split, PART_RECORDS};
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
const MAX_RETRIES: u32 = 1;
/// How long to wait for in-flight data streams after `SessionDone`.
const DRAIN_SENDS: Duration = Duration::from_secs(10);

/// Completions of the sender's tasks, delivered to its main loop.
pub(super) enum Done {
    Digested {
        file_id: u64,
        gen: u32,
        result: io::Result<FileDigest>,
    },
    Sent {
        file_id: u64,
        gen: u32,
        /// Bytes sent on success.
        result: io::Result<u64>,
    },
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
    /// chunk index; each is used for one request and dropped.
    bases: HashMap<u32, Basis>,
    /// Chunk indices the receiver asked for so far under this digest: the
    /// rest of the file is matched (`FileMatched`). A request may come in
    /// several parts (chunks without a basis first, then those with one).
    requested: HashSet<u32>,
    /// Send tasks of this generation still running; `FileSent` when the
    /// last one finishes.
    streams: u32,
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
    /// stored plan.
    Undecided,
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
    /// Files in flight (no final ack yet, not given up).
    files: HashMap<u64, SFile>,
    /// Every file id ever planned (duplicate detection).
    seen: HashSet<u64>,
    /// Files given up with `SOURCE_ERROR`, by id, with their path. If
    /// the receiver still places one (it had every chunk verified when our
    /// give-up arrived), its DONE withdraws the error.
    abandoned: HashMap<u64, RelPath>,
    files_done: u64,
    bytes: u64,
    /// `send_chunks` calls not yet reported back.
    sending: usize,
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
    fn start_listing(&mut self) {
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
            },
            self.out.clone(),
            None,
            Some(self.listed.clone()),
        ));
    }

    /// `check`: admit the path only if the walk would have listed it as a
    /// regular file (a resumed plan has no listing of this connection).
    /// `walked`: this connection's walk stat of the file, which spares the
    /// re-stat; a retry passes none, so it describes the file as it is now.
    fn spawn_digest(
        &self,
        file_id: u64,
        gen: u32,
        path: RelPath,
        force: bool,
        check: bool,
        walked: Option<Freshness>,
    ) {
        let (ctx, sender, events) = (self.ctx.clone(), self.sender.clone(), self.events.clone());
        let append_from = self.files.get(&file_id).and_then(|f| f.append_from);
        let hash = self.files.get(&file_id).map_or_else(
            tracing::Span::none,
            |f| tracing::debug_span!(parent: &f.span, "hash"),
        );
        tokio::spawn(
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
                    if let Some(walked) = walked {
                        // The walk saw a regular file; the opens below refuse
                        // anything else now.
                        if let Some(from) = append_from {
                            let full = ctx.full_key(&key);
                            return blocking(move || tail_digest(&full, from)).await;
                        }
                        let since = super::wait_start(&ctx.meter);
                        let digest = sender.lookahead_digest_walked(key, walked).await;
                        super::record_hash_wait(&ctx.meter, since);
                        return digest;
                    }
                    // Re-stat: the digest must describe the file as it is now.
                    let (base, rel) = (ctx.base.clone(), key.clone());
                    let (entry, ctime) =
                        blocking(move || lstat_entry_with_ctime(&base, &rel)).await?;
                    if entry.kind() != Kind::Regular {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "no longer a regular file",
                        ));
                    }
                    if let Some(from) = append_from {
                        // Only the tail: the receiver trusts its prefix.
                        let full = ctx.full_key(&key);
                        return blocking(move || tail_digest(&full, from)).await;
                    }
                    let item = ScanItem {
                        path: key,
                        entry,
                        ctime,
                    };
                    let since = super::wait_start(&ctx.meter);
                    let digest = if force {
                        ctx.forced.digest(&item).await
                    } else {
                        sender.lookahead_digest(&item).await
                    };
                    super::record_hash_wait(&ctx.meter, since);
                    digest
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
    }

    /// Gives up on the file and tells the receiver to stop waiting for it.
    fn source_error(&mut self, file_id: u64, msg: String) {
        let Some(f) = self.files.remove(&file_id) else {
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
        f.digest = None;
        f.bases.clear();
        f.requested.clear();
        f.streams = 0;
        let (gen, path) = (f.gen, f.path.clone());
        self.spawn_digest(file_id, gen, path, true, false, None);
    }

    fn on_plan(&mut self, p: PlanItem) -> anyhow::Result<()> {
        let file_id = p.file_id;
        if !self.seen.insert(file_id) {
            bail!("protocol error: duplicate file id {file_id}");
        }
        let path = RelPath::new(&p.path).unwrap_or_default();
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
                requested: HashSet::new(),
                streams: 0,
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
        // Only paths this side listed as regular files are ever read: a peer
        // cannot ask for something outside the walk (e.g. through a symlink).
        if path.as_bytes().is_empty() {
            self.source_error(file_id, "not a listed regular file".into());
            return Ok(());
        }
        if let Listing::FromPlan = self.listing {
            self.spawn_digest(file_id, 0, path, false, true, None);
            return Ok(());
        }
        let walked = self
            .listed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&path)
            .copied();
        let Some(walked) = walked else {
            self.source_error(file_id, "not a listed regular file".into());
            return Ok(());
        };
        self.spawn_digest(file_id, 0, path, false, false, Some(walked));
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
                .emit(super::Event::FileDone(path.clone(), 0));
        }
        let code = match i.code.strip_prefix('>') {
            Some(rest) => format!("<{rest}"),
            None => i.code,
        };
        self.ctx.hooks.bus.emit(super::Event::Itemized(path, code));
    }

    fn on_digested(&mut self, file_id: u64, gen: u32, result: io::Result<FileDigest>) {
        let Some(f) = self.files.get_mut(&file_id) else {
            return;
        };
        if f.gen != gen {
            return;
        }
        match result {
            Ok(d) => {
                let with_hash = f.append_from.is_none();
                for p in split(file_id, &d, part_records(&self.ctx), with_hash) {
                    self.out.send(Msg::ChunkListPart(p));
                    #[cfg(feature = "test-hooks")]
                    if let Some(h) = &self.ctx.hooks.drop_after_parts {
                        if h.queued() {
                            tracing::warn!(
                                "fault injection: dropping the connection between parts"
                            );
                            self.ctx.conn.close(99u32.into(), b"fault");
                        }
                    }
                }
                f.span.record("size", d.size);
                f.digest = Some(Arc::new(d));
            }
            Err(e) => self.source_error(file_id, e.to_string()),
        }
    }

    /// A basis for one chunk of a file in flight: kept for the request
    /// that follows. A malformed or misplaced basis is dropped (the chunk
    /// then goes raw); it is never a protocol error.
    fn on_basis(&mut self, b: ChunkBasis) {
        let Some(f) = self.files.get_mut(&b.file_id) else {
            return;
        };
        let Some(d) = &f.digest else {
            return;
        };
        let idx = b.chunk_idx;
        if (idx as usize) >= d.chunks.len()
            || (f.bases.len() >= d.chunks.len() && !f.bases.contains_key(&idx))
        {
            return;
        }
        match Basis::from_wire(b) {
            Ok(basis) => {
                f.bases.insert(idx, basis);
            }
            Err(e) => tracing::debug!(path = ?f.path, idx, error = %e, "ignoring a chunk basis"),
        }
    }

    fn on_need(&mut self, n: NeedChunks) {
        let file_id = n.file_id;
        let Some(f) = self.files.get_mut(&file_id) else {
            tracing::debug!(file_id, "NeedChunks for a file no longer in flight");
            return;
        };
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
        #[cfg_attr(not(feature = "test-hooks"), allow(unused_mut))]
        let mut bases: HashMap<u32, Basis> = idxs
            .iter()
            .filter_map(|i| f.bases.remove_entry(i))
            .collect();
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
        self.sending += 1;
        f.streams += 1;
        let send_span = tracing::debug_span!(parent: &f.span, "send");
        tokio::spawn(
            async move {
                #[cfg(feature = "test-hooks")]
                let result = match hook {
                    Some(h) if idxs.len() > 1 => {
                        let first: HashMap<u32, Basis> =
                            bases.remove_entry(&idxs[0]).into_iter().collect();
                        match sender
                            .send_chunks_against(file_id, &key, &digest, &idxs[..1], first)
                            .await
                        {
                            Ok(n) => {
                                h(&path).await;
                                sender
                                    .send_chunks_against(file_id, &key, &digest, &idxs[1..], bases)
                                    .await
                                    .map(|m| n + m)
                            }
                            Err(e) => Err(e),
                        }
                    }
                    _ => {
                        sender
                            .send_chunks_against(file_id, &key, &digest, &idxs, bases)
                            .await
                    }
                };
                #[cfg(not(feature = "test-hooks"))]
                let result = sender
                    .send_chunks_against(file_id, &key, &digest, &idxs, bases)
                    .await;
                let _ = events.send(Ev::Send(Done::Sent {
                    file_id,
                    gen,
                    result,
                }));
            }
            .instrument(send_span),
        );
    }

    fn on_sent(&mut self, file_id: u64, gen: u32, result: io::Result<u64>) {
        self.sending -= 1;
        // The last send of the current generation: only the ack is left.
        // (A superseded send must not mark the re-offered file sent.)
        if let Some(f) = self.files.get_mut(&file_id).filter(|f| f.gen == gen) {
            f.streams = f.streams.saturating_sub(1);
            if f.streams == 0 && result.is_ok() {
                self.ctx
                    .hooks
                    .bus
                    .emit(super::Event::FileSent(f.path.clone()));
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
            Err(e) => {
                let Some(f) = self.files.get(&file_id) else {
                    return;
                };
                if f.gen != gen {
                    return;
                }
                if e.kind() == io::ErrorKind::InvalidData {
                    // The file changed under us; never send stale data.
                    self.retry(file_id, e.to_string());
                } else {
                    self.source_error(file_id, e.to_string());
                }
            }
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
                if let Some(f) = self.files.remove(&file_id) {
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
                if let Some(f) = self.files.remove(&file_id) {
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
    events_tx: mpsc::UnboundedSender<Ev>,
    mut events: Events,
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
        listing: Listing::Undecided,
        files: HashMap::new(),
        seen: HashSet::new(),
        abandoned: HashMap::new(),
        files_done: 0,
        bytes: 0,
        transferred_size: 0,
        created: 0,
        sending: 0,
        errors: Vec::new(),
        persist,
        transfer: TransferPhase::default(),
        deleted: Vec::new(),
        would_delete: Vec::new(),
        max_delete_reached: false,
    };
    if !ctx.resumed {
        s.start_listing();
    }
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    let res: anyhow::Result<()> = loop {
        let ev = tokio::select! {
            ev = events.recv() => ev,
            _ = tick.tick() => {
                s.persist.bytes(s.bytes);
                s.persist.maybe_flush().await;
                continue;
            }
        };
        let Some(ev) = ev else {
            break Err(anyhow::anyhow!("event channel closed"));
        };
        if let Listing::Undecided = s.listing {
            match &ev {
                Ev::PeerListing | Ev::Ctrl(Msg::ListingEnd(_)) => s.start_listing(),
                Ev::Ctrl(Msg::Plan(_)) => {
                    tracing::info!("the receiver resumes its stored plan");
                    s.listing = Listing::FromPlan;
                }
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
                result,
            }) => {
                s.on_sent(file_id, gen, result);
                Ok(())
            }
            Ev::Ctrl(Msg::Plan(p)) => s.on_plan(p),
            Ev::Ctrl(Msg::Itemized(i)) => {
                s.on_itemized(i);
                Ok(())
            }
            Ev::Ctrl(Msg::ChunkBasis(b)) => {
                s.on_basis(b);
                Ok(())
            }
            Ev::Ctrl(Msg::Need(n)) => {
                s.on_need(n);
                Ok(())
            }
            Ev::Ctrl(Msg::Ack(a)) => {
                s.on_ack(a);
                Ok(())
            }
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
            Ev::PeerFailed(e) | Ev::WriterFailed(e) => {
                Err(TransportError(format!("control stream: {e}")).into())
            }
            Ev::PeerListing | Ev::Recv(_) => Ok(()),
        };
        if let Err(e) = step {
            break Err(e);
        }
    };
    s.transfer.end();
    if res.is_ok() {
        // The receiver can finish before our last streams report back (it
        // has the data; we wait for the transport's acknowledgement): collect
        // those results so the byte count is complete.
        let _ = tokio::time::timeout(DRAIN_SENDS, async {
            while s.sending > 0 {
                match events.recv().await {
                    Some(Ev::Send(Done::Sent {
                        file_id,
                        gen,
                        result,
                    })) => s.on_sent(file_id, gen, result),
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
        Listing::Undecided | Listing::FromPlan => Vec::new(),
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
