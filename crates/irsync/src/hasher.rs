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

/// Files of at least this many bytes are read [`LARGE_WORKERS`] at a time.
pub const LARGE_FILE: u64 = 4 << 20;

/// Large files read at once, whatever a hasher's worker limit. On a spinning
/// disk many small files in flight let it order its reads, while more than
/// a few long sequential reads make it seek between them (see
/// `docs/IRSYNC_DECISIONS.md`, 2.1 item 7).
pub const LARGE_WORKERS: usize = 4;

/// Clones share the worker limits, the counters and the activity marker.
#[derive(Clone)]
pub struct Hasher {
    index: IndexHandle,
    root: PathBuf,
    sem: Arc<Semaphore>,
    /// Taken before `sem` by a file of at least [`LARGE_FILE`] bytes.
    large: Arc<Semaphore>,
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
    /// Told the bytes read so far during each read of a file (a test
    /// changes the file mid-pass).
    hash_hook: Option<HashHook>,
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

/// Told about each pass that reads a file for its digest: once with `0`
/// before the file is opened, then after each read with the total bytes
/// read so far. A call with `0` marks the start of a pass.
pub type HashHook = Arc<dyn Fn(&RelPath, u64) + Send + Sync>;

/// While hashing, bump the activity marker after this many bytes read.
pub const TICK_BYTES: u64 = 64 << 20;

/// A reader that bumps an activity marker every [`TICK_BYTES`] read (a single
/// file can take many minutes to hash), and tells a [`HashHook`] after each
/// read if it has one.
pub struct Ticking<'a, R> {
    inner: R,
    activity: &'a AtomicU64,
    since: u64,
    read: u64,
    hook: Option<(&'a HashHook, &'a RelPath)>,
}

impl<'a, R> Ticking<'a, R> {
    pub fn new(inner: R, activity: &'a AtomicU64) -> Self {
        Ticking {
            inner,
            activity,
            since: 0,
            read: 0,
            hook: None,
        }
    }

    /// Tells `hook`, if any, the bytes read so far of `path` after each read.
    pub fn with_hook(mut self, hook: Option<&'a HashHook>, path: &'a RelPath) -> Self {
        self.hook = hook.map(|h| (h, path));
        self
    }
}

impl<R: io::Read> io::Read for Ticking<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.since += n as u64;
        self.read += n as u64;
        if self.since >= TICK_BYTES {
            self.since = 0;
            self.activity.fetch_add(1, Ordering::Relaxed);
        }
        if let Some((hook, path)) = self.hook {
            hook(path, self.read);
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
        Freshness::of_entry(&item.entry, item.ctime)
    }

    /// The freshness of a stat taken as `entry` and its change time.
    pub fn of_entry(e: &FileEntry, ctime: (i64, u32)) -> Freshness {
        Freshness {
            size: e.size,
            mtime: (e.mtime_sec, e.mtime_nsec),
            ctime,
            inode: e.inode,
        }
    }
}

/// A file's digest, with the stat of the file it was taken under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hashed {
    pub digest: FileDigest,
    /// The stat at open (the same after the read, or the read fails); on a
    /// cache hit, the one that matched the index row.
    pub stat: Freshness,
}

/// The error of a file that changed while it was read for its digest.
pub fn changed_while_hashed(path: &RelPath) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "{}: changed while it was hashed",
            crate::fs::paths::escape_path(path.as_bytes())
        ),
    )
}

/// Fails with [`changed_while_hashed`] unless the open `file` still has
/// the stat `opened` it had before it was read.
pub fn check_unchanged(file: &std::fs::File, path: &RelPath, opened: Freshness) -> io::Result<()> {
    let (entry, ctime) = crate::fs::meta::fstat_entry_with_ctime(file, path)?;
    if Freshness::of_entry(&entry, ctime) == opened {
        Ok(())
    } else {
        Err(changed_while_hashed(path))
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
    /// `workers` bounds how many files are read concurrently (minimum 1),
    /// and at most [`LARGE_WORKERS`] of them large.
    pub fn new(index: IndexHandle, root: PathBuf, workers: usize) -> Hasher {
        let workers = workers.max(1);
        Hasher {
            index,
            root,
            sem: Arc::new(Semaphore::new(workers)),
            large: Arc::new(Semaphore::new(workers.min(LARGE_WORKERS))),
            force: false,
            stats: Arc::new(HashStats::default()),
            activity: Arc::default(),
            progress: None,
            meter: None,
            hash_hook: None,
        }
    }

    /// Tell `hook` about every pass that reads a file (see [`HashHook`]).
    pub fn with_hash_hook(mut self, hook: HashHook) -> Self {
        self.hash_hook = Some(hook);
        self
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
    /// otherwise read, chunked, and stored in the index. A file that
    /// changes while it is read fails with [`changed_while_hashed`] and
    /// stores nothing.
    pub async fn digest(&self, item: &ScanItem) -> io::Result<Hashed> {
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
    /// the digest, and the receiver the whole file. The returned stat is
    /// the one the digest was taken under, not the walk's.
    pub async fn digest_walked(&self, path: RelPath, walked: Freshness) -> io::Result<Hashed> {
        self.digest_as(path, walked, None).await
    }

    /// The digest of `path` if the index has one matching `want`, else read.
    /// The stored row is `stat`, or the open file's when `None`.
    async fn digest_as(
        &self,
        path: RelPath,
        want: Freshness,
        stat: Option<(FileEntry, (i64, u32))>,
    ) -> io::Result<Hashed> {
        let queued = self
            .meter
            .as_deref()
            .map(|m| m.hold("irsync_hash_queue_depth"));
        // Owned and moved into the blocking read: aborting this task must not
        // free the worker while the read runs on. A large file waits for its
        // large slot first, so it holds no worker other files could use.
        // The walk's size decides: a cache hit takes the slot too, briefly.
        let large = if want.size >= LARGE_FILE {
            Some(
                self.large
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|_| io::Error::other("hasher closed"))?,
            )
        } else {
            None
        };
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
        let hook = self.hash_hook.clone();
        let res = tokio::task::spawn_blocking(move || {
            let _permit = (permit, large);
            if !force {
                if let Some(digest) = lookup(&index, &path, want)? {
                    return Ok(Hashed { digest, stat: want });
                }
            }
            if let Some(h) = &hook {
                h(&path, 0);
            }
            // Never follow a symlink swapped in since the walk lstat'ed it.
            let (file, st) = crate::fs::safe::open_regular_nofollow_stat(&full)?;
            let at_open = crate::fs::meta::stat_entry_with_ctime(st, &path);
            let opened = Freshness::of_entry(&at_open.0, at_open.1);
            let (mut entry, ctime) = stat.unwrap_or(at_open);
            if let Some(p) = &progress {
                p(&path, HashEvent::Start { size: entry.size });
            }
            let guard = meter.as_deref().map(|m| m.start(Activity::Hash));
            let d = digest_reader(Ticking::new(&file, &activity).with_hook(hook.as_ref(), &path));
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
            // A write during the pass may have given a digest of a state
            // the file never had: report it, and store no row.
            check_unchanged(&file, &path, opened)?;
            entry.whole_hash = Some(Hash {
                bytes: d.hash.to_vec(),
            });
            index.queue_entry(NewEntry {
                path,
                entry,
                ctime,
                digest: Some(d.clone()),
            });
            Ok(Hashed {
                digest: d,
                stat: opened,
            })
        })
        .await
        .map_err(io::Error::other)?;
        self.activity.fetch_add(1, Ordering::Relaxed);
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn digest_reports_the_stat_it_hashed_under() {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let index = crate::index::Index::open(state.path(), root.path())
            .unwrap()
            .into_handle();
        let h = Hasher::new(index, root.path().to_path_buf(), 1);
        std::fs::write(root.path().join("f"), b"some content").unwrap();
        let path = RelPath::new(b"f").unwrap();
        let (entry, ctime) = crate::fs::meta::lstat_entry_with_ctime(root.path(), &path).unwrap();
        let item = ScanItem {
            path: path.clone(),
            entry,
            ctime,
        };
        let fresh = Freshness::of(&item);

        let read = h.digest_walked(path.clone(), fresh).await.unwrap();
        assert_eq!(read.stat, fresh);
        h.flush();
        let cached = h.digest_walked(path, fresh).await.unwrap();
        assert_eq!(cached.stat, fresh);
        assert_eq!(cached.digest, read.digest);
        assert_eq!(h.stats().files_hashed.load(Ordering::Relaxed), 1);
    }
}
