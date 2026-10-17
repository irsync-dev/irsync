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
        updated_at: 0,
    };
    {
        let mut ix = Index::open(d.path(), d.path()).unwrap();
        ix.put_partial(&rp("p"), &p).unwrap();
    }
    let mut ix = Index::open(d.path(), d.path()).unwrap();
    let got = ix.partial(&rp("p")).unwrap().unwrap();
    assert_eq!(got.expected_hash, [1; 32]);
    assert_eq!(got.size, 100);
    assert_eq!(got.ranges, p.ranges);
    assert!(got.updated_at > 1_700_000_000);
    assert!(ix
        .partials_older_than(Duration::from_secs(3600))
        .unwrap()
        .is_empty());
    std::thread::sleep(Duration::from_millis(1100));
    assert_eq!(
        ix.partials_older_than(Duration::ZERO).unwrap(),
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
    ix.save_session(&s).unwrap();
    s.progress = vec![3];
    ix.save_session(&s).unwrap();
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
