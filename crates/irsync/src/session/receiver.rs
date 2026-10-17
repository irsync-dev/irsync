//! The receiver role: walks its own tree, merge-joins it with the sender's
//! listing and decides per path (rsync-generator style). Directories,
//! symlinks, specials and metadata are applied locally; regular files are
//! requested with `PlanItem`, opened as verified `.part` writers when their
//! `ChunkList` arrives, filled from local copies and data streams, and
//! acknowledged only after `finalize` has verified and renamed them.

use std::collections::HashMap;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::bail;
use iroh::endpoint::{RecvStream, VarInt};
use irsync_proto::chunk::{ChunkRecord, FileDigest};
use irsync_proto::v1::{
    control::Msg, file_ack::Status, file_entry::Kind, plan_item::Action, ChunkList, DataChunk,
    DataStreamHeader, FileAck, FileEntry, Hash, NeedChunks, PathError, PlanEntry, SessionDone,
};
use tokio::sync::mpsc;

use super::control::{spawn_listing, walk_errors, Ev, ListingSpec, Out, Persister};
use super::engine::{blocking, Ctx, Events};
use super::planner::{plan, Planned, PlannerReport};
use super::Outcome;
use crate::filter::FilterSet;
use crate::fs::meta::{apply_metadata, lstat_entry_with_ctime};
use crate::fs::paths::RelPath;
use crate::net::frame::read_msg;
use crate::plan::Side;
use crate::recv::{FileWriter, FinalizeError};
use crate::scan::ScanItem;

/// How long to wait for the sender to close its end after `SessionDone`.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);
/// A file is given up after this many whole-file hash mismatches.
const MAX_MISMATCHES: u32 = 2;

/// Completions of the receiver's tasks, delivered to its main loop.
pub(super) enum Done {
    Planned(Box<Planned>),
    PlannerFinished(Box<PlannerReport>),
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
}

pub(super) struct Opened {
    writer: FileWriter,
    missing: Vec<u32>,
    digest: FileDigest,
}

type Slot = Arc<tokio::sync::Mutex<Option<FileWriter>>>;

/// Where a data stream for a file id goes: the current writer generation.
#[derive(Clone)]
struct Route {
    gen: u32,
    slot: Slot,
    chunks: Arc<Vec<ChunkRecord>>,
}

type Routes = Arc<std::sync::Mutex<HashMap<u64, Route>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum St {
    /// `PlanItem` sent (or a retry requested); waiting for a `ChunkList`.
    Requested,
    Opening,
    Receiving,
    Finalizing,
}

struct RFile {
    path: RelPath,
    entry: FileEntry,
    action: Action,
    local_copy: Option<RelPath>,
    /// Bumped for every new `ChunkList` and on failure; stale task results
    /// and data streams carry an older value and are ignored.
    gen: u32,
    mismatches: u32,
    st: St,
    /// A `ChunkList` that arrived while the `.part` was busy (opening or finalizing).
    pending: Option<ChunkList>,
    /// The sender gave up on this file while we were finalizing it.
    abandoned: Option<String>,
    /// The current chunk list as native records (set once opened).
    digest: Option<FileDigest>,
}

fn is_regular(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file())
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
/// remaining chunks missing, so the file can never complete from it.
async fn read_stream(
    ctx: Arc<Ctx>,
    routes: Routes,
    events: mpsc::UnboundedSender<Ev>,
    bytes: Arc<AtomicU64>,
    mut s: RecvStream,
) {
    let stop = |s: &mut RecvStream, code: u32| {
        let _ = s.stop(VarInt::from_u32(code));
    };
    let header: DataStreamHeader = match read_msg(&mut s).await {
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
    let first = header.first_chunk_idx as usize;
    let Some(n) = range_len(&route.chunks, first, header.offset, header.length) else {
        tracing::warn!(file_id, "data stream range does not match the chunk list");
        return stop(&mut s, 1);
    };
    let mut arrived = 0;
    for k in 0..n {
        let data = match read_msg::<DataChunk, _>(&mut s).await {
            Ok(Some(c)) => c.data,
            Ok(None) => break,
            Err(e) => {
                tracing::debug!(file_id, error = %e, "data stream failed");
                break;
            }
        };
        arrived += 1;
        bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
        let mut guard = route.slot.lock().await;
        let Some(w) = guard.as_mut() else {
            // The writer was retired (new chunk list, failure): drop the rest.
            drop(guard);
            return stop(&mut s, 0);
        };
        match w.write_chunk((first + k) as u32, &data).await {
            Ok(()) => {}
            // Stale or corrupt data never verifies; the chunk stays missing.
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                tracing::debug!(file_id, idx = first + k, error = %e, "rejected chunk");
            }
            Err(e) => {
                drop(guard);
                let _ = events.send(Ev::Recv(Done::StreamFailed {
                    file_id,
                    gen: route.gen,
                    msg: e.to_string(),
                }));
                return stop(&mut s, 0);
            }
        }
    }
    if arrived < n {
        tracing::debug!(
            file_id,
            arrived,
            promised = n,
            "incomplete range; its missing chunks stay missing"
        );
    } else {
        // Read to the end so the sender sees a clean finish rather than STOP.
        match read_msg::<DataChunk, _>(&mut s).await {
            Ok(None) => {}
            Ok(Some(_)) => {
                tracing::warn!(file_id, "data stream carries more chunks than its header");
                stop(&mut s, 1);
            }
            Err(e) => tracing::debug!(file_id, error = %e, "data stream end"),
        }
    }
    let mut guard = route.slot.lock().await;
    if guard
        .as_ref()
        .is_some_and(|w| w.missing_chunks().is_empty())
    {
        if let Some(writer) = guard.take() {
            let _ = events.send(Ev::Recv(Done::Ready {
                file_id,
                gen: route.gen,
                writer,
            }));
        }
    }
}

async fn accept_streams(
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

fn native_digest(cl: &ChunkList) -> io::Result<FileDigest> {
    let hash: [u8; 32] = cl
        .whole_hash
        .as_ref()
        .and_then(|h| h.bytes.clone().try_into().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "chunk list without hash"))?;
    let chunks = cl
        .chunks
        .iter()
        .map(|c| ChunkRecord::try_from(c.clone()))
        .collect::<io::Result<Vec<_>>>()?;
    Ok(FileDigest {
        size: cl.size,
        hash,
        chunks,
    })
}

/// Copies chunk `idx` from a local file if it is still a regular file there.
/// Failures only mean the chunk has to come over the network.
async fn try_copy(w: &mut FileWriter, idx: u32, from: PathBuf, offset: u64) {
    let probe = from.clone();
    if !blocking(move || Ok(is_regular(&probe)))
        .await
        .unwrap_or(false)
    {
        return;
    }
    if let Err(e) = w.copy_local(idx, &from, offset).await {
        tracing::debug!(from = ?from, idx, error = %e, "local copy failed");
    }
}

/// Opens the `.part` writer (adopting earlier progress), then fills what it
/// can locally: from the rename-detection source, then any indexed file that
/// holds a chunk with the same hash. Returns the chunks still missing.
async fn open_writer(
    ctx: Arc<Ctx>,
    path: RelPath,
    action: Action,
    local_copy: Option<RelPath>,
    cl: ChunkList,
) -> io::Result<Opened> {
    let digest = native_digest(&cl)?;
    // The writer works on index keys under the index root, so its partial
    // record is keyed like every other index row; the file lands at the same place.
    let key = ctx.key(&path);
    let (base, rel, index, pd) = (
        ctx.base.clone(),
        key.clone(),
        ctx.index().clone(),
        ctx.opts.partial_dir.clone(),
    );
    let mut w = blocking(move || FileWriter::open(&base, &rel, &cl, pd.as_deref(), index)).await?;

    if action == Action::Delta {
        // Make sure the old file's chunks are indexed (cached if fresh).
        let (base, rel) = (ctx.base.clone(), key.clone());
        if let Ok((entry, ctime)) = blocking(move || lstat_entry_with_ctime(&base, &rel)).await {
            if entry.kind() == Kind::Regular {
                let item = ScanItem {
                    path: key.clone(),
                    entry,
                    ctime,
                };
                if let Err(e) = ctx.hasher.digest(&item).await {
                    tracing::debug!(?path, error = %e, "hashing old file for delta failed");
                }
            }
        }
    }
    if let Some(from) = &local_copy {
        let from = ctx.full_key(from);
        for idx in w.missing_chunks() {
            let offset = digest.chunks[idx as usize].offset;
            try_copy(&mut w, idx, from.clone(), offset).await;
        }
    }
    let wanted: Vec<(u32, [u8; 32])> = w
        .missing_chunks()
        .into_iter()
        .map(|i| (i, digest.chunks[i as usize].hash))
        .collect();
    if !wanted.is_empty() {
        let index = ctx.index().clone();
        let found = blocking(move || {
            let ix = index
                .lock()
                .map_err(|_| io::Error::other("index poisoned"))?;
            let mut v = Vec::new();
            for (i, h) in wanted {
                if let Some((p, off)) = ix.find_chunk(&h).map_err(io::Error::other)? {
                    v.push((i, p, off));
                }
            }
            Ok(v)
        })
        .await;
        match found {
            Ok(found) => {
                for (idx, p, off) in found {
                    try_copy(&mut w, idx, ctx.full_key(&p), off).await;
                }
            }
            Err(e) => tracing::warn!(?path, error = %e, "chunk lookup failed"),
        }
    }
    Ok(Opened {
        missing: w.missing_chunks(),
        writer: w,
        digest,
    })
}

/// Verifies and places the file, then records it (with the digest the sender
/// gave) in the index so later sessions can quick-check and reuse it.
async fn finalize(
    ctx: Arc<Ctx>,
    w: FileWriter,
    path: RelPath,
    entry: FileEntry,
    digest: FileDigest,
) -> Result<(), FinalizeError> {
    let apply = ctx.apply_opts(&entry);
    w.finalize(&entry, &apply).await?;
    let (base, key, index) = (ctx.base.clone(), ctx.key(&path), ctx.index().clone());
    let rec = blocking(move || {
        let (mut e, ctime) = lstat_entry_with_ctime(&base, &key)?;
        if e.size != digest.size {
            return Ok(());
        }
        e.whole_hash = Some(Hash {
            bytes: digest.hash.to_vec(),
        });
        index
            .lock()
            .map_err(|_| io::Error::other("index poisoned"))?
            .put_entry(&key, &e, ctime, Some(&digest))
            .map_err(io::Error::other)
    })
    .await;
    if let Err(e) = rec {
        tracing::warn!(error = %e, "recording received file in the index failed");
    }
    Ok(())
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
    files_done: u64,
    errors: Vec<(RelPath, String)>,
    persist: Persister,
}

impl Receiver {
    fn complete(&self) -> bool {
        self.report.is_some() && self.peer_listing_done && self.active == 0
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
        self.errors.push((f.path, msg.clone()));
        if let Some(status) = ack {
            self.ack(file_id, status, msg);
        }
    }

    async fn on_chunk_list(&mut self, cl: ChunkList) {
        let file_id = cl.file_id;
        let Some(f) = self.files.get_mut(&file_id) else {
            tracing::debug!(file_id, "chunk list for a file not in flight");
            return;
        };
        match f.st {
            St::Opening | St::Finalizing => {
                f.pending = Some(cl);
                return;
            }
            St::Requested | St::Receiving => {}
        }
        f.gen += 1;
        f.st = St::Opening;
        let (gen, path, action, local_copy) =
            (f.gen, f.path.clone(), f.action, f.local_copy.clone());
        // A new chunk list replaces the previous writer; it must be gone
        // before the next one opens the same `.part`.
        self.retire_writer(file_id).await;
        let (ctx, events) = (self.ctx.clone(), self.events.clone());
        tokio::spawn(async move {
            let result = open_writer(ctx, path, action, local_copy, cl).await;
            let _ = events.send(Ev::Recv(Done::Opened {
                file_id,
                gen,
                result,
            }));
        });
    }

    fn spawn_finalize(&self, file_id: u64, gen: u32, w: FileWriter, digest: FileDigest) {
        let f = &self.files[&file_id];
        let (ctx, events, path, entry) = (
            self.ctx.clone(),
            self.events.clone(),
            f.path.clone(),
            f.entry.clone(),
        );
        tokio::spawn(async move {
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
            Done::Planned(p) => {
                self.persist.plan(PlanEntry {
                    file_id: p.file_id,
                    path: p.path.as_bytes().to_vec(),
                    action: p.action as i32,
                    size: p.entry.size,
                    whole_hash: p.entry.whole_hash.clone(),
                });
                self.active += 1;
                self.files.insert(
                    p.file_id,
                    RFile {
                        path: p.path,
                        entry: p.entry,
                        action: p.action,
                        local_copy: p.local_copy,
                        gen: 0,
                        mismatches: 0,
                        st: St::Requested,
                        pending: None,
                        abandoned: None,
                        digest: None,
                    },
                );
            }
            Done::PlannerFinished(r) => self.report = Some(*r),
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
            self.on_chunk_list(cl).await;
            return;
        }
        let o = match result {
            Ok(o) => o,
            Err(e) => {
                let msg = format!("opening partial file: {e}");
                self.fail(file_id, Some(Status::WriteError), msg).await;
                return;
            }
        };
        f.digest = Some(o.digest.clone());
        let missing = o.missing.clone();
        if missing.is_empty() {
            f.st = St::Finalizing;
            self.spawn_finalize(file_id, gen, o.writer, o.digest);
        } else {
            f.st = St::Receiving;
            // Routed before the request goes out, so no data stream can miss it.
            self.routes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    file_id,
                    Route {
                        gen,
                        slot: Arc::new(tokio::sync::Mutex::new(Some(o.writer))),
                        chunks: Arc::new(o.digest.chunks),
                    },
                );
        }
        // Empty means "nothing needed": everything was local.
        self.out.send(Msg::Need(NeedChunks {
            file_id,
            chunk_idx: missing,
        }));
    }

    async fn on_finalized(&mut self, file_id: u64, gen: u32, result: Result<(), FinalizeError>) {
        let Some(f) = self.files.get_mut(&file_id) else {
            return;
        };
        if f.gen != gen || f.st != St::Finalizing {
            return;
        }
        let abandoned = f.abandoned.take();
        match result {
            Ok(()) => {
                // Placed: acknowledged DONE even if the sender gave up meanwhile
                // (it withdraws its error on this ack).
                self.files.remove(&file_id);
                self.active -= 1;
                self.files_done += 1;
                self.persist.done(file_id);
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
                    self.on_chunk_list(cl).await;
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
            Ev::Ctrl(Msg::ChunkList(cl)) => self.on_chunk_list(cl).await,
            Ev::Ctrl(Msg::Ack(a)) if a.status == Status::SourceError as i32 => {
                self.on_source_error(a.file_id, a.message).await
            }
            Ev::Ctrl(Msg::ListingEnd(e)) => {
                if e.incomplete {
                    tracing::warn!("the sender's listing is incomplete");
                }
                self.peer_listing_incomplete = e.incomplete;
                self.peer_listing_done = true;
            }
            Ev::Ctrl(Msg::Error(e)) => bail!("peer error: {}", e.message),
            Ev::Ctrl(m) => bail!(
                "protocol error: unexpected {} from the sender",
                msg_name(&m)
            ),
            Ev::PeerEof => bail!("the sender closed the control stream before the session ended"),
            Ev::PeerFailed(e) => bail!("control stream: {e}"),
            Ev::WriterFailed(e) => bail!("control stream: {e}"),
            Ev::Send(_) => {}
        }
        Ok(())
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

pub(super) fn msg_name(m: &Msg) -> &'static str {
    match m {
        Msg::Listing(_) => "ListingEntry",
        Msg::ListingEnd(_) => "ListingEnd",
        Msg::Plan(_) => "PlanItem",
        Msg::ChunkList(_) => "ChunkList",
        Msg::Need(_) => "NeedChunks",
        Msg::Ack(_) => "FileAck",
        Msg::Done(_) => "SessionDone",
        Msg::Error(_) => "Error",
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
pub(super) async fn run(
    ctx: Arc<Ctx>,
    out: Out,
    events_tx: mpsc::UnboundedSender<Ev>,
    mut events: Events,
    peer_listing: mpsc::UnboundedReceiver<Side>,
) -> anyhow::Result<Outcome> {
    let hashed_before = ctx.files_hashed();
    let persist = Persister::start(ctx.index().clone(), ctx.session_id, ctx.key.clone()).await;
    let (local_tx, local_rx) = mpsc::unbounded_channel();
    let listing = spawn_listing(
        ListingSpec {
            root: ctx.root.clone(),
            // The receiver lists everything it has; filters shape the sender's walk.
            filters: Arc::new(FilterSet::new(vec![])),
            hasher: ctx.hasher.clone(),
            scope: ctx.scope.clone(),
            skip: partial_dir_rel(ctx.opts.partial_dir.as_deref()),
        },
        out.clone(),
        Some(local_tx),
        None,
    );
    let planner = tokio::spawn(plan(
        ctx.clone(),
        out.clone(),
        events_tx.clone(),
        peer_listing,
        local_rx,
    ));
    let planner_abort = planner.abort_handle();
    {
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
        events: events_tx,
        routes,
        files: HashMap::new(),
        active: 0,
        report: None,
        peer_listing_done: false,
        peer_listing_incomplete: false,
        files_done: 0,
        errors: Vec::new(),
        persist,
    };
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    let res: anyhow::Result<()> = loop {
        if r.complete() {
            break Ok(());
        }
        tokio::select! {
            ev = events.recv() => match ev {
                Some(ev) => if let Err(e) = r.handle(ev).await { break Err(e) },
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
        planner_abort.abort();
        listing.abort();
        r.persist.bytes(bytes.load(Ordering::Relaxed));
        r.persist.flush().await;
        return Err(e);
    }
    let mut report = r.report.take().unwrap_or_default();
    r.directory_pass(&mut report).await;
    let bytes_total = bytes.load(Ordering::Relaxed);
    r.persist.bytes(bytes_total);
    r.persist.flush().await;
    // Every problem this side recorded: its own walk, the planner (mkdir,
    // symlink, special, metadata, refused paths), files and directories.
    // The peer folds them into its outcome, so a push reports them too.
    let mut errors = match listing.await {
        Ok(summary) => walk_errors(&summary),
        Err(e) => vec![(RelPath::root(), format!("listing failed: {e}"))],
    };
    errors.append(&mut report.errors);
    errors.append(&mut r.errors);
    out.send(Msg::Done(SessionDone {
        files: r.files_done,
        bytes: bytes_total,
        errors: errors
            .iter()
            .map(|(p, m)| PathError {
                path: p.as_bytes().to_vec(),
                message: m.clone(),
            })
            .collect(),
    }));
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
        files_transferred: r.files_done,
        bytes_transferred: bytes_total,
        files_skipped: report.skipped,
        files_hashed: ctx.files_hashed() - hashed_before,
        errors,
        #[cfg(feature = "test-hooks")]
        retained_files: r.files.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
