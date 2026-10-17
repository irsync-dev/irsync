//! Persistent SQLite index: cached file metadata and chunk hashes, partial-file
//! progress, and opaque session rows. Local only; crash-safe via WAL and
//! transactions around every multi-statement write.

use crate::fs::paths::RelPath;
use irsync_proto::chunk::{ChunkRecord, FileDigest, CHUNK_PARAMS};
use irsync_proto::v1::{FileEntry, Hash, RangeSet};
use prost::Message;
use rusqlite::{params, Connection, OptionalExtension};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
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
    #[error("corrupt index: {0}")]
    Corrupt(&'static str),
}

pub type Result<T> = std::result::Result<T, IndexError>;

/// Shared handle; async callers lock it inside `spawn_blocking`.
pub type IndexHandle = Arc<Mutex<Index>>;

#[derive(Debug, Clone, PartialEq)]
pub struct IndexEntry {
    pub entry: FileEntry,
    pub ctime: (i64, u32),
    pub digest: Option<FileDigest>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Partial {
    pub expected_hash: [u8; 32],
    pub size: u64,
    pub ranges: RangeSet,
    /// Set to the current time by `put_partial`; the supplied value is ignored.
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

pub struct Index {
    conn: Connection,
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
    pub fn open(state_dir: &Path, root: &Path) -> Result<Index> {
        let canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let h = blake3::hash(canon.as_os_str().as_bytes());
        let name: String = h.as_bytes()[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let dir = state_dir.join("index");
        std::fs::create_dir_all(&dir)?;
        let conn = Connection::open(dir.join(format!("{name}.sqlite")))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut ix = Index { conn };
        ix.init()?;
        Ok(ix)
    }

    pub fn into_handle(self) -> IndexHandle {
        Arc::new(Mutex::new(self))
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
               whole_hash BLOB);
             CREATE INDEX IF NOT EXISTS entries_whole_hash ON entries(whole_hash);
             CREATE TABLE IF NOT EXISTS chunks (
               path BLOB NOT NULL REFERENCES entries(path) ON DELETE CASCADE,
               idx INTEGER NOT NULL, offset INTEGER NOT NULL, len INTEGER NOT NULL,
               hash BLOB NOT NULL, PRIMARY KEY (path, idx));
             CREATE INDEX IF NOT EXISTS chunks_hash ON chunks(hash);
             CREATE TABLE IF NOT EXISTS partials (
               path BLOB PRIMARY KEY, expected_hash BLOB NOT NULL, size INTEGER NOT NULL,
               ranges BLOB NOT NULL, updated_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS sessions (
               id BLOB PRIMARY KEY, key BLOB NOT NULL UNIQUE, plan BLOB NOT NULL,
               progress BLOB NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);",
        )?;
        let want = [
            ("schema_version", SCHEMA_VERSION),
            ("chunk_min", CHUNK_PARAMS.min as i64),
            ("chunk_avg", CHUNK_PARAMS.avg as i64),
            ("chunk_max", CHUNK_PARAMS.max as i64),
        ];
        for (k, v) in want {
            let have: Option<i64> = tx
                .query_row("SELECT value FROM meta WHERE key = ?1", [k], |r| r.get(0))
                .optional()?;
            match have {
                None => {
                    tx.execute(
                        "INSERT INTO meta (key, value) VALUES (?1, ?2)",
                        params![k, v],
                    )?;
                }
                Some(h) if h != v => return Err(IndexError::ChunkParamsChanged),
                Some(_) => {}
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_entry(&self, path: &RelPath) -> Result<Option<IndexEntry>> {
        let p = path.as_bytes();
        let row = self
            .conn
            .query_row(
                "SELECT kind, size, mtime_sec, mtime_nsec, ctime_sec, ctime_nsec, inode, dev,
                        mode, uid, gid, nlink, rdev, link_target, sparse, whole_hash
                 FROM entries WHERE path = ?1",
                [p],
                |r| {
                    let whole: Option<Vec<u8>> = r.get(15)?;
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
                        },
                        (r.get::<_, i64>(4)?, r.get::<_, u32>(5)?),
                        whole,
                    ))
                },
            )
            .optional()?;
        let Some((entry, ctime, whole)) = row else {
            return Ok(None);
        };
        let digest = match whole {
            None => None,
            Some(w) => {
                let mut st = self
                    .conn
                    .prepare("SELECT offset, len, hash FROM chunks WHERE path = ?1 ORDER BY idx")?;
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

    /// Replaces the entry and its chunk rows atomically. `whole_hash` is taken
    /// from `entry`, falling back to `digest.hash`.
    pub fn put_entry(
        &mut self,
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
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM chunks WHERE path = ?1", [p])?;
        tx.execute(
            "INSERT OR REPLACE INTO entries (path, kind, size, mtime_sec, mtime_nsec, ctime_sec,
               ctime_nsec, inode, dev, mode, uid, gid, nlink, rdev, link_target, sparse, whole_hash)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            params![
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
            ],
        )?;
        if let Some(d) = digest {
            let mut st = tx.prepare(
                "INSERT INTO chunks (path, idx, offset, len, hash) VALUES (?1,?2,?3,?4,?5)",
            )?;
            for (i, c) in d.chunks.iter().enumerate() {
                st.execute(params![p, i as i64, c.offset as i64, c.len, &c.hash[..]])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn find_paths_by_whole_hash(&self, hash: &[u8; 32]) -> Result<Vec<RelPath>> {
        let mut st = self
            .conn
            .prepare("SELECT path FROM entries WHERE whole_hash = ?1 ORDER BY path")?;
        let rows = st.query_map([&hash[..]], |r| r.get::<_, Vec<u8>>(0))?;
        rows.map(|r| relpath(&r?)).collect()
    }

    /// Any file holding a chunk with this hash, and the chunk's offset in it.
    pub fn find_chunk(&self, hash: &[u8; 32]) -> Result<Option<(RelPath, u64)>> {
        let row = self
            .conn
            .query_row(
                "SELECT path, offset FROM chunks WHERE hash = ?1 ORDER BY path, idx LIMIT 1",
                [&hash[..]],
                |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)? as u64)),
            )
            .optional()?;
        row.map(|(p, o)| Ok((relpath(&p)?, o))).transpose()
    }

    pub fn partial(&self, path: &RelPath) -> Result<Option<Partial>> {
        let row = self
            .conn
            .query_row(
                "SELECT expected_hash, size, ranges, updated_at FROM partials WHERE path = ?1",
                [path.as_bytes()],
                |r| {
                    Ok((
                        r.get::<_, Vec<u8>>(0)?,
                        r.get::<_, i64>(1)? as u64,
                        r.get::<_, Vec<u8>>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                },
            )
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

    pub fn put_partial(&mut self, path: &RelPath, p: &Partial) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO partials (path, expected_hash, size, ranges, updated_at)
             VALUES (?1,?2,?3,?4,?5)",
            params![
                path.as_bytes(),
                &p.expected_hash[..],
                p.size as i64,
                p.ranges.encode_to_vec(),
                now()
            ],
        )?;
        Ok(())
    }

    pub fn remove_partial(&mut self, path: &RelPath) -> Result<()> {
        self.conn
            .execute("DELETE FROM partials WHERE path = ?1", [path.as_bytes()])?;
        Ok(())
    }

    pub fn partials_older_than(&self, age: Duration) -> Result<Vec<RelPath>> {
        let mut st = self
            .conn
            .prepare("SELECT path FROM partials WHERE updated_at <= ?1 ORDER BY path")?;
        let rows = st.query_map([cutoff(age)], |r| r.get::<_, Vec<u8>>(0))?;
        rows.map(|r| relpath(&r?)).collect()
    }

    /// Upsert by id. A different id holding the same `key` is replaced.
    pub fn save_session(&mut self, s: &SessionRow) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM sessions WHERE key = ?1 AND id != ?2",
            params![s.key, &s.id[..]],
        )?;
        tx.execute(
            "INSERT INTO sessions (id, key, plan, progress, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(id) DO UPDATE SET key=excluded.key, plan=excluded.plan,
               progress=excluded.progress, created_at=excluded.created_at,
               updated_at=excluded.updated_at",
            params![
                &s.id[..],
                s.key,
                s.plan,
                s.progress,
                s.created_at,
                s.updated_at
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn load_session_by_id(&self, id: &[u8; 16]) -> Result<Option<SessionRow>> {
        self.load_session("id", &id[..])
    }

    pub fn load_session_by_key(&self, key: &[u8]) -> Result<Option<SessionRow>> {
        self.load_session("key", key)
    }

    fn load_session(&self, col: &str, v: &[u8]) -> Result<Option<SessionRow>> {
        let sql = format!(
            "SELECT id, key, plan, progress, created_at, updated_at FROM sessions WHERE {col} = ?1"
        );
        let row = self
            .conn
            .query_row(&sql, [v], |r| {
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
            let mut st = tx.prepare("SELECT id FROM sessions WHERE updated_at <= ?1")?;
            let rows = st.query_map([cutoff(age)], |r| r.get::<_, Vec<u8>>(0))?;
            rows.map(|r| arr(r?, "session id"))
                .collect::<Result<Vec<_>>>()?
        };
        for id in &ids {
            tx.execute("DELETE FROM sessions WHERE id = ?1", [&id[..]])?;
        }
        tx.commit()?;
        Ok(ids)
    }
}
