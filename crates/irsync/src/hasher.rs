//! Quick check and index-cached hashing of regular files.
//!
//! A cached digest is reused only if the index has a digest for the path and
//! size, mtime (sec+nsec), ctime and inode all match the scanned item.
//! `force` (`--checksum`) trusts no row: `cached` reports nothing and
//! `digest` re-reads every file, storing what it read so the next plain
//! run is quick again.

use crate::fs::paths::RelPath;
use crate::index::IndexHandle;
use crate::scan::ScanItem;
use irsync_proto::chunk::{digest_reader, FileDigest};
use irsync_proto::v1::file_entry::Kind;
use irsync_proto::v1::Hash;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Semaphore;

/// Counters of work actually done (cache hits are not counted). Read the
/// fields directly with `load`.
#[derive(Debug, Default)]
pub struct HashStats {
    pub files_hashed: AtomicU64,
    pub bytes_hashed: AtomicU64,
}

/// Clones share the worker limit, the counters and the activity marker.
#[derive(Clone)]
pub struct Hasher {
    index: IndexHandle,
    root: PathBuf,
    sem: Arc<Semaphore>,
    force: bool,
    stats: Arc<HashStats>,
    /// The session's activity marker: bumped on every digest completion and
    /// every [`TICK_BYTES`] read, so long local hashing keeps the inactivity
    /// watchdogs quiet.
    activity: Arc<AtomicU64>,
    /// Told when a file is actually read for its digest, and when that ends.
    progress: Option<HashProgress>,
}

/// What a hasher reports about a file it reads.
pub enum HashEvent {
    /// Reading starts (the cache had nothing fresh); the file's size.
    Start { size: u64 },
    /// Reading ended, successfully or not.
    Done,
}

/// A callback told when a file is read for its digest.
pub type HashProgress = Arc<dyn Fn(&RelPath, HashEvent) + Send + Sync>;

/// While hashing, bump the activity marker after this many bytes read.
pub const TICK_BYTES: u64 = 64 << 20;

/// A reader that bumps an activity marker every [`TICK_BYTES`] read (a single
/// file can take many minutes to hash).
pub struct Ticking<'a, R> {
    inner: R,
    activity: &'a AtomicU64,
    since: u64,
}

impl<'a, R> Ticking<'a, R> {
    pub fn new(inner: R, activity: &'a AtomicU64) -> Self {
        Ticking {
            inner,
            activity,
            since: 0,
        }
    }
}

impl<R: io::Read> io::Read for Ticking<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.since += n as u64;
        if self.since >= TICK_BYTES {
            self.since = 0;
            self.activity.fetch_add(1, Ordering::Relaxed);
        }
        Ok(n)
    }
}

fn io_err(e: crate::index::IndexError) -> io::Error {
    io::Error::other(e)
}

fn lookup(index: &IndexHandle, item: &ScanItem) -> io::Result<Option<FileDigest>> {
    let found = index
        .lock()
        .map_err(|_| io::Error::other("index mutex poisoned"))?
        .get_entry(&item.path)
        .map_err(io_err)?;
    let Some(ie) = found else { return Ok(None) };
    let (a, b) = (&ie.entry, &item.entry);
    let fresh = a.size == b.size
        && a.mtime_sec == b.mtime_sec
        && a.mtime_nsec == b.mtime_nsec
        && ie.ctime == item.ctime
        && a.inode == b.inode;
    Ok(if fresh { ie.digest } else { None })
}

impl Hasher {
    /// `workers` bounds how many files are read concurrently (minimum 1).
    pub fn new(index: IndexHandle, root: PathBuf, workers: usize) -> Hasher {
        Hasher {
            index,
            root,
            sem: Arc::new(Semaphore::new(workers.max(1))),
            force: false,
            stats: Arc::new(HashStats::default()),
            activity: Arc::default(),
            progress: None,
        }
    }

    /// Report every file actually read (not a cache hit) to `p`.
    pub fn with_progress(mut self, p: HashProgress) -> Self {
        self.progress = Some(p);
        self
    }

    /// Share `activity` as this hasher's activity marker.
    pub fn with_activity(mut self, activity: Arc<AtomicU64>) -> Self {
        self.activity = activity;
        self
    }

    /// The marker bumped by this hasher's local work.
    pub fn activity(&self) -> &Arc<AtomicU64> {
        &self.activity
    }

    /// When `force` is set the quick check is bypassed (`--checksum`).
    pub fn with_force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }

    pub fn stats(&self) -> &HashStats {
        &self.stats
    }

    /// Cached digest if the quick check passes; never reads the file. A
    /// forced hasher (`--checksum`) reports none: a fresh row is not
    /// evidence about the content.
    pub fn cached(&self, item: &ScanItem) -> io::Result<Option<FileDigest>> {
        if self.force {
            return Ok(None);
        }
        lookup(&self.index, item)
    }

    /// Digest of a regular file: from the index when the quick check passes,
    /// otherwise read, chunked, and stored in the index.
    pub async fn digest(&self, item: &ScanItem) -> io::Result<FileDigest> {
        if item.entry.kind() != Kind::Regular {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "digest requires a regular file",
            ));
        }
        let _permit = self
            .sem
            .acquire()
            .await
            .map_err(|_| io::Error::other("hasher closed"))?;
        let index = self.index.clone();
        let stats = self.stats.clone();
        let full = self.root.join(item.path.to_os());
        let item = item.clone();
        let force = self.force;
        let activity = self.activity.clone();
        let progress = self.progress.clone();
        let res = tokio::task::spawn_blocking(move || {
            if !force {
                if let Some(d) = lookup(&index, &item)? {
                    return Ok(d);
                }
            }
            // Never follow a symlink swapped in since the walk lstat'ed it.
            if let Some(p) = &progress {
                p(
                    &item.path,
                    HashEvent::Start {
                        size: item.entry.size,
                    },
                );
            }
            let d = crate::fs::safe::open_regular_nofollow(&full)
                .and_then(|file| digest_reader(Ticking::new(file, &activity)));
            if let Some(p) = &progress {
                p(&item.path, HashEvent::Done);
            }
            let d = d?;
            stats.files_hashed.fetch_add(1, Ordering::Relaxed);
            stats.bytes_hashed.fetch_add(d.size, Ordering::Relaxed);
            let mut entry = item.entry;
            entry.whole_hash = Some(Hash {
                bytes: d.hash.to_vec(),
            });
            index
                .lock()
                .map_err(|_| io::Error::other("index mutex poisoned"))?
                .put_entry(&item.path, &entry, item.ctime, Some(&d))
                .map_err(io_err)?;
            Ok(d)
        })
        .await
        .map_err(io::Error::other)?;
        self.activity.fetch_add(1, Ordering::Relaxed);
        res
    }
}
