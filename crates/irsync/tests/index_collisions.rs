//! Index ids that collide: a file's rows are keyed by a hash of its path,
//! and two paths may share an id. Here every path shares one
//! (`force_id_bits(0)`, set by every test: the hook holds for the whole
//! process). A collision must cost a cache miss, never a wrong answer.

mod common;

use common::{archive_opts, assert_trees_equal, Flow, Harness};
use irsync::cli::DeleteMode;
use irsync::fs::paths::RelPath;
use irsync::index::{force_id_bits, Index, NewEntry};
use irsync_proto::chunk::{digest_bytes, FileDigest};
use irsync_proto::v1::{FileEntry, Hash};

fn collide() {
    force_id_bits(0);
}

fn rp(s: &str) -> RelPath {
    RelPath::new(s.as_bytes()).unwrap()
}

fn entry(d: &FileDigest, inode: u64) -> FileEntry {
    FileEntry {
        size: d.size,
        inode,
        mode: 0o644,
        whole_hash: Some(Hash {
            bytes: d.hash.to_vec(),
        }),
        ..Default::default()
    }
}

#[test]
fn a_path_with_the_same_id_replaces_the_other_and_never_answers_for_it() {
    collide();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let mut ix = Index::open(d.path(), &root).unwrap();
    let (da, db) = (digest_bytes(&[1u8; 4096]), digest_bytes(&[2u8; 4096]));
    let (ea, eb) = (entry(&da, 1), entry(&db, 2));
    ix.put_entry(&rp("a"), &ea, (1, 2), Some(&da)).unwrap();
    ix.put_entry(&rp("b"), &eb, (1, 2), Some(&db)).unwrap();

    // `b` took the id: `a` is not indexed, and nothing finds its chunk.
    assert!(ix.get_entry(&rp("a")).unwrap().is_none());
    assert_eq!(
        ix.get_entry(&rp("b")).unwrap().unwrap().digest,
        Some(db.clone())
    );
    assert_eq!(ix.find_chunk(&da.chunks[0].hash).unwrap(), None);
    assert_eq!(
        ix.find_chunk(&db.chunks[0].hash).unwrap(),
        Some((rp("b"), 0))
    );

    // Forgetting `a`, which the id no longer holds, leaves `b`.
    ix.remove_entry(&rp("a")).unwrap();
    ix.remove_entry_if(&rp("a"), eb.inode, eb.size).unwrap();
    assert!(ix.get_entry(&rp("b")).unwrap().is_some());
    assert!(ix.find_chunk(&db.chunks[0].hash).unwrap().is_some());

    // `a` written again takes the id back, chunks and all.
    ix.put_entry(&rp("a"), &ea, (1, 2), Some(&da)).unwrap();
    assert_eq!(
        ix.get_entry(&rp("a")).unwrap().unwrap().digest,
        Some(da.clone())
    );
    assert!(ix.get_entry(&rp("b")).unwrap().is_none());
    assert_eq!(ix.find_chunk(&db.chunks[0].hash).unwrap(), None);
    assert_eq!(
        ix.find_chunk(&da.chunks[0].hash).unwrap(),
        Some((rp("a"), 0))
    );
    assert_eq!(ix.entry_identities().unwrap().len(), 1);
}

#[test]
fn queued_writes_and_reader_connections_collide_alike() {
    collide();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let h = Index::open(d.path(), &root).unwrap().into_handle();
    for (p, byte, inode) in [("a", 1u8, 1), ("b", 2, 2)] {
        let digest = digest_bytes(&[byte; 4096]);
        h.queue_entry(NewEntry {
            path: rp(p),
            entry: entry(&digest, inode),
            ctime: (1, 2),
            digest: Some(digest),
        });
    }
    h.flush();
    assert!(h.read(|r| r.get_entry(&rp("a"))).unwrap().is_none());
    let b = h.read(|r| r.get_entry(&rp("b"))).unwrap().unwrap();
    assert_eq!(b.entry.inode, 2);
    h.queue_forget(rp("a"));
    h.flush();
    assert!(h.read(|r| r.get_entry(&rp("b"))).unwrap().is_some());
}

/// Files of a few kilobytes each, some sharing content, and two of several
/// chunks.
fn fill(root: &std::path::Path) {
    for d in 0..10 {
        let dir = root.join(format!("d{d}"));
        std::fs::create_dir_all(&dir).unwrap();
        for f in 0..30 {
            let body: Vec<u8> = (0..4096u32).map(|i| (i * 7 + f * 13 + d) as u8).collect();
            std::fs::write(dir.join(format!("f{f}")), body).unwrap();
        }
    }
    let big: Vec<u8> = (0..3u32 << 20).map(|i| (i % 251) as u8).collect();
    std::fs::write(root.join("big1"), &big).unwrap();
    std::fs::write(root.join("big2"), &big[1 << 20..]).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn syncs_stay_correct_when_every_path_shares_an_id() {
    collide();
    for flow in [Flow::Pull, Flow::Push] {
        let h = Harness::new().await;
        fill(h.src(flow));
        let opts = archive_opts();
        // Into an empty destination, then again with nothing changed (every
        // lookup finds another path's row, or none).
        for _ in 0..2 {
            let out = h.run(&opts, flow).await.expect("sync");
            assert!(out.errors.is_empty(), "{:?}", out.errors);
            assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
        }
        // Moved and changed files: the receiver looks for their chunks in
        // its index, which may name the wrong path or nothing.
        let src = h.src(flow);
        std::fs::rename(src.join("d3"), src.join("moved")).unwrap();
        std::fs::rename(src.join("big1"), src.join("big3")).unwrap();
        let mut body = std::fs::read(src.join("big2")).unwrap();
        body[100] ^= 0xff;
        std::fs::write(src.join("big2"), body).unwrap();
        let mut opts = opts.clone();
        opts.delete = DeleteMode::After;
        let out = h.run(&opts, flow).await.expect("sync after changes");
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
    }
}
