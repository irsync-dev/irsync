mod common;

use fileset::model::{Entry, Flags, Kind, Timespec};
use fileset::store::{self, Backend, Meta, OpenMode, Store, SCHEMA_VERSION};
use std::path::Path;

fn entry(generation: u64) -> Entry {
    Entry {
        kind: Kind::File,
        mode: 0o644,
        uid: 1000,
        gid: 100,
        size: u64::MAX - 5,
        nlink: 1,
        ino: 42,
        dev: 7,
        rdev: 0,
        mtime: Timespec {
            sec: 1_700_000_000,
            nsec: 123,
        },
        ctime: Timespec { sec: -5, nsec: 999 },
        blake3: Some([9u8; 32]),
        target: Some(b"tgt".to_vec()),
        xattrs: vec![
            (b"user.a".to_vec(), b"1".to_vec()),
            (b"user.b".to_vec(), vec![0, 255]),
        ],
        flags: Flags::STORE,
        generation,
    }
}

fn suite(backend: Backend, open: impl Fn(&Path, OpenMode) -> Box<dyn Store>) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("m");

    // fresh create meta
    let mut s = open(&p, OpenMode::Create);
    let m = s.meta().unwrap();
    assert_eq!(m.schema_version, SCHEMA_VERSION);
    assert_eq!(m.generation, 0);
    assert!(
        m.unfinished,
        "a manifest being created says so until set_meta clears it"
    );

    // put/get round trip
    let e = entry(1);
    s.put_batch(&[(b"x".to_vec(), e.clone())]).unwrap();
    assert_eq!(s.get(b"x").unwrap(), Some(e));
    assert_eq!(s.get(b"nope").unwrap(), None);

    // xattrs replaced wholesale
    let mut e2 = entry(1);
    e2.xattrs = vec![(b"user.c".to_vec(), b"z".to_vec())];
    s.put_batch(&[(b"x".to_vec(), e2.clone())]).unwrap();
    assert_eq!(s.get(b"x").unwrap().unwrap().xattrs, e2.xattrs);

    // byte order
    s.put_batch(&[
        (b"b".to_vec(), entry(2)),
        (b"a/c".to_vec(), entry(2)),
        (b"a".to_vec(), entry(2)),
    ])
    .unwrap();
    let mut keys = Vec::new();
    s.for_each(&mut |k, _| {
        keys.push(k.to_vec());
        Ok(())
    })
    .unwrap();
    assert_eq!(
        keys,
        vec![b"a".to_vec(), b"a/c".to_vec(), b"b".to_vec(), b"x".to_vec()]
    );

    // sweep: only x was not seen
    assert_eq!(s.sweep(&|k| k != b"x").unwrap(), 1);
    assert_eq!(s.get(b"x").unwrap(), None);
    assert!(s.get(b"a").unwrap().is_some());

    // meta round trip
    let m2 = Meta {
        schema_version: SCHEMA_VERSION,
        generation: 2,
        tool_version: "t".into(),
        created_at: 10,
        updated_at: 20,
        unfinished: false,
    };
    s.set_meta(&m2).unwrap();
    assert_eq!(s.meta().unwrap(), m2);
    s.close().unwrap();

    // create refuses what exists
    let err = store::open(&p, backend, OpenMode::Create).err().unwrap();
    assert!(err.to_string().contains("already exists"), "{err}");
    assert_eq!(
        store::open(&p, backend, OpenMode::ReadOnly)
            .unwrap()
            .meta()
            .unwrap(),
        m2
    );

    // reopen read-only
    let s = open(&p, OpenMode::ReadOnly);
    assert!(s.get(b"a/c").unwrap().is_some());
    assert!(!s.meta().unwrap().unfinished);
    s.close().unwrap();

    // newer schema is refused, for reading and for writing
    let mut s = open(&p, OpenMode::ReadWrite);
    let mut m3 = m2.clone();
    m3.schema_version = SCHEMA_VERSION + 1;
    s.set_meta(&m3).unwrap();
    s.close().unwrap();
    for mode in [OpenMode::ReadOnly, OpenMode::ReadWrite] {
        let err = store::open(&p, backend, mode).err().unwrap();
        assert!(err.to_string().contains("newer"), "{mode:?}: {err}");
    }
}

#[test]
fn sqlite_suite() {
    suite(Backend::Sqlite, |p, mode| {
        store::open(p, Backend::Sqlite, mode).unwrap()
    });
}

#[test]
fn fjall_suite() {
    suite(Backend::Fjall, |p, mode| {
        store::open(p, Backend::Fjall, mode).unwrap()
    });
}

/// A reader shared by several threads sees what the open store has committed, and once
/// dropped it does not keep the store from closing cleanly.
fn reader_suite(backend: Backend) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("m");
    let mut s = store::open(&p, backend, OpenMode::Create).unwrap();
    s.put_batch(&[(b"a".to_vec(), entry(1))]).unwrap();
    let reader = s.reader().unwrap();
    s.put_batch(&[(b"b".to_vec(), entry(2))]).unwrap();
    std::thread::scope(|sc| {
        for _ in 0..4 {
            sc.spawn(|| {
                for _ in 0..50 {
                    assert_eq!(reader.get(b"a").unwrap(), Some(entry(1)));
                    assert_eq!(reader.get(b"b").unwrap(), Some(entry(2)));
                    assert_eq!(reader.get(b"c").unwrap(), None);
                    assert_eq!(
                        reader.get_many(&[b"c", b"b", b"a"]).unwrap(),
                        vec![None, Some(entry(2)), Some(entry(1))]
                    );
                    assert_eq!(reader.get_many(&[]).unwrap(), vec![]);
                }
            });
        }
    });
    drop(reader);
    assert_eq!(s.close().unwrap(), None);
    if backend == Backend::Sqlite {
        assert!(!dir.path().join("m-wal").exists());
    }
}

#[test]
fn sqlite_reader() {
    reader_suite(Backend::Sqlite);
}

#[test]
fn fjall_reader() {
    reader_suite(Backend::Fjall);
}
