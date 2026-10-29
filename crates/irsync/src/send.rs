//! Sender data streams: requested chunks of a file go out over parallel
//! unidirectional QUIC streams, one stream per contiguous range.
//!
//! Each stream carries one `DataStreamHeader` frame followed by one
//! `DataChunk` frame per chunk. Chunks are re-hashed as they are read; a
//! mismatch with the digest means the file changed under us and is reported
//! as `InvalidData` instead of sending stale bytes.
//!
//! A chunk the receiver offered a basis for (block signatures over its old
//! file, see [`crate::delta`]) goes out as copy/literal operations when
//! those are clearly smaller than the chunk, as its bytes otherwise.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::endpoint::{Connection, VarInt};
use irsync_proto::chunk::{ChunkRecord, FileDigest};
use irsync_proto::v1::{DataChunk, DataStreamHeader};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::delta::{encode, encoded_len, Basis, Op};
use crate::fs::paths::RelPath;
use crate::hasher::Hasher;
use crate::net::frame::write_msg;
use crate::scan::ScanItem;
use crate::telemetry::meter::{Activity, Counter, Meter};

pub const DEFAULT_PARALLEL: usize = 32;
pub const DEFAULT_SPLIT_THRESHOLD: u64 = 64 * 1024 * 1024;

/// Matches the transport idle timeout: how long to wait for the peer to take a range.
const STOPPED_TIMEOUT: Duration = Duration::from_secs(60);
const MIN_BURST: u64 = 1024 * 1024;

/// Token bucket refilled continuously at `rate` bytes/s. Tokens may go
/// negative: a taker deducts first, then sleeps off its deficit.
struct Bucket {
    rate: f64,
    burst: f64,
    tokens: f64,
    last: Instant,
}

impl Bucket {
    fn new(bytes_per_sec: u64) -> Bucket {
        let rate = bytes_per_sec.max(1);
        Bucket {
            rate: rate as f64,
            burst: rate.max(MIN_BURST) as f64,
            tokens: 0.0,
            last: Instant::now(),
        }
    }

    /// Deducts `n` and returns how long the caller must wait.
    fn take(&mut self, n: u64) -> Duration {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.rate).min(self.burst);
        self.tokens -= n as f64;
        if self.tokens >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-self.tokens / self.rate)
        }
    }
}

/// What the sender reports about a file as it goes.
pub enum SendProgress {
    /// The first data stream for this call opened; the file's size.
    Opened { size: u64 },
    /// Bytes put on the wire.
    Sent(u64),
}

/// A callback told when a file's first stream opens and of bytes sent.
pub type ProgressFn = Arc<dyn Fn(&RelPath, SendProgress) + Send + Sync>;

struct Inner {
    conn: Connection,
    session_id: Vec<u8>,
    root: PathBuf,
    hasher: Arc<Hasher>,
    split_threshold: u64,
    sem: Arc<Semaphore>,
    bucket: Option<Mutex<Bucket>>,
    active: AtomicU32,
    max_active: AtomicU32,
    /// Told of every file's first stream and every chunk put on the wire.
    progress: std::sync::OnceLock<ProgressFn>,
    /// Times the disk reads and stream writes, and counts literal bytes.
    meter: std::sync::OnceLock<Arc<Meter>>,
    #[cfg(feature = "test-hooks")]
    truncate: std::sync::OnceLock<Arc<TruncateHook>>,
}

/// Test hook: finishes a data stream early, after `after_chunks` chunks,
/// as if it had been cut short (once, or every time).
#[cfg(feature = "test-hooks")]
#[derive(Debug)]
pub struct TruncateHook {
    after_chunks: usize,
    always: bool,
    fired: AtomicU32,
}

#[cfg(feature = "test-hooks")]
impl TruncateHook {
    /// Cuts the first stream with more than `k` chunks.
    pub fn once(k: usize) -> Arc<TruncateHook> {
        Arc::new(TruncateHook {
            after_chunks: k,
            always: false,
            fired: AtomicU32::new(0),
        })
    }

    /// Cuts every stream with more than `k` chunks.
    pub fn always(k: usize) -> Arc<TruncateHook> {
        Arc::new(TruncateHook {
            after_chunks: k,
            always: true,
            fired: AtomicU32::new(0),
        })
    }

    /// Streams cut so far.
    pub fn fired(&self) -> u32 {
        self.fired.load(Ordering::SeqCst)
    }

    /// Where to cut a stream of `chunks` chunks, if at all.
    fn cut(&self, chunks: usize) -> Option<usize> {
        if chunks <= self.after_chunks {
            return None;
        }
        let take = if self.always {
            self.fired.fetch_add(1, Ordering::SeqCst);
            true
        } else {
            self.fired
                .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        };
        take.then_some(self.after_chunks)
    }
}

pub struct Sender {
    inner: Arc<Inner>,
}

/// A contiguous run of chunk indices, `first..=last`.
struct Range {
    first: usize,
    last: usize,
}

impl Sender {
    pub fn new(
        conn: Connection,
        session_id: [u8; 16],
        root: PathBuf,
        hasher: Arc<Hasher>,
        parallel: usize,
        split_threshold: u64,
        bwlimit: Option<u64>,
    ) -> Sender {
        Sender {
            inner: Arc::new(Inner {
                conn,
                session_id: session_id.to_vec(),
                root,
                hasher,
                split_threshold,
                sem: Arc::new(Semaphore::new(parallel.max(1))),
                bucket: bwlimit.map(|b| Mutex::new(Bucket::new(b))),
                active: AtomicU32::new(0),
                max_active: AtomicU32::new(0),
                progress: std::sync::OnceLock::new(),
                meter: std::sync::OnceLock::new(),
                #[cfg(feature = "test-hooks")]
                truncate: std::sync::OnceLock::new(),
            }),
        }
    }

    /// The hashing step before a `ChunkList` is sent.
    pub async fn lookahead_digest(&self, item: &ScanItem) -> io::Result<FileDigest> {
        self.inner.hasher.digest(item).await
    }

    /// Installs (once) a callback told when a file's first stream opens and
    /// the length of every chunk sent.
    pub fn set_progress(&self, f: ProgressFn) {
        let _ = self.inner.progress.set(f);
    }

    /// Installs (once) the meter that times reads and stream writes.
    pub fn set_meter(&self, m: Arc<Meter>) {
        let _ = self.inner.meter.set(m);
    }

    /// Installs (once) a hook that cuts data streams short.
    #[cfg(feature = "test-hooks")]
    pub fn set_truncate(&self, hook: Arc<TruncateHook>) {
        let _ = self.inner.truncate.set(hook);
    }

    /// High-water mark of simultaneously open data streams.
    #[cfg(feature = "test-hooks")]
    pub fn max_concurrency(&self) -> u32 {
        self.inner.max_active.load(Ordering::SeqCst)
    }

    /// Sends the chunks `idxs` of `digest` as one stream per contiguous range,
    /// at most `parallel` streams at a time. Returns the bytes put on the
    /// wire, or the first error.
    pub async fn send_chunks(
        &self,
        file_id: u64,
        path: &RelPath,
        digest: &FileDigest,
        idxs: &[u32],
    ) -> io::Result<u64> {
        self.send_chunks_against(file_id, path, digest, idxs, HashMap::new())
            .await
    }

    /// [`Sender::send_chunks`], with the receiver's bases for some of the
    /// chunks (by index): those go as operations against its old file when
    /// that is clearly smaller.
    pub async fn send_chunks_against(
        &self,
        file_id: u64,
        path: &RelPath,
        digest: &FileDigest,
        idxs: &[u32],
        bases: HashMap<u32, Basis>,
    ) -> io::Result<u64> {
        let mut sorted: Vec<usize> = idxs.iter().map(|&i| i as usize).collect();
        sorted.sort_unstable();
        sorted.dedup();
        let name = path.to_os().to_string_lossy().into_owned();
        if let Some(&bad) = sorted.iter().find(|&&i| i >= digest.chunks.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name}: chunk index {bad} out of range"),
            ));
        }
        let ranges = split_ranges(&sorted, digest, self.inner.split_threshold);
        let digest = Arc::new(digest.clone());
        let full = self.inner.root.join(path.to_os());
        let name = Arc::new(name);
        let rel = Arc::new(path.clone());
        let bases = Arc::new(bases);
        let sent = Arc::new(AtomicU64::new(0));

        let cancelled = Arc::new(AtomicBool::new(false));
        let mut tasks = JoinSet::new();
        let mut first_err = None;
        let mut opened = false;
        'sched: for r in ranges {
            let permit = loop {
                tokio::select! {
                    biased;
                    Some(joined) = tasks.join_next(), if !tasks.is_empty() => {
                        if let Err(e) = joined.unwrap_or_else(|e| Err(io::Error::other(e))) {
                            first_err.get_or_insert(e);
                            break 'sched;
                        }
                    }
                    permit = self.inner.sem.clone().acquire_owned() => {
                        break permit.map_err(io::Error::other)?;
                    }
                }
            };
            if cancelled.load(Ordering::SeqCst) {
                break;
            }
            if !opened {
                opened = true;
                if let Some(p) = self.inner.progress.get() {
                    p(path, SendProgress::Opened { size: digest.size });
                }
            }
            let inner = self.inner.clone();
            let (digest, full, name, rel, cancelled, bases, sent) = (
                digest.clone(),
                full.clone(),
                name.clone(),
                rel.clone(),
                cancelled.clone(),
                bases.clone(),
                sent.clone(),
            );
            tasks.spawn(async move {
                let _permit = permit;
                if cancelled.load(Ordering::SeqCst) {
                    return Ok(());
                }
                let now = inner.active.fetch_add(1, Ordering::SeqCst) + 1;
                inner.max_active.fetch_max(now, Ordering::SeqCst);
                let res = inner
                    .send_range(file_id, &full, &name, &rel, &digest, r, &bases, &sent)
                    .await;
                inner.active.fetch_sub(1, Ordering::SeqCst);
                if res.is_err() {
                    cancelled.store(true, Ordering::SeqCst);
                }
                res
            });
        }
        while let Some(joined) = tasks.join_next().await {
            if let Err(e) = joined.unwrap_or_else(|e| Err(io::Error::other(e))) {
                first_err.get_or_insert(e);
                cancelled.store(true, Ordering::SeqCst);
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(sent.load(Ordering::SeqCst)),
        }
    }
}

/// Groups sorted, deduplicated indices into consecutive runs, splitting a run
/// before the range would exceed `threshold` bytes.
fn split_ranges(sorted: &[usize], digest: &FileDigest, threshold: u64) -> Vec<Range> {
    let mut out: Vec<Range> = Vec::new();
    let mut bytes = 0u64;
    for &i in sorted {
        let len = digest.chunks[i].len as u64;
        match out.last_mut() {
            Some(r) if r.last + 1 == i && bytes + len <= threshold => {
                r.last = i;
                bytes += len;
            }
            _ => {
                out.push(Range { first: i, last: i });
                bytes = len;
            }
        }
    }
    out
}

impl Inner {
    async fn throttle(&self, n: u64) {
        if let Some(b) = &self.bucket {
            let wait = b.lock().unwrap_or_else(|e| e.into_inner()).take(n);
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn send_range(
        &self,
        file_id: u64,
        full: &Path,
        name: &str,
        rel: &RelPath,
        digest: &FileDigest,
        r: Range,
        bases: &Arc<HashMap<u32, Basis>>,
        sent: &AtomicU64,
    ) -> io::Result<()> {
        let chunks = &digest.chunks[r.first..=r.last];
        // Never follow a symlink swapped in since the file was listed.
        let file = Arc::new(
            crate::fs::safe::open_regular_nofollow(full)
                .map_err(|e| io::Error::new(e.kind(), format!("{name}: {e}")))?,
        );
        // Verify the first chunk before opening a stream: the common
        // "file changed" case then never touches the wire.
        let first = read_chunk(&file, &chunks[0], name, r.first, self.meter.get()).await?;
        let mut send = self.conn.open_uni().await.map_err(io::Error::other)?;
        let res = self
            .write_range(
                &mut send, file_id, &file, name, rel, digest, &r, first, bases, sent,
            )
            .await;
        if res.is_err() {
            let _ = send.reset(VarInt::from_u32(1));
        }
        res
    }

    #[allow(clippy::too_many_arguments)]
    async fn write_range(
        &self,
        send: &mut iroh::endpoint::SendStream,
        file_id: u64,
        file: &Arc<File>,
        name: &str,
        rel: &RelPath,
        digest: &FileDigest,
        r: &Range,
        first: Vec<u8>,
        bases: &Arc<HashMap<u32, Basis>>,
        sent: &AtomicU64,
    ) -> io::Result<()> {
        let chunks = &digest.chunks[r.first..=r.last];
        let header = DataStreamHeader {
            session_id: self.session_id.clone(),
            file_id,
            offset: chunks[0].offset,
            length: chunks.iter().map(|c| c.len as u64).sum(),
            first_chunk_idx: r.first as u32,
        };
        let meter = self.meter.get().map(|m| &**m);
        let timed = || meter.map(|m| m.start(Activity::Send));
        {
            let _send = timed();
            write_msg(send, &header).await?;
        }
        #[cfg(feature = "test-hooks")]
        let cut = self.truncate.get().and_then(|t| t.cut(chunks.len()));
        let mut next = Some(first);
        for (k, c) in chunks.iter().enumerate() {
            #[cfg(feature = "test-hooks")]
            if cut == Some(k) {
                let _send = timed();
                send.finish().map_err(io::Error::other)?;
                return Ok(());
            }
            let data = match next.take() {
                Some(d) => d,
                None => read_chunk(file, c, name, r.first + k, self.meter.get()).await?,
            };
            let idx = (r.first + k) as u32;
            let (frame, n) = if bases.contains_key(&idx) {
                let _send = timed();
                payload(data, idx, bases.clone()).await?
            } else {
                let n = data.len() as u64;
                (
                    DataChunk {
                        data,
                        ops: Vec::new(),
                    },
                    n,
                )
            };
            self.throttle(n).await;
            {
                let _send = timed();
                write_msg(send, &frame).await?;
            }
            if let Some(m) = meter {
                m.add(Counter::BytesLiteral, n);
            }
            sent.fetch_add(n, Ordering::Relaxed);
            if let Some(p) = self.progress.get() {
                p(rel, SendProgress::Sent(n));
            }
        }
        {
            let _send = timed();
            send.finish().map_err(io::Error::other)?;
        }
        match tokio::time::timeout(STOPPED_TIMEOUT, send.stopped()).await {
            Ok(Ok(None)) => Ok(()),
            Ok(Ok(Some(code))) => Err(io::Error::other(format!(
                "{name}: peer stopped stream with code {code}"
            ))),
            Ok(Err(e)) => Err(io::Error::other(e)),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{name}: peer did not take the range in time"),
            )),
        }
    }
}

/// The frame for chunk `idx` given the receiver's basis for it: the
/// operations when they are clearly smaller than the bytes, the bytes
/// otherwise. With how many bytes it puts on the wire.
async fn payload(
    data: Vec<u8>,
    idx: u32,
    bases: Arc<HashMap<u32, Basis>>,
) -> io::Result<(DataChunk, u64)> {
    tokio::task::spawn_blocking(move || {
        let basis = &bases[&idx];
        let ops = encode(&data, basis);
        let n = encoded_len(&ops);
        if n * 4 < data.len() * 3 {
            let frame = DataChunk {
                data: Vec::new(),
                ops: ops.iter().map(Op::to_wire).collect(),
            };
            (frame, n as u64)
        } else {
            let n = data.len() as u64;
            (
                DataChunk {
                    data,
                    ops: Vec::new(),
                },
                n,
            )
        }
    })
    .await
    .map_err(io::Error::other)
}

/// Reads chunk `idx` with `pread` and checks it still hashes to the digest.
async fn read_chunk(
    file: &Arc<File>,
    c: &ChunkRecord,
    name: &str,
    idx: usize,
    meter: Option<&Arc<Meter>>,
) -> io::Result<Vec<u8>> {
    let (f, offset, len, want) = (file.clone(), c.offset, c.len as usize, c.hash);
    let m = meter.cloned();
    let read = tokio::task::spawn_blocking(move || {
        let _timed = m.as_deref().map(|m| m.start(Activity::Read));
        let mut buf = vec![0u8; len];
        f.read_exact_at(&mut buf, offset)?;
        if let Some(m) = &m {
            m.add(Counter::DiskRead, len as u64);
        }
        Ok::<_, io::Error>(buf)
    })
    .await
    .map_err(io::Error::other)?;
    let changed = |why: &str| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{name}: chunk {idx} {why} since it was hashed"),
        )
    };
    // Checking the bytes still match is part of reading them.
    let _timed = meter.map(|m| m.start(Activity::Read));
    match read {
        Ok(buf) if blake3::hash(&buf).as_bytes() == &want => Ok(buf),
        Ok(_) => Err(changed("changed")),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err(changed("was truncated")),
        Err(e) => Err(io::Error::new(e.kind(), format!("{name}: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(lens: &[u32]) -> FileDigest {
        let mut off = 0;
        let chunks = lens
            .iter()
            .map(|&len| {
                let c = ChunkRecord {
                    offset: off,
                    len,
                    hash: [0; 32],
                };
                off += len as u64;
                c
            })
            .collect();
        FileDigest {
            size: off,
            hash: [0; 32],
            chunks,
        }
    }

    fn spans(r: Vec<Range>) -> Vec<(usize, usize)> {
        r.into_iter().map(|r| (r.first, r.last)).collect()
    }

    #[test]
    fn gap_starts_new_range() {
        let d = digest(&[10; 6]);
        assert_eq!(
            spans(split_ranges(&[0, 1, 3, 4, 5], &d, 1000)),
            [(0, 1), (3, 5)]
        );
    }

    #[test]
    fn oversize_chunk_goes_alone() {
        let d = digest(&[10, 50, 10]);
        assert_eq!(
            spans(split_ranges(&[0, 1, 2], &d, 20)),
            [(0, 0), (1, 1), (2, 2)]
        );
    }

    #[test]
    fn exact_threshold_fits_one_more_does_not() {
        let d = digest(&[10; 5]);
        assert_eq!(
            spans(split_ranges(&[0, 1, 2, 3, 4], &d, 20)),
            [(0, 1), (2, 3), (4, 4)]
        );
    }

    #[test]
    fn unsorted_duplicates_are_normalised_before_splitting() {
        // send_chunks sorts and dedups before calling split_ranges; mirror that.
        let d = digest(&[10; 4]);
        let mut idx = vec![3usize, 1, 0, 1, 3];
        idx.sort_unstable();
        idx.dedup();
        assert_eq!(spans(split_ranges(&idx, &d, 1000)), [(0, 1), (3, 3)]);
    }
}
