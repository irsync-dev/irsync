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
    assert!(files
        .iter()
        .any(|f| f.as_ref().unwrap().path().extension().unwrap() == "sqlite"));
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
    assert_eq!(pragma("user_version"), "1");
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
