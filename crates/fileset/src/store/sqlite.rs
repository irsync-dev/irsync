//! SQLite backend.

use super::{check_schema, Meta, OpenMode, Store, SCHEMA_VERSION};
use crate::model::{Entry, Flags, Kind, Timespec};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row};
use std::path::Path;

const V1_SQL: &str = "
CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE entries(
    path BLOB PRIMARY KEY,
    kind INTEGER NOT NULL,
    mode INTEGER NOT NULL,
    uid INTEGER NOT NULL,
    gid INTEGER NOT NULL,
    size INTEGER NOT NULL,
    nlink INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    dev INTEGER NOT NULL,
    rdev INTEGER NOT NULL,
    mtime_sec INTEGER NOT NULL,
    mtime_nsec INTEGER NOT NULL,
    ctime_sec INTEGER NOT NULL,
    ctime_nsec INTEGER NOT NULL,
    blake3 BLOB,
    target BLOB,
    flags INTEGER NOT NULL,
    generation INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX entries_generation ON entries(generation);
CREATE TABLE xattrs(
    path BLOB NOT NULL,
    name BLOB NOT NULL,
    value BLOB NOT NULL,
    PRIMARY KEY (path, name)
) WITHOUT ROWID;
";

const MIGRATIONS: &[&str] = &[V1_SQL];

const ENTRY_COLS: &str = "kind, mode, uid, gid, size, nlink, ino, dev, rdev, mtime_sec, \
     mtime_nsec, ctime_sec, ctime_nsec, blake3, target, flags, generation";

/// Runs `scripts[from..]` in one transaction and returns the new version.
/// The resulting `schema_version` is persisted in the same transaction when a
/// `meta` table exists afterwards.
pub(crate) fn apply_migrations(conn: &Connection, scripts: &[&str], from: u32) -> Result<u32> {
    migrate_with(conn, scripts, from, |_| Ok(()))
}

/// Like `apply_migrations`, but runs `extra` inside the same transaction, after the scripts
/// and before the version bump.
fn migrate_with(
    conn: &Connection,
    scripts: &[&str],
    from: u32,
    extra: impl FnOnce(&Connection) -> Result<()>,
) -> Result<u32> {
    let tx = conn.unchecked_transaction()?;
    for script in scripts.iter().skip(from as usize) {
        tx.execute_batch(script)?;
    }
    extra(&tx)?;
    let version = scripts.len().max(from as usize) as u32;
    let has_meta: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='meta')",
        [],
        |r| r.get(0),
    )?;
    if has_meta {
        tx.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', ?)",
            [version.to_string()],
        )?;
    }
    tx.commit()?;
    Ok(version)
}

pub struct SqliteStore {
    conn: Connection,
    mode: OpenMode,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn read_meta(conn: &Connection) -> Result<Meta> {
    let get = |key: &str| -> Result<String> {
        conn.query_row("SELECT value FROM meta WHERE key = ?", [key], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
        .ok_or_else(|| anyhow!("manifest meta key '{key}' missing"))
    };
    Ok(Meta {
        schema_version: get("schema_version")?.parse()?,
        generation: get("generation")?.parse()?,
        tool_version: get("tool_version")?,
        created_at: get("created_at")?.parse()?,
        updated_at: get("updated_at")?.parse()?,
    })
}

/// Writes all meta rows; the caller supplies the transaction.
fn write_meta_rows(conn: &Connection, m: &Meta) -> Result<()> {
    let mut st = conn.prepare("INSERT OR REPLACE INTO meta(key, value) VALUES (?, ?)")?;
    st.execute(params!["schema_version", m.schema_version.to_string()])?;
    st.execute(params!["generation", m.generation.to_string()])?;
    st.execute(params!["tool_version", m.tool_version])?;
    st.execute(params!["created_at", m.created_at.to_string()])?;
    st.execute(params!["updated_at", m.updated_at.to_string()])?;
    Ok(())
}

fn write_meta(conn: &Connection, m: &Meta) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    write_meta_rows(&tx, m)?;
    tx.commit()?;
    Ok(())
}

fn row_to_entry(row: &Row, base: usize) -> rusqlite::Result<Entry> {
    let blake3: Option<Vec<u8>> = row.get(base + 13)?;
    let blake3 = match blake3 {
        Some(b) => Some(<[u8; 32]>::try_from(b.as_slice()).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                base + 13,
                rusqlite::types::Type::Blob,
                "blake3 is not 32 bytes".into(),
            )
        })?),
        None => None,
    };
    Ok(Entry {
        kind: Kind::from_u8(row.get::<_, i64>(base)? as u8),
        mode: row.get::<_, i64>(base + 1)? as u32,
        uid: row.get::<_, i64>(base + 2)? as u32,
        gid: row.get::<_, i64>(base + 3)? as u32,
        size: row.get::<_, i64>(base + 4)? as u64,
        nlink: row.get::<_, i64>(base + 5)? as u64,
        ino: row.get::<_, i64>(base + 6)? as u64,
        dev: row.get::<_, i64>(base + 7)? as u64,
        rdev: row.get::<_, i64>(base + 8)? as u64,
        mtime: Timespec {
            sec: row.get(base + 9)?,
            nsec: row.get::<_, i64>(base + 10)? as u32,
        },
        ctime: Timespec {
            sec: row.get(base + 11)?,
            nsec: row.get::<_, i64>(base + 12)? as u32,
        },
        blake3,
        target: row.get(base + 14)?,
        xattrs: Vec::new(),
        flags: Flags(row.get::<_, i64>(base + 15)? as u32),
        generation: row.get::<_, i64>(base + 16)? as u64,
    })
}

fn load_xattrs(conn: &Connection, path: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let mut st =
        conn.prepare_cached("SELECT name, value FROM xattrs WHERE path = ? ORDER BY name")?;
    let rows = st.query_map([path], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

impl SqliteStore {
    pub fn open(path: &Path, mode: OpenMode) -> Result<SqliteStore> {
        match mode {
            OpenMode::Create => {
                if path.exists() {
                    bail!("manifest already exists: {}", path.display());
                }
                let conn = Connection::open(path)
                    .with_context(|| format!("creating {}", path.display()))?;
                Self::configure(&conn)?;
                let t = now();
                let initial = Meta {
                    schema_version: SCHEMA_VERSION,
                    generation: 0,
                    tool_version: env!("CARGO_PKG_VERSION").to_string(),
                    created_at: t,
                    updated_at: t,
                };
                migrate_with(&conn, MIGRATIONS, 0, |tx| write_meta_rows(tx, &initial))?;
                Ok(SqliteStore { conn, mode })
            }
            OpenMode::ReadWrite => {
                let conn = Connection::open_with_flags(
                    path,
                    OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )
                .with_context(|| format!("opening {}", path.display()))?;
                Self::configure(&conn)?;
                let meta = read_meta(&conn)?;
                check_schema(&meta, mode)?;
                if meta.schema_version < SCHEMA_VERSION {
                    apply_migrations(&conn, MIGRATIONS, meta.schema_version)?;
                }
                Ok(SqliteStore { conn, mode })
            }
            OpenMode::ReadOnly => {
                let conn = Connection::open_with_flags(
                    path,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )
                .with_context(|| format!("opening {}", path.display()))?;
                check_schema(&read_meta(&conn)?, mode)?;
                Ok(SqliteStore { conn, mode })
            }
        }
    }

    fn configure(conn: &Connection) -> Result<()> {
        conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))?;
        conn.execute_batch("PRAGMA synchronous=NORMAL")?;
        Ok(())
    }
}

impl Store for SqliteStore {
    fn meta(&self) -> Result<Meta> {
        read_meta(&self.conn)
    }

    fn get(&self, path: &[u8]) -> Result<Option<Entry>> {
        let sql = format!("SELECT {ENTRY_COLS} FROM entries WHERE path = ?");
        let e = self
            .conn
            .query_row(&sql, [path], |r| row_to_entry(r, 0))
            .optional()?;
        match e {
            Some(mut e) => {
                e.xattrs = load_xattrs(&self.conn, path)?;
                Ok(Some(e))
            }
            None => Ok(None),
        }
    }

    fn for_each(&self, f: &mut dyn FnMut(&[u8], &Entry) -> Result<()>) -> Result<()> {
        let sql = format!("SELECT path, {ENTRY_COLS} FROM entries ORDER BY path");
        let mut st = self.conn.prepare(&sql)?;
        let mut rows = st.query([])?;
        while let Some(row) = rows.next()? {
            let path: Vec<u8> = row.get(0)?;
            let mut e = row_to_entry(row, 1)?;
            e.xattrs = load_xattrs(&self.conn, &path)?;
            f(&path, &e)?;
        }
        Ok(())
    }

    fn put_batch(&mut self, items: &[(Vec<u8>, Entry)]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut ins = tx.prepare_cached(&format!(
                "INSERT OR REPLACE INTO entries(path, {ENTRY_COLS}) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
            ))?;
            let mut del = tx.prepare_cached("DELETE FROM xattrs WHERE path = ?")?;
            let mut insx =
                tx.prepare_cached("INSERT INTO xattrs(path, name, value) VALUES (?, ?, ?)")?;
            for (path, e) in items {
                ins.execute(params![
                    path,
                    e.kind.as_u8() as i64,
                    e.mode as i64,
                    e.uid as i64,
                    e.gid as i64,
                    e.size as i64,
                    e.nlink as i64,
                    e.ino as i64,
                    e.dev as i64,
                    e.rdev as i64,
                    e.mtime.sec,
                    e.mtime.nsec as i64,
                    e.ctime.sec,
                    e.ctime.nsec as i64,
                    e.blake3.as_ref().map(|b| b.as_slice()),
                    e.target,
                    e.flags.0 as i64,
                    e.generation as i64,
                ])?;
                del.execute([path])?;
                for (name, value) in &e.xattrs {
                    insx.execute(params![path, name, value])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn sweep(&mut self, keep: u64) -> Result<u64> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM xattrs WHERE path IN (SELECT path FROM entries WHERE generation != ?)",
            [keep as i64],
        )?;
        let n = tx.execute("DELETE FROM entries WHERE generation != ?", [keep as i64])?;
        tx.commit()?;
        Ok(n as u64)
    }

    fn set_meta(&mut self, meta: &Meta) -> Result<()> {
        write_meta(&self.conn, meta)
    }

    fn close(self: Box<Self>) -> Result<()> {
        if self.mode != OpenMode::ReadOnly {
            self.conn
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
            self.conn
                .query_row("PRAGMA journal_mode=DELETE", [], |_| Ok(()))?;
        }
        self.conn.close().map_err(|(_, e)| e)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{open, Backend};

    #[test]
    fn migrations_apply_only_newer_scripts() {
        let conn = Connection::open_in_memory().unwrap();
        let v = apply_migrations(&conn, &["CREATE TABLE a(x)", "CREATE TABLE b(x)"], 1).unwrap();
        assert_eq!(v, 2);
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(tables, vec!["b".to_string()]);
    }

    #[test]
    fn failed_migration_leaves_nothing_behind() {
        let conn = Connection::open_in_memory().unwrap();
        let scripts = [
            "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT)",
            "NOT VALID SQL",
        ];
        assert!(apply_migrations(&conn, &scripts, 0).is_err());
        let n: i64 = conn
            .query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn migration_persists_version_with_scripts() {
        let conn = Connection::open_in_memory().unwrap();
        let scripts = ["CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT)"];
        assert_eq!(apply_migrations(&conn, &scripts, 0).unwrap(), 1);
        let v: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, "1");
    }

    #[test]
    fn close_leaves_no_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("m.sqlite");
        let mut s = open(&p, Backend::Sqlite, OpenMode::Create).unwrap();
        s.put_batch(&[(
            b"a".to_vec(),
            Entry {
                kind: Kind::Dir,
                mode: 0o755,
                uid: 0,
                gid: 0,
                size: 0,
                nlink: 1,
                ino: 1,
                dev: 1,
                rdev: 0,
                mtime: Timespec { sec: 0, nsec: 0 },
                ctime: Timespec { sec: 0, nsec: 0 },
                blake3: None,
                target: None,
                xattrs: vec![],
                flags: Flags::default(),
                generation: 1,
            },
        )])
        .unwrap();
        s.close().unwrap();
        assert!(p.exists());
        assert!(!dir.path().join("m.sqlite-wal").exists());
        assert!(!dir.path().join("m.sqlite-shm").exists());
    }

    #[test]
    fn readonly_refuses_old_schema() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("m.sqlite");
        open(&p, Backend::Sqlite, OpenMode::Create)
            .unwrap()
            .close()
            .unwrap();
        let conn = Connection::open(&p).unwrap();
        conn.execute(
            "UPDATE meta SET value = '0' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
        drop(conn);
        let err = open(&p, Backend::Sqlite, OpenMode::ReadOnly).err().unwrap();
        assert!(err.to_string().contains("run 'fileset update'"), "{err}");
    }
}
