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
        kind: Kind::from_u8(p.kind as u8),
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
    })
}

fn write_meta(db: &Database, ks: &Keyspace, m: &Meta) -> Result<()> {
    let mut b = db.batch();
    b.insert(ks, "schema_version", m.schema_version.to_string());
    b.insert(ks, "generation", m.generation.to_string());
    b.insert(ks, "tool_version", m.tool_version.as_str());
    b.insert(ks, "created_at", m.created_at.to_string());
    b.insert(ks, "updated_at", m.updated_at.to_string());
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

    fn sweep(&mut self, keep: u64) -> Result<u64> {
        let mut doomed: Vec<Vec<u8>> = Vec::new();
        for guard in self.entries.range::<&[u8], _>(..) {
            let (k, v) = guard.into_inner()?;
            if decode(&v)?.generation != keep {
                doomed.push(k.to_vec());
            }
        }
        let mut b = self.db.batch();
        for k in &doomed {
            b.remove(&self.entries, k.as_slice());
        }
        b.commit()?;
        Ok(doomed.len() as u64)
    }

    fn set_meta(&mut self, meta: &Meta) -> Result<()> {
        write_meta(&self.db, &self.meta, meta)
    }

    fn close(self: Box<Self>) -> Result<()> {
        self.db.persist(PersistMode::SyncAll)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{open, Backend};

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
        assert_eq!(s.sweep(2).unwrap(), 2);
        let mut keys = Vec::new();
        s.for_each(&mut |k, _| {
            keys.push(k.to_vec());
            Ok(())
        })
        .unwrap();
        assert_eq!(keys, vec![b"c".to_vec()]);
    }
}
