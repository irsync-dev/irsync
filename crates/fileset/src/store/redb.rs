//! redb backend: the manifest is one file, and after every `init` and `update` it is
//! compacted and closed cleanly, so that it is as small as it gets and can be read from
//! a device that cannot be written to.
//!
//! Entries are stored as the fjall backend stores them, the path as the key and the entry
//! as protobuf, with a checksum of both at the end (see `sealed`).

use super::fjall::{decode, to_pb};
use super::{
    check_schema, parse_platform, parse_unfinished, unfinished_text, Meta, OpenMode, Reader, Store,
    SCHEMA_VERSION,
};
use crate::model::Entry;
use crate::platform::{self, Platform};
use anyhow::{anyhow, bail, Context, Result};
use prost::Message;
use redb::{
    Builder, CompactionMode, Database, DatabaseError, Durability, ReadOnlyDatabase,
    ReadTransaction, ReadableDatabase, ReadableTable, TableDefinition, WriteTransaction,
};
use std::cell::Cell;
use std::io::Read;
use std::ops::Bound;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

const ENTRIES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("entries");
const META: TableDefinition<&str, &str> = TableDefinition::new("meta");

/// How a redb file starts.
const MAGIC: [u8; 9] = [b'r', b'e', b'd', b'b', 0x1A, 0x0A, 0xA9, 0x0D, 0x0A];

/// The most passes `compact` makes.
const COMPACTION_PASSES: usize = 8;

/// Keys read per page by `sweep`.
const SWEEP_CHUNK: usize = 2000;

/// The longest that entries are written without being forced to disk. An `update` that is
/// killed loses what it wrote since, and no more: the next one finds the rest hashed.
const UNSYNCED: Duration = Duration::from_secs(1);

/// Whether the file at `path` starts the way a redb file does. A file that cannot be
/// read does not, but for one case: where byte-range locks are mandatory (Windows),
/// redb open for writing locks every byte of its file, and a read refused by such a lock
/// is taken for redb. redb's own open then says that another fileset has it.
pub fn is_redb_file(path: &Path) -> bool {
    let mut start = [0u8; MAGIC.len()];
    match std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut start)) {
        Ok(()) => start == MAGIC,
        Err(e) => platform::is_lock_violation(&e),
    }
}

/// Runs `f`, which reads the manifest through redb. redb trusts the pages it reads: it
/// verifies their checksums in a repair and in the check an open for writing asks for,
/// and its own documentation says that a plain open of an altered file may panic. A
/// manifest can come with a tree someone else made, or sit on a medium that rots, so
/// such a panic is an error here.
///
/// The message Rust prints for such a panic is kept off stderr: the error says what
/// there is to say. That holds only for a panic raised in redb's own code while `f`
/// runs on this thread. Any other panic is a fault of ours and prints as usual.
fn guarded<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let print = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let redb = info.location().is_some_and(|l| in_redb(l.file()));
            if !(redb && GUARDED.get() > 0) {
                print(info);
            }
        }));
    });
    GUARDED.set(GUARDED.get() + 1);
    let caught = catch_unwind(AssertUnwindSafe(f));
    GUARDED.set(GUARDED.get() - 1);
    match caught {
        Ok(r) => r,
        Err(_) => bail!("the manifest is damaged: its redb database cannot be read"),
    }
}

thread_local! {
    /// How many `guarded` calls this thread is inside.
    static GUARDED: Cell<u32> = const { Cell::new(0) };
}

/// Whether `file`, the source file a panic names, is one of redb's: Cargo unpacks the
/// crate into a directory called `redb-` and its version.
fn in_redb(file: &str) -> bool {
    file.split(['/', '\\'])
        .any(|part| part.starts_with("redb-"))
}

/// The database, as `init` and `update` open it or as the commands that only read do.
enum Db {
    Write(Database),
    Read(ReadOnlyDatabase),
}

impl Db {
    fn begin_read(&self) -> Result<ReadTransaction> {
        Ok(match self {
            Db::Write(db) => db.begin_read()?,
            Db::Read(db) => db.begin_read()?,
        })
    }

    fn get(&self, path: &[u8]) -> Result<Option<Entry>> {
        guarded(|| get_in(&self.begin_read()?, path))
    }

    fn get_many(&self, paths: &[&[u8]]) -> Result<Vec<Option<Entry>>> {
        guarded(|| {
            let txn = self.begin_read()?;
            paths.iter().map(|p| get_in(&txn, p)).collect()
        })
    }
}

fn get_in(txn: &ReadTransaction, path: &[u8]) -> Result<Option<Entry>> {
    let table = txn.open_table(ENTRIES)?;
    let found = table.get(path)?;
    let entry = found.map(|v| unseal(path, v.value())).transpose()?;
    Ok(entry.map(|(e, _)| e))
}

/// Bytes of checksum at the end of every stored entry.
const SEAL: usize = 4;

/// The meta row that holds the digest of all entries, as 32 hex digits.
const DIGEST_KEY: &str = "entries_digest";

/// The BLAKE3 hash of a path and the entry stored for it. Its start is the entry's seal,
/// which ties the entry to its path. Its end is the entry's mark.
///
/// The marks of all entries, combined with XOR, are the digest in the meta table. Every
/// write keeps it current in the transaction that changes the entries, and a read of
/// all entries compares it. The seals cannot tell when an entry is gone, or is the one
/// an earlier run stored, in a file that is otherwise sound; the digest can.
fn hash_of(path: &[u8], record: &[u8]) -> ([u8; SEAL], u128) {
    let mut h = blake3::Hasher::new();
    h.update(&(path.len() as u64).to_le_bytes());
    h.update(path);
    h.update(record);
    let hash = h.finalize();
    let (seal, _) = hash
        .as_bytes()
        .split_first_chunk()
        .expect("a hash is 32 bytes");
    let (_, mark) = hash
        .as_bytes()
        .split_last_chunk()
        .expect("a hash is 32 bytes");
    (*seal, u128::from_le_bytes(*mark))
}

/// The digest in `meta`, the meta table.
fn read_digest(meta: &impl ReadableTable<&'static str, &'static str>) -> Result<u128> {
    let found = meta.get(DIGEST_KEY)?.ok_or_else(|| {
        anyhow!(
            "manifest is damaged, or an older fileset made it: it lacks the digest of \
             its entries. Run 'fileset init' again"
        )
    })?;
    u128::from_str_radix(found.value(), 16)
        .map_err(|_| anyhow!("manifest is damaged: the digest of its entries is not a number"))
}

/// Applies `change`, the marks of the entries a transaction added and removed, to the
/// digest.
fn shift_digest(txn: &WriteTransaction, change: u128) -> Result<()> {
    if change != 0 {
        let mut meta = txn.open_table(META)?;
        let digest = read_digest(&meta)? ^ change;
        meta.insert(DIGEST_KEY, format!("{digest:032x}").as_str())?;
    }
    Ok(())
}

/// The entry as it is stored: its protobuf encoding, then its seal.
///
/// redb verifies the checksums of its pages when it repairs a file or opens one for
/// writing, not when it reads one. A manifest on a medium that cannot be written to is
/// only ever read, so without the seal a flipped bit in it came back as an entry that
/// was a little different.
///
/// With it comes the entry's mark.
fn sealed(path: &[u8], e: &Entry) -> (Vec<u8>, u128) {
    let mut bytes = to_pb(e).encode_to_vec();
    let (seal, mark) = hash_of(path, &bytes);
    bytes.extend_from_slice(&seal);
    (bytes, mark)
}

/// The protobuf encoding in a stored entry whose seal holds, and the entry's mark.
fn open_seal<'a>(path: &[u8], bytes: &'a [u8]) -> Result<(&'a [u8], u128)> {
    let damaged = || {
        anyhow!(
            "manifest is damaged: the entry of {} does not match its checksum",
            crate::escape::escape(path)
        )
    };
    let (record, seal) = bytes.split_last_chunk::<SEAL>().ok_or_else(damaged)?;
    let (want, mark) = hash_of(path, record);
    if *seal != want {
        return Err(damaged());
    }
    Ok((record, mark))
}

fn unseal(path: &[u8], bytes: &[u8]) -> Result<(Entry, u128)> {
    let (record, mark) = open_seal(path, bytes)?;
    Ok((decode(record)?, mark))
}

/// Shares the open database. Every lookup is a read transaction of its own, so it sees
/// what the store has committed by then.
struct RedbReader(Arc<Db>);

impl Reader for RedbReader {
    fn get(&self, path: &[u8]) -> Result<Option<Entry>> {
        self.0.get(path)
    }

    fn get_many(&self, paths: &[&[u8]]) -> Result<Vec<Option<Entry>>> {
        self.0.get_many(paths)
    }
}

pub struct RedbStore {
    db: Arc<Db>,
    path: PathBuf,
    /// When what was written was last forced to disk.
    synced: Instant,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn read_meta(db: &Db) -> Result<Meta> {
    guarded(|| {
        let txn = db.begin_read()?;
        let table = txn.open_table(META)?;
        read_digest(&table)?;
        let get = |key: &str| -> Result<Option<String>> {
            Ok(table.get(key)?.map(|v| v.value().to_string()))
        };
        let need = |key: &str| -> Result<String> {
            get(key)?.ok_or_else(|| anyhow!("manifest meta key '{key}' missing"))
        };
        Ok(Meta {
            schema_version: need("schema_version")?.parse()?,
            generation: need("generation")?.parse()?,
            tool_version: need("tool_version")?,
            created_at: need("created_at")?.parse()?,
            updated_at: need("updated_at")?.parse()?,
            unfinished: parse_unfinished(&need("unfinished")?)?,
            platform: parse_platform(&need("platform")?)?,
        })
    })
}

/// What an open says about a file redb will not open as it is.
fn open_error(path: &Path, mode: OpenMode, e: DatabaseError) -> anyhow::Error {
    match e {
        // An open for reading repairs nothing, and the file lacks what a clean close
        // writes last.
        DatabaseError::RepairAborted if mode == OpenMode::ReadOnly => anyhow!(
            "{} was not closed cleanly, or is damaged: if an init or update on it was \
             interrupted, it must be repaired before it can be read. Where the manifest can \
             be written, run 'fileset update', or 'fileset init' again if it was an init",
            path.display()
        ),
        DatabaseError::RepairAborted => anyhow!(
            "{} is damaged: its redb database cannot be repaired",
            path.display()
        ),
        DatabaseError::DatabaseAlreadyOpen if mode == OpenMode::ReadOnly => anyhow!(
            "{} is open in another fileset that is writing it",
            path.display()
        ),
        // A writer needs the file alone, and a reader holds it too.
        DatabaseError::DatabaseAlreadyOpen => {
            anyhow!("{} is open in another fileset", path.display())
        }
        e => anyhow::Error::new(e).context(format!("opening {}", path.display())),
    }
}

impl RedbStore {
    pub fn open(path: &Path, mode: OpenMode) -> Result<RedbStore> {
        let exists = path.symlink_metadata().is_ok();
        match mode {
            OpenMode::Create if exists => bail!("manifest already exists: {}", path.display()),
            OpenMode::ReadWrite | OpenMode::ReadOnly if !exists => {
                bail!("manifest not found: {}", path.display())
            }
            // redb makes a database of an empty file it is pointed at; refuse before that,
            // so a wrong --manifest is never written to.
            OpenMode::ReadWrite | OpenMode::ReadOnly if !is_redb_file(path) => bail!(
                "{} is not a fileset manifest (no redb database found)",
                path.display()
            ),
            _ => {}
        }
        let db = guarded(|| {
            Ok(match mode {
                OpenMode::Create => Db::Write(
                    Database::create(path)
                        .with_context(|| format!("creating {}", path.display()))?,
                ),
                // The file may have come with a tree: every page's checksum is verified
                // before anything is written on top of it.
                OpenMode::ReadWrite => Db::Write(
                    Builder::new()
                        .set_check_integrity_on_open(true)
                        .open(path)
                        .map_err(|e| open_error(path, mode, e))?,
                ),
                OpenMode::ReadOnly => Db::Read(
                    Builder::new()
                        .open_read_only(path)
                        .map_err(|e| open_error(path, mode, e))?,
                ),
            })
        })?;
        let mut store = RedbStore {
            db: Arc::new(db),
            path: path.to_path_buf(),
            synced: Instant::now(),
        };
        if mode == OpenMode::Create {
            let t = now();
            store.write_meta(
                &Meta {
                    schema_version: SCHEMA_VERSION,
                    generation: 0,
                    tool_version: env!("CARGO_PKG_VERSION").to_string(),
                    created_at: t,
                    updated_at: t,
                    unfinished: true,
                    platform: Platform::CURRENT,
                },
                true,
            )?;
        } else {
            let mut m = read_meta(&store.db)
                .map_err(|e| e.context(format!("{} is not a fileset manifest", path.display())))?;
            check_schema(&m, mode)?;
            if mode == OpenMode::ReadWrite && m.schema_version < SCHEMA_VERSION {
                // No redb migrations exist yet; just record the current version.
                m.schema_version = SCHEMA_VERSION;
                store.write_meta(&m, false)?;
            }
        }
        Ok(store)
    }

    fn writable(&self) -> Result<&Database> {
        match &*self.db {
            Db::Write(db) => Ok(db),
            Db::Read(_) => bail!("{} is open read-only", self.path.display()),
        }
    }

    /// Whether this commit is to be forced to disk: every one would cost an `fsync` per
    /// batch of entries, none would lose a killed run all it had hashed.
    fn durability(&mut self) -> Durability {
        if self.synced.elapsed() >= UNSYNCED {
            self.synced = Instant::now();
            Durability::Immediate
        } else {
            Durability::None
        }
    }

    /// Writes all meta rows and forces them, and everything written before, to disk. With
    /// `create`, the entries table is made too.
    fn write_meta(&mut self, m: &Meta, create: bool) -> Result<()> {
        let txn = self.writable()?.begin_write()?;
        {
            let mut t = txn.open_table(META)?;
            t.insert("schema_version", m.schema_version.to_string().as_str())?;
            t.insert("generation", m.generation.to_string().as_str())?;
            t.insert("tool_version", m.tool_version.as_str())?;
            t.insert("created_at", m.created_at.to_string().as_str())?;
            t.insert("updated_at", m.updated_at.to_string().as_str())?;
            t.insert("platform", m.platform.as_str())?;
            t.insert("unfinished", unfinished_text(m.unfinished))?;
            if create {
                txn.open_table(ENTRIES)?;
                t.insert(DIGEST_KEY, format!("{:032x}", 0).as_str())?;
            }
        }
        txn.commit()?;
        self.synced = Instant::now();
        Ok(())
    }
}

impl Store for RedbStore {
    fn meta(&self) -> Result<Meta> {
        read_meta(&self.db)
    }

    fn get(&self, path: &[u8]) -> Result<Option<Entry>> {
        self.db.get(path)
    }

    fn reader(&self) -> Result<Box<dyn Reader>> {
        Ok(Box::new(RedbReader(self.db.clone())))
    }

    fn for_each(&self, f: &mut dyn FnMut(&[u8], &Entry) -> Result<()>) -> Result<()> {
        let (txn, table) = guarded(|| {
            let txn = self.db.begin_read()?;
            let table = txn.open_table(ENTRIES)?;
            Ok((txn, table))
        })?;
        let mut rows = guarded(|| Ok(table.iter()?))?;
        let mut last: Option<Vec<u8>> = None;
        let mut digest = 0;
        loop {
            // Only the read is guarded: what `f` does is not redb's.
            let entry = guarded(|| {
                let Some(row) = rows.next() else {
                    return Ok(None);
                };
                let (k, v) = row?;
                let path = k.value();
                // A sound table gives paths in rising order. A crafted one can hand one
                // back again, and then never end.
                anyhow::ensure!(
                    last.as_deref().is_none_or(|p| p < path),
                    "manifest is damaged: entries out of order at {}",
                    crate::escape::escape(path)
                );
                let prev = last.get_or_insert_with(Vec::new);
                prev.clear();
                prev.extend_from_slice(path);
                let (entry, mark) = unseal(path, v.value())?;
                digest ^= mark;
                Ok(Some(entry))
            })?;
            let (Some(entry), Some(path)) = (entry, last.as_deref()) else {
                break;
            };
            f(path, &entry)?;
        }
        drop(rows);
        drop(table);
        let written = guarded(|| read_digest(&txn.open_table(META)?))?;
        drop(txn);
        anyhow::ensure!(
            digest == written,
            "manifest is damaged: entries are missing or are not the ones last written"
        );
        Ok(())
    }

    fn put_batch(&mut self, items: &[(Vec<u8>, Entry)]) -> Result<()> {
        let durability = self.durability();
        let mut txn = self.writable()?.begin_write()?;
        txn.set_durability(durability)?;
        let mut change = 0;
        {
            let mut t = txn.open_table(ENTRIES)?;
            for (path, e) in items {
                let (bytes, mark) = sealed(path, e);
                change ^= mark;
                if let Some(old) = t.insert(path.as_slice(), bytes.as_slice())? {
                    change ^= open_seal(path, old.value())?.1;
                }
            }
        }
        shift_digest(&txn, change)?;
        txn.commit()?;
        Ok(())
    }

    fn sweep(&mut self, keep: &dyn Fn(&[u8]) -> bool) -> Result<u64> {
        let mut deleted = 0u64;
        let mut change = 0;
        let mut txn = self.writable()?.begin_write()?;
        // The meta rows that end a run are written next, and force this to disk too.
        txn.set_durability(Durability::None)?;
        {
            // Finds the paths to drop a page of keys at a time, then removes them one by
            // one. `retain` would do it in one call, but with it an update that dropped
            // a thousandth of a million entries doubled the file.
            let mut t = txn.open_table(ENTRIES)?;
            let mut after: Option<Vec<u8>> = None;
            let mut doomed: Vec<Vec<u8>> = Vec::new();
            loop {
                let mut scanned = 0;
                {
                    let from = match &after {
                        Some(k) => Bound::Excluded(k.as_slice()),
                        None => Bound::Unbounded,
                    };
                    for row in t.range((from, Bound::Unbounded))?.take(SWEEP_CHUNK) {
                        let (k, _) = row?;
                        scanned += 1;
                        if !keep(k.value()) {
                            doomed.push(k.value().to_vec());
                        }
                        if scanned == SWEEP_CHUNK {
                            after = Some(k.value().to_vec());
                        }
                    }
                }
                deleted += doomed.len() as u64;
                for k in doomed.drain(..) {
                    if let Some(old) = t.remove(k.as_slice())? {
                        change ^= open_seal(&k, old.value())?.1;
                    }
                }
                if scanned < SWEEP_CHUNK {
                    break;
                }
            }
        }
        shift_digest(&txn, change)?;
        txn.commit()?;
        Ok(deleted)
    }

    fn set_meta(&mut self, meta: &Meta) -> Result<()> {
        self.write_meta(meta, false)
    }

    /// redb uses the pages an update frees again, but never gives them back: without
    /// this, a manifest only grows.
    fn compact(&mut self) -> Result<()> {
        let db = match Arc::get_mut(&mut self.db) {
            Some(Db::Write(db)) => db,
            // Nothing was written.
            Some(Db::Read(_)) => return Ok(()),
            None => bail!(
                "cannot compact {}: it is still being read",
                self.path.display()
            ),
        };
        // Maximum also packs half-full pages, for the smallest file. A pass says whether
        // it did anything, and the one after a pass that did has always said no; the
        // bound is for a redb that would not.
        for _ in 0..COMPACTION_PASSES {
            if !db.compact(CompactionMode::Maximum)? {
                break;
            }
        }
        Ok(())
    }

    fn close(self: Box<Self>) -> Result<Option<String>> {
        let path = self.path;
        let db = Arc::try_unwrap(self.db)
            .map_err(|_| anyhow!("cannot close {}: it is still being read", path.display()))?;
        match db {
            // Writes what a later open for reading needs, and flushes it.
            Db::Write(db) => db.close()?,
            Db::Read(db) => db.close()?,
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Flags, Kind, Timespec};
    use crate::store::{open, Backend};

    const BACKEND: Backend = Backend::Redb;

    fn set_raw_platform(p: &Path, v: Option<&str>) {
        let db = Database::open(p).unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut t = txn.open_table(META).unwrap();
            match v {
                Some(v) => drop(t.insert("platform", v).unwrap()),
                None => drop(t.remove("platform").unwrap()),
            }
        }
        txn.commit().unwrap();
    }

    fn new_manifest(dir: &Path) -> PathBuf {
        let p = dir.join("m");
        open(&p, BACKEND, OpenMode::Create)
            .unwrap()
            .close()
            .unwrap();
        p
    }

    #[test]
    fn a_new_manifest_records_this_platform() {
        let dir = tempfile::tempdir().unwrap();
        let p = new_manifest(dir.path());
        let s = open(&p, BACKEND, OpenMode::ReadOnly).unwrap();
        assert_eq!(s.meta().unwrap().platform, Platform::CURRENT);
    }

    #[test]
    fn a_manifest_without_the_key_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = new_manifest(dir.path());
        set_raw_platform(&p, None);
        let e = open(&p, BACKEND, OpenMode::ReadOnly)
            .err()
            .expect("a manifest without a platform was accepted");
        let e = format!("{e:#}");
        assert!(e.contains("manifest meta key 'platform' missing"), "{e}");
    }

    #[test]
    fn an_unknown_platform_is_refused() {
        for v in ["Windows", "unix ", "", "macos"] {
            let dir = tempfile::tempdir().unwrap();
            let p = new_manifest(dir.path());
            set_raw_platform(&p, Some(v));
            let e = open(&p, BACKEND, OpenMode::ReadOnly)
                .err()
                .unwrap_or_else(|| panic!("{v:?} was accepted"));
            let e = format!("{e:#}");
            let want = format!("manifest platform '{v}' is not known to this fileset");
            assert!(e.contains(&want), "{e}");
        }
    }

    #[test]
    fn set_meta_round_trips_the_platform() {
        for want in [Platform::Windows, Platform::Unix] {
            let dir = tempfile::tempdir().unwrap();
            let p = new_manifest(dir.path());
            let mut s = open(&p, BACKEND, OpenMode::ReadWrite).unwrap();
            let mut m = s.meta().unwrap();
            m.platform = want;
            s.set_meta(&m).unwrap();
            assert_eq!(s.meta().unwrap().platform, want);
            s.close().unwrap();
            let s = open(&p, BACKEND, OpenMode::ReadOnly).unwrap();
            assert_eq!(s.meta().unwrap().platform, want);
        }
    }
    use std::collections::HashMap;

    fn entry(i: u64) -> Entry {
        Entry {
            kind: Kind::File,
            mode: 0o644,
            uid: 1000,
            gid: 1000,
            size: i,
            nlink: 1,
            ino: i,
            dev: 1,
            rdev: 0,
            mtime: Timespec { sec: 1, nsec: 2 },
            ctime: Timespec { sec: 3, nsec: 4 },
            blake3: Some([7; 32]),
            target: None,
            xattrs: vec![(b"user.a".to_vec(), vec![1; 40])],
            flags: Flags::default(),
            generation: 1,
        }
    }

    fn items(n: u64) -> Vec<(Vec<u8>, Entry)> {
        (0..n)
            .map(|i| (format!("d{:02}/f{i:05}", i % 20).into_bytes(), entry(i)))
            .collect()
    }

    /// A finished, compacted manifest holding `items`.
    fn manifest(path: &Path, items: &[(Vec<u8>, Entry)]) {
        let mut s = open(path, Backend::Redb, OpenMode::Create).unwrap();
        s.put_batch(items).unwrap();
        let mut m = s.meta().unwrap();
        m.unfinished = false;
        s.set_meta(&m).unwrap();
        s.compact().unwrap();
        s.close().unwrap();
    }

    #[test]
    fn only_a_panic_in_redbs_own_source_is_redbs() {
        assert!(in_redb(
            "/home/me/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/redb-5.0.0/src/tree_store/btree.rs"
        ));
        assert!(in_redb(
            r"C:\Users\me\.cargo\registry\src\x\redb-5.0.0\src\db.rs"
        ));
        assert!(!in_redb(file!()), "{}", file!());
        assert!(!in_redb("/rustc/abc/library/core/src/slice/index.rs"));
    }

    /// A panic of our own inside a guarded read still prints: only redb's are kept quiet.
    #[test]
    fn a_panic_that_is_not_redbs_is_still_printed() {
        let _ = guarded(|| Ok(())); // installs the hook this test wraps
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let inner = Arc::new(std::panic::take_hook());
        let (log, print) = (seen.clone(), inner.clone());
        std::panic::set_hook(Box::new(move |info| {
            log.lock().unwrap().push(info.to_string());
            print(info);
        }));
        let err = guarded::<()>(|| panic!("ours")).unwrap_err();
        std::panic::set_hook(Box::new(move |info| inner(info)));
        assert!(err.to_string().contains("damaged"), "{err}");
        assert!(seen.lock().unwrap().iter().any(|m| m.contains("ours")));
    }

    /// An entry that is gone, or is an older one, in a file that is sound to redb: each
    /// entry still matches its seal, so only the digest of all of them can tell.
    #[test]
    fn a_lost_or_stale_entry_is_found_by_a_full_read() {
        let dir = tempfile::tempdir().unwrap();
        let read_all = |p: &Path| -> Result<usize> {
            let s = RedbStore::open(p, OpenMode::ReadOnly)?;
            let mut n = 0;
            s.for_each(&mut |_, _| {
                n += 1;
                Ok(())
            })?;
            Ok(n)
        };
        let all = items(50);
        type Entries<'a> = redb::Table<'a, &'static [u8], &'static [u8]>;
        let tamper = |name: &str, change: &dyn Fn(&mut Entries)| {
            let p = dir.path().join(name);
            manifest(&p, &all);
            assert_eq!(read_all(&p).unwrap(), 50);
            let db = Database::open(&p).unwrap();
            let txn = db.begin_write().unwrap();
            change(&mut txn.open_table(ENTRIES).unwrap());
            txn.commit().unwrap();
            drop(db);
            read_all(&p).unwrap_err().to_string()
        };
        let (path, _) = &all[7];
        let err = tamper("lost", &|t| {
            t.remove(path.as_slice()).unwrap().unwrap();
        });
        assert!(err.contains("entries are missing or are not"), "{err}");
        let err = tamper("stale", &|t| {
            let (other, _) = sealed(path, &entry(999));
            t.insert(path.as_slice(), other.as_slice()).unwrap();
        });
        assert!(err.contains("entries are missing or are not"), "{err}");
    }

    /// What `update` does to a manifest keeps the digest that of its entries.
    #[test]
    fn the_digest_follows_replaced_and_swept_entries() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("m");
        let all = items(300);
        manifest(&p, &all);
        let mut s = RedbStore::open(&p, OpenMode::ReadWrite).unwrap();
        let changed: Vec<_> = all[..40]
            .iter()
            .map(|(k, _)| (k.clone(), entry(5000)))
            .collect();
        s.put_batch(&changed).unwrap();
        s.put_batch(&changed).unwrap();
        assert_eq!(s.sweep(&|k| k != all[100].0.as_slice()).unwrap(), 1);
        let mut n = 0;
        s.for_each(&mut |_, _| {
            n += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(n, 299);
        Box::new(s).close().unwrap();
        let s = RedbStore::open(&p, OpenMode::ReadOnly).unwrap();
        s.for_each(&mut |_, _| Ok(())).unwrap();
    }

    /// redb locks the file, in a way that differs between systems. Whatever the way:
    /// readers share a manifest, and a writer has it alone.
    #[test]
    fn readers_share_a_manifest_and_a_writer_has_it_alone() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("m");
        manifest(&p, &items(10));
        let refused = |mode| match RedbStore::open(&p, mode) {
            Ok(_) => panic!("opened {mode:?}"),
            Err(e) => e.to_string(),
        };

        let writer = RedbStore::open(&p, OpenMode::ReadWrite).unwrap();
        for mode in [OpenMode::ReadOnly, OpenMode::ReadWrite] {
            let err = refused(mode);
            assert!(
                err.contains("is open in another fileset"),
                "{mode:?}: {err}"
            );
        }
        Box::new(writer).close().unwrap();

        let first = RedbStore::open(&p, OpenMode::ReadOnly).unwrap();
        let second = RedbStore::open(&p, OpenMode::ReadOnly).unwrap();
        assert!(first.get(b"d03/f00003").unwrap().is_some());
        assert!(second.get(b"d03/f00003").unwrap().is_some());
        let err = refused(OpenMode::ReadWrite);
        assert!(err.contains("is open in another fileset"), "{err}");
        Box::new(first).close().unwrap();
        Box::new(second).close().unwrap();
        RedbStore::open(&p, OpenMode::ReadWrite).unwrap();
    }

    #[test]
    fn only_a_file_that_starts_like_redb_is_one() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("m");
        assert!(!is_redb_file(&p));
        std::fs::write(&p, b"SQLite format 3\0").unwrap();
        assert!(!is_redb_file(&p));
        std::fs::write(&p, b"redb").unwrap();
        assert!(!is_redb_file(&p));
        assert!(!is_redb_file(dir.path()));
        std::fs::remove_file(&p).unwrap();
        manifest(&p, &items(3));
        assert!(is_redb_file(&p));
    }

    #[test]
    fn open_refuses_another_file_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("m");
        for content in [&b""[..], b"not a manifest"] {
            std::fs::write(&p, content).unwrap();
            for mode in [OpenMode::ReadOnly, OpenMode::ReadWrite] {
                let err = open(&p, Backend::Redb, mode).err().unwrap();
                assert!(
                    format!("{err:#}").contains("not a fileset manifest"),
                    "{mode:?}: {err:#}"
                );
                assert_eq!(std::fs::read(&p).unwrap(), content, "{mode:?} wrote");
            }
        }
    }

    #[test]
    fn the_manifest_is_one_file_with_nothing_beside_it() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("m.redb");
        manifest(&p, &items(500));
        let s = open(&p, Backend::Redb, OpenMode::ReadOnly).unwrap();
        assert!(s.get(b"d03/f00003").unwrap().is_some());
        s.close().unwrap();
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["m.redb"]);
    }

    #[test]
    fn an_entry_that_does_not_match_its_seal_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("m");
        manifest(&p, &items(3));
        // The entry of one path under the key of another, as a damaged page could hold it.
        let stored = sealed(b"d00/f00000", &entry(0));
        let db = Database::open(&p).unwrap();
        let txn = db.begin_write().unwrap();
        txn.open_table(ENTRIES)
            .unwrap()
            .insert(&b"d01/f00001"[..], stored.0.as_slice())
            .unwrap();
        txn.commit().unwrap();
        db.close().unwrap();
        let s = open(&p, Backend::Redb, OpenMode::ReadOnly).unwrap();
        assert_eq!(s.get(b"d00/f00000").unwrap(), Some(entry(0)));
        let err = s.get(b"d01/f00001").unwrap_err().to_string();
        assert!(err.contains("does not match its checksum"), "{err}");
        let err = s.for_each(&mut |_, _| Ok(())).unwrap_err().to_string();
        assert!(err.contains("d01/f00001"), "{err}");
    }

    #[test]
    fn a_manifest_not_closed_cleanly_is_refused_for_reading_and_repaired_for_writing() {
        let dir = tempfile::tempdir().unwrap();
        let killed = dir.path().join("killed");
        #[cfg(unix)]
        {
            let p = dir.path().join("m");
            let mut s = open(&p, Backend::Redb, OpenMode::Create).unwrap();
            s.put_batch(&items(100)).unwrap();
            let mut m = s.meta().unwrap();
            m.unfinished = false;
            s.set_meta(&m).unwrap();
            // The file as a killed update leaves it: written, forced to disk, never closed.
            std::fs::copy(&p, &killed).unwrap();
            s.close().unwrap();
        }
        // Here the writer's locks keep every other handle from reading its file, so the
        // copy above cannot be made: a child process writes it and exits unclosed.
        #[cfg(windows)]
        {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "store::redb::tests::child_leaves_a_killed_update_manifest",
                    "--exact",
                ])
                .env(KILLED_AT, &killed)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "{status}");
        }

        let err = open(&killed, Backend::Redb, OpenMode::ReadOnly)
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("was not closed cleanly"), "{err}");
        assert!(err.contains("fileset update"), "{err}");
        let before = std::fs::read(&killed).unwrap();
        assert_eq!(std::fs::read(&killed).unwrap(), before, "the refusal wrote");

        let s = open(&killed, Backend::Redb, OpenMode::ReadWrite).unwrap();
        assert_eq!(s.get(b"d01/f00001").unwrap(), Some(entry(1)));
        s.close().unwrap();
        let s = open(&killed, Backend::Redb, OpenMode::ReadOnly).unwrap();
        assert!(!s.meta().unwrap().unfinished);
        assert_eq!(s.get(b"d19/f00099").unwrap(), Some(entry(99)));
    }

    /// Where `child_leaves_a_killed_update_manifest` writes.
    #[cfg(windows)]
    const KILLED_AT: &str = "FILESET_TEST_KILLED_AT";

    /// Not a test of its own: run in a child process by
    /// `a_manifest_not_closed_cleanly_is_refused_for_reading_and_repaired_for_writing`,
    /// it writes at `KILLED_AT` the manifest a killed update leaves (written, forced to
    /// disk, never closed) and exits. Without `KILLED_AT` it does nothing.
    #[cfg(windows)]
    #[test]
    fn child_leaves_a_killed_update_manifest() {
        let Some(p) = std::env::var_os(KILLED_AT) else {
            return;
        };
        let mut s = open(Path::new(&p), Backend::Redb, OpenMode::Create).unwrap();
        s.put_batch(&items(100)).unwrap();
        let mut m = s.meta().unwrap();
        m.unfinished = false;
        s.set_meta(&m).unwrap();
        // Exiting runs no destructor: the store is not closed.
        std::process::exit(0);
    }

    /// The size of the manifest at `p`, and whether redb finds anything left to compact.
    fn size_and_compactable(p: &Path) -> (u64, bool) {
        let mut db = Database::open(p).unwrap();
        let more = db.compact(CompactionMode::Maximum).unwrap();
        db.close().unwrap();
        (std::fs::metadata(p).unwrap().len(), more)
    }

    #[test]
    fn compaction_leaves_nothing_to_compact_and_a_manifest_the_size_of_a_new_one() {
        let dir = tempfile::tempdir().unwrap();
        let all = items(20_000);
        // What an update leaves: a third of the entries rewritten, a fifth gone.
        let rewritten: Vec<_> = all
            .iter()
            .filter(|(_, e)| e.ino % 3 == 0)
            .map(|(k, e)| {
                let mut e = e.clone();
                e.mtime.sec = 9;
                e.xattrs.push((b"user.b".to_vec(), vec![2; 90]));
                (k.clone(), e)
            })
            .collect();
        let gone = |k: &[u8]| k.starts_with(b"d04/") || k.starts_with(b"d11/") || k.ends_with(b"7");
        let update = |p: &Path, compact: bool| {
            manifest(p, &all);
            let mut s = open(p, Backend::Redb, OpenMode::ReadWrite).unwrap();
            for batch in rewritten.chunks(700) {
                s.put_batch(batch).unwrap();
            }
            assert!(s.sweep(&|k| !gone(k)).unwrap() > 3000);
            s.set_meta(&s.meta().unwrap()).unwrap();
            if compact {
                s.compact().unwrap();
            }
            s.close().unwrap();
            std::fs::metadata(p).unwrap().len()
        };
        let (compacted, left) = (dir.path().join("compacted"), dir.path().join("left"));
        let small = update(&compacted, true);
        let large = update(&left, false);
        assert!(small < large, "{small} against {large} without compaction");

        // The same entries, written once.
        let fresh = dir.path().join("fresh");
        let now: HashMap<Vec<u8>, Entry> = all
            .iter()
            .chain(&rewritten)
            .filter(|(k, _)| !gone(k))
            .cloned()
            .collect();
        let mut now: Vec<_> = now.into_iter().collect();
        now.sort_by(|a, b| a.0.cmp(&b.0));
        manifest(&fresh, &now);
        let fresh_size = std::fs::metadata(&fresh).unwrap().len();
        // Not to the byte: where the pages split differs. Within a hundredth.
        assert!(
            small <= fresh_size + fresh_size / 100,
            "{small} against {fresh_size} written at once"
        );

        assert_eq!(size_and_compactable(&compacted), (small, false));
        // Left as it was, it still holds everything, and can be compacted later.
        let s = open(&left, Backend::Redb, OpenMode::ReadOnly).unwrap();
        let mut n = 0;
        s.for_each(&mut |k, e| {
            assert_eq!(
                now.binary_search_by(|(nk, _)| nk.as_slice().cmp(k))
                    .map(|i| &now[i].1),
                Ok(e)
            );
            n += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(n, now.len());
        s.close().unwrap();
        assert!(size_and_compactable(&left).1);
    }

    #[test]
    fn sweep_count_is_exact_across_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = open(&dir.path().join("m"), Backend::Redb, OpenMode::Create).unwrap();
        let all = items(3 * SWEEP_CHUNK as u64 + 17);
        s.put_batch(&all).unwrap();
        let keep = |k: &[u8]| k.last().is_some_and(|d| (d - b'0').is_multiple_of(3));
        let kept = all.iter().filter(|(k, _)| keep(k)).count();
        assert_eq!(s.sweep(&keep).unwrap(), (all.len() - kept) as u64);
        let mut n = 0;
        s.for_each(&mut |k, _| {
            assert!(keep(k));
            n += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(n, kept);
        assert_eq!(s.sweep(&|_| false).unwrap(), kept as u64);
        assert_eq!(s.sweep(&|_| false).unwrap(), 0);
    }

    /// A manifest can come from anywhere, and a medium can rot. Whatever is done to the
    /// file, reading it gives the entries that were stored or an error: never other
    /// entries, a panic, or a read that does not end.
    #[test]
    fn a_damaged_manifest_gives_an_error_or_what_was_stored() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good");
        let all = items(2000);
        manifest(&good, &all);
        let want: HashMap<Vec<u8>, Entry> = all.iter().cloned().collect();
        let bytes = std::fs::read(&good).unwrap();
        let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let bad = dir.path().join("bad");
        let (mut read, mut refused) = (0, 0);
        for case in 0..600u32 {
            let mut b = bytes.clone();
            let at = (rnd() as usize) % b.len();
            match case % 4 {
                // A few flipped bits.
                0 => {
                    for _ in 0..1 + rnd() % 3 {
                        let i = (rnd() as usize) % b.len();
                        b[i] ^= 1 << (rnd() % 8);
                    }
                }
                // A zeroed stretch, up to a page.
                1 => {
                    let n = ((rnd() % 4096) as usize).min(b.len() - at);
                    b[at..at + n].fill(0);
                }
                // Cut short.
                2 => b.truncate(at.max(MAGIC.len())),
                // A stretch of noise.
                _ => {
                    let n = ((rnd() % 64) as usize).min(b.len() - at);
                    b[at..at + n].fill_with(|| rnd() as u8);
                }
            }
            std::fs::write(&bad, &b).unwrap();
            let mode = if case % 8 < 4 {
                OpenMode::ReadOnly
            } else {
                OpenMode::ReadWrite
            };
            let result = (|| -> Result<()> {
                let s = open(&bad, Backend::Redb, mode)?;
                s.meta()?;
                let mut n = 0;
                s.for_each(&mut |k, e| {
                    assert_eq!(want.get(k), Some(e), "case {case} ({mode:?})");
                    n += 1;
                    Ok(())
                })?;
                assert_eq!(n, want.len(), "case {case} ({mode:?})");
                assert_eq!(s.get(b"d03/f00003")?.as_ref(), want.get(&b"d03/f00003"[..]));
                Ok(())
            })();
            match result {
                Ok(()) => read += 1,
                Err(_) => refused += 1,
            }
        }
        // Both happen: damage where nothing is stored is harmless, the rest is refused.
        assert!(read > 0 && refused > 300, "read {read}, refused {refused}");
    }
}
