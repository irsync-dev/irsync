use irsync::fs::paths::RelPath;
use irsync::index::{Index, IndexError, Partial, SessionRow};
use irsync_proto::chunk::{digest_bytes, CHUNK_PARAMS};
use irsync_proto::v1::{FileEntry, Hash, Range, RangeSet};
use std::time::Duration;

fn rp(s: &str) -> RelPath {
    RelPath::new(s.as_bytes()).unwrap()
}

fn entry(path: &str, hash: Option<[u8; 32]>) -> FileEntry {
    FileEntry {
        path: path.as_bytes().to_vec(),
        size: 42,
        mtime_sec: 1_700_000_000,
        mtime_nsec: 123,
        mode: 0o644,
        uid: 1000,
        gid: 100,
        link_target: b"tgt".to_vec(),
        rdev: 7,
        dev: 8,
        inode: 9,
        nlink: 2,
        sparse: true,
        whole_hash: hash.map(|h| Hash { bytes: h.to_vec() }),
        ..Default::default()
    }
}

#[test]
fn open_creates_file_and_reopens() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("root");
    std::fs::create_dir(&root).unwrap();
    {
        let mut ix = Index::open(d.path(), &root).unwrap();
        ix.put_entry(&rp("a"), &entry("a", None), (1, 2), None)
            .unwrap();
    }
    let files: Vec<_> = std::fs::read_dir(d.path().join("index")).unwrap().collect();
    // One file per schema version: `<root hash>.v2.sqlite`.
    assert!(files.iter().any(|f| {
        let p = f.as_ref().unwrap().path();
        p.to_str().unwrap().ends_with(".v2.sqlite") && Index::is_current_file(&p)
    }));
    let ix = Index::open(d.path(), &root).unwrap();
    let pragma = |n: &str| -> String {
        let f = std::fs::read_dir(d.path().join("index"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.extension().unwrap() == "sqlite")
            .unwrap();
        let c = rusqlite::Connection::open(f).unwrap();
        c.query_row(&format!("PRAGMA {n}"), [], |r| {
            r.get::<_, rusqlite::types::Value>(0)
        })
        .map(|v| match v {
            rusqlite::types::Value::Integer(i) => i.to_string(),
            rusqlite::types::Value::Text(t) => t,
            _ => String::new(),
        })
        .unwrap()
    };
    assert_eq!(pragma("user_version"), "2");
    assert_eq!(pragma("journal_mode"), "wal");
    let got = ix.get_entry(&rp("a")).unwrap().unwrap();
    assert_eq!(got.ctime, (1, 2));
    assert!(got.digest.is_none());
    assert!(ix.get_entry(&rp("b")).unwrap().is_none());
}

#[test]
fn entry_roundtrip_with_digest() {
    let d = tempfile::tempdir().unwrap();
    let data: Vec<u8> = (0..3_000_000u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
        .collect();
    let dg = digest_bytes(&data);
    assert!(dg.chunks.len() > 1);
    let mut e = entry("dir/f", Some(dg.hash));
    e.size = dg.size;
    let mut ix = Index::open(d.path(), d.path()).unwrap();
    ix.put_entry(&rp("dir/f"), &e, (5, 6), Some(&dg)).unwrap();
    // replacing with the same path must not duplicate chunks
    ix.put_entry(&rp("dir/f"), &e, (5, 6), Some(&dg)).unwrap();
    let got = ix.get_entry(&rp("dir/f")).unwrap().unwrap();
    assert_eq!(got.entry, e);
    assert_eq!(got.ctime, (5, 6));
    assert_eq!(got.digest.unwrap(), dg);
    let c = &dg.chunks[1];
    assert_eq!(
        ix.find_chunk(&c.hash).unwrap(),
        Some((rp("dir/f"), c.offset))
    );
    assert_eq!(ix.find_chunk(&[9; 32]).unwrap(), None);
}

#[test]
fn hash_only_entry_has_no_digest() {
    let d = tempfile::tempdir().unwrap();
    let mut ix = Index::open(d.path(), d.path()).unwrap();
    let h = [5u8; 32];
    ix.put_entry(&rp("h"), &entry("h", Some(h)), (0, 0), None)
        .unwrap();
    assert!(ix.get_entry(&rp("h")).unwrap().unwrap().digest.is_none());
    assert_eq!(ix.find_paths_by_whole_hash(&h).unwrap(), vec![rp("h")]);
    // an explicit empty digest is distinguishable
    let dg = digest_bytes(b"");
    ix.put_entry(&rp("e"), &entry("e", Some(dg.hash)), (0, 0), Some(&dg))
        .unwrap();
    let got = ix.get_entry(&rp("e")).unwrap().unwrap().digest.unwrap();
    assert!(got.chunks.is_empty());
    // mismatched hash and bad length are rejected
    assert!(ix
        .put_entry(&rp("m"), &entry("m", Some([1; 32])), (0, 0), Some(&dg))
        .is_err());
    let mut bad = entry("b", None);
    bad.whole_hash = Some(Hash { bytes: vec![1; 5] });
    assert!(ix.put_entry(&rp("b"), &bad, (0, 0), None).is_err());
}

#[test]
fn find_by_whole_hash_returns_all_paths() {
    let d = tempfile::tempdir().unwrap();
    let mut ix = Index::open(d.path(), d.path()).unwrap();
    let h = [3u8; 32];
    for p in ["x", "y/z"] {
        ix.put_entry(&rp(p), &entry(p, Some(h)), (0, 0), None)
            .unwrap();
    }
    ix.put_entry(&rp("w"), &entry("w", Some([4; 32])), (0, 0), None)
        .unwrap();
    let mut got = ix.find_paths_by_whole_hash(&h).unwrap();
    got.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    assert_eq!(got, vec![rp("x"), rp("y/z")]);
}

#[test]
fn partial_roundtrip() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let d = tempfile::tempdir().unwrap();
    let p = Partial {
        expected_hash: [1; 32],
        size: 100,
        ranges: RangeSet {
            ranges: vec![
                Range {
                    offset: 0,
                    length: 10,
                },
                Range {
                    offset: 20,
                    length: 5,
                },
            ],
        },
        updated_at: now - 10 * 86400,
    };
    {
        let mut ix = Index::open(d.path(), d.path()).unwrap();
        ix.put_partial(&rp("p"), &p).unwrap();
        let fresh = Partial {
            updated_at: now,
            ..p.clone()
        };
        ix.put_partial(&rp("q"), &fresh).unwrap();
    }
    let mut ix = Index::open(d.path(), d.path()).unwrap();
    let got = ix.partial(&rp("p")).unwrap().unwrap();
    assert_eq!(got.expected_hash, [1; 32]);
    assert_eq!(got.size, 100);
    assert_eq!(got.ranges, p.ranges);
    assert_eq!(got.updated_at, now - 10 * 86400);
    assert_eq!(
        ix.partials_older_than(Duration::from_secs(7 * 86400))
            .unwrap(),
        vec![rp("p")]
    );
    ix.remove_partial(&rp("p")).unwrap();
    assert!(ix.partial(&rp("p")).unwrap().is_none());
}

#[test]
fn session_lookup_by_key_and_id() {
    let d = tempfile::tempdir().unwrap();
    let mut ix = Index::open(d.path(), d.path()).unwrap();
    let mut s = SessionRow {
        id: [7; 16],
        key: b"k".to_vec(),
        plan: vec![1],
        progress: vec![2],
        created_at: 10,
        updated_at: 20,
    };
    assert_eq!(ix.save_session(&s).unwrap(), None);
    s.progress = vec![3];
    assert_eq!(ix.save_session(&s).unwrap(), None);
    let other = SessionRow {
        id: [8; 16],
        updated_at: 30,
        ..s.clone()
    };
    assert_eq!(ix.save_session(&other).unwrap(), Some([7; 16]));
    assert!(ix.load_session_by_id(&[7; 16]).unwrap().is_none());
    assert_eq!(ix.save_session(&s).unwrap(), Some([8; 16]));
    let recent = SessionRow {
        id: [9; 16],
        key: b"r".to_vec(),
        updated_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
        ..s.clone()
    };
    ix.save_session(&recent).unwrap();
    assert_eq!(ix.load_session_by_id(&[7; 16]).unwrap().unwrap(), s);
    assert_eq!(
        ix.load_session_by_key(b"k").unwrap().unwrap().progress,
        vec![3]
    );
    assert!(ix.load_session_by_key(b"nope").unwrap().is_none());
    assert!(ix.load_session_by_id(&[0; 16]).unwrap().is_none());
    // updated_at=20 is ancient, so it is older than any duration
    assert_eq!(
        ix.delete_sessions_older_than(Duration::from_secs(60))
            .unwrap(),
        vec![[7; 16]]
    );
    assert!(ix.load_session_by_id(&[7; 16]).unwrap().is_none());
    assert!(ix.load_session_by_id(&[9; 16]).unwrap().is_some());
}

#[test]
fn chunk_params_mismatch_refuses_open() {
    let d = tempfile::tempdir().unwrap();
    drop(Index::open(d.path(), d.path()).unwrap());
    let f = std::fs::read_dir(d.path().join("index"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().unwrap() == "sqlite")
        .unwrap();
    let c = rusqlite::Connection::open(&f).unwrap();
    c.execute(
        "UPDATE meta SET value = ?1 WHERE key = 'chunk_avg'",
        [CHUNK_PARAMS.avg as i64 + 1],
    )
    .unwrap();
    drop(c);
    assert!(matches!(
        Index::open(d.path(), d.path()),
        Err(IndexError::ChunkParamsChanged)
    ));
}

#[test]
fn schema_version_mismatch_refuses_open() {
    let d = tempfile::tempdir().unwrap();
    drop(Index::open(d.path(), d.path()).unwrap());
    let f = std::fs::read_dir(d.path().join("index"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().unwrap() == "sqlite")
        .unwrap();
    let c = rusqlite::Connection::open(&f).unwrap();
    c.execute(
        "UPDATE meta SET value = value + 1 WHERE key = 'schema_version'",
        [],
    )
    .unwrap();
    drop(c);
    assert!(matches!(
        Index::open(d.path(), d.path()),
        Err(IndexError::SchemaVersionChanged)
    ));
}

/// `save_session` reads, then writes. Another process committing a write
/// in between must make it wait for the lock, not fail with a stale
/// snapshot (`SQLITE_BUSY`): its transaction takes the write lock up front.
#[test]
fn save_session_waits_out_another_writer() {
    let d = tempfile::tempdir().unwrap();
    let mut ix = Index::open(d.path(), d.path()).unwrap();
    let f = std::fs::read_dir(d.path().join("index"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().unwrap() == "sqlite")
        .unwrap();
    let (locked_tx, locked) = std::sync::mpsc::channel();
    let other = std::thread::spawn(move || {
        let c = rusqlite::Connection::open(&f).unwrap();
        c.execute_batch(
            "BEGIN IMMEDIATE; INSERT OR REPLACE INTO meta (key, value) VALUES ('x', 1);",
        )
        .unwrap();
        locked_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        c.execute_batch("COMMIT").unwrap();
    });
    locked.recv().unwrap();
    let s = SessionRow {
        id: [7; 16],
        key: b"k".to_vec(),
        plan: vec![1],
        progress: vec![2],
        created_at: 10,
        updated_at: 20,
    };
    assert_eq!(ix.save_session(&s).unwrap(), None);
    other.join().unwrap();
    assert!(ix.load_session_by_id(&[7; 16]).unwrap().is_some());
}

/// The index files under `state`.
fn index_files(state: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(state.join("index"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "sqlite"))
        .collect();
    v.sort();
    v
}

/// A root is keyed by its canonical path: through a symlink, and before it
/// exists (a dry run's destination, a removed root under `irsync gc`), it
/// opens the index it has once created.
#[test]
fn a_root_opens_one_index_however_it_is_named() {
    let (state, d) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let real = d.path().join("real");
    let root = real.join("a/b");
    drop(Index::open(state.path(), &root).unwrap());
    std::fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(&real, d.path().join("link")).unwrap();
    drop(Index::open(state.path(), &root).unwrap());
    drop(Index::open(state.path(), &d.path().join("link/a/b")).unwrap());
    drop(Index::open(state.path(), &d.path().join("link/a/./b")).unwrap());
    assert_eq!(index_files(state.path()).len(), 1);
    let file = &index_files(state.path())[0];
    assert_eq!(
        Index::root_of_file(file).unwrap().unwrap(),
        std::fs::canonicalize(&root).unwrap()
    );
}

/// A root that cannot be resolved (here: below a regular file, or a
/// missing one with `..` in it) is an error, not an index keyed by
/// whatever the path happened to say.
#[test]
fn an_unresolvable_root_refuses_to_open() {
    let (state, d) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let file = d.path().join("file");
    std::fs::write(&file, b"x").unwrap();
    assert!(matches!(
        Index::open(state.path(), &file.join("sub")),
        Err(IndexError::Io(_))
    ));
    assert!(matches!(
        Index::open(state.path(), &d.path().join("missing/../x")),
        Err(IndexError::Io(_))
    ));
}

#[test]
fn metered_handle_times_only_contended_locks() {
    use irsync::telemetry::meter::{Activity, Meter};
    use irsync::telemetry::{Role, Side};
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let plain = Index::open(d.path(), &root).unwrap().into_handle();
    let meter = Meter::new(Side::Daemon, Role::Receiver, None, true);
    let ix = plain.metered(meter.clone());
    drop(ix.lock().unwrap());
    assert_eq!(meter.usage().busy(Activity::IndexWait), Duration::ZERO);
    let held = plain.lock().unwrap();
    std::thread::scope(|s| {
        let waiter = s.spawn(|| drop(ix.lock().unwrap()));
        std::thread::sleep(Duration::from_millis(30));
        drop(held);
        waiter.join().unwrap();
    });
    let waited = meter.usage().busy(Activity::IndexWait);
    assert!(waited >= Duration::from_millis(25), "{waited:?}");
}

#[test]
fn remove_entry_forgets_the_entry_and_its_chunks() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let mut ix = Index::open(d.path(), &root).unwrap();
    let digest = digest_bytes(&[7u8; 4096]);
    let mut e = entry("a", Some(digest.hash));
    e.size = digest.size;
    ix.put_entry(&rp("a"), &e, (1, 2), Some(&digest)).unwrap();
    ix.put_entry(&rp("b"), &e, (1, 2), Some(&digest)).unwrap();
    let h = digest.chunks[0].hash;
    assert_eq!(ix.find_chunk(&h).unwrap().unwrap().0, rp("a"));
    ix.remove_entry(&rp("a")).unwrap();
    assert!(ix.get_entry(&rp("a")).unwrap().is_none());
    assert_eq!(ix.find_chunk(&h).unwrap().unwrap().0, rp("b"));
    ix.remove_entry(&rp("b")).unwrap();
    assert!(ix.find_chunk(&h).unwrap().is_none());
    // Nothing there: still fine.
    ix.remove_entry(&rp("b")).unwrap();
}

fn handle_with(paths: &[&str]) -> (tempfile::TempDir, irsync::index::IndexHandle, [u8; 32]) {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let h = Index::open(d.path(), &root).unwrap().into_handle();
    let digest = digest_bytes(&[3u8; 4096]);
    let mut e = entry("x", Some(digest.hash));
    e.size = digest.size;
    for p in paths {
        h.lock()
            .unwrap()
            .put_entry(&rp(p), &e, (1, 2), Some(&digest))
            .unwrap();
    }
    (d, h, digest.chunks[0].hash)
}

#[test]
fn reads_neither_wait_for_nor_take_the_index_lock() {
    let (_d, h, chunk) = handle_with(&["a"]);
    let held = h.lock().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = h.clone();
    std::thread::spawn(move || {
        let _ = tx.send(reader.read(|r| r.find_chunk(&chunk)).unwrap());
    });
    let found = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("a read waited for the index lock");
    assert_eq!(found.unwrap().0, rp("a"));
    drop(held);
}

#[test]
fn reads_see_committed_writes() {
    let (_d, h, chunk) = handle_with(&[]);
    assert!(h.read(|r| r.find_chunk(&chunk)).unwrap().is_none());
    let digest = digest_bytes(&[3u8; 4096]);
    let mut e = entry("b", Some(digest.hash));
    e.size = digest.size;
    h.lock()
        .unwrap()
        .put_entry(&rp("b"), &e, (1, 2), Some(&digest))
        .unwrap();
    assert_eq!(
        h.read(|r| r.find_chunk(&chunk)).unwrap().unwrap().0,
        rp("b")
    );
    let paths = h
        .read(|r| r.find_paths_by_whole_hash(&digest.hash))
        .unwrap();
    assert_eq!(paths, vec![rp("b")]);
}

#[test]
fn queued_entries_from_many_threads_land_by_flush() {
    let (_d, h, _) = handle_with(&[]);
    let digest = digest_bytes(&[5u8; 4096]);
    std::thread::scope(|s| {
        for i in 0..16 {
            let (h, digest) = (h.clone(), digest.clone());
            s.spawn(move || {
                let path = format!("f{i:02}");
                let mut e = entry(&path, Some(digest.hash));
                e.size = digest.size;
                h.queue_entry(irsync::index::NewEntry {
                    path: rp(&path),
                    entry: e,
                    ctime: (1, 2),
                    digest: Some(digest),
                });
                h.flush();
                // Written by the time the flush returns.
                assert!(h.read(|r| r.get_entry(&rp(&path))).unwrap().is_some());
            });
        }
    });
    let paths = h
        .read(|r| r.find_paths_by_whole_hash(&digest.hash))
        .unwrap();
    assert_eq!(paths.len(), 16);
}

#[test]
fn queueing_never_waits_for_the_index_lock() {
    let (_d, h, _) = handle_with(&[]);
    let digest = digest_bytes(&[6u8; 4096]);
    let mut e = entry("q", Some(digest.hash));
    e.size = digest.size;
    let held = h.lock().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let queuer = h.clone();
    std::thread::spawn(move || {
        queuer.queue_entry(irsync::index::NewEntry {
            path: rp("q"),
            entry: e,
            ctime: (1, 2),
            digest: Some(digest),
        });
        let _ = tx.send(());
    });
    rx.recv_timeout(Duration::from_secs(10))
        .expect("queueing waited for the index lock");
    drop(held);
    h.flush();
    assert!(h.read(|r| r.get_entry(&rp("q"))).unwrap().is_some());
}

#[test]
fn a_bad_entry_does_not_cost_its_batch() {
    let (_d, h, _) = handle_with(&[]);
    let digest = digest_bytes(&[8u8; 4096]);
    let held = h.lock().unwrap();
    // Queued while the writer waits for the lock: one batch.
    for (p, whole) in [("a", digest.hash), ("bad", [0u8; 32]), ("c", digest.hash)] {
        let mut e = entry(p, Some(whole));
        e.size = digest.size;
        h.queue_entry(irsync::index::NewEntry {
            path: rp(p),
            entry: e,
            ctime: (1, 2),
            digest: Some(digest.clone()),
        });
    }
    drop(held);
    h.flush();
    let has = |p| h.read(|r| r.get_entry(&rp(p))).unwrap().is_some();
    assert!(has("a") && has("c"));
    assert!(!has("bad"));
}

#[test]
fn find_chunks_lists_each_file_once_by_entry_id() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let mut ix = Index::open(d.path(), &root).unwrap();
    // The same chunk twice in each file.
    let digest = digest_bytes(&[4u8; 3 * 1024 * 1024]);
    assert!(digest.chunks.len() >= 2 && digest.chunks[0].hash == digest.chunks[1].hash);
    let mut e = entry("x", Some(digest.hash));
    e.size = digest.size;
    for p in ["c", "a", "b"] {
        ix.put_entry(&rp(p), &e, (1, 2), Some(&digest)).unwrap();
    }
    let h = digest.chunks[0].hash;
    let r = ix.reader();
    let page = |after: Option<i64>, limit| r.find_chunks(&h, after, limit).unwrap();
    // Each file once, at the chunk's first offset, in entry id order.
    let all = page(None, 9);
    let mut paths: Vec<_> = all.iter().map(|h| h.path.clone()).collect();
    paths.sort();
    assert_eq!(paths, vec![rp("a"), rp("b"), rp("c")]);
    assert!(all.windows(2).all(|w| w[0].entry_id < w[1].entry_id));
    assert!(all
        .iter()
        .all(|h| (h.offset, h.inode, h.size) == (0, e.inode, e.size)));
    // Pages pick up after the last holder's id.
    let first = page(None, 2);
    assert_eq!(first, all[..2]);
    assert_eq!(page(Some(first[1].entry_id), 2), all[2..]);
    assert_eq!(ix.find_chunk(&h).unwrap(), Some((all[0].path.clone(), 0)));
}

#[test]
fn lookups_are_never_re_prepared() {
    // A statement whose plan depends on a bound value is re-prepared by
    // SQLite on every new binding; that once cost a push more than the
    // lookups themselves. Every statement the per-file paths run, with
    // varied bindings, must stay prepared.
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let mut ix = Index::open(d.path(), &root).unwrap();
    let digest = digest_bytes(&[4u8; 3 * 1024 * 1024]);
    let mut e = entry("x", Some(digest.hash));
    e.size = digest.size;
    let paths: Vec<_> = (0..40).map(|i| rp(&format!("d/f{i:02}"))).collect();
    for p in &paths {
        ix.put_entry(p, &e, (1, 2), Some(&digest)).unwrap();
    }
    let h = digest.chunks[0].hash;
    let partial = Partial {
        expected_hash: digest.hash,
        size: digest.size,
        ranges: RangeSet {
            ranges: vec![Range {
                offset: 0,
                length: 1,
            }],
        },
        updated_at: 0,
    };
    for (i, p) in paths.iter().enumerate() {
        let r = ix.reader();
        for limit in [1, 16] {
            r.find_chunks(&h, None, limit).unwrap();
            r.find_chunks(&h, Some(i as i64), limit).unwrap();
        }
        r.find_chunk(&digest.chunks[i % digest.chunks.len()].hash)
            .unwrap();
        r.get_entry(p).unwrap();
        r.partial(p).unwrap();
        ix.put_partial(p, &partial).unwrap();
        ix.put_entry(p, &e, (1, i as u32), Some(&digest)).unwrap();
        ix.remove_partial(p).unwrap();
        ix.remove_entry_if(p, e.inode + 1, e.size).unwrap();
    }
    assert_eq!(ix.reader().reprepared(), Vec::<(String, i32)>::new());
}

#[test]
fn remove_entry_if_keeps_an_entry_recorded_since() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let mut ix = Index::open(d.path(), &root).unwrap();
    let digest = digest_bytes(&[7u8; 4096]);
    let mut e = entry("a", Some(digest.hash));
    e.size = digest.size;
    ix.put_entry(&rp("a"), &e, (1, 2), Some(&digest)).unwrap();
    // Judged stale as inode 8: the entry says 9, so it stays.
    ix.remove_entry_if(&rp("a"), 8, digest.size).unwrap();
    ix.remove_entry_if(&rp("a"), 9, digest.size + 1).unwrap();
    assert!(ix.get_entry(&rp("a")).unwrap().is_some());
    assert!(ix.find_chunk(&digest.chunks[0].hash).unwrap().is_some());
    ix.remove_entry_if(&rp("a"), 9, digest.size).unwrap();
    assert!(ix.get_entry(&rp("a")).unwrap().is_none());
    assert!(ix.find_chunk(&digest.chunks[0].hash).unwrap().is_none());
}

#[test]
fn queued_removals_and_entries_land_in_order() {
    let (_d, h, chunk) = handle_with(&["a", "b"]);
    let digest = digest_bytes(&[3u8; 4096]);
    let mut e = entry("a", Some(digest.hash));
    e.size = digest.size;
    e.inode = 10;
    h.queue_removal(rp("a"), 9, digest.size);
    h.queue_removal(rp("b"), 9, digest.size);
    // A new file at `b`, recorded after it was judged stale: it stays.
    h.queue_entry(irsync::index::NewEntry {
        path: rp("b"),
        entry: e,
        ctime: (1, 2),
        digest: Some(digest.clone()),
    });
    h.flush();
    let found = h.read(|r| r.find_chunks(&chunk, None, 9)).unwrap();
    let found: Vec<_> = found.into_iter().map(|h| (h.path, h.inode)).collect();
    assert_eq!(found, vec![(rp("b"), 10)]);
}

/// The index file of `root` in `state` (this version's), and the name a
/// version 1 build gave it.
fn v2_and_v1_names(state: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let v2 = std::fs::read_dir(state.join("index"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.to_str().unwrap().ends_with(".v2.sqlite"))
        .expect("a version 2 index file");
    let v1 = std::path::PathBuf::from(v2.to_str().unwrap().replace(".v2.sqlite", ".sqlite"));
    (v2, v1)
}

fn sidecars(file: &std::path::Path) -> Vec<std::path::PathBuf> {
    ["", "-wal", "-shm"]
        .iter()
        .map(|s| std::path::PathBuf::from(format!("{}{s}", file.display())))
        .filter(|p| p.exists())
        .collect()
}

#[test]
fn a_new_index_takes_the_partials_and_sessions_of_the_version_1_file() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let digest = digest_bytes(&[5u8; 4096]);
    let partial = Partial {
        expected_hash: digest.hash,
        size: 4096,
        ranges: RangeSet {
            ranges: vec![Range {
                offset: 0,
                length: 1024,
            }],
        },
        updated_at: 30,
    };
    let session = SessionRow {
        id: [7; 16],
        key: b"k".to_vec(),
        plan: vec![1],
        progress: vec![2],
        created_at: 10,
        updated_at: 20,
    };
    // An index with an entry, a partial and a session, then renamed to the
    // version 1 name: the copy reads only `partials` and `sessions`, whose
    // columns version 2 kept.
    {
        let mut ix = Index::open(d.path(), &root).unwrap();
        ix.put_entry(&rp("a"), &entry("a", None), (1, 2), None)
            .unwrap();
        ix.put_partial(&rp("p"), &partial).unwrap();
        ix.save_session(&session).unwrap();
    }
    let (v2, v1) = v2_and_v1_names(d.path());
    assert_eq!(sidecars(&v2), vec![v2.clone()], "closed: no WAL left");
    std::fs::rename(&v2, &v1).unwrap();

    let mut ix = Index::open(d.path(), &root).unwrap();
    assert!(v2.exists());
    assert_eq!(ix.partial(&rp("p")).unwrap(), Some(partial));
    assert_eq!(ix.load_session_by_id(&[7; 16]).unwrap(), Some(session));
    // Entries are not taken: their files are hashed again.
    assert!(ix.get_entry(&rp("a")).unwrap().is_none());
    // Only a new file takes them: one reopened does not again.
    ix.remove_partial(&rp("p")).unwrap();
    drop(ix);
    let ix = Index::open(d.path(), &root).unwrap();
    assert!(ix.partial(&rp("p")).unwrap().is_none());
    // The old file is left as it was.
    let old = rusqlite::Connection::open(&v1).unwrap();
    let n: i64 = old
        .query_row("SELECT count(*) FROM partials", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn remove_unused_keeps_a_file_another_connection_has_open() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("root");
    std::fs::create_dir(&root).unwrap();
    drop(Index::open(d.path(), &root).unwrap());
    let (v2, v1) = v2_and_v1_names(d.path());
    std::fs::rename(&v2, &v1).unwrap();
    // Another process's connection, idle after a read, as an old daemon's.
    let other = rusqlite::Connection::open(&v1).unwrap();
    let _: i64 = other
        .query_row("SELECT count(*) FROM meta", [], |r| r.get(0))
        .unwrap();
    assert!(!Index::remove_unused(&v1).unwrap());
    assert!(v1.exists());
    drop(other);
    assert!(Index::remove_unused(&v1).unwrap());
    assert_eq!(sidecars(&v1), Vec::<std::path::PathBuf>::new());
    assert!(!Index::is_current_file(&v1));
}
