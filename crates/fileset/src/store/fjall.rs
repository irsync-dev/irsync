//! fjall (LSM) backend with protobuf-encoded entry values.

use super::{check_schema, Meta, OpenMode, Store, SCHEMA_VERSION};
use crate::model::{Entry, Flags, Kind, Timespec};
use anyhow::{anyhow, bail, Context, Result};
use fjall::{Database, Keyspace, KeyspaceCreateOptions, PersistMode};
use prost::Message;
use std::path::Path;

#[allow(clippy::all)]
pub mod pb {
    include!(concat!(env!("OUT_DIR"), "/fileset.v1.rs"));
}

/// Keys deleted per batch by `sweep`.
const SWEEP_CHUNK: usize = 2000;

pub struct FjallStore {
    db: Database,
    entries: Keyspace,
    meta: Keyspace,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn to_pb(e: &Entry) -> pb::Entry {
    pb::Entry {
        kind: e.kind.as_u8() as u32,
        mode: e.mode,
        uid: e.uid,
        gid: e.gid,
        size: e.size,
        nlink: e.nlink,
        ino: e.ino,
        dev: e.dev,
        rdev: e.rdev,
        mtime_sec: e.mtime.sec,
        mtime_nsec: e.mtime.nsec,
        ctime_sec: e.ctime.sec,
        ctime_nsec: e.ctime.nsec,
        blake3: e.blake3.map(|b| b.to_vec()),
        target: e.target.clone(),
        xattrs: e
            .xattrs
            .iter()
            .map(|(n, v)| pb::Xattr {
                name: n.clone(),
                value: v.clone(),
            })
            .collect(),
        flags: e.flags.0,
        generation: e.generation,
    }
}

fn from_pb(p: pb::Entry) -> Result<Entry> {
    let blake3 = match p.blake3 {
        Some(b) => Some(
            <[u8; 32]>::try_from(b.as_slice())
                .map_err(|_| anyhow!("blake3 is {} bytes, expected 32", b.len()))?,
        ),
        None => None,
    };
    Ok(Entry {
        kind: Kind::from_u8(
            u8::try_from(p.kind).map_err(|_| anyhow!("entry kind {} out of range", p.kind))?,
        ),
        mode: p.mode,
        uid: p.uid,
        gid: p.gid,
        size: p.size,
        nlink: p.nlink,
        ino: p.ino,
        dev: p.dev,
        rdev: p.rdev,
        mtime: Timespec {
            sec: p.mtime_sec,
            nsec: p.mtime_nsec,
        },
        ctime: Timespec {
            sec: p.ctime_sec,
            nsec: p.ctime_nsec,
        },
        blake3,
        target: p.target,
        xattrs: p.xattrs.into_iter().map(|x| (x.name, x.value)).collect(),
        flags: Flags(p.flags),
        generation: p.generation,
    })
}

fn decode(bytes: &[u8]) -> Result<Entry> {
    from_pb(pb::Entry::decode(bytes).context("decoding entry")?)
}

fn read_meta(ks: &Keyspace) -> Result<Meta> {
    let get = |key: &str| -> Result<String> {
        let v = ks
            .get(key)?
            .ok_or_else(|| anyhow!("manifest meta key '{key}' missing"))?;
        Ok(String::from_utf8(v.to_vec())?)
    };
    Ok(Meta {
        schema_version: get("schema_version")?.parse()?,
        generation: get("generation")?.parse()?,
        tool_version: get("tool_version")?,
        created_at: get("created_at")?.parse()?,
        updated_at: get("updated_at")?.parse()?,
        unfinished: ks.get("unfinished")?.is_some(),
    })
}

fn write_meta(db: &Database, ks: &Keyspace, m: &Meta) -> Result<()> {
    let mut b = db.batch();
    b.insert(ks, "schema_version", m.schema_version.to_string());
    b.insert(ks, "generation", m.generation.to_string());
    b.insert(ks, "tool_version", m.tool_version.as_str());
    b.insert(ks, "created_at", m.created_at.to_string());
    b.insert(ks, "updated_at", m.updated_at.to_string());
    if m.unfinished {
        b.insert(ks, "unfinished", "1");
    } else {
        b.remove(ks, "unfinished");
    }
    b.commit()?;
    Ok(())
}

impl FjallStore {
    pub fn open(path: &Path, mode: OpenMode) -> Result<FjallStore> {
        match mode {
            OpenMode::Create if path.exists() => {
                bail!("manifest already exists: {}", path.display())
            }
            OpenMode::ReadWrite | OpenMode::ReadOnly if !path.exists() => {
                bail!("manifest not found: {}", path.display())
            }
            // fjall creates a database in any directory it is pointed at; refuse before
            // that, so a wrong --manifest never writes into it.
            OpenMode::ReadWrite | OpenMode::ReadOnly
                if !path.join("version").is_file() || !path.join("keyspaces").is_dir() =>
            {
                bail!(
                    "{} is not a fileset manifest (no fjall database found)",
                    path.display()
                )
            }
            _ => {}
        }
        let db = Database::builder(path)
            .open()
            .with_context(|| format!("opening {}", path.display()))?;
        let entries = db.keyspace("entries", KeyspaceCreateOptions::default)?;
        let meta = db.keyspace("meta", KeyspaceCreateOptions::default)?;
        if mode == OpenMode::Create {
            let t = now();
            write_meta(
                &db,
                &meta,
                &Meta {
                    schema_version: SCHEMA_VERSION,
                    generation: 0,
                    tool_version: env!("CARGO_PKG_VERSION").to_string(),
                    created_at: t,
                    updated_at: t,
                    unfinished: true,
                },
            )?;
        } else {
            let mut m = read_meta(&meta)?;
            check_schema(&m, mode)?;
            if mode == OpenMode::ReadWrite && m.schema_version < SCHEMA_VERSION {
                // No fjall migrations exist yet; just record the current version.
                m.schema_version = SCHEMA_VERSION;
                write_meta(&db, &meta, &m)?;
            }
        }
        Ok(FjallStore { db, entries, meta })
    }
}

impl Store for FjallStore {
    fn meta(&self) -> Result<Meta> {
        read_meta(&self.meta)
    }

    fn get(&self, path: &[u8]) -> Result<Option<Entry>> {
        self.entries.get(path)?.map(|v| decode(&v)).transpose()
    }

    fn for_each(&self, f: &mut dyn FnMut(&[u8], &Entry) -> Result<()>) -> Result<()> {
        for guard in self.entries.range::<&[u8], _>(..) {
            let (k, v) = guard.into_inner()?;
            f(&k, &decode(&v)?)?;
        }
        Ok(())
    }

    fn put_batch(&mut self, items: &[(Vec<u8>, Entry)]) -> Result<()> {
        let mut b = self.db.batch();
        for (path, e) in items {
            b.insert(&self.entries, path.as_slice(), to_pb(e).encode_to_vec());
        }
        b.commit()?;
        Ok(())
    }

    fn sweep(&mut self, keep: &dyn Fn(&[u8]) -> bool) -> Result<u64> {
        // Deletes as it goes, one bounded batch at a time, so memory does not grow with
        // the tree. Removing keys behind the iterator does not disturb it (snapshot read).
        let mut deleted = 0u64;
        let mut chunk: Vec<Vec<u8>> = Vec::with_capacity(SWEEP_CHUNK);
        let commit = |db: &Database, entries: &Keyspace, chunk: &mut Vec<Vec<u8>>| {
            let mut b = db.batch();
            for k in chunk.drain(..) {
                b.remove(entries, k);
            }
            b.commit()
        };
        for guard in self.entries.range::<&[u8], _>(..) {
            let k = guard.key()?;
            if !keep(&k) {
                chunk.push(k.to_vec());
                deleted += 1;
                if chunk.len() >= SWEEP_CHUNK {
                    commit(&self.db, &self.entries, &mut chunk)?;
                }
            }
        }
        if !chunk.is_empty() {
            commit(&self.db, &self.entries, &mut chunk)?;
        }
        Ok(deleted)
    }

    fn set_meta(&mut self, meta: &Meta) -> Result<()> {
        write_meta(&self.db, &self.meta, meta)
    }

    fn close(self: Box<Self>) -> Result<Option<String>> {
        self.db.persist(PersistMode::SyncAll)?;
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{open, Backend};

    #[test]
    fn decode_refuses_a_kind_that_is_no_byte() {
        let mut p = pb::Entry {
            kind: 0x101, // would truncate to 1, a regular file
            ..Default::default()
        };
        let e = decode(&p.encode_to_vec()).unwrap_err().to_string();
        assert!(e.contains("kind") && e.contains("257"), "{e}");
        p.kind = 7;
        assert_eq!(decode(&p.encode_to_vec()).unwrap().kind, Kind::Other);
    }

    fn full_entry(generation: u64) -> Entry {
        Entry {
            kind: Kind::Symlink,
            mode: 0o120777,
            uid: 1,
            gid: 2,
            size: u64::MAX,
            nlink: 3,
            ino: 4,
            dev: 5,
            rdev: 6,
            mtime: Timespec { sec: -7, nsec: 8 },
            ctime: Timespec { sec: 9, nsec: 10 },
            blake3: Some([0xab; 32]),
            target: Some(b"t".to_vec()),
            xattrs: vec![
                (b"user.a".to_vec(), b"1".to_vec()),
                (b"user.b".to_vec(), vec![0, 255]),
            ],
            flags: Flags(3),
            generation,
        }
    }

    #[test]
    fn entry_proto_round_trip() {
        let e = full_entry(11);
        let bytes = to_pb(&e).encode_to_vec();
        let back = from_pb(pb::Entry::decode(bytes.as_slice()).unwrap()).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn bad_blake3_length_is_an_error() {
        let mut p = to_pb(&full_entry(1));
        p.blake3 = Some(vec![1, 2, 3]);
        assert!(from_pb(p).is_err());
    }

    #[test]
    fn sweep_scans_all_keys() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = open(&dir.path().join("m"), Backend::Fjall, OpenMode::Create).unwrap();
        s.put_batch(&[
            (b"a".to_vec(), full_entry(1)),
            (b"b".to_vec(), full_entry(1)),
            (b"c".to_vec(), full_entry(2)),
        ])
        .unwrap();
        assert_eq!(s.sweep(&|k| k == b"c").unwrap(), 2);
        let mut keys = Vec::new();
        s.for_each(&mut |k, _| {
            keys.push(k.to_vec());
            Ok(())
        })
        .unwrap();
        assert_eq!(keys, vec![b"c".to_vec()]);
    }

    #[test]
    fn open_refuses_a_plain_directory_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        for mode in [OpenMode::ReadOnly, OpenMode::ReadWrite] {
            let err = open(dir.path(), Backend::Fjall, mode).err().unwrap();
            assert!(
                format!("{err:#}").contains("not a fileset manifest"),
                "{mode:?}: {err:#}"
            );
            let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
            assert!(left.is_empty(), "{mode:?} wrote {left:?}");
        }
    }

    #[test]
    fn old_schema_is_refused_read_only_and_bumped_read_write() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("m");
        let mut s = open(&p, Backend::Fjall, OpenMode::Create).unwrap();
        let mut m = s.meta().unwrap();
        m.schema_version = SCHEMA_VERSION - 1;
        m.unfinished = false;
        s.set_meta(&m).unwrap();
        s.close().unwrap();
        let err = open(&p, Backend::Fjall, OpenMode::ReadOnly).err().unwrap();
        assert!(err.to_string().contains("run 'fileset update'"), "{err}");
        // No fjall migrations exist yet: opening for writing only records the version.
        let s = open(&p, Backend::Fjall, OpenMode::ReadWrite).unwrap();
        assert_eq!(s.meta().unwrap().schema_version, SCHEMA_VERSION);
        s.close().unwrap();
        let s = open(&p, Backend::Fjall, OpenMode::ReadOnly).unwrap();
        assert_eq!(s.meta().unwrap().schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn sweep_count_is_exact_across_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = open(&dir.path().join("m"), Backend::Fjall, OpenMode::Create).unwrap();
        let items: Vec<_> = (0..5000u32)
            .map(|i| (format!("k{i:05}").into_bytes(), full_entry(1)))
            .collect();
        s.put_batch(&items).unwrap();
        let seen = |k: &[u8]| k.last().is_some_and(|d| (d - b'0').is_multiple_of(3));
        let kept = items.iter().filter(|(k, _)| seen(k)).count();
        assert_eq!(s.sweep(&seen).unwrap(), (5000 - kept) as u64);
        let mut n = 0;
        s.for_each(&mut |k, _| {
            assert!(seen(k));
            n += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(n, kept);
    }
}
