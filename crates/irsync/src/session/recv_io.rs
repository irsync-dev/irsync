//! The receiver role's file and data-stream I/O: opening `.part` writers,
//! deciding which chunks the receiver's own files can supply, copying those
//! while the sender delivers the rest, finalizing, and routing each incoming
//! data stream into the current writer of its file. A request whose streams
//! and local copies have all ended with chunks still missing is reported
//! back as `Drained`.
//!
//! For a `DELTA`, chunks the sender has to deliver are first offered a
//! basis: block signatures over the old file around the chunk's place
//! (`offer_bases`), so the sender can answer with operations against it
//! (`crate::delta`). A chunk rebuilt from operations is verified like any
//! other; one that does not verify stays missing and is re-requested raw.
//!
//! With `--inplace` the writer is the destination file itself, so every
//! write goes through a [`Gate`] that admits chunks in ascending order,
//! and the old content is only ever read above the write cursor.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use iroh::endpoint::{RecvStream, VarInt};
use irsync_proto::chunk::{ChunkRecord, FileDigest, CHUNK_PARAMS};
use irsync_proto::v1::{
    control::Msg, file_entry::Kind, plan_item::Action, ChunkBasis, DataChunk, DataStreamHeader,
    FileEntry, Hash, NeedChunks,
};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use super::control::{Ev, Out};
use super::engine::{blocking, Ctx};
use super::limits::{basis_wire_len, need_piece, ByteBudget, Held, BASIS_BATCH};
use super::receiver::Done;
use crate::delta::{decode, encoded_len, Basis, Op, MIN_BLOCK};
use crate::fs::meta::{apply_metadata_fd, lstat_entry_with_ctime};
use crate::fs::paths::RelPath;
use crate::index::{ChunkHolder, IndexHandle, NewEntry};
use crate::net::frame::read_msg;
use crate::recv::{discard_part, CloneSource, FileWriter, FinalizeError};
use crate::scan::ScanItem;
use crate::telemetry::meter::{Activity, Counter, Meter};

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_owned())
}

/// The file's writer. Never held across a network read or anything else
/// that needs the peer (only local disk work), so the main loop may await it.
pub(super) type Slot = Arc<tokio::sync::Mutex<Option<FileWriter>>>;

/// Where a data stream for a file id goes: the current writer generation.
#[derive(Clone)]
pub(super) struct Route {
    pub gen: u32,
    pub slot: Slot,
    /// The chunk list, shared with the file's state and its `Fill`.
    pub digest: Arc<FileDigest>,
    /// Units of delivery still running: one per requested chunk whose data
    /// stream has not ended, plus one for the local-copy task while it runs.
    pub pending: Arc<AtomicUsize>,
    /// The file's session path (events and logs).
    pub path: RelPath,
    /// The old destination file (`DELTA`), held open while bases over it
    /// are out: copies in the sender's operations read from it.
    pub old: Option<Arc<File>>,
    /// `--inplace`: admits writes in ascending chunk order.
    pub gate: Option<Arc<Gate>>,
    /// `APPEND`: the sender's list is the tail only, so its chunk indices
    /// (requests, bases, stream headers) are ours minus this.
    pub idx_base: u32,
}

/// The sender's index of our chunk `idx` (see [`Route::idx_base`]).
pub(super) fn sender_idx(base: u32, idx: u32) -> u32 {
    idx - base
}

pub(super) type Routes = Arc<std::sync::Mutex<HashMap<u64, Route>>>;

/// A chunk one of the receiver's own files holds: its index in the chunk
/// list, the file (absolute) and the chunk's offset in that file.
pub(super) type LocalChunk = (u32, PathBuf, u64);

/// A basis to offer for a chunk: its index, and the region of the old file
/// (offset, length) to sign for it.
pub(super) type BasisSpec = (u32, u64, u32);

struct GateState {
    /// The first chunk not yet settled: written, rejected or given up.
    cursor: u32,
    settled: Vec<bool>,
}

/// Orders the writes of an in-place file: a chunk may be written once
/// every chunk before it is settled (so nothing above the cursor has been
/// overwritten yet and forward reads of the old content stay valid), or
/// at any time once it lies below the cursor (a re-sent chunk rewrites a
/// range every forward reference has already passed).
pub(super) struct Gate {
    state: std::sync::Mutex<GateState>,
    tx: watch::Sender<u32>,
}

const GATE_CLOSED: u32 = u32::MAX;

impl Gate {
    /// A gate for `total` chunks of which `missing` are still to come.
    pub fn new(missing: &[u32], total: usize) -> Gate {
        let mut settled = vec![true; total];
        for &i in missing {
            if let Some(b) = settled.get_mut(i as usize) {
                *b = false;
            }
        }
        let cursor = Self::advance(&settled, 0);
        Gate {
            state: std::sync::Mutex::new(GateState { cursor, settled }),
            tx: watch::channel(cursor).0,
        }
    }

    fn advance(settled: &[bool], mut c: u32) -> u32 {
        while settled.get(c as usize).copied().unwrap_or(false) {
            c += 1;
        }
        c
    }

    /// Waits until chunk `idx` may be written; false once the gate is
    /// closed (the writer was retired).
    pub async fn admit(&self, idx: u32) -> bool {
        let mut rx = self.tx.subscribe();
        loop {
            let c = *rx.borrow_and_update();
            if c == GATE_CLOSED {
                return false;
            }
            if idx <= c {
                return true;
            }
            if rx.changed().await.is_err() {
                return false;
            }
        }
    }

    /// Chunk `idx` is done with, delivered or not.
    pub fn settle(&self, idx: u32) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(b) = s.settled.get_mut(idx as usize) {
            *b = true;
        }
        if idx == s.cursor {
            s.cursor = Self::advance(&s.settled, s.cursor);
            self.tx.send_replace(s.cursor);
        }
    }

    pub fn close(&self) {
        self.tx.send_replace(GATE_CLOSED);
    }
}

/// The old destination file of a `DELTA`, hashed: its digest and the stat
/// the digest was computed under.
pub(super) struct OldFile {
    pub digest: FileDigest,
    pub entry: FileEntry,
    pub ctime: (i64, u32),
}

pub(super) enum Opened {
    /// The `.part` writer and the chunks it still needs.
    Writer(Box<Fill>),
    /// The destination already holds the content (a `DELTA` whose
    /// whole-file hash matches): settled in place by [`settle_identical`].
    Identical { file: File, digest: Arc<FileDigest> },
}

pub(super) struct Fill {
    pub writer: FileWriter,
    /// Chunks with no local source: to be requested from the sender.
    pub remote: Vec<u32>,
    /// Chunks an indexed file holds: copied while the sender delivers the rest.
    pub local: Vec<LocalChunk>,
    pub digest: Arc<FileDigest>,
    /// Chunks of `remote` to offer a basis for (see [`offer_bases`]).
    pub bases: Vec<BasisSpec>,
    /// The old file the bases are over (set when `bases` is not empty).
    pub old: Option<Arc<File>>,
    /// `--inplace`: the writer is the destination; writes are ordered.
    pub gate: Option<Arc<Gate>>,
    /// See [`Route::idx_base`].
    pub idx_base: u32,
}

/// Picks the chunks of `remote` worth a basis and the old-file region for
/// each: from one chunk length before the chunk to two after, clamped to
/// the old file (from the chunk's own offset on when `forward_only`: in
/// place, bytes below it are already overwritten). Nothing is offered for
/// a file that lost more than half its chunks (a rewrite: the signatures
/// would match nothing) unless it is tiny, for a chunk starting past the
/// old file's end (appended data), or once the regions add up to the old
/// file's size (one cached pass at most; a few chunk lengths at least, so
/// a small file is covered whole).
fn basis_specs(
    chunks: &[ChunkRecord],
    remote: &[u32],
    old_size: u64,
    forward_only: bool,
) -> Vec<BasisSpec> {
    if remote.len() * 2 > chunks.len() && chunks.len() > 2 {
        return Vec::new();
    }
    let budget = old_size.max(3 * u64::from(CHUNK_PARAMS.max));
    let mut specs = Vec::new();
    let mut used = 0u64;
    for &idx in remote {
        let Some(c) = chunks.get(idx as usize) else {
            continue;
        };
        if c.offset >= old_size {
            continue;
        }
        let len = u64::from(c.len);
        let start = if forward_only {
            c.offset
        } else {
            c.offset.saturating_sub(len)
        };
        let end = old_size.min(c.offset + 2 * len);
        let region = end - start;
        if region < 2 * MIN_BLOCK as u64 {
            continue;
        }
        if used + region > budget {
            break;
        }
        used += region;
        specs.push((idx, start, region as u32));
    }
    specs
}

/// Makes sure the destination's current file at `path` has its chunks in
/// the index (a cache hit if it is fresh there, a read otherwise), so a
/// `DELTA` can take them from it. Failures only mean nothing local.
/// Hashes the old destination file (from the index when its row is fresh)
/// so its chunks can be found and its whole-file hash compared.
pub(super) async fn index_old_file(ctx: &Ctx, path: &RelPath) -> Option<OldFile> {
    hash_key(ctx, &ctx.key(path)).await
}

/// Hashes the regular file at index key `key` through the session's
/// hasher (cached when its row is fresh), so its chunks are in the index.
async fn hash_key(ctx: &Ctx, key: &RelPath) -> Option<OldFile> {
    let (base, rel) = (ctx.base.clone(), key.clone());
    let (entry, ctime) = blocking(move || lstat_entry_with_ctime(&base, &rel))
        .await
        .ok()?;
    if entry.kind() != Kind::Regular {
        return None;
    }
    let item = ScanItem {
        path: key.clone(),
        entry: entry.clone(),
        ctime,
    };
    let since = super::wait_start(&ctx.meter);
    let hashed = ctx.hasher.digest(&item).await;
    super::record_hash_wait(&ctx.meter, since);
    if hashed.is_ok() {
        // The chunk lookup that follows reads the index: wait for the
        // digest to be written there.
        let index = ctx.index().clone();
        let _ = blocking(move || {
            index.flush();
            Ok(())
        })
        .await;
    }
    match hashed {
        Ok(digest) => Some(OldFile {
            digest,
            entry,
            ctime,
        }),
        Err(e) => {
            tracing::debug!(?key, error = %e, "hashing failed");
            None
        }
    }
}

/// Before a whole file of `size` bytes is fetched: hashes the destination
/// files of exactly that size the walk found without a fresh index row
/// (up to [`crate::warm::CANDIDATES`]), so the chunk lookup that follows
/// can find a renamed or copied file. Each candidate is tried once per
/// session.
async fn warm_candidates(ctx: &Ctx, size: u64) {
    let keys = ctx.cold.take(size);
    if keys.is_empty() {
        return;
    }
    futures::future::join_all(keys.iter().map(|key| async move {
        if hash_key(ctx, key).await.is_some() {
            ctx.hooks.event("warm", key);
        }
    }))
    .await;
}

/// Opens the destination if it is still the file `old` was hashed from:
/// same size, mtime, ctime, device and inode.
async fn unchanged_file(ctx: &Ctx, key: &RelPath, old: &OldFile) -> Option<File> {
    let full = ctx.full_key(key);
    let (entry, ctime) = (old.entry.clone(), old.ctime);
    blocking(move || {
        let file = crate::fs::safe::open_regular_nofollow(&full)?;
        let m = file.metadata()?;
        let same = m.len() == entry.size
            && m.mtime() == entry.mtime_sec
            && m.mtime_nsec() == i64::from(entry.mtime_nsec)
            && m.ctime() == ctime.0
            && m.ctime_nsec() == i64::from(ctime.1)
            && m.dev() == entry.dev
            && m.ino() == entry.inode;
        Ok(same.then_some(file))
    })
    .await
    .ok()
    .flatten()
}

/// Opens the `.part` writer (adopting earlier progress) and sorts the
/// chunks still missing into those an indexed local file holds and those
/// the sender has to deliver. Nothing is copied here: the request for the
/// remote set should go out before the local copies start.
///
/// A `DELTA` whose old file hashes to the chunk list's whole-file hash
/// needs no `.part` at all: it is returned as [`Opened::Identical`].
///
/// `prehash`: the planner's early hashing of the old file (`DELTA`), awaited
/// rather than repeated.
pub(super) async fn open_writer(
    ctx: Arc<Ctx>,
    path: RelPath,
    action: Action,
    prehash: Option<JoinHandle<Option<OldFile>>>,
    mut digest: Arc<FileDigest>,
    sparse: bool,
) -> io::Result<Opened> {
    // The writer works on index keys under the index root, so its partial
    // record is keyed like every other index row; the file lands at the same place.
    let key = ctx.key(&path);
    // The old file's size, when there is one to offer bases over; its
    // descriptor and chunk list when it is still the file that was hashed.
    let mut old_size = None;
    let mut old_file: Option<Arc<File>> = None;
    let mut old_digest: Option<FileDigest> = None;
    // The old file's hash: the planner's early start, or now.
    let mut old = None;
    if matches!(action, Action::Delta | Action::Append) {
        old = match prehash {
            Some(h) => h.await.ok().flatten(),
            None => index_old_file(&ctx, &path).await,
        };
    }
    if action == Action::Delta {
        let same = old
            .as_ref()
            .filter(|o| o.digest.size == digest.size && o.digest.hash == digest.hash);
        #[cfg(feature = "test-hooks")]
        if same.is_some() {
            if let Some(h) = &ctx.hooks.before_identical_check {
                h(&path).await;
            }
        }
        // The old file, still the one that was hashed: the source of a
        // clone for the `.part`, of the bases, and settled in place when
        // the content is already there.
        if let Some(o) = &old {
            if let Some(file) = unchanged_file(&ctx, &key, o).await {
                // Settled in place, the metadata would reach every other name
                // of a linked old file: it is rebuilt from itself instead.
                if same.is_some() && o.entry.nlink <= 1 {
                    return Ok(Opened::Identical { file, digest });
                }
                old_file = Some(Arc::new(file));
            } else {
                tracing::debug!(?path, "destination changed since it was hashed");
            }
        }
        old_size = old.as_ref().map(|o| o.entry.size);
        old_digest = old.take().map(|o| o.digest);
    }
    // An `APPEND` list covers the tail only: the old file, which must
    // still be the planned prefix, supplies the rest of the list and its
    // bytes; the whole-file hash is folded here, not checked.
    let mut verify_whole = true;
    let mut idx_base = 0u32;
    if action == Action::Append {
        let from = digest.chunks.first().map_or(digest.size, |c| c.offset);
        let basis = match old.take() {
            Some(o) if o.digest.size == from && o.entry.size == from => o,
            _ => return Err(invalid("append: destination is not the planned prefix")),
        };
        let Some(file) = unchanged_file(&ctx, &key, &basis).await else {
            return Err(invalid("append: destination changed since it was planned"));
        };
        let mut chunks = basis.digest.chunks.clone();
        idx_base = chunks.len() as u32;
        chunks.extend_from_slice(&digest.chunks);
        digest = Arc::new(FileDigest {
            size: digest.size,
            hash: [0; 32],
            chunks,
        });
        ctx.hooks.event("append", &path);
        old_size = Some(from);
        old_file = Some(Arc::new(file));
        old_digest = Some(basis.digest);
        verify_whole = false;
    }
    let (base, rel, index, pd, activity, meter) = (
        ctx.base.clone(),
        key.clone(),
        ctx.index().clone(),
        ctx.partial_dir_from_base(),
        ctx.activity.clone(),
        Some(ctx.meter.clone()),
    );
    let expected = digest.clone();
    let (clone_file, clone_digest) = (old_file.clone(), old_digest);
    let inplace = ctx.opts.inplace && matches!(action, Action::Delta | Action::Append);
    // Same-size files not hashed yet may hold the chunks: hashed first, so
    // the lookup below finds them.
    let reuse = ctx.opts.reuse_local;
    if reuse && action == Action::Whole && !digest.chunks.is_empty() {
        warm_candidates(&ctx, digest.size).await;
    }
    let lookup_base = ctx.base.clone();
    // The writer, and where its missing chunks can be copied from, in one
    // blocking call.
    let (mut w, found) = blocking(move || {
        let clone = match (&clone_file, &clone_digest) {
            (Some(file), Some(digest)) => Some(CloneSource { file, digest }),
            _ => None,
        };
        let w = match clone {
            // In place needs the old file as hashed; otherwise a `.part`.
            Some(old) if inplace => FileWriter::open_inplace(
                &base,
                &rel,
                &expected,
                index.clone(),
                activity,
                meter,
                old,
            ),
            clone => FileWriter::open_from(
                &base,
                &rel,
                &expected,
                pd.as_ref(),
                index.clone(),
                activity,
                meter,
                clone,
            ),
        }?;
        let wanted: Vec<(u32, [u8; 32])> = w
            .missing_chunks()
            .into_iter()
            .map(|i| (i, expected.chunks[i as usize].hash))
            .collect();
        let found = if wanted.is_empty() {
            Ok(Vec::new())
        } else if reuse {
            local_sources(&index, &lookup_base, &wanted)
        } else {
            // `--no-rename-detection`: no other local file is a source.
            Ok(wanted.iter().map(|&(i, _)| (i, None)).collect())
        };
        Ok((w, found))
    })
    .await?;
    if w.cloned() {
        ctx.hooks.event("clone", &path);
    }
    if w.inplace() {
        ctx.hooks.event("inplace", &path);
    }
    w.set_sparse(sparse);
    w.set_verify_whole(verify_whole);
    w.set_sync(ctx.opts.fsync);
    let (mut remote, mut local) = (Vec::new(), Vec::new());
    match found {
        Ok(found) => {
            for (idx, hit) in found {
                match hit {
                    // In place, the file's own bytes below the chunk
                    // are gone by the time it is written.
                    Some((p, off))
                        if w.inplace() && p == key && off < digest.chunks[idx as usize].offset =>
                    {
                        remote.push(idx)
                    }
                    Some((p, off)) => local.push((idx, ctx.full_key(&p), off)),
                    None => remote.push(idx),
                }
            }
        }
        Err(e) => {
            tracing::warn!(?path, error = %e, "chunk lookup failed");
            remote = w.missing_chunks();
        }
    }
    let mut bases = match old_size {
        Some(size) if !remote.is_empty() => basis_specs(&digest.chunks, &remote, size, w.inplace()),
        _ => Vec::new(),
    };
    let gate = w
        .inplace()
        .then(|| Arc::new(Gate::new(&w.missing_chunks(), digest.chunks.len())));
    let mut old = old_file;
    if !bases.is_empty() && old.is_none() {
        let full = ctx.full_key(&key);
        match blocking(move || crate::fs::safe::open_regular_nofollow(&full)).await {
            Ok(f) => old = Some(Arc::new(f)),
            Err(e) => {
                tracing::debug!(?path, error = %e, "old file not opened; no bases");
                bases.clear();
            }
        }
    }
    Ok(Opened::Writer(Box::new(Fill {
        writer: w,
        remote,
        local,
        digest,
        bases,
        old,
        gate,
        idx_base,
    })))
}

/// Signs each region in `specs` from the old file and sends it as a
/// `ChunkBasis`, then requests the chunks `request` (those of the specs,
/// or in place every remote chunk, so that one request is ascending). The
/// request always goes out: a basis that could not be computed just means
/// a raw chunk. The sender uses each basis for the request that follows
/// it and drops it.
///
/// Bases are held under the session's basis budget (`budget`, see
/// [`BASIS_BUDGET`](super::limits::BASIS_BUDGET)) from being sent until
/// their request is queued. They go out in batches of at most
/// [`BASIS_BATCH`], each followed by the request for its chunks; a basis
/// that does not fit first requests the batch so far, which frees its
/// bytes without anything from the peer, and only then waits for room. In
/// place, the one ascending request comes last, so nothing could be freed
/// before it: a basis that does not fit now is not offered (its chunk goes
/// raw), and nothing waits for the budget.
///
/// Every frame is paced (`Out::send_paced`): a mostly rewritten large
/// file signs bases at disk speed, so a slow link stalls this task instead
/// of the queue growing. It holds no lock while it waits (see [`Slot`]),
/// and what it holds of the budget goes back however it ends, dropped
/// included (see [`Held`]). Once the file's writer is retired (a new chunk
/// list, a failure) its bases are stale: the task stops before signing
/// another or waiting for room for one (see [`retired`]).
#[allow(clippy::too_many_arguments)]
pub(super) async fn offer_bases(
    ctx: Arc<Ctx>,
    out: Out,
    route: Route,
    old: Arc<File>,
    specs: Vec<BasisSpec>,
    request: Vec<u32>,
    file_id: u64,
    budget: Arc<ByteBudget>,
) {
    let held = Held::new(budget);
    if route.gate.is_some() {
        offer_in_place(ctx, out, route, old, specs, request, file_id, held).await;
    } else {
        offer_batched(ctx, out, route, old, specs, request, file_id, held).await;
    }
}

/// [`offer_bases`] for a new file: bases in batches, each followed by its
/// request, then the request for the rest.
#[allow(clippy::too_many_arguments)]
async fn offer_batched(
    ctx: Arc<Ctx>,
    out: Out,
    route: Route,
    old: Arc<File>,
    specs: Vec<BasisSpec>,
    request: Vec<u32>,
    file_id: u64,
    // The bytes of the bases sent and not yet requested.
    mut held: Held,
) {
    let mut batch: Vec<u32> = Vec::new();
    // Chunks a batch requested; the last request has the rest.
    let mut batched: HashSet<u32> = HashSet::new();
    for spec in specs {
        if retired(&route) {
            break;
        }
        let wire = match sign(&ctx, &old, &route, file_id, spec).await {
            Ok(Some(wire)) => wire,
            Ok(None) => continue,
            Err(()) => break,
        };
        let n = basis_wire_len(&wire);
        if batch.len() == BASIS_BATCH || !held.try_take(n) {
            if !batch.is_empty() {
                if !request_chunks(&out, &route, file_id, &batch, &mut held).await {
                    return;
                }
                batch.clear();
            }
            if retired(&route) {
                break;
            }
            // Holding nothing: others free what this waits for.
            held.take(n).await;
        }
        out.send_paced(Msg::ChunkBasis(wire)).await;
        ctx.hooks.event("basis", &route.path);
        batch.push(spec.0);
        batched.insert(spec.0);
    }
    #[cfg(feature = "test-hooks")]
    if let Some(h) = &ctx.hooks.after_bases {
        h(&route.path).await;
    }
    if !batch.is_empty() && !request_chunks(&out, &route, file_id, &batch, &mut held).await {
        return;
    }
    // The rest, raw (always sent, even empty), in pieces that fit a frame.
    let rest: Vec<u32> = request
        .into_iter()
        .filter(|i| !batched.contains(i))
        .collect();
    for piece in pieces(&rest, need_piece(&ctx.hooks)) {
        if !request_chunks(&out, &route, file_id, piece, &mut held).await {
            return;
        }
    }
}

/// [`offer_bases`] in place: the one ascending request, in pieces that fit
/// a frame, each after the bases of its chunks. Each request takes every
/// basis the sender holds for the file, so a basis sent before an earlier
/// piece would be dropped unused. A piece's bases are signed once the
/// pieces before it are requested, which is safe: the writes are ordered,
/// so none has reached this piece's first chunk, and a basis region starts
/// at its chunk (`forward_only`).
#[allow(clippy::too_many_arguments)]
async fn offer_in_place(
    ctx: Arc<Ctx>,
    out: Out,
    route: Route,
    old: Arc<File>,
    specs: Vec<BasisSpec>,
    request: Vec<u32>,
    file_id: u64,
    mut held: Held,
) {
    let pieces = pieces(&request, need_piece(&ctx.hooks));
    let last = pieces.len() - 1;
    // The specs follow the request's order (a subsequence of it).
    let mut specs = specs.into_iter().peekable();
    let mut signing = true;
    for (k, piece) in pieces.into_iter().enumerate() {
        for &idx in piece {
            while let Some(spec) = specs.next_if(|s| s.0 == idx) {
                signing = signing && !retired(&route);
                if !signing {
                    continue;
                }
                match sign(&ctx, &old, &route, file_id, spec).await {
                    Ok(Some(wire)) => {
                        // No room: the chunk goes raw. Nothing waits here,
                        // since only this task's own request frees room.
                        if held.try_take(basis_wire_len(&wire)) {
                            out.send_paced(Msg::ChunkBasis(wire)).await;
                            ctx.hooks.event("basis", &route.path);
                        }
                    }
                    Ok(None) => {}
                    Err(()) => signing = false,
                }
            }
        }
        #[cfg(feature = "test-hooks")]
        if k == last {
            if let Some(h) = &ctx.hooks.after_bases {
                h(&route.path).await;
            }
        }
        #[cfg(not(feature = "test-hooks"))]
        let _ = (k, last);
        if !request_chunks(&out, &route, file_id, piece, &mut held).await {
            return;
        }
    }
}

/// `chunks` in pieces of at most `piece`; none at all as one empty piece
/// (an empty request still goes out).
fn pieces(chunks: &[u32], piece: usize) -> Vec<&[u32]> {
    if chunks.is_empty() {
        return vec![chunks];
    }
    chunks.chunks(piece).collect()
}

/// Signs the basis of chunk `idx` over `len` bytes of the old file at
/// `offset`, as it goes on the wire; `None` for a region without a whole
/// block, `Err` when reading the old file fails (no more bases for it).
async fn sign(
    ctx: &Ctx,
    old: &Arc<File>,
    route: &Route,
    file_id: u64,
    (idx, offset, len): BasisSpec,
) -> Result<Option<ChunkBasis>, ()> {
    let (f, meter) = (old.clone(), ctx.meter.clone());
    let basis = blocking(move || {
        let _timed = meter.start(Activity::Read);
        let mut buf = vec![0u8; len as usize];
        f.read_exact_at(&mut buf, offset)?;
        meter.add(Counter::DiskRead, u64::from(len));
        Ok(Basis::sign(offset, &buf))
    })
    .await;
    ctx.activity.fetch_add(1, Ordering::Relaxed);
    match basis {
        Ok(b) if b.blocks() > 0 => Ok(Some(b.to_wire(file_id, sender_idx(route.idx_base, idx)))),
        Ok(_) => Ok(None),
        Err(e) => {
            tracing::debug!(path = ?route.path, idx, error = %e, "reading the old file for a basis failed");
            Err(())
        }
    }
}

/// Whether the file's writer was retired: its bases are no longer wanted.
/// Never waits: a slot someone else holds (for local disk work, see
/// [`Slot`]) counts as current, and the request that ends the task checks
/// again (see [`request_chunks`]).
fn retired(route: &Route) -> bool {
    route.slot.try_lock().is_ok_and(|w| w.is_none())
}

/// Queues the request for `chunks` once the writer has room for it, then
/// gives back `held`, the bases it uses: the sender drops them on reading
/// it. A writer retired meanwhile (new chunk list, failure) has nothing to
/// request, so it returns false; if bases are out, an empty request still
/// makes the sender drop them (see `SenderRole::on_need`) before their
/// bytes go back, so the sender never holds more than this side counts.
async fn request_chunks(
    out: &Out,
    route: &Route,
    file_id: u64,
    chunks: &[u32],
    held: &mut Held,
) -> bool {
    let need = out
        .reserve(Msg::Need(NeedChunks {
            file_id,
            chunk_idx: chunks
                .iter()
                .map(|&i| sender_idx(route.idx_base, i))
                .collect(),
        }))
        .await;
    // Checked after the wait for room, and the lock let go before queueing.
    let current = route.slot.lock().await.is_some();
    if current {
        need.send();
    } else if held.bytes() > 0 {
        drop(need);
        out.send_paced(Msg::Need(NeedChunks {
            file_id,
            chunk_idx: Vec::new(),
        }))
        .await;
    }
    held.release();
    current
}

/// Rebuilds chunk `idx` from the sender's operations against the old file.
async fn rebuild(
    route: &Route,
    idx: usize,
    ops: Vec<Op>,
    meter: Arc<Meter>,
) -> io::Result<Vec<u8>> {
    let old = route
        .old
        .clone()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "operations without a basis"))?;
    let len = route
        .digest
        .chunks
        .get(idx)
        .map(|c| c.len as usize)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "chunk index out of range"))?;
    blocking(move || {
        // Rebuilding mixes disk reads with literal bytes; all of it is the read side of the chunk.
        let _timed = meter.start(Activity::Read);
        decode(&ops, len, &mut |off, buf| {
            meter.add(Counter::DiskRead, buf.len() as u64);
            old.read_exact_at(buf, off)
        })
    })
    .await
}

/// Settles a `DELTA` file whose destination already holds the content:
/// applies the sender's metadata to it in place, discards a leftover
/// `.part`, and records it in the index with the digest the sender gave.
pub(super) async fn settle_identical(
    ctx: Arc<Ctx>,
    file: File,
    path: RelPath,
    entry: FileEntry,
    digest: Arc<FileDigest>,
) -> Result<(), FinalizeError> {
    let apply = ctx.apply_opts(&entry);
    let (base, key, index, pd) = (
        ctx.base.clone(),
        ctx.key(&path),
        ctx.index().clone(),
        ctx.partial_dir_from_base(),
    );
    blocking(move || {
        if let Err(e) = discard_part(&base, &key, pd.as_ref(), &index) {
            tracing::warn!(?key, error = %e, "discarding a stale partial file failed");
        }
        apply_metadata_fd(&file, &entry, &apply)
    })
    .await?;
    record(&ctx, &path, &digest, digest.hash, None).await;
    Ok(())
}

/// Reads `len` bytes at `offset` of `from` (a regular file, never a symlink).
async fn read_local(
    from: PathBuf,
    offset: u64,
    len: u32,
    meter: Arc<Meter>,
) -> io::Result<Vec<u8>> {
    blocking(move || {
        let _timed = meter.start(Activity::Read);
        let mut buf = vec![0u8; len as usize];
        crate::fs::safe::open_regular_nofollow(&from)?.read_exact_at(&mut buf, offset)?;
        meter.add(Counter::DiskRead, u64::from(len));
        Ok(buf)
    })
    .await
}

/// Fills `local` chunks from the receiver's own files while the sender
/// delivers the rest. Each copy is verified like a network chunk; one that
/// no longer matches stays missing and is re-requested once every unit of
/// the request has ended. Counts as one unit of the file's request.
/// `alone`: the sender delivers none of the file's chunks, so nothing else
/// waits for the writer lock.
pub(super) async fn copy_local_chunks(
    ctx: Arc<Ctx>,
    route: Route,
    local: Vec<LocalChunk>,
    alone: bool,
    events: mpsc::UnboundedSender<Ev>,
    file_id: u64,
) {
    for (idx, from, offset) in local {
        #[cfg(feature = "test-hooks")]
        if let Some(d) = ctx.hooks.slow_local_copy {
            tokio::time::sleep(d).await;
        }
        let Some(len) = route.digest.chunks.get(idx as usize).map(|c| c.len) else {
            continue;
        };
        // Read outside the writer lock while network streams write into
        // the file; alone, read and write in one blocking call under it.
        let data = if alone {
            None
        } else {
            match read_local(from.clone(), offset, len, ctx.meter.clone()).await {
                Ok(d) => Some(d),
                Err(e) => {
                    tracing::debug!(from = ?from, idx, error = %e, "local copy failed");
                    settle(&route, idx);
                    continue;
                }
            }
        };
        if let Some(g) = &route.gate {
            if !g.admit(idx).await {
                break;
            }
        }
        let mut guard = route.slot.lock().await;
        let Some(w) = guard.as_mut() else {
            break; // The writer was retired (new chunk list, failure).
        };
        let res = match &data {
            Some(data) => write_into(&ctx, &route, w, idx, data, false).await,
            None => {
                let was_open = w.is_open();
                let res = w.copy_local(idx, &from, offset).await;
                if !was_open && w.is_open() {
                    ctx.hooks.event("open", &route.path);
                }
                res
            }
        };
        if let Err(e) = res {
            // Stale bytes never verify; any failure leaves the chunk
            // missing for the follow-up request.
            tracing::debug!(from = ?from, idx, error = %e, "local copy rejected");
        }
        settle(&route, idx);
    }
    ctx.hooks.event("copied", &route.path);
    finish_unit(&route, &events, file_id, 1).await;
}

/// A chunk index and the indexed file and offset holding it, if any.
type ChunkHit = (u32, Option<(RelPath, u64)>);

/// Indexed files holding a chunk read from the index in one go after the
/// first; they are checked in order up to the first still the indexed file.
/// The first is read alone: it nearly always is.
const SOURCES_PER_PAGE: usize = 16;

/// Where each of the `wanted` chunks (index, hash) can be copied from
/// locally, per the index under `base`. A source file that is gone or was
/// replaced (another inode or size than indexed) is passed over for the
/// next file holding the chunk, and queued to be dropped from the index,
/// so no copy is planned from it; bytes changed in place are still caught
/// when the copied chunk is verified.
fn local_sources(
    index: &IndexHandle,
    base: &Path,
    wanted: &[(u32, [u8; 32])],
) -> io::Result<Vec<ChunkHit>> {
    let mut hits: Vec<_> = wanted.iter().map(|&(i, _)| (i, None)).collect();
    // The chunks still without a source, each with the last path looked at.
    let mut todo: Vec<(usize, Option<RelPath>)> = (0..wanted.len()).map(|k| (k, None)).collect();
    // Whether each source looked at is still the file the index describes,
    // by the inode and size it described (a file this session records at a
    // path judged gone is looked at afresh).
    let mut live: HashMap<(RelPath, u64, u64), bool> = HashMap::new();
    while !todo.is_empty() {
        // On a read connection: the next page of candidates.
        let pages = index
            .read(|r| {
                todo.iter()
                    .map(|(k, after)| {
                        let len = if after.is_some() { SOURCES_PER_PAGE } else { 1 };
                        let page = r.find_chunks(&wanted[*k].1, after.as_ref(), len)?;
                        Ok((*k, len, page))
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .map_err(io::Error::other)?;
        todo.clear();
        for (k, len, page) in pages {
            // The candidates in order, up to the first still on disk.
            for h in &page {
                let key = (h.path.clone(), h.inode, h.size);
                let ok = *live.entry(key).or_insert_with(|| {
                    let ok = still_indexed(base, h);
                    if !ok {
                        tracing::debug!(path = ?h.path, "local source gone; dropped from the index");
                        index.queue_removal(h.path.clone(), h.inode, h.size);
                    }
                    ok
                });
                if ok {
                    hits[k].1 = Some((h.path.clone(), h.offset));
                    break;
                }
            }
            // Every candidate on the page is gone: the next page. The
            // cursor only moves forward, so this ends.
            if hits[k].1.is_none() && page.len() == len {
                todo.push((k, page.last().map(|h| h.path.clone())));
            }
        }
    }
    Ok(hits)
}

/// Whether the file at `h.path` under `base` is still the regular file the
/// index describes, by inode and size.
fn still_indexed(base: &Path, h: &ChunkHolder) -> bool {
    let full = if h.path.as_bytes().is_empty() {
        base.to_path_buf()
    } else {
        base.join(h.path.to_os())
    };
    std::fs::symlink_metadata(&full)
        .is_ok_and(|m| m.is_file() && m.ino() == h.inode && m.len() == h.size)
}

/// Writes a chunk, from the sender (`remote`) or a local file, into the
/// file's current writer, noting the `.part` open a first write performs.
async fn write_into(
    ctx: &Ctx,
    route: &Route,
    w: &mut FileWriter,
    idx: u32,
    data: &[u8],
    remote: bool,
) -> io::Result<()> {
    let was_open = w.is_open();
    let res = if remote {
        w.write_chunk(idx, data).await
    } else {
        w.write_copied(idx, data).await
    };
    if !was_open && w.is_open() {
        ctx.hooks.event("open", &route.path);
    }
    res
}

/// In place: chunk `idx` is done with (written, rejected or given up).
fn settle(route: &Route, idx: u32) {
    if let Some(g) = &route.gate {
        g.settle(idx);
    }
}

/// Accounts `units` of the file's request as ended, delivered or not, and
/// reports the file ready (every chunk verified on disk) or drained (every
/// unit ended with chunks still missing). Whichever unit ends last makes
/// the report; a retired writer means the file is no longer ours to report.
async fn finish_unit(
    route: &Route,
    events: &mpsc::UnboundedSender<Ev>,
    file_id: u64,
    units: usize,
) {
    let before = route
        .pending
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |p| {
            Some(p.saturating_sub(units))
        })
        .unwrap_or(0);
    let drained = before <= units;
    let mut guard = route.slot.lock().await;
    let Some(complete) = guard.as_ref().map(|w| w.missing_chunks().is_empty()) else {
        return;
    };
    if complete {
        if let Some(writer) = guard.take() {
            let _ = events.send(Ev::Recv(Done::Ready {
                file_id,
                gen: route.gen,
                writer: Box::new(writer),
            }));
        }
    } else if drained {
        let _ = events.send(Ev::Recv(Done::Drained {
            file_id,
            gen: route.gen,
        }));
    }
}

/// Verifies and places the file, then records it (with the digest the sender
/// gave) in the index so later sessions can quick-check and reuse it.
pub(super) async fn finalize(
    ctx: Arc<Ctx>,
    w: FileWriter,
    path: RelPath,
    entry: FileEntry,
    digest: Arc<FileDigest>,
) -> Result<(), FinalizeError> {
    let apply = ctx.apply_opts(&entry);
    #[cfg(feature = "test-hooks")]
    let mut w = w;
    #[cfg(feature = "test-hooks")]
    if let Some(d) = ctx.hooks.slow_finalize {
        w.slow_finalize(d);
    }
    if !w.is_open() {
        // Nothing was ever written (an empty file): finalize opens it.
        ctx.hooks.event("open", &path);
    }
    let placed = w.place(&entry, &apply).await?;
    if ctx.opts.fsync {
        ctx.hooks.event("fsync", &path);
    }
    ctx.hooks.event("placed", &path);
    record(&ctx, &path, &digest, placed.hash, placed.stat).await;
    Ok(())
}

/// Records the placed file in the index with its chunk list and whole-file
/// hash (the sender's, or for an `APPEND` the one folded from the bytes),
/// so later sessions can quick-check and reuse it. `stat` is the file's
/// entry as read when it was placed; without it the path is looked at.
async fn record(
    ctx: &Ctx,
    path: &RelPath,
    digest: &FileDigest,
    hash: [u8; 32],
    stat: Option<(FileEntry, (i64, u32))>,
) {
    let (base, key, index, mut digest) = (
        ctx.base.clone(),
        ctx.key(path),
        ctx.index().clone(),
        digest.clone(),
    );
    digest.hash = hash;
    if let Some(stat) = stat {
        queue_record(&index, key, digest, stat);
        return;
    }
    let rec = blocking(move || {
        let stat = lstat_entry_with_ctime(&base, &key)?;
        queue_record(&index, key, digest, stat);
        Ok(())
    })
    .await;
    if let Err(e) = rec {
        tracing::warn!(error = %e, "recording received file in the index failed");
    }
}

/// Queues the index entry of a placed file: `stat` with the whole-file
/// hash and chunk list of `digest`, unless the sizes disagree (the file
/// changed since).
fn queue_record(
    index: &IndexHandle,
    key: RelPath,
    digest: FileDigest,
    (mut e, ctime): (FileEntry, (i64, u32)),
) {
    if e.size != digest.size {
        return;
    }
    e.whole_hash = Some(Hash {
        bytes: digest.hash.to_vec(),
    });
    index.queue_entry(NewEntry {
        path: key,
        entry: e,
        ctime,
        digest: Some(digest),
    });
}

/// Chunk count of a data stream's range, if it lines up with the chunk list.
fn range_len(chunks: &[ChunkRecord], first: usize, offset: u64, length: u64) -> Option<usize> {
    let start = chunks.get(first)?;
    if start.offset != offset || length == 0 {
        return None;
    }
    let mut sum = 0u64;
    for (k, c) in chunks[first..].iter().enumerate() {
        sum += u64::from(c.len);
        if sum == length {
            return Some(k + 1);
        }
        if sum > length {
            return None;
        }
    }
    None
}

/// Reads one data stream into the current writer of its file. Chunks are
/// verified one by one; a stream that ends early (EOF or reset) leaves its
/// remaining chunks missing, so the file can never complete from it. Every
/// stream routed to a file is accounted against the file's pending request,
/// whatever way it ends, so the request always drains.
async fn read_stream(
    ctx: Arc<Ctx>,
    routes: Routes,
    events: mpsc::UnboundedSender<Ev>,
    bytes: Arc<AtomicU64>,
    mut s: RecvStream,
) {
    let _open = ctx.meter.hold("irsync_streams_open");
    let header = {
        let _recv = ctx.meter.start(Activity::Recv);
        read_msg(&mut s).await
    };
    let header: DataStreamHeader = match header {
        Ok(Some(h)) => h,
        Ok(None) => return,
        Err(e) => {
            tracing::debug!(error = %e, "data stream without header");
            return stop(&mut s, 1);
        }
    };
    if header.session_id != ctx.session_id {
        tracing::warn!(file_id = header.file_id, "data stream for another session");
        return stop(&mut s, 1);
    }
    let file_id = header.file_id;
    let route = routes
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&file_id)
        .cloned();
    let Some(route) = route else {
        tracing::debug!(file_id, "data stream for a file not being received");
        return stop(&mut s, 0);
    };
    let accounted = read_range(&ctx, &route, &events, &bytes, &header, &mut s).await;
    // This stream's range is accounted for, delivered or not.
    finish_unit(&route, &events, file_id, accounted).await;
}

fn stop(s: &mut RecvStream, code: u32) {
    let _ = s.stop(VarInt::from_u32(code));
}

/// Writes the stream's chunks; returns how many requested chunks the stream
/// accounts for (all outstanding ones when its range is unusable).
async fn read_range(
    ctx: &Ctx,
    route: &Route,
    events: &mpsc::UnboundedSender<Ev>,
    bytes: &AtomicU64,
    header: &DataStreamHeader,
    s: &mut RecvStream,
) -> usize {
    let file_id = header.file_id;
    let first = header.first_chunk_idx as usize + route.idx_base as usize;
    let Some(n) = range_len(&route.digest.chunks, first, header.offset, header.length) else {
        tracing::warn!(file_id, "data stream range does not match the chunk list");
        stop(s, 1);
        return usize::MAX;
    };
    let mut arrived = 0;
    for k in 0..n {
        let frame = {
            let _recv = ctx.meter.start(Activity::Recv);
            read_msg::<DataChunk, _>(s).await
        };
        let frame = match frame {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(e) => {
                tracing::debug!(file_id, error = %e, "data stream failed");
                break;
            }
        };
        arrived += 1;
        // The chunk's bytes, raw or rebuilt from operations against the
        // old file (outside the writer lock), with what they cost on the
        // wire. A rebuild that fails leaves the chunk missing.
        let rebuilt = !frame.ops.is_empty();
        let (data, wire) = if rebuilt {
            let ops: io::Result<Vec<Op>> = frame.ops.into_iter().map(Op::from_wire).collect();
            let (ops, wire) = match ops {
                Ok(ops) => {
                    let wire = encoded_len(&ops) as u64;
                    (
                        rebuild(route, first + k, ops, ctx.meter.clone()).await,
                        wire,
                    )
                }
                Err(e) => (Err(e), 0),
            };
            match ops {
                Ok(d) => (Some(d), wire),
                Err(e) => {
                    tracing::debug!(file_id, idx = first + k, error = %e, "rebuilding a chunk failed");
                    (None, wire)
                }
            }
        } else {
            let wire = frame.data.len() as u64;
            (Some(frame.data), wire)
        };
        ctx.progress.store(true, Ordering::Relaxed);
        ctx.activity.fetch_add(1, Ordering::Relaxed);
        ctx.hooks
            .bus
            .emit(super::Event::FileBytes(route.path.clone(), wire));
        ctx.hooks.bus.emit(super::Event::Bytes(wire));
        ctx.meter.add(Counter::BytesLiteral, wire);
        let _total = bytes.fetch_add(wire, Ordering::Relaxed) + wire;
        #[cfg(feature = "test-hooks")]
        if let Some(f) = &ctx.fault {
            f.observe(_total, &ctx.conn);
        }
        let idx = (first + k) as u32;
        let Some(data) = data else {
            settle(route, idx);
            continue;
        };
        if let Some(g) = &route.gate {
            // In place: wait for the chunks before this one.
            if !g.admit(idx).await {
                stop(s, 0);
                return n;
            }
        }
        let mut guard = route.slot.lock().await;
        let Some(w) = guard.as_mut() else {
            // The writer was retired (new chunk list, failure): drop the rest.
            drop(guard);
            stop(s, 0);
            return n;
        };
        match write_into(ctx, route, w, idx, &data, true).await {
            Ok(()) => {
                ctx.hooks.event("chunk", &route.path);
                if rebuilt {
                    ctx.hooks.event("ops", &route.path);
                }
            }
            // Stale or corrupt data never verifies; the chunk stays missing.
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                tracing::debug!(file_id, idx, error = %e, "rejected chunk");
            }
            Err(e) => {
                drop(guard);
                let _ = events.send(Ev::Recv(Done::StreamFailed {
                    file_id,
                    gen: route.gen,
                    msg: e.to_string(),
                }));
                stop(s, 0);
                return n;
            }
        }
        drop(guard);
        settle(route, idx);
    }
    if arrived < n {
        tracing::debug!(
            file_id,
            arrived,
            promised = n,
            "incomplete range; its missing chunks stay missing"
        );
        // In place: the cursor must not wait for chunks that will not come.
        for k in arrived..n {
            settle(route, (first + k) as u32);
        }
    } else {
        // Read to the end so the sender sees a clean finish rather than STOP.
        let _recv = ctx.meter.start(Activity::Recv);
        match read_msg::<DataChunk, _>(s).await {
            Ok(None) => {}
            Ok(Some(_)) => {
                tracing::warn!(file_id, "data stream carries more chunks than its header");
                stop(s, 1);
            }
            Err(e) => tracing::debug!(file_id, error = %e, "data stream end"),
        }
    }
    n
}

pub(super) async fn accept_streams(
    ctx: Arc<Ctx>,
    routes: Routes,
    events: mpsc::UnboundedSender<Ev>,
    bytes: Arc<AtomicU64>,
) {
    while let Ok(s) = ctx.conn.accept_uni().await {
        tokio::spawn(read_stream(
            ctx.clone(),
            routes.clone(),
            events.clone(),
            bytes.clone(),
            s,
        ));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn chunks(lens: &[u32]) -> Vec<ChunkRecord> {
        let mut off = 0;
        lens.iter()
            .map(|&len| {
                let c = ChunkRecord {
                    offset: off,
                    len,
                    hash: [0; 32],
                };
                off += u64::from(len);
                c
            })
            .collect()
    }

    /// A file whose writer was retired (a new chunk list arrived, or it
    /// failed) before its bases went out: none is signed or sent, and no
    /// request either (nothing is held, so nothing needs dropping).
    #[tokio::test]
    async fn a_retired_writer_offers_no_bases() {
        use irsync_proto::v1::Control;
        let t = super::super::engine::test_ctx().await;
        let ctx = t.ctx.clone();
        const LEN: u32 = 64 * 1024;
        let path = ctx.root.join("old");
        let bytes: Vec<u8> = (0..4 * LEN).map(|i| (i * 7919 % 251) as u8).collect();
        std::fs::write(&path, &bytes).unwrap();
        let old = Arc::new(File::open(&path).unwrap());
        let (send, _) = ctx.conn.open_bi().await.unwrap();
        let (events, _events) = mpsc::unbounded_channel();
        let (out, writer) =
            super::super::control::spawn_writer(send, events, &ctx.hooks, usize::MAX);
        let route = Route {
            gen: 1,
            slot: Arc::new(tokio::sync::Mutex::new(None)),
            digest: Arc::new(FileDigest {
                size: bytes.len() as u64,
                hash: [0; 32],
                chunks: chunks(&[LEN; 4]),
            }),
            pending: Arc::default(),
            path: RelPath::new(b"old").unwrap(),
            old: Some(old.clone()),
            gate: None,
            idx_base: 0,
        };
        let specs = (0..4).map(|i| (i, u64::from(i * LEN), LEN)).collect();
        let budget = Arc::new(ByteBudget::new(1 << 20));
        let read = async {
            let (_, mut recv) = t._peer.accept_bi().await.unwrap();
            let mut frames = Vec::new();
            while let Some(c) = read_msg::<Control, _>(&mut recv).await.unwrap() {
                frames.extend(c.msg);
            }
            frames
        };
        let offer = async {
            offer_bases(
                ctx.clone(),
                out.clone(),
                route,
                old,
                specs,
                vec![0, 1, 2, 3],
                9,
                budget,
            )
            .await;
            out.finish();
            drop(out);
            writer.await.unwrap();
        };
        let (frames, ()) = tokio::join!(read, offer);
        let bases = frames
            .iter()
            .filter(|m| matches!(m, Msg::ChunkBasis(_)))
            .count();
        assert_eq!(bases, 0, "bases signed for a retired writer");
        assert!(frames.is_empty(), "{} frames sent", frames.len());
    }

    #[test]
    fn bases_cover_the_neighbourhood_of_a_missing_chunk_inside_the_old_file() {
        let c = chunks(&[100_000; 10]);
        let old = 1_000_000;
        // Interior: one chunk before to two after.
        assert_eq!(basis_specs(&c, &[4], old, false), [(4, 300_000, 300_000)]);
        // In place: only from the chunk's own offset on.
        assert_eq!(basis_specs(&c, &[4], old, true), [(4, 400_000, 200_000)]);
        // Clamped at the start and at the old file's end.
        assert_eq!(basis_specs(&c, &[0], old, false), [(0, 0, 200_000)]);
        assert_eq!(basis_specs(&c, &[9], old, false), [(9, 800_000, 200_000)]);
        // A chunk starting at or past the old end (appended data) gets none.
        assert_eq!(
            basis_specs(&c, &[8, 9], 850_000, false),
            [(8, 700_000, 150_000)]
        );
        assert_eq!(basis_specs(&c, &[8, 9], 800_000, false), []);
        // A region too small to sign is not offered.
        assert_eq!(basis_specs(&c, &[0], 600, false), []);
    }

    #[test]
    fn bases_are_withheld_for_a_rewrite_and_capped_at_one_pass_over_the_old_file() {
        let c = chunks(&[100_000; 10]);
        // More than half missing: a rewrite, unless the file is tiny.
        assert_eq!(basis_specs(&c, &[0, 1, 2, 3, 4, 5], 1_000_000, false), []);
        assert_eq!(basis_specs(&c, &[0, 1, 2, 3, 4], 1_000_000, false).len(), 5);
        // A tiny file is covered whole even where every chunk is missing.
        let two = chunks(&[100_000; 2]);
        assert_eq!(basis_specs(&two, &[0, 1], 200_000, false).len(), 2);
        let one = chunks(&[100_000]);
        assert_eq!(basis_specs(&one, &[0], 100_000, false), [(0, 0, 100_000)]);
        // Regions add up to at most the old file's size (once it is larger
        // than a few chunks): 200,000 + 12 * 300,000 fit 4,000,000 bytes.
        let c = chunks(&[100_000; 40]);
        let remote: Vec<u32> = (0..20).collect();
        let specs = basis_specs(&c, &remote, 4_000_000, false);
        let total: u64 = specs.iter().map(|s| u64::from(s.2)).sum();
        assert!(total <= 4_000_000, "{specs:?}");
        assert_eq!(specs.len(), 13, "{specs:?}");
    }

    #[tokio::test]
    async fn gate_admits_in_order_and_skips_settled_chunks() {
        // Chunks 0 and 2 are already there; 1, 3, 4 are to come.
        let g = Arc::new(Gate::new(&[1, 3, 4], 5));
        assert!(g.admit(1).await);
        assert!(g.admit(0).await); // below the cursor: any time
        let g2 = g.clone();
        let waiter = tokio::spawn(async move { g2.admit(3).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished(), "3 admitted before 1 was settled");
        g.settle(1);
        assert!(waiter.await.unwrap());
        // 2 was settled from the start: the cursor is at 3, then 4.
        g.settle(4); // out of order: noted, cursor stays
        let g2 = g.clone();
        let waiter = tokio::spawn(async move { g2.admit(4).await });
        g.settle(3);
        assert!(waiter.await.unwrap());
    }

    #[tokio::test]
    async fn a_closed_gate_releases_its_waiters_with_false() {
        let g = Arc::new(Gate::new(&[0, 1], 2));
        let g2 = g.clone();
        let waiter = tokio::spawn(async move { g2.admit(1).await });
        g.close();
        assert!(!waiter.await.unwrap());
        assert!(!g.admit(0).await);
    }

    #[test]
    fn range_len_requires_exact_chunk_boundaries() {
        let c = chunks(&[10, 20, 30]);
        assert_eq!(range_len(&c, 0, 0, 60), Some(3));
        assert_eq!(range_len(&c, 1, 10, 20), Some(1));
        assert_eq!(range_len(&c, 1, 10, 50), Some(2));
        // Wrong offset, a length ending mid-chunk, past the end, empty.
        assert_eq!(range_len(&c, 1, 0, 20), None);
        assert_eq!(range_len(&c, 0, 0, 15), None);
        assert_eq!(range_len(&c, 0, 0, 61), None);
        assert_eq!(range_len(&c, 3, 60, 1), None);
        assert_eq!(range_len(&c, 0, 0, 0), None);
    }
}
