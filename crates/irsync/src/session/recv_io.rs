//! The receiver role's file and data-stream I/O: opening `.part` writers
//! (with local chunk reuse), finalizing them, and routing each incoming data
//! stream into the current writer of its file. A request whose streams have
//! all ended with chunks still missing is reported back as `Drained`.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use iroh::endpoint::{RecvStream, VarInt};
use irsync_proto::chunk::{ChunkRecord, FileDigest};
use irsync_proto::v1::{
    file_entry::Kind, plan_item::Action, ChunkList, DataChunk, DataStreamHeader, FileEntry, Hash,
};
use tokio::sync::mpsc;

use super::control::Ev;
use super::engine::{blocking, Ctx};
use super::receiver::Done;
use crate::fs::meta::lstat_entry_with_ctime;
use crate::fs::paths::RelPath;
use crate::net::frame::read_msg;
use crate::recv::{FileWriter, FinalizeError};
use crate::scan::ScanItem;

pub(super) type Slot = Arc<tokio::sync::Mutex<Option<FileWriter>>>;

/// Where a data stream for a file id goes: the current writer generation.
#[derive(Clone)]
pub(super) struct Route {
    pub gen: u32,
    pub slot: Slot,
    pub chunks: Arc<Vec<ChunkRecord>>,
    /// Requested chunks whose data streams have not ended yet.
    pub pending: Arc<AtomicUsize>,
}

pub(super) type Routes = Arc<std::sync::Mutex<HashMap<u64, Route>>>;
pub(super) struct Opened {
    pub writer: FileWriter,
    pub missing: Vec<u32>,
    pub digest: FileDigest,
}

fn is_regular(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file())
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
pub(super) async fn open_writer(
    ctx: Arc<Ctx>,
    path: RelPath,
    action: Action,
    local_copy: Option<RelPath>,
    cl: ChunkList,
    sparse: bool,
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
    w.set_sparse(sparse);

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
pub(super) async fn finalize(
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
    let before = route
        .pending
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |p| {
            Some(p.saturating_sub(accounted))
        })
        .unwrap_or(0);
    let drained = before <= accounted;
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
