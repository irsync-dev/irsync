//! Persistent SQLite index: cached file metadata and chunk hashes, partial-file
//! progress, and opaque session rows. Local only; crash-safe via WAL and
//! transactions around every multi-statement write.

use crate::fs::paths::RelPath;
use crate::telemetry::meter::{Activity, Meter};
use irsync_proto::chunk::{ChunkRecord, FileDigest, CHUNK_PARAMS};
use irsync_proto::v1::{FileEntry, Hash, RangeSet};
use prost::Message;
use rusqlite::{params, Connection, OptionalExtension};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Condvar, LockResult, Mutex, MutexGuard, TryLockError, TryLockResult};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("index was built with different chunking parameters")]
    ChunkParamsChanged,
    #[error("index schema version differs from this build")]
    SchemaVersionChanged,
    #[error("invalid argument: {0}")]
    BadArgument(&'static str),
    #[error("corrupt index: {0}")]
    Corrupt(&'static str),
}

pub type Result<T> = std::result::Result<T, IndexError>;

/// The index's read-only queries, on whichever connection holds them: the
/// writer's ([`Index`]) or one of a handle's readers ([`IndexHandle::read`]).
pub struct Reader<'a> {
    conn: &'a Connection,
}

impl Reader<'_> {
    pub fn get_entry(&self, path: &RelPath) -> Result<Option<IndexEntry>> {
        let p = path.as_bytes();
        let row = self
            .conn
            .prepare_cached(
                "SELECT kind, size, mtime_sec, mtime_nsec, ctime_sec, ctime_nsec, inode, dev,
                        mode, uid, gid, nlink, rdev, link_target, sparse, whole_hash, has_digest
                 FROM entries WHERE path = ?1",
            )?
            .query_row([p], |r| {
                let whole: Option<Vec<u8>> = r.get(15)?;
                let has_digest: bool = r.get(16)?;
                Ok((
                    FileEntry {
                        path: p.to_vec(),
                        kind: r.get(0)?,
                        size: r.get::<_, i64>(1)? as u64,
                        mtime_sec: r.get(2)?,
                        mtime_nsec: r.get(3)?,
                        inode: r.get::<_, i64>(6)? as u64,
                        dev: r.get::<_, i64>(7)? as u64,
                        mode: r.get(8)?,
                        uid: r.get(9)?,
                        gid: r.get(10)?,
                        nlink: r.get(11)?,
                        rdev: r.get::<_, i64>(12)? as u64,
                        link_target: r.get(13)?,
                        sparse: r.get(14)?,
                        whole_hash: whole.clone().map(|bytes| Hash { bytes }),
                        // A stored plan holds ids the receiver already localized.
                        user: String::new(),
                        group: String::new(),
                    },
                    (r.get::<_, i64>(4)?, r.get::<_, u32>(5)?),
                    whole,
                    has_digest,
                ))
            })
            .optional()?;
        let Some((entry, ctime, whole, has_digest)) = row else {
            return Ok(None);
        };
        let digest = match whole.filter(|_| has_digest) {
            None => None,
            Some(w) => {
                let mut st = self.conn.prepare_cached(
                    "SELECT offset, len, hash FROM chunks WHERE path = ?1 ORDER BY idx",
                )?;
                let chunks = st
                    .query_map([p], |r| {
                        Ok((
                            r.get::<_, i64>(0)? as u64,
                            r.get::<_, u32>(1)?,
                            r.get::<_, Vec<u8>>(2)?,
                        ))
                    })?
                    .map(|c| {
                        let (offset, len, h) = c?;
                        Ok(ChunkRecord {
                            offset,
                            len,
                            hash: arr(h, "chunk hash")?,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                Some(FileDigest {
                    size: entry.size,
                    hash: arr(w, "whole hash")?,
                    chunks,
                })
            }
        };
        Ok(Some(IndexEntry {
            entry,
            ctime,
            digest,
        }))
    }
    pub fn find_paths_by_whole_hash(&self, hash: &[u8; 32]) -> Result<Vec<RelPath>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT path FROM entries WHERE whole_hash = ?1 ORDER BY path")?;
        let rows = st.query_map([&hash[..]], |r| r.get::<_, Vec<u8>>(0))?;
        rows.map(|r| relpath(&r?)).collect()
    }

    /// Any file holding a chunk with this hash, and the chunk's offset in it.
    pub fn find_chunk(&self, hash: &[u8; 32]) -> Result<Option<(RelPath, u64)>> {
        Ok(self
            .find_chunks(hash, None, 1)?
            .pop()
            .map(|h| (h.path, h.offset)))
    }

    /// Up to `limit` files holding a chunk with this hash, by path; with
    /// `after`, only paths sorting after it (the next page).
    pub fn find_chunks(
        &self,
        hash: &[u8; 32],
        after: Option<&RelPath>,
        limit: usize,
    ) -> Result<Vec<ChunkHolder>> {
        // The cursor and limit are spelled into the statement (one cached
        // statement each): SQLite re-prepares a statement whose plan depends
        // on a bound parameter (`?2 IS NULL OR ...`, `LIMIT ?3`) on every
        // new binding, which costs more than the lookup.
        let cursor = if after.is_some() {
            "AND c.path > ?2"
        } else {
            ""
        };
        let mut st = self.conn.prepare_cached(&format!(
            "SELECT c.path, MIN(c.offset), e.inode, e.size FROM chunks c
             JOIN entries e ON e.path = c.path
             WHERE c.hash = ?1 {cursor}
             GROUP BY c.path ORDER BY c.path LIMIT {limit}"
        ))?;
        let row = |r: &rusqlite::Row<'_>| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, i64>(1)? as u64,
                r.get::<_, i64>(2)? as u64,
                r.get::<_, i64>(3)? as u64,
            ))
        };
        let rows: Vec<_> = match after {
            Some(p) => st.query_map(params![&hash[..], p.as_bytes()], row)?,
            None => st.query_map(params![&hash[..]], row)?,
        }
        .collect::<rusqlite::Result<_>>()?;
        rows.into_iter()
            .map(|(p, offset, inode, size)| {
                Ok(ChunkHolder {
                    path: relpath(&p)?,
                    offset,
                    inode,
                    size,
                })
            })
            .collect()
    }

    pub fn partial(&self, path: &RelPath) -> Result<Option<Partial>> {
        let row = self
            .conn
            .prepare_cached(
                "SELECT expected_hash, size, ranges, updated_at FROM partials WHERE path = ?1",
            )?
            .query_row([path.as_bytes()], |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, i64>(1)? as u64,
                    r.get::<_, Vec<u8>>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })
            .optional()?;
        row.map(|(h, size, ranges, updated_at)| {
            Ok(Partial {
                expected_hash: arr(h, "partial hash")?,
                size,
                ranges: RangeSet::decode(&ranges[..])?,
                updated_at,
            })
        })
        .transpose()
    }
}

/// An entry to write with [`IndexHandle::queue_entry`].
#[derive(Debug, Clone)]
pub struct NewEntry {
    pub path: RelPath,
    pub entry: FileEntry,
    pub ctime: (i64, u32),
    pub digest: Option<FileDigest>,
}

/// The statements of [`Index::put_entry`], inside the caller's transaction.
fn write_entry(
    conn: &Connection,
    path: &RelPath,
    entry: &FileEntry,
    ctime: (i64, u32),
    digest: Option<&FileDigest>,
) -> Result<()> {
    let p = path.as_bytes();
    let whole = entry
        .whole_hash
        .as_ref()
        .map(|h| h.bytes.clone())
        .or_else(|| digest.map(|d| d.hash.to_vec()));
    if let Some(w) = &whole {
        if w.len() != 32 {
            return Err(IndexError::BadArgument("whole_hash must be 32 bytes"));
        }
    }
    if let Some(d) = digest {
        if whole.as_deref() != Some(&d.hash[..]) {
            return Err(IndexError::BadArgument(
                "whole_hash differs from digest hash",
            ));
        }
    }
    conn.prepare_cached("DELETE FROM chunks WHERE path = ?1")?
        .execute([p])?;
    let insert = "INSERT OR REPLACE INTO entries (path, kind, size, mtime_sec, mtime_nsec,
          ctime_sec, ctime_nsec, inode, dev, mode, uid, gid, nlink, rdev, link_target, sparse,
          whole_hash, has_digest)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)";
    conn.prepare_cached(insert)?.execute(params![
        p,
        entry.kind,
        entry.size as i64,
        entry.mtime_sec,
        entry.mtime_nsec,
        ctime.0,
        ctime.1,
        entry.inode as i64,
        entry.dev as i64,
        entry.mode,
        entry.uid,
        entry.gid,
        entry.nlink,
        entry.rdev as i64,
        entry.link_target,
        entry.sparse,
        whole,
        digest.is_some(),
    ])?;
    if let Some(d) = digest {
        let mut st = conn.prepare_cached(
            "INSERT INTO chunks (path, idx, offset, len, hash) VALUES (?1,?2,?3,?4,?5)",
        )?;
        for (i, c) in d.chunks.iter().enumerate() {
            st.execute(params![p, i as i64, c.offset as i64, c.len, &c.hash[..]])?;
        }
    }
    Ok(())
}

/// Shared handle; async callers lock it inside `spawn_blocking`. A handle
/// with a meter times the waits for a lock another task holds, and for
/// [`IndexHandle::flush`], as [`Activity::IndexWait`].
#[derive(Clone)]
pub struct IndexHandle {
    inner: Arc<Mutex<Index>>,
    meter: Option<Arc<Meter>>,
    // The fields drop in this order. The read connections close first:
    // SQLite removes the WAL only when the last connection to close is the
    // writer's, which the thread closes once the last sender is gone.
    readers: Arc<Readers>,
    /// To the thread that writes queued entries.
    writer: mpsc::Sender<Job>,
    writer_thread: Arc<WriterThread>,
}

/// The writer thread, waited for once every handle is dropped, so its
/// connection is closed (and the WAL removed) before the process can exit.
struct WriterThread(Option<std::thread::JoinHandle<()>>);

impl Drop for WriterThread {
    fn drop(&mut self) {
        if let Some(t) = self.0.take() {
            let _ = t.join();
        }
    }
}

/// A write the writer thread makes.
enum Write {
    Put(Box<NewEntry>),
    /// [`Index::remove_entry_if`].
    Remove {
        path: RelPath,
        inode: u64,
        size: u64,
    },
}

/// Work for the writer thread, in the order it was queued.
enum Job {
    Write(Write),
    /// Answered once every entry queued before it is written.
    Flush(mpsc::Sender<()>),
}

/// At most this many jobs per transaction, so the writer thread never holds
/// the index lock for long.
const BATCH: usize = 1024;

/// The writer thread: writes the queued entries, each batch of whatever
/// queued up meanwhile in one transaction. Ends once every handle is gone
/// and the queue is empty.
fn write_queued(index: Arc<Mutex<Index>>, jobs: mpsc::Receiver<Job>) {
    while let Ok(first) = jobs.recv() {
        let (mut batch, mut flushed) = (Vec::new(), Vec::new());
        for job in std::iter::once(first).chain(jobs.try_iter().take(BATCH - 1)) {
            match job {
                Job::Write(w) => batch.push(w),
                Job::Flush(done) => flushed.push(done),
            }
        }
        if !batch.is_empty() {
            write_batch(&index, &batch);
        }
        for done in flushed {
            let _ = done.send(());
        }
    }
}

/// Makes `batch` in one transaction; if that fails, write by write, so one
/// bad entry costs only itself. The index is a cache: failures are logged.
fn write_batch(index: &Mutex<Index>, batch: &[Write]) {
    let mut ix = index.lock().unwrap_or_else(|e| e.into_inner());
    if apply_all(&mut ix.conn, batch).is_ok() {
        return;
    }
    for w in batch {
        if let Err(err) = apply_all(&mut ix.conn, std::slice::from_ref(w)) {
            let path = match w {
                Write::Put(e) => &e.path,
                Write::Remove { path, .. } => path,
            };
            tracing::warn!(?path, error = %err, "updating the index failed");
        }
    }
}

/// `writes`, in one transaction.
fn apply_all(conn: &mut Connection, writes: &[Write]) -> Result<()> {
    let tx = conn.transaction()?;
    for w in writes {
        apply(&tx, w)?;
    }
    tx.commit()?;
    Ok(())
}

/// One queued write, inside the caller's transaction.
fn apply(conn: &Connection, w: &Write) -> Result<()> {
    match w {
        Write::Put(e) => write_entry(conn, &e.path, &e.entry, e.ctime, e.digest.as_ref()),
        Write::Remove { path, inode, size } => remove_entry_if(conn, path, *inode, *size),
    }
}

/// The statements of [`Index::remove_entry_if`].
fn remove_entry_if(conn: &Connection, path: &RelPath, inode: u64, size: u64) -> Result<()> {
    let args = params![path.as_bytes(), inode as i64, size as i64];
    conn.prepare_cached(
        "DELETE FROM chunks WHERE path = ?1 AND EXISTS
           (SELECT 1 FROM entries WHERE path = ?1 AND inode = ?2 AND size = ?3)",
    )?
    .execute(args)?;
    conn.prepare_cached("DELETE FROM entries WHERE path = ?1 AND inode = ?2 AND size = ?3")?
        .execute(args)?;
    Ok(())
}

/// At most this many read connections per index.
const READERS: usize = 8;

/// A handle's read-only connections to the index file, opened as needed.
struct Readers {
    file: PathBuf,
    /// Idle connections, and how many are open in all.
    idle: Mutex<(Vec<Connection>, usize)>,
    freed: Condvar,
}

impl Readers {
    fn get(&self) -> Result<Connection> {
        let mut g = self.idle.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(c) = g.0.pop() {
                return Ok(c);
            }
            if g.1 < READERS {
                g.1 += 1;
                drop(g);
                let opened = Connection::open_with_flags(
                    &self.file,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )
                .and_then(|c| {
                    c.busy_timeout(Duration::from_secs(5))?;
                    Ok(c)
                });
                if opened.is_err() {
                    self.idle.lock().unwrap_or_else(|e| e.into_inner()).1 -= 1;
                    self.freed.notify_one();
                }
                return Ok(opened?);
            }
            g = self.freed.wait(g).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn put(&self, c: Connection) {
        self.idle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .0
            .push(c);
        self.freed.notify_one();
    }
}

/// Returns a read connection to its pool, also when the reader panics.
struct Lent<'a> {
    pool: &'a Readers,
    conn: Option<Connection>,
}

impl Drop for Lent<'_> {
    fn drop(&mut self) {
        if let Some(c) = self.conn.take() {
            self.pool.put(c);
        }
    }
}

impl IndexHandle {
    pub fn new(index: Index) -> IndexHandle {
        let readers = Arc::new(Readers {
            file: index.file.clone(),
            idle: Mutex::default(),
            freed: Condvar::new(),
        });
        let inner = Arc::new(Mutex::new(index));
        let (writer, jobs) = mpsc::channel();
        let queued = inner.clone();
        let thread = std::thread::Builder::new()
            .name("irsync-index".into())
            .spawn(move || write_queued(queued, jobs))
            .expect("spawn the index writer thread");
        IndexHandle {
            inner,
            meter: None,
            writer,
            readers,
            writer_thread: Arc::new(WriterThread(Some(thread))),
        }
    }

    /// The same index, its lock waits timed on `meter`.
    pub fn metered(&self, meter: Arc<Meter>) -> IndexHandle {
        IndexHandle {
            inner: self.inner.clone(),
            meter: Some(meter),
            writer: self.writer.clone(),
            readers: self.readers.clone(),
            writer_thread: self.writer_thread.clone(),
        }
    }

    /// Runs `f` on a read-only connection of its own: lookups neither wait
    /// for the index lock nor hold it, and see what writers have committed.
    /// Blocking; waits when all [`READERS`] are in use.
    pub fn read<T>(&self, f: impl FnOnce(&Reader<'_>) -> Result<T>) -> Result<T> {
        let lent = Lent {
            pool: &self.readers,
            conn: Some(self.readers.get()?),
        };
        let conn = lent.conn.as_ref().expect("lent until dropped");
        f(&Reader { conn })
    }

    /// Queues `e` for the writer thread to write like [`Index::put_entry`],
    /// without waiting for it: lookups see it once it is written, and
    /// [`IndexHandle::flush`] waits for that.
    pub fn queue_entry(&self, e: NewEntry) {
        self.queue(Write::Put(Box::new(e)));
    }

    /// Queues [`Index::remove_entry_if`] for the writer thread, without
    /// waiting for it, like [`IndexHandle::queue_entry`].
    pub fn queue_removal(&self, path: RelPath, inode: u64, size: u64) {
        self.queue(Write::Remove { path, inode, size });
    }

    fn queue(&self, w: Write) {
        if let Err(mpsc::SendError(Job::Write(w))) = self.writer.send(Job::Write(w)) {
            // The writer thread is gone (it panicked): write it here.
            write_batch(&self.inner, &[w]);
        }
    }

    /// Waits until every entry queued before the call is written (or its
    /// write failed and was logged). Blocking.
    pub fn flush(&self) {
        let (done, written) = mpsc::channel();
        if self.writer.send(Job::Flush(done)).is_ok() {
            let _waiting = self.meter.as_deref().map(|m| m.start(Activity::IndexWait));
            let _ = written.recv();
        }
    }

    pub fn try_lock(&self) -> TryLockResult<MutexGuard<'_, Index>> {
        self.inner.try_lock()
    }

    pub fn lock(&self) -> LockResult<MutexGuard<'_, Index>> {
        match self.inner.try_lock() {
            Ok(g) => return Ok(g),
            Err(TryLockError::Poisoned(e)) => return Err(e),
            Err(TryLockError::WouldBlock) => {}
        }
        let _waiting = self.meter.as_deref().map(|m| m.start(Activity::IndexWait));
        self.inner.lock()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexEntry {
    pub entry: FileEntry,
    pub ctime: (i64, u32),
    pub digest: Option<FileDigest>,
}

/// A file holding a chunk: the chunk's first offset in it, and the inode
/// and size its entry records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkHolder {
    pub path: RelPath,
    pub offset: u64,
    pub inode: u64,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Partial {
    pub expected_hash: [u8; 32],
    pub size: u64,
    pub ranges: RangeSet,
    /// Unix seconds of the last progress update; stored as given.
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub id: [u8; 16],
    pub key: Vec<u8>,
    pub plan: Vec<u8>,
    pub progress: Vec<u8>,
    pub created_at: i64,
    pub updated_at: i64,
}

enum SessionBy {
    Id,
    Key,
}

pub struct Index {
    conn: Connection,
    /// The database file, for a handle's read connections.
    file: PathBuf,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn cutoff(age: Duration) -> i64 {
    now().saturating_sub(age.as_secs().min(i64::MAX as u64) as i64)
}

fn arr<const N: usize>(b: Vec<u8>, what: &'static str) -> Result<[u8; N]> {
    b.try_into().map_err(|_| IndexError::Corrupt(what))
}

fn relpath(b: &[u8]) -> Result<RelPath> {
    RelPath::new(b).map_err(|_| IndexError::Corrupt("invalid path"))
}

impl Index {
    /// The read-only queries on this connection.
    pub fn reader(&self) -> Reader<'_> {
        Reader { conn: &self.conn }
    }

    pub fn get_entry(&self, path: &RelPath) -> Result<Option<IndexEntry>> {
        self.reader().get_entry(path)
    }

    pub fn find_paths_by_whole_hash(&self, hash: &[u8; 32]) -> Result<Vec<RelPath>> {
        self.reader().find_paths_by_whole_hash(hash)
    }

    /// Any file holding a chunk with this hash, and the chunk's offset in it.
    pub fn find_chunk(&self, hash: &[u8; 32]) -> Result<Option<(RelPath, u64)>> {
        self.reader().find_chunk(hash)
    }

    pub fn open(state_dir: &Path, root: &Path) -> Result<Index> {
        let canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let h = blake3::hash(canon.as_os_str().as_bytes());
        let name: String = h.as_bytes()[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let dir = state_dir.join("index");
        std::fs::create_dir_all(&dir)?;
        let file = dir.join(format!("{name}.sqlite"));
        let conn = Connection::open(&file)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(Duration::from_secs(5))?;
        // Every statement is cached; this many are in use.
        conn.set_prepared_statement_cache_capacity(64);
        let mut ix = Index { conn, file };
        ix.init()?;
        // `meta.value` is declared INTEGER but SQLite is dynamically typed:
        // `root_path` is stored (and read back) as a BLOB of the path bytes.
        // Remember which root this (hash-named) file belongs to, so `irsync gc` can find it.
        ix.conn
            .prepare_cached("INSERT OR IGNORE INTO meta (key, value) VALUES ('root_path', ?1)")?
            .execute([canon.as_os_str().as_bytes()])?;
        Ok(ix)
    }

    /// The canonical root recorded in the index file at `file`, if any
    /// (indexes from before the key existed have none).
    pub fn root_of_file(file: &Path) -> Result<Option<std::path::PathBuf>> {
        use std::os::unix::ffi::OsStringExt;
        let conn = Connection::open_with_flags(file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let v: Option<Vec<u8>> = conn
            .prepare_cached("SELECT value FROM meta WHERE key = 'root_path'")?
            .query_row([], |r| r.get(0))
            .optional()?;
        Ok(v.map(|b| std::path::PathBuf::from(std::ffi::OsString::from_vec(b))))
    }

    /// When garbage collection last walked this root (seconds since the epoch).
    pub fn last_gc_at(&self) -> Result<Option<i64>> {
        Ok(self
            .conn
            .prepare_cached("SELECT value FROM meta WHERE key = 'last_gc_at'")?
            .query_row([], |r| r.get(0))
            .optional()?)
    }

    pub fn set_last_gc_at(&mut self, secs: i64) -> Result<()> {
        self.conn
            .prepare_cached("INSERT OR REPLACE INTO meta (key, value) VALUES ('last_gc_at', ?1)")?
            .execute([secs])?;
        Ok(())
    }

    /// Every session row (for GC reference checks).
    pub fn all_sessions(&self) -> Result<Vec<SessionRow>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT id FROM sessions ORDER BY created_at")?;
        let ids = st
            .query_map([], |r| r.get::<_, Vec<u8>>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut out = Vec::new();
        for id in ids {
            if let Some(s) = self.load_session(SessionBy::Id, &id)? {
                out.push(s);
            }
        }
        Ok(out)
    }

    pub fn into_handle(self) -> IndexHandle {
        IndexHandle::new(self)
    }

    fn init(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS entries (
               path BLOB PRIMARY KEY, kind INTEGER NOT NULL, size INTEGER NOT NULL,
               mtime_sec INTEGER NOT NULL, mtime_nsec INTEGER NOT NULL,
               ctime_sec INTEGER NOT NULL, ctime_nsec INTEGER NOT NULL,
               inode INTEGER NOT NULL, dev INTEGER NOT NULL, mode INTEGER NOT NULL,
               uid INTEGER NOT NULL, gid INTEGER NOT NULL, nlink INTEGER NOT NULL,
               rdev INTEGER NOT NULL, link_target BLOB NOT NULL, sparse INTEGER NOT NULL,
               whole_hash BLOB, has_digest INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS entries_whole_hash ON entries(whole_hash);
             CREATE TABLE IF NOT EXISTS chunks (
               path BLOB NOT NULL REFERENCES entries(path) ON DELETE CASCADE,
               idx INTEGER NOT NULL, offset INTEGER NOT NULL, len INTEGER NOT NULL,
               hash BLOB NOT NULL, PRIMARY KEY (path, idx));
             DROP INDEX IF EXISTS chunks_hash;
             CREATE INDEX IF NOT EXISTS chunks_hash_path ON chunks(hash, path);
             CREATE TABLE IF NOT EXISTS partials (
               path BLOB PRIMARY KEY, expected_hash BLOB NOT NULL, size INTEGER NOT NULL,
               ranges BLOB NOT NULL, updated_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS sessions (
               id BLOB PRIMARY KEY, key BLOB NOT NULL UNIQUE, plan BLOB NOT NULL,
               progress BLOB NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);",
        )?;
        let have_v: Option<i64> = tx
            .prepare_cached("SELECT value FROM meta WHERE key = 'schema_version'")?
            .query_row([], |r| r.get(0))
            .optional()?;
        match have_v {
            None => {
                tx.prepare_cached("INSERT INTO meta (key, value) VALUES ('schema_version', ?1)")?
                    .execute([SCHEMA_VERSION])?;
            }
            Some(v) if v != SCHEMA_VERSION => return Err(IndexError::SchemaVersionChanged),
            Some(_) => {}
        }
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        let want = [
            ("chunk_min", CHUNK_PARAMS.min as i64),
            ("chunk_avg", CHUNK_PARAMS.avg as i64),
            ("chunk_max", CHUNK_PARAMS.max as i64),
        ];
        for (k, v) in want {
            let have: Option<i64> = tx
                .prepare_cached("SELECT value FROM meta WHERE key = ?1")?
                .query_row([k], |r| r.get(0))
                .optional()?;
            match have {
                None => {
                    tx.prepare_cached("INSERT INTO meta (key, value) VALUES (?1, ?2)")?
                        .execute(params![k, v])?;
                }
                Some(h) if h != v => return Err(IndexError::ChunkParamsChanged),
                Some(_) => {}
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Replaces the entry and its chunk rows atomically. `whole_hash` is taken
    /// from `entry`, falling back to `digest.hash`.
    pub fn put_entry(
        &mut self,
        path: &RelPath,
        entry: &FileEntry,
        ctime: (i64, u32),
        digest: Option<&FileDigest>,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        write_entry(&tx, path, entry, ctime, digest)?;
        tx.commit()?;
        Ok(())
    }

    /// Forgets the entry at `path` and its chunk rows if it still describes
    /// the file with this inode and size (one recorded since, for a new file
    /// at the path, stays).
    pub fn remove_entry_if(&mut self, path: &RelPath, inode: u64, size: u64) -> Result<()> {
        let tx = self.conn.transaction()?;
        remove_entry_if(&tx, path, inode, size)?;
        tx.commit()?;
        Ok(())
    }

    /// Every entry's path, kind, inode and size.
    pub fn entry_identities(&self) -> Result<Vec<(RelPath, i32, u64, u64)>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT path, kind, inode, size FROM entries ORDER BY path")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, i32>(1)?,
                r.get::<_, i64>(2)? as u64,
                r.get::<_, i64>(3)? as u64,
            ))
        })?;
        rows.map(|r| {
            let (p, kind, inode, size) = r?;
            Ok((relpath(&p)?, kind, inode, size))
        })
        .collect()
    }

    /// Forgets the entry at `path` and its chunk rows (its file is gone or
    /// was replaced).
    pub fn remove_entry(&mut self, path: &RelPath) -> Result<()> {
        let p = path.as_bytes();
        let tx = self.conn.transaction()?;
        tx.prepare_cached("DELETE FROM chunks WHERE path = ?1")?
            .execute([p])?;
        tx.prepare_cached("DELETE FROM entries WHERE path = ?1")?
            .execute([p])?;
        tx.commit()?;
        Ok(())
    }

    pub fn partial(&self, path: &RelPath) -> Result<Option<Partial>> {
        self.reader().partial(path)
    }

    pub fn put_partial(&mut self, path: &RelPath, p: &Partial) -> Result<()> {
        self.conn
            .prepare_cached(
                "INSERT OR REPLACE INTO partials (path, expected_hash, size, ranges, updated_at)
             VALUES (?1,?2,?3,?4,?5)",
            )?
            .execute(params![
                path.as_bytes(),
                &p.expected_hash[..],
                p.size as i64,
                p.ranges.encode_to_vec(),
                p.updated_at
            ])?;
        Ok(())
    }

    pub fn remove_partial(&mut self, path: &RelPath) -> Result<()> {
        self.conn
            .prepare_cached("DELETE FROM partials WHERE path = ?1")?
            .execute([path.as_bytes()])?;
        Ok(())
    }

    /// Every destination with a partial record, whatever its age.
    pub fn partial_paths(&self) -> Result<Vec<RelPath>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT path FROM partials ORDER BY path")?;
        let rows = st.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
        rows.map(|r| relpath(&r?)).collect()
    }

    pub fn partials_older_than(&self, age: Duration) -> Result<Vec<RelPath>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT path FROM partials WHERE updated_at <= ?1 ORDER BY path")?;
        let rows = st.query_map([cutoff(age)], |r| r.get::<_, Vec<u8>>(0))?;
        rows.map(|r| relpath(&r?)).collect()
    }

    /// Upsert by id; newest wins on `key`. If a row with a different id holds the
    /// same `key` it is evicted, and its id is returned (`None` if nothing was evicted).
    pub fn save_session(&mut self, s: &SessionRow) -> Result<Option<[u8; 16]>> {
        let tx = self.conn.transaction()?;
        let evicted: Option<Vec<u8>> = tx
            .prepare_cached("SELECT id FROM sessions WHERE key = ?1 AND id != ?2")?
            .query_row(params![s.key, &s.id[..]], |r| r.get(0))
            .optional()?;
        tx.prepare_cached("DELETE FROM sessions WHERE key = ?1 AND id != ?2")?
            .execute(params![s.key, &s.id[..]])?;
        tx.prepare_cached(
            "INSERT INTO sessions (id, key, plan, progress, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(id) DO UPDATE SET key=excluded.key, plan=excluded.plan,
               progress=excluded.progress, created_at=excluded.created_at,
               updated_at=excluded.updated_at",
        )?
        .execute(params![
            &s.id[..],
            s.key,
            s.plan,
            s.progress,
            s.created_at,
            s.updated_at
        ])?;
        tx.commit()?;
        evicted.map(|id| arr(id, "session id")).transpose()
    }

    pub fn load_session_by_id(&self, id: &[u8; 16]) -> Result<Option<SessionRow>> {
        self.load_session(SessionBy::Id, &id[..])
    }

    pub fn load_session_by_key(&self, key: &[u8]) -> Result<Option<SessionRow>> {
        self.load_session(SessionBy::Key, key)
    }

    fn load_session(&self, by: SessionBy, v: &[u8]) -> Result<Option<SessionRow>> {
        let sql = match by {
            SessionBy::Id => {
                "SELECT id, key, plan, progress, created_at, updated_at FROM sessions WHERE id = ?1"
            }
            SessionBy::Key => {
                "SELECT id, key, plan, progress, created_at, updated_at FROM sessions WHERE key = ?1"
            }
        };
        let row = self
            .conn
            .query_row(sql, [v], |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    SessionRow {
                        id: [0; 16],
                        key: r.get(1)?,
                        plan: r.get(2)?,
                        progress: r.get(3)?,
                        created_at: r.get(4)?,
                        updated_at: r.get(5)?,
                    },
                ))
            })
            .optional()?;
        row.map(|(id, mut s)| {
            s.id = arr(id, "session id")?;
            Ok(s)
        })
        .transpose()
    }

    pub fn delete_sessions_older_than(&mut self, age: Duration) -> Result<Vec<[u8; 16]>> {
        let tx = self.conn.transaction()?;
        let ids = {
            let mut st = tx.prepare_cached("SELECT id FROM sessions WHERE updated_at <= ?1")?;
            let rows = st.query_map([cutoff(age)], |r| r.get::<_, Vec<u8>>(0))?;
            rows.map(|r| arr(r?, "session id"))
                .collect::<Result<Vec<_>>>()?
        };
        for id in &ids {
            tx.prepare_cached("DELETE FROM sessions WHERE id = ?1")?
                .execute([&id[..]])?;
        }
        tx.commit()?;
        Ok(ids)
    }

    /// How many sessions `delete_sessions_older_than(age)` would remove.
    pub fn count_sessions_older_than(&self, age: Duration) -> Result<u32> {
        Ok(self
            .conn
            .prepare_cached("SELECT COUNT(*) FROM sessions WHERE updated_at <= ?1")?
            .query_row([cutoff(age)], |r| r.get(0))?)
    }

    /// Removes one session row (a finished session). Returns whether it existed.
    pub fn delete_session(&mut self, id: &[u8; 16]) -> Result<bool> {
        let n = self
            .conn
            .prepare_cached("DELETE FROM sessions WHERE id = ?1")?
            .execute([&id[..]])?;
        Ok(n > 0)
    }
}
