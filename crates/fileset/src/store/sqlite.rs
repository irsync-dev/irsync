//! SQLite backend.

use super::{check_schema, Meta, OpenMode, Reader, Store, SCHEMA_VERSION};
use crate::model::{Entry, Flags, Kind, Timespec};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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

/// Keys read per page by `sweep`.
const SWEEP_CHUNK: usize = 2000;

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
    path: PathBuf,
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
        unfinished: get("unfinished").is_ok(),
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
    if m.unfinished {
        st.execute(params!["unfinished", "1"])?;
    } else {
        conn.execute("DELETE FROM meta WHERE key = 'unfinished'", [])?;
    }
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

/// The entry at `path` with its xattrs, in one query: a row per xattr, or one without.
fn get_entry(conn: &Connection, path: &[u8]) -> Result<Option<Entry>> {
    let sql = format!(
        "SELECT {ENTRY_COLS}, x.name, x.value FROM entries e \
         LEFT JOIN xattrs x ON x.path = e.path WHERE e.path = ? ORDER BY x.name"
    );
    // Called once per walked entry: parsing the SQL each time cost as much as the lookup.
    let mut st = conn.prepare_cached(&sql)?;
    let mut rows = st.query([path])?;
    let mut entry: Option<Entry> = None;
    while let Some(row) = rows.next()? {
        let e = match &mut entry {
            Some(e) => e,
            None => entry.insert(row_to_entry(row, 0)?),
        };
        if let Some(name) = row.get::<_, Option<Vec<u8>>>(17)? {
            e.xattrs.push((name, row.get(18)?));
        }
    }
    Ok(entry)
}

/// Read-only connections of its own, one per thread that is reading at the moment; WAL
/// mode lets them read while the store's own connection writes.
struct SqliteReader {
    path: PathBuf,
    idle: Mutex<Vec<Connection>>,
}

impl Reader for SqliteReader {
    fn get(&self, path: &[u8]) -> Result<Option<Entry>> {
        let conn = self.take()?;
        let e = get_entry(&conn, path);
        self.idle.lock().unwrap().push(conn);
        e
    }

    // One read transaction for the lot, instead of one per query.
    fn get_many(&self, paths: &[&[u8]]) -> Result<Vec<Option<Entry>>> {
        let conn = self.take()?;
        let read = (|| {
            let tx = conn.unchecked_transaction()?;
            let entries = paths
                .iter()
                .map(|p| get_entry(&tx, p))
                .collect::<Result<Vec<_>>>()?;
            tx.finish()?;
            Ok(entries)
        })();
        self.idle.lock().unwrap().push(conn);
        read
    }
}

/// Keeps SQLite from choosing a query plan by the values bound to a
/// statement (its "query planner stability guarantee"). Since SQLite 3.54 a
/// plan can depend on the value bound in `col = ?`, and every
/// sqlite3_clear_bindings() then expires the statement. rusqlite's
/// statement cache calls that on each return, so the per-entry lookup
/// would be prepared again on every call;
/// `lookups_and_writes_are_never_re_prepared` catches it.
fn stable_plans(conn: &Connection) -> Result<()> {
    use rusqlite::config::DbConfig;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_QPSG, true)?;
    // A manifest can come with a tree someone else made. These are SQLite's
    // settings for a database file that is not trusted: its schema cannot
    // call functions or virtual tables with side effects, and SQL cannot
    // change the file's internals.
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
    Ok(())
}

/// Refuses a manifest whose schema holds anything but plain tables and
/// indexes. A manifest that came with a tree could otherwise define
/// `entries` as a view that never ends, or carry triggers that run when
/// `update` writes.
fn check_objects(conn: &Connection) -> Result<()> {
    use rusqlite::types::ValueRef;
    // As bytes: a damaged or crafted schema need not hold valid text.
    let bytes = |v: ValueRef<'_>| match v {
        ValueRef::Text(b) | ValueRef::Blob(b) => b.to_vec(),
        _ => Vec::new(),
    };
    let mut stmt = conn.prepare("SELECT type, name, sql FROM sqlite_schema")?;
    let mut rows = stmt.query([])?;
    let mut tables = Vec::new();
    while let Some(row) = rows.next()? {
        let (kind, name) = (bytes(row.get_ref(0)?), bytes(row.get_ref(1)?));
        let sql = bytes(row.get_ref(2)?);
        let is_virtual = sql
            .split(u8::is_ascii_whitespace)
            .filter(|w| !w.is_empty())
            .nth(1)
            .is_some_and(|w| w.eq_ignore_ascii_case(b"virtual"));
        if !matches!(&kind[..], b"table" | b"index") || is_virtual {
            let kind = if is_virtual {
                &b"virtual table"[..]
            } else {
                &kind
            };
            bail!(
                "not a fileset manifest: it holds a {} named {}",
                crate::escape::escape(kind),
                crate::escape::escape(&name)
            );
        }
        if kind == b"table" {
            tables.push(name);
        }
    }
    for needed in ["meta", "entries", "xattrs"] {
        if !tables.iter().any(|t| t == needed.as_bytes()) {
            bail!("not a fileset manifest: no table named {needed}");
        }
    }
    Ok(())
}

/// SQLite follows a symlink at the name of the database and of the files
/// it keeps beside it. A manifest found in a tree is refused when it is one
/// (see `locate`); the files beside it are refused here.
fn refuse_linked_sidecars(path: &Path) -> Result<()> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        let side = PathBuf::from(name);
        if side
            .symlink_metadata()
            .is_ok_and(|m| !m.is_file() || crate::platform::is_link(&m))
        {
            bail!(
                "{} is not a regular file; refusing to open the manifest beside it",
                side.display()
            );
        }
    }
    Ok(())
}

impl SqliteReader {
    /// An idle connection, or a new one.
    fn take(&self) -> Result<Connection> {
        let idle = self.idle.lock().unwrap().pop();
        match idle {
            Some(c) => Ok(c),
            None => {
                let conn = Connection::open_with_flags(
                    &self.path,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )
                .with_context(|| format!("opening {}", self.path.display()))?;
                stable_plans(&conn)?;
                Ok(conn)
            }
        }
    }
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
                    unfinished: true,
                };
                migrate_with(&conn, MIGRATIONS, 0, |tx| write_meta_rows(tx, &initial))?;
                Ok(SqliteStore {
                    conn,
                    mode,
                    path: path.to_path_buf(),
                })
            }
            OpenMode::ReadWrite => {
                refuse_linked_sidecars(path)?;
                let conn = Connection::open_with_flags(
                    path,
                    OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )
                .with_context(|| format!("opening {}", path.display()))?;
                Self::configure(&conn)?;
                check_objects(&conn)?;
                let meta = read_meta(&conn)?;
                check_schema(&meta, mode)?;
                if meta.schema_version < SCHEMA_VERSION {
                    apply_migrations(&conn, MIGRATIONS, meta.schema_version)?;
                }
                Ok(SqliteStore {
                    conn,
                    mode,
                    path: path.to_path_buf(),
                })
            }
            OpenMode::ReadOnly => {
                refuse_linked_sidecars(path)?;
                let conn = Connection::open_with_flags(
                    path,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )
                .with_context(|| format!("opening {}", path.display()))?;
                stable_plans(&conn)?;
                check_objects(&conn)?;
                check_schema(&read_meta(&conn)?, mode)?;
                Ok(SqliteStore {
                    conn,
                    mode,
                    path: path.to_path_buf(),
                })
            }
        }
    }

    fn configure(conn: &Connection) -> Result<()> {
        stable_plans(conn)?;
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
        get_entry(&self.conn, path)
    }

    fn reader(&self) -> Result<Box<dyn Reader>> {
        Ok(Box::new(SqliteReader {
            path: self.path.clone(),
            idle: Mutex::new(Vec::new()),
        }))
    }

    fn for_each(&self, f: &mut dyn FnMut(&[u8], &Entry) -> Result<()>) -> Result<()> {
        // One statement for entries and their xattrs: a row per xattr, or one row
        // without. Both tables are keyed by path, so nothing is sorted.
        let sql = format!(
            "SELECT e.path, {ENTRY_COLS}, x.name, x.value FROM entries e \
             LEFT JOIN xattrs x ON x.path = e.path ORDER BY e.path, x.name"
        );
        let mut st = self.conn.prepare(&sql)?;
        let mut rows = st.query([])?;
        let mut current: Option<(Vec<u8>, Entry)> = None;
        while let Some(row) = rows.next()? {
            let path: Vec<u8> = row.get(0)?;
            if current.as_ref().is_some_and(|(p, _)| *p != path) {
                if let Some((p, e)) = current.take() {
                    // A sound table gives paths in rising order. A damaged one
                    // can hand back an earlier path, and then never end: after
                    // a lookup on this connection SQLite finds its place in
                    // this scan again by key, which in a misordered table is
                    // a row already visited.
                    anyhow::ensure!(
                        p < path,
                        "manifest is damaged: entries out of order at {}",
                        crate::escape::escape(&path)
                    );
                    f(&p, &e)?;
                }
            }
            let (_, e) = match &mut current {
                Some(c) => c,
                None => current.insert((path, row_to_entry(row, 1)?)),
            };
            if let Some(name) = row.get::<_, Option<Vec<u8>>>(18)? {
                e.xattrs.push((name, row.get(19)?));
            }
        }
        if let Some((p, e)) = current {
            f(&p, &e)?;
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

    fn sweep(&mut self, keep: &dyn Fn(&[u8]) -> bool) -> Result<u64> {
        let tx = self.conn.transaction()?;
        let mut deleted = 0u64;
        {
            // Pages through the keys rather than deleting under an open cursor; the empty
            // blob sorts before every path.
            let mut page =
                tx.prepare("SELECT path FROM entries WHERE path > ? ORDER BY path LIMIT ?")?;
            let mut del = tx.prepare("DELETE FROM entries WHERE path = ?")?;
            let mut delx = tx.prepare("DELETE FROM xattrs WHERE path = ?")?;
            let mut after: Vec<u8> = Vec::new();
            loop {
                let paths: Vec<Vec<u8>> = page
                    .query_map(params![after, SWEEP_CHUNK as i64], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                let Some(last) = paths.last() else { break };
                after = last.clone();
                for p in paths.iter().filter(|p| !keep(p)) {
                    delx.execute([p])?;
                    del.execute([p])?;
                    deleted += 1;
                }
            }
        }
        tx.commit()?;
        Ok(deleted)
    }

    fn set_meta(&mut self, meta: &Meta) -> Result<()> {
        write_meta(&self.conn, meta)
    }

    fn close(self: Box<Self>) -> Result<Option<String>> {
        let mut note = None;
        if self.mode != OpenMode::ReadOnly {
            // A reader elsewhere pins the WAL: the checkpoint reports busy and the
            // journal mode cannot change back. The data is committed either way.
            let busy: i64 = self
                .conn
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
            let journal: String = match self
                .conn
                .query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))
            {
                Ok(mode) => mode,
                Err(rusqlite::Error::SqliteFailure(f, _))
                    if f.code == rusqlite::ErrorCode::DatabaseBusy =>
                {
                    "wal".to_string()
                }
                Err(e) => return Err(e.into()),
            };
            if busy != 0 || !journal.eq_ignore_ascii_case("delete") {
                note = Some(format!(
                    "another process has {} open, so its -wal sidecar stays; the next update \
                     folds it in",
                    self.path.display()
                ));
            }
        }
        self.conn.close().map_err(|(_, e)| e)?;
        Ok(note)
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
    fn close_notes_a_reader_that_keeps_the_wal_from_being_folded_in() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("m.sqlite");
        let mut s = open(&p, Backend::Sqlite, OpenMode::Create).unwrap();
        s.put_batch(&[(b"a".to_vec(), dir_entry())]).unwrap();
        // Another process reads the manifest meanwhile: a read transaction pins the WAL.
        let reader = Connection::open(&p).unwrap();
        reader
            .execute_batch("BEGIN; SELECT count(*) FROM entries;")
            .unwrap();
        let note = s.close().unwrap().expect("close says the WAL stayed");
        assert!(
            note.contains("another process") && note.contains("-wal"),
            "{note}"
        );
        assert!(dir.path().join("m.sqlite-wal").exists());
        drop(reader);
        // Alone, the next close folds it in.
        let s = open(&p, Backend::Sqlite, OpenMode::ReadWrite).unwrap();
        assert_eq!(s.close().unwrap(), None);
        assert!(!dir.path().join("m.sqlite-wal").exists());
    }

    #[test]
    fn sweep_count_is_exact_across_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = open(&dir.path().join("m"), Backend::Sqlite, OpenMode::Create).unwrap();
        let mut with_xattr = dir_entry();
        with_xattr.xattrs = vec![(b"user.a".to_vec(), b"1".to_vec())];
        let items: Vec<_> = (0..5000u32)
            .map(|i| (format!("k{i:05}").into_bytes(), with_xattr.clone()))
            .collect();
        s.put_batch(&items).unwrap();
        let seen = |k: &[u8]| k.last().is_some_and(|d| (d - b'0').is_multiple_of(3));
        let kept = items.iter().filter(|(k, _)| seen(k)).count();
        assert_eq!(s.sweep(&seen).unwrap(), (5000 - kept) as u64);
        let mut n = 0;
        s.for_each(&mut |k, e| {
            assert!(seen(k));
            assert_eq!(e.xattrs.len(), 1);
            n += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(n, kept);
        let s = s.close();
        assert!(s.is_ok());
        // The swept entries' xattrs went with them.
        let conn = Connection::open(dir.path().join("m")).unwrap();
        let xattrs: i64 = conn
            .query_row("SELECT count(*) FROM xattrs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(xattrs as usize, kept);
    }

    fn dir_entry() -> Entry {
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
        }
    }

    /// The statements prepared on `conn` that SQLite has re-prepared
    /// since, with how often.
    fn reprepared(conn: &Connection) -> Vec<(String, i32)> {
        use rusqlite::ffi;
        let mut out = Vec::new();
        // SAFETY: `handle` is this open connection's raw pointer; `conn`
        // is borrowed for the whole walk and a rusqlite `Connection` is not
        // `Sync`, so no other thread uses it meanwhile. The statements
        // `sqlite3_next_stmt` lists stay prepared (in rusqlite's cache)
        // while the connection is borrowed, since nothing here finalizes
        // one. `sqlite3_sql` is not null for a statement rusqlite prepared,
        // and the text stays valid until the statement is finalized; it is
        // copied out before the next call. Only SQL text and counters are
        // read.
        unsafe {
            let db = conn.handle();
            let mut st = ffi::sqlite3_next_stmt(db, std::ptr::null_mut());
            while !st.is_null() {
                let n = ffi::sqlite3_stmt_status(st, ffi::SQLITE_STMTSTATUS_REPREPARE, 0);
                if n > 0 {
                    let sql = std::ffi::CStr::from_ptr(ffi::sqlite3_sql(st));
                    out.push((sql.to_string_lossy().into_owned(), n));
                }
                st = ffi::sqlite3_next_stmt(db, st);
            }
        }
        out
    }

    #[test]
    fn lookups_and_writes_are_never_re_prepared() {
        // A statement whose plan depends on a bound value is prepared again
        // by SQLite on every new binding (see `stable_plans`), which costs
        // as much as the lookup it serves. The statements run once per
        // entry, with varied bindings, must stay prepared: on the store's
        // own connection and on a reader's.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("m.sqlite");
        let mut s = SqliteStore::open(&p, OpenMode::Create).unwrap();
        let mut with_xattrs = dir_entry();
        with_xattrs.xattrs = vec![
            (b"user.a".to_vec(), b"1".to_vec()),
            (b"user.b".to_vec(), b"2".to_vec()),
        ];
        let items: Vec<_> = (0..40u32)
            .map(|i| {
                let e = if i % 2 == 0 {
                    with_xattrs.clone()
                } else {
                    dir_entry()
                };
                (format!("d/f{i:02}").into_bytes(), e)
            })
            .collect();
        for batch in items.chunks(7) {
            s.put_batch(batch).unwrap();
        }
        let reader = SqliteReader {
            path: p.clone(),
            idle: Mutex::new(Vec::new()),
        };
        let paths: Vec<&[u8]> = items.iter().map(|(k, _)| &k[..]).collect();
        for (k, e) in &items {
            assert_eq!(s.get(k).unwrap().unwrap().xattrs, e.xattrs);
            assert_eq!(reader.get(k).unwrap().unwrap().xattrs, e.xattrs);
            assert!(s.get(b"d/none").unwrap().is_none());
        }
        assert_eq!(reader.get_many(&paths).unwrap().len(), items.len());
        let none = Vec::<(String, i32)>::new();
        assert_eq!(reprepared(&s.conn), none);
        let idle = reader.idle.lock().unwrap();
        assert_eq!(idle.len(), 1);
        assert_eq!(reprepared(&idle[0]), none);
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
        for suffix in ["-wal", "-shm", "-journal"] {
            let sidecar = dir.path().join(format!("m.sqlite{suffix}"));
            assert!(!sidecar.exists(), "{} left behind", sidecar.display());
        }
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
