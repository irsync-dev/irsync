//! Sender data streams: requested chunks of a file go out over parallel
//! unidirectional QUIC streams, one stream per contiguous range.
//!
//! Each stream carries one `DataStreamHeader` frame followed by one
//! `DataChunk` frame per chunk. Chunks are re-hashed as they are read; a
//! mismatch with the digest means the file changed under us and is reported
//! as `InvalidData` instead of sending stale bytes.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::endpoint::Connection;
use irsync_proto::chunk::FileDigest;
use irsync_proto::v1::{DataChunk, DataStreamHeader};
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;

use crate::fs::paths::RelPath;
use crate::hasher::Hasher;
use crate::net::frame::write_msg;
use crate::scan::ScanItem;

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
            }),
        }
    }

    /// The hashing step before a `ChunkList` is sent.
    pub async fn lookahead_digest(&self, item: &ScanItem) -> io::Result<FileDigest> {
        self.inner.hasher.digest(item).await
    }

    /// High-water mark of simultaneously open data streams.
    #[cfg(feature = "test-hooks")]
    pub fn max_concurrency(&self) -> u32 {
        self.inner.max_active.load(Ordering::SeqCst)
    }

    /// Sends the chunks `idxs` of `digest` as one stream per contiguous range,
    /// at most `parallel` streams at a time. Returns the first error.
    pub async fn send_chunks(
        &self,
        file_id: u64,
        path: &RelPath,
        digest: &FileDigest,
        idxs: &[u32],
    ) -> io::Result<()> {
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

        let mut tasks = JoinSet::new();
        for r in ranges {
            let permit = self
                .inner
                .sem
                .clone()
                .acquire_owned()
                .await
                .map_err(io::Error::other)?;
            let inner = self.inner.clone();
            let (digest, full, name) = (digest.clone(), full.clone(), name.clone());
            tasks.spawn(async move {
                let _permit = permit;
                let now = inner.active.fetch_add(1, Ordering::SeqCst) + 1;
                inner.max_active.fetch_max(now, Ordering::SeqCst);
                let res = inner.send_range(file_id, &full, &name, &digest, r).await;
                inner.active.fetch_sub(1, Ordering::SeqCst);
                res
            });
        }
        let mut first_err = None;
        while let Some(joined) = tasks.join_next().await {
            let res = joined.unwrap_or_else(|e| Err(io::Error::other(e)));
            if let Err(e) = res {
                if first_err.is_none() {
                    first_err = Some(e);
                    tasks.abort_all();
                }
            }
        }
        first_err.map_or(Ok(()), Err)
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
            let wait = b.lock().await.take(n);
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }
        }
    }

    async fn send_range(
        &self,
        file_id: u64,
        full: &Path,
        name: &str,
        digest: &FileDigest,
        r: Range,
    ) -> io::Result<()> {
        let chunks = &digest.chunks[r.first..=r.last];
        let file = Arc::new(File::open(full)?);
        let mut send = self.conn.open_uni().await.map_err(io::Error::other)?;
        let header = DataStreamHeader {
            session_id: self.session_id.clone(),
            file_id,
            offset: chunks[0].offset,
            length: chunks.iter().map(|c| c.len as u64).sum(),
            first_chunk_idx: r.first as u32,
        };
        write_msg(&mut send, &header).await?;
        for (k, c) in chunks.iter().enumerate() {
            let (f, offset, len, want) = (file.clone(), c.offset, c.len as usize, c.hash);
            let read = tokio::task::spawn_blocking(move || {
                let mut buf = vec![0u8; len];
                f.read_exact_at(&mut buf, offset)?;
                Ok::<_, io::Error>((blake3::hash(&buf).as_bytes() == &want).then_some(buf))
            })
            .await
            .map_err(io::Error::other)??;
            let Some(data) = read else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{name}: chunk {} changed since it was hashed", r.first + k),
                ));
            };
            self.throttle(len as u64).await;
            write_msg(&mut send, &DataChunk { data }).await?;
        }
        send.finish().map_err(io::Error::other)?;
        match tokio::time::timeout(STOPPED_TIMEOUT, send.stopped()).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(io::Error::other(e)),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{name}: peer did not take the range in time"),
            )),
        }
    }
}
