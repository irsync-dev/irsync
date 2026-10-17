use irsync::fs::meta::ApplyOpts;
use irsync::fs::paths::RelPath;
use irsync::index::{Index, IndexHandle};
use irsync::recv::{FileWriter, FinalizeError};
use irsync_proto::chunk::digest_bytes;
use irsync_proto::v1::{self, file_entry::Kind, ChunkList, FileEntry};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// ~1.5 MiB of xorshift pseudo-random data, so FastCDC yields several chunks.
fn content(seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    let mut out = Vec::with_capacity(1_536 * 1024);
    while out.len() < 1_536 * 1024 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

fn chunk_list(data: &[u8]) -> ChunkList {
    let d = digest_bytes(data);
    ChunkList {
        file_id: 1,
        size: d.size,
        whole_hash: Some(v1::Hash {
            bytes: d.hash.to_vec(),
        }),
        chunks: d.chunks.into_iter().map(Into::into).collect(),
    }
}

fn piece<'a>(data: &'a [u8], cl: &ChunkList, idx: usize) -> &'a [u8] {
    let c = &cl.chunks[idx];
    &data[c.offset as usize..c.offset as usize + c.len as usize]
}

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    index: IndexHandle,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let state = tmp.path().join("state");
    fs::create_dir_all(root.join("dir")).unwrap();
    fs::create_dir_all(&state).unwrap();
    let index = Index::open(&state, &root).unwrap().into_handle();
    Env {
        _tmp: tmp,
        root,
        index,
    }
}

fn rel() -> RelPath {
    RelPath::new(b"dir/f.bin").unwrap()
}

fn part_path(root: &Path) -> PathBuf {
    root.join("dir").join(".irsync.f.bin.part")
}

fn final_path(root: &Path) -> PathBuf {
    root.join("dir").join("f.bin")
}

fn entry(size: u64) -> FileEntry {
    FileEntry {
        path: b"dir/f.bin".to_vec(),
        kind: Kind::Regular as i32,
        size,
        mtime_sec: 1_600_000_000,
        mtime_nsec: 0,
        mode: 0o100644,
        ..Default::default()
    }
}

const OPTS: ApplyOpts = ApplyOpts {
    perms: true,
    times: true,
    owner: false,
    group: false,
};

async fn write_all(w: &mut FileWriter, data: &[u8], cl: &ChunkList, idxs: &[u32]) {
    for &idx in idxs {
        w.write_chunk(idx, piece(data, cl, idx as usize))
            .await
            .unwrap();
    }
}

fn all(cl: &ChunkList) -> Vec<u32> {
    (0..cl.chunks.len() as u32).collect()
}

#[tokio::test]
async fn out_of_order_chunks_assemble_byte_exact() {
    let e = env();
    let data = content(7);
    let cl = chunk_list(&data);
    assert!(cl.chunks.len() >= 3, "want several chunks");
    let mut w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    assert_eq!(w.missing_chunks(), all(&cl));
    let mut order = all(&cl);
    order.reverse();
    write_all(&mut w, &data, &cl, &order).await;
    assert!(w.missing_chunks().is_empty());
    assert_eq!(w.have().ranges.len(), 1);
    assert_eq!(w.have().ranges[0].length, data.len() as u64);
    // Bytes that do not match the chunk's hash are rejected before any write.
    let bad = vec![0u8; cl.chunks[0].len as usize];
    let err = w.write_chunk(0, &bad).await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    w.finalize(&entry(data.len() as u64), &OPTS).await.unwrap();
    assert_eq!(fs::read(final_path(&e.root)).unwrap(), data);
    assert!(!part_path(&e.root).exists());
    assert!(e.index.lock().unwrap().partial(&rel()).unwrap().is_none());
}

#[tokio::test]
async fn finalize_rejects_hash_mismatch_and_leaves_no_final_file() {
    let e = env();
    let data = content(11);
    let mut cl = chunk_list(&data);
    cl.whole_hash = Some(v1::Hash {
        bytes: vec![0xAB; 32],
    });
    let mut w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    write_all(&mut w, &data, &cl, &all(&cl)).await;
    let err = w
        .finalize(&entry(data.len() as u64), &OPTS)
        .await
        .unwrap_err();
    match err {
        FinalizeError::HashMismatch { expected, actual } => {
            assert_eq!(expected, [0xAB; 32]);
            assert_eq!(actual, *blake3::hash(&data).as_bytes());
        }
        other => panic!("expected HashMismatch, got {other:?}"),
    }
    assert!(!final_path(&e.root).exists());
    assert!(part_path(&e.root).exists());
    assert!(e.index.lock().unwrap().partial(&rel()).unwrap().is_some());
}

#[tokio::test]
async fn adopts_partial_from_previous_writer() {
    let e = env();
    let data = content(17);
    let cl = chunk_list(&data);
    let n = cl.chunks.len() as u32;
    let evens: Vec<u32> = (0..n).filter(|i| i % 2 == 0).collect();
    let odds: Vec<u32> = (0..n).filter(|i| i % 2 == 1).collect();
    {
        let mut w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
        write_all(&mut w, &data, &cl, &evens).await;
    }
    let p = e.index.lock().unwrap().partial(&rel()).unwrap().unwrap();
    assert_eq!(p.size, data.len() as u64);

    let mut w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    assert_eq!(w.missing_chunks(), odds);
    write_all(&mut w, &data, &cl, &odds).await;
    w.finalize(&entry(data.len() as u64), &OPTS).await.unwrap();
    assert_eq!(fs::read(final_path(&e.root)).unwrap(), data);

    // A recorded range whose bytes were corrupted behind the record's back is dropped.
    let rel2 = RelPath::new(b"dir/g.bin").unwrap();
    {
        let mut w = FileWriter::open(&e.root, &rel2, &cl, None, e.index.clone()).unwrap();
        write_all(&mut w, &data, &cl, &[0, 1]).await;
    }
    let part2 = e.root.join("dir/.irsync.g.bin.part");
    let mut bytes = fs::read(&part2).unwrap();
    bytes[cl.chunks[1].offset as usize] ^= 0xFF;
    fs::write(&part2, &bytes).unwrap();
    let w = FileWriter::open(&e.root, &rel2, &cl, None, e.index.clone()).unwrap();
    assert_eq!(w.missing_chunks(), (1..n).collect::<Vec<_>>());
}

#[tokio::test]
async fn adopts_unrecorded_part_file_by_rechunking() {
    let e = env();
    let data = content(23);
    let cl = chunk_list(&data);
    let n = cl.chunks.len() as u32;
    assert!(n >= 3);
    // A .part holding exactly the first two chunks, with no index record.
    let cut = cl.chunks[2].offset as usize;
    fs::write(part_path(&e.root), &data[..cut]).unwrap();
    let mut w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    assert_eq!(w.missing_chunks(), (2..n).collect::<Vec<_>>());
    assert_eq!(w.have().ranges.len(), 1);
    assert_eq!(w.have().ranges[0].offset, 0);
    assert_eq!(w.have().ranges[0].length, cut as u64);
    let missing = w.missing_chunks();
    write_all(&mut w, &data, &cl, &missing).await;
    w.finalize(&entry(data.len() as u64), &OPTS).await.unwrap();
    assert_eq!(fs::read(final_path(&e.root)).unwrap(), data);
}

#[tokio::test]
async fn write_error_keeps_partial_and_no_final_file() {
    let e = env();
    let data = content(29);
    let cl = chunk_list(&data);
    let mut w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    w.fail_write_at(2);
    write_all(&mut w, &data, &cl, &[0, 1]).await;
    let err = w.write_chunk(2, piece(&data, &cl, 2)).await.unwrap_err();
    assert_eq!(err.raw_os_error(), Some(nix::libc::ENOSPC));
    assert!(part_path(&e.root).exists());
    assert!(!final_path(&e.root).exists());
    let p = e.index.lock().unwrap().partial(&rel()).unwrap().unwrap();
    assert_eq!(p.expected_hash, *blake3::hash(&data).as_bytes());
    assert_eq!(p.ranges.ranges.len(), 1);
    assert_eq!(p.ranges.ranges[0].offset, 0);
    assert_eq!(p.ranges.ranges[0].length, cl.chunks[2].offset);
    drop(w);
    assert!(!final_path(&e.root).exists());
}

#[tokio::test]
async fn finalize_applies_mtime_and_mode() {
    let e = env();
    let data = content(31);
    let cl = chunk_list(&data);
    // An existing regular file at the destination is replaced atomically.
    fs::write(final_path(&e.root), b"old").unwrap();
    let mut w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    write_all(&mut w, &data, &cl, &all(&cl)).await;
    let mut ent = entry(data.len() as u64);
    ent.mode = 0o100640;
    ent.mtime_sec = 1_234_567_890;
    ent.mtime_nsec = 500;
    w.finalize(&ent, &OPTS).await.unwrap();
    let m = fs::metadata(final_path(&e.root)).unwrap();
    assert_eq!(m.permissions().mode() & 0o7777, 0o640);
    assert_eq!(m.mtime(), 1_234_567_890);
    assert_eq!(m.mtime_nsec(), 500);
    assert_eq!(fs::read(final_path(&e.root)).unwrap(), data);
}

#[tokio::test]
async fn finalize_refuses_incomplete_and_directory_destination() {
    let e = env();
    let data = content(37);
    let cl = chunk_list(&data);
    let mut w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    write_all(&mut w, &data, &cl, &[0]).await;
    let err = w
        .finalize(&entry(data.len() as u64), &OPTS)
        .await
        .unwrap_err();
    assert!(matches!(err, FinalizeError::Incomplete), "{err:?}");
    assert!(!final_path(&e.root).exists());

    fs::create_dir_all(final_path(&e.root).join("sub")).unwrap();
    let mut w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    assert_eq!(
        w.missing_chunks(),
        (1..cl.chunks.len() as u32).collect::<Vec<_>>()
    );
    let missing = w.missing_chunks();
    write_all(&mut w, &data, &cl, &missing).await;
    let err = w
        .finalize(&entry(data.len() as u64), &OPTS)
        .await
        .unwrap_err();
    assert!(matches!(err, FinalizeError::Io(_)), "{err:?}");
    assert!(final_path(&e.root).join("sub").is_dir());
}

#[tokio::test]
async fn partial_dir_and_copy_local() {
    let e = env();
    let data = content(41);
    let cl = chunk_list(&data);
    let src = e.root.join("src.bin");
    fs::write(&src, &data).unwrap();
    let pd = e.root.join(".partials");
    let mut w = FileWriter::open(&e.root, &rel(), &cl, Some(&pd), e.index.clone()).unwrap();
    assert!(pd.join("dir/.irsync.f.bin.part").exists());
    // A wrong source offset is caught by the hash check.
    let err = w.copy_local(0, &src, 1).await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    for (idx, c) in cl.chunks.iter().enumerate() {
        w.copy_local(idx as u32, &src, c.offset).await.unwrap();
    }
    w.finalize(&entry(data.len() as u64), &OPTS).await.unwrap();
    assert_eq!(fs::read(final_path(&e.root)).unwrap(), data);
    assert!(!pd.join("dir/.irsync.f.bin.part").exists());
}
