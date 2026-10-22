//! The receiver role's file and data-stream I/O: opening `.part` writers,
//! deciding which chunks the receiver's own files can supply, copying those
//! while the sender delivers the rest, finalizing, and routing each incoming
//! data stream into the current writer of its file. A request whose streams
//! and local copies have all ended with chunks still missing is reported
//! back as `Drained`.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use iroh::endpoint::{RecvStream, VarInt};
use irsync_proto::chunk::{ChunkRecord, FileDigest};
use irsync_proto::v1::{
    file_entry::Kind, plan_item::Action, ChunkList, DataChunk, DataStreamHeader, FileEntry, Hash,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::control::Ev;
use super::engine::{blocking, Ctx};
use super::receiver::Done;
use crate::fs::meta::{apply_metadata_fd, lstat_entry_with_ctime};
use crate::fs::paths::RelPath;
use crate::net::frame::read_msg;
use crate::recv::{discard_part, FileWriter, FinalizeError};
use crate::scan::ScanItem;

pub(super) type Slot = Arc<tokio::sync::Mutex<Option<FileWriter>>>;

/// Where a data stream for a file id goes: the current writer generation.
#[derive(Clone)]
pub(super) struct Route {
    pub gen: u32,
    pub slot: Slot,
    pub chunks: Arc<Vec<ChunkRecord>>,
    /// Units of delivery still running: one per requested chunk whose data
    /// stream has not ended, plus one for the local-copy task while it runs.
    pub pending: Arc<AtomicUsize>,
    /// The file's session path (events and logs).
    pub path: RelPath,
}

pub(super) type Routes = Arc<std::sync::Mutex<HashMap<u64, Route>>>;

/// A chunk one of the receiver's own files holds: its index in the chunk
/// list, the file (absolute) and the chunk's offset in that file.
pub(super) type LocalChunk = (u32, PathBuf, u64);

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
    Identical { file: File, digest: FileDigest },
}

pub(super) struct Fill {
    pub writer: FileWriter,
    /// Chunks with no local source: to be requested from the sender.
    pub remote: Vec<u32>,
    /// Chunks an indexed file holds: copied while the sender delivers the rest.
    pub local: Vec<LocalChunk>,
    pub digest: FileDigest,
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

/// Makes sure the destination's current file at `path` has its chunks in
/// the index (a cache hit if it is fresh there, a read otherwise), so a
/// `DELTA` can take them from it. Failures only mean nothing local.
/// Hashes the old destination file (from the index when its row is fresh)
/// so its chunks can be found and its whole-file hash compared.
pub(super) async fn index_old_file(ctx: &Ctx, path: &RelPath) -> Option<OldFile> {
    let key = ctx.key(path);
    let (base, rel) = (ctx.base.clone(), key.clone());
    let (entry, ctime) = blocking(move || lstat_entry_with_ctime(&base, &rel))
        .await
        .ok()?;
    if entry.kind() != Kind::Regular {
        return None;
    }
    let item = ScanItem {
        path: key,
        entry: entry.clone(),
        ctime,
    };
    match ctx.hasher.digest(&item).await {
        Ok(digest) => Some(OldFile {
            digest,
            entry,
            ctime,
        }),
        Err(e) => {
            tracing::debug!(?path, error = %e, "hashing old file for delta failed");
            None
        }
    }
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
    cl: ChunkList,
    sparse: bool,
) -> io::Result<Opened> {
    let digest = native_digest(&cl)?;
    // The writer works on index keys under the index root, so its partial
    // record is keyed like every other index row; the file lands at the same place.
    let key = ctx.key(&path);
    if action == Action::Delta {
        let old = match prehash {
            Some(h) => h.await.ok().flatten(),
            None => index_old_file(&ctx, &path).await,
        };
        let same = old.filter(|o| o.digest.size == digest.size && o.digest.hash == digest.hash);
        if let Some(old) = same {
            #[cfg(feature = "test-hooks")]
            if let Some(h) = &ctx.hooks.before_identical_check {
                h(&path).await;
            }
            if let Some(file) = unchanged_file(&ctx, &key, &old).await {
                return Ok(Opened::Identical { file, digest });
            }
            tracing::debug!(?path, "destination changed since it was hashed");
        }
    }
    let (base, rel, index, pd, activity) = (
        ctx.base.clone(),
        key.clone(),
        ctx.index().clone(),
        ctx.opts.partial_dir.clone(),
        ctx.activity.clone(),
    );
    let mut w = blocking(move || {
        FileWriter::open_tracked(&base, &rel, &cl, pd.as_deref(), index, activity)
    })
    .await?;
    w.set_sparse(sparse);
    let wanted: Vec<(u32, [u8; 32])> = w
        .missing_chunks()
        .into_iter()
        .map(|i| (i, digest.chunks[i as usize].hash))
        .collect();
    let (mut remote, mut local) = (Vec::new(), Vec::new());
    if !wanted.is_empty() {
        let index = ctx.index().clone();
        let found = blocking(move || {
            let ix = index
                .lock()
                .map_err(|_| io::Error::other("index poisoned"))?;
            let mut v = Vec::with_capacity(wanted.len());
            for (i, h) in wanted {
                v.push((i, ix.find_chunk(&h).map_err(io::Error::other)?));
            }
            Ok(v)
        })
        .await;
        match found {
            Ok(found) => {
                for (idx, hit) in found {
                    match hit {
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
    }
    Ok(Opened::Writer(Box::new(Fill {
        writer: w,
        remote,
        local,
        digest,
    })))
}

/// Settles a `DELTA` file whose destination already holds the content:
/// applies the sender's metadata to it in place, discards a leftover
/// `.part`, and records it in the index with the digest the sender gave.
pub(super) async fn settle_identical(
    ctx: Arc<Ctx>,
    file: File,
    path: RelPath,
    entry: FileEntry,
    digest: FileDigest,
) -> Result<(), FinalizeError> {
    let apply = ctx.apply_opts(&entry);
    let (base, key, index, pd) = (
        ctx.base.clone(),
        ctx.key(&path),
        ctx.index().clone(),
        ctx.opts.partial_dir.clone(),
    );
    blocking(move || {
        if let Err(e) = discard_part(&base, &key, pd.as_deref(), &index) {
            tracing::warn!(?key, error = %e, "discarding a stale partial file failed");
        }
        apply_metadata_fd(&file, &entry, &apply)
    })
    .await?;
    record(&ctx, &path, &digest).await;
    Ok(())
}

/// Reads `len` bytes at `offset` of `from` (a regular file, never a symlink).
async fn read_local(from: PathBuf, offset: u64, len: u32) -> io::Result<Vec<u8>> {
    blocking(move || {
        let mut buf = vec![0u8; len as usize];
        crate::fs::safe::open_regular_nofollow(&from)?.read_exact_at(&mut buf, offset)?;
        Ok(buf)
    })
    .await
}

/// Fills `local` chunks from the receiver's own files while the sender
/// delivers the rest. Each copy is verified like a network chunk; one that
/// no longer matches stays missing and is re-requested once every unit of
/// the request has ended. Counts as one unit of the file's request.
pub(super) async fn copy_local_chunks(
    ctx: Arc<Ctx>,
    route: Route,
    local: Vec<LocalChunk>,
    events: mpsc::UnboundedSender<Ev>,
    file_id: u64,
) {
    for (idx, from, offset) in local {
        #[cfg(feature = "test-hooks")]
        if let Some(d) = ctx.hooks.slow_local_copy {
            tokio::time::sleep(d).await;
        }
        let Some(len) = route.chunks.get(idx as usize).map(|c| c.len) else {
            continue;
        };
        // Read outside the writer lock: network streams keep writing meanwhile.
        let data = match read_local(from.clone(), offset, len).await {
            Ok(d) => d,
            Err(e) => {
                tracing::debug!(from = ?from, idx, error = %e, "local copy failed");
                continue;
            }
        };
        let mut guard = route.slot.lock().await;
        let Some(w) = guard.as_mut() else {
            break; // The writer was retired (new chunk list, failure).
        };
        if let Err(e) = w.write_chunk(idx, &data).await {
            // Stale bytes never verify; any failure leaves the chunk
            // missing for the follow-up request.
            tracing::debug!(from = ?from, idx, error = %e, "local copy rejected");
        }
    }
    ctx.hooks.event("copied", &route.path);
    finish_unit(&route, &events, file_id, 1).await;
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
                writer,
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
    digest: FileDigest,
) -> Result<(), FinalizeError> {
    let apply = ctx.apply_opts(&entry);
    #[cfg(feature = "test-hooks")]
    let mut w = w;
    #[cfg(feature = "test-hooks")]
    if let Some(d) = ctx.hooks.slow_finalize {
        w.slow_finalize(d);
    }
    w.finalize(&entry, &apply).await?;
    record(&ctx, &path, &digest).await;
    Ok(())
}

/// Records the placed file in the index with the digest the sender gave, so
/// later sessions can quick-check and reuse it.
async fn record(ctx: &Ctx, path: &RelPath, digest: &FileDigest) {
    let (base, key, index, digest) = (
        ctx.base.clone(),
        ctx.key(path),
        ctx.index().clone(),
        digest.clone(),
    );
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
    let first = header.first_chunk_idx as usize;
    let Some(n) = range_len(&route.chunks, first, header.offset, header.length) else {
        tracing::warn!(file_id, "data stream range does not match the chunk list");
        stop(s, 1);
        return usize::MAX;
    };
    let mut arrived = 0;
    for k in 0..n {
        let data = match read_msg::<DataChunk, _>(s).await {
            Ok(Some(c)) => c.data,
            Ok(None) => break,
            Err(e) => {
                tracing::debug!(file_id, error = %e, "data stream failed");
                break;
            }
        };
        arrived += 1;
        ctx.progress.store(true, Ordering::Relaxed);
        ctx.activity.fetch_add(1, Ordering::Relaxed);
        ctx.hooks.bus.emit(super::Event::Bytes(data.len() as u64));
        let _total = bytes.fetch_add(data.len() as u64, Ordering::Relaxed) + data.len() as u64;
        #[cfg(feature = "test-hooks")]
        if let Some(f) = &ctx.fault {
            f.observe(_total, &ctx.conn);
        }
        let mut guard = route.slot.lock().await;
        let Some(w) = guard.as_mut() else {
            // The writer was retired (new chunk list, failure): drop the rest.
            drop(guard);
            stop(s, 0);
            return n;
        };
        match w.write_chunk((first + k) as u32, &data).await {
            Ok(()) => ctx.hooks.event("chunk", &route.path),
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
                stop(s, 0);
                return n;
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
