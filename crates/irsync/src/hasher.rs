//! Quick check and index-cached hashing of regular files.
//!
//! A cached digest is reused only if the index has a digest for the path and
//! size, mtime (sec+nsec), ctime and inode all match the scanned item.
//! `force` (`--checksum`) trusts no row: `cached` reports nothing and
//! `digest` re-reads every file, storing what it read so the next plain
//! run is quick again.

use crate::fs::paths::RelPath;
use crate::index::{IndexHandle, NewEntry};
use crate::scan::ScanItem;
use crate::telemetry::meter::{Activity, Counter, Meter};
use irsync_proto::chunk::{digest_reader, FileDigest};
use irsync_proto::v1::file_entry::Kind;
use irsync_proto::v1::{FileEntry, Hash};
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
    /// Times and counts the files actually read.
    meter: Option<Arc<Meter>>,
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

/// The stat fields a cached digest must match to be reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Freshness {
    pub size: u64,
    pub mtime: (i64, u32),
    pub ctime: (i64, u32),
    pub inode: u64,
}

impl Freshness {
    pub fn of(item: &ScanItem) -> Freshness {
        let e = &item.entry;
        Freshness {
            size: e.size,
            mtime: (e.mtime_sec, e.mtime_nsec),
            ctime: item.ctime,
            inode: e.inode,
        }
    }
}

fn lookup(index: &IndexHandle, path: &RelPath, want: Freshness) -> io::Result<Option<FileDigest>> {
    let found = index.read(|r| r.get_entry(path)).map_err(io_err)?;
    let Some(ie) = found else { return Ok(None) };
    let a = &ie.entry;
    let have = Freshness {
        size: a.size,
        mtime: (a.mtime_sec, a.mtime_nsec),
        ctime: ie.ctime,
        inode: a.inode,
    };
    Ok(if have == want { ie.digest } else { None })
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
            meter: None,
        }
    }

    /// Report every file actually read (not a cache hit) to `p`.
    pub fn with_progress(mut self, p: HashProgress) -> Self {
        self.progress = Some(p);
        self
    }

    /// Meter every file actually read (not a cache hit) into `m`.
    /// Time this hasher's work, and its waits on the index, on `m`.
    pub fn with_meter(mut self, m: Arc<Meter>) -> Self {
        self.index = self.index.metered(m.clone());
        self.meter = Some(m);
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
        lookup(&self.index, &item.path, Freshness::of(item))
    }

    /// Waits until the digests stored so far are in the index (they are
    /// written by its writer thread). Blocking.
    pub fn flush(&self) {
        self.index.flush();
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
        let stat = (item.entry.clone(), item.ctime);
        self.digest_as(item.path.clone(), Freshness::of(item), Some(stat))
            .await
    }

    /// [`Hasher::digest`] of the regular file at index key `path`, whose
    /// stat the walk took: `walked` serves the quick check, so a cache hit
    /// costs no stat. On a miss the file is read and its row describes the
    /// open file as it is now. A file changed since the walk may get a
    /// digest of what the walk saw; the send checks what it reads against
    /// the digest, and the receiver the whole file, so that never lands.
    pub async fn digest_walked(&self, path: RelPath, walked: Freshness) -> io::Result<FileDigest> {
        self.digest_as(path, walked, None).await
    }

    /// The digest of `path` if the index has one matching `want`, else read.
    /// The stored row is `stat`, or the open file's when `None`.
    async fn digest_as(
        &self,
        path: RelPath,
        want: Freshness,
        stat: Option<(FileEntry, (i64, u32))>,
    ) -> io::Result<FileDigest> {
        let queued = self
            .meter
            .as_deref()
            .map(|m| m.hold("irsync_hash_queue_depth"));
        // Owned and moved into the blocking read: aborting this task must not
        // free the worker while the read runs on.
        let permit = self
            .sem
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| io::Error::other("hasher closed"))?;
        drop(queued);
        let index = self.index.clone();
        let stats = self.stats.clone();
        let full = self.root.join(path.to_os());
        let force = self.force;
        let activity = self.activity.clone();
        let progress = self.progress.clone();
        let meter = self.meter.clone();
        let res = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            if !force {
                if let Some(d) = lookup(&index, &path, want)? {
                    return Ok(d);
                }
            }
            // Never follow a symlink swapped in since the walk lstat'ed it.
            let (file, st) = crate::fs::safe::open_regular_nofollow_stat(&full)?;
            let (mut entry, ctime) =
                stat.unwrap_or_else(|| crate::fs::meta::stat_entry_with_ctime(st, &path));
            if let Some(p) = &progress {
                p(&path, HashEvent::Start { size: entry.size });
            }
            let guard = meter.as_deref().map(|m| m.start(Activity::Hash));
            let d = digest_reader(Ticking::new(file, &activity));
            drop(guard);
            if let Some(p) = &progress {
                p(&path, HashEvent::Done);
            }
            let d = d?;
            stats.files_hashed.fetch_add(1, Ordering::Relaxed);
            stats.bytes_hashed.fetch_add(d.size, Ordering::Relaxed);
            if let Some(m) = &meter {
                m.add(Counter::FilesHashed, 1);
                m.add(Counter::BytesHashed, d.size);
            }
            entry.whole_hash = Some(Hash {
                bytes: d.hash.to_vec(),
            });
            index.queue_entry(NewEntry {
                path,
                entry,
                ctime,
                digest: Some(d.clone()),
            });
            Ok(d)
        })
        .await
        .map_err(io::Error::other)?;
        self.activity.fetch_add(1, Ordering::Relaxed);
        res
    }
}
