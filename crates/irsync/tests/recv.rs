use irsync::fs::meta::ApplyOpts;
use irsync::fs::paths::RelPath;
use irsync::index::{Index, IndexHandle};
use irsync::recv::{FileWriter, FinalizeError};
use irsync_proto::chunk::{digest_bytes, ChunkRecord, FileDigest};
use irsync_proto::v1::{self, file_entry::Kind, FileEntry};
use std::fs;
use std::os::unix::fs::symlink;
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

fn chunk_list(data: &[u8]) -> FileDigest {
    digest_bytes(data)
}

fn piece<'a>(data: &'a [u8], cl: &FileDigest, idx: usize) -> &'a [u8] {
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

async fn write_all(w: &mut FileWriter, data: &[u8], cl: &FileDigest, idxs: &[u32]) {
    for &idx in idxs {
        w.write_chunk(idx, piece(data, cl, idx as usize))
            .await
            .unwrap();
    }
}

fn all(cl: &FileDigest) -> Vec<u32> {
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
async fn received_chunks_are_timed_and_counted_as_hashing() {
    use irsync::telemetry::meter::{Activity, Counter, Meter};
    use irsync::telemetry::{Role, Side};
    let e = env();
    let data = content(3);
    let cl = chunk_list(&data);
    let meter = Meter::new(Side::Client, Role::Receiver, None, true);
    let mut w = FileWriter::open_tracked(
        &e.root,
        &rel(),
        &cl,
        None,
        e.index.clone(),
        Default::default(),
        Some(meter.clone()),
    )
    .unwrap();
    write_all(&mut w, &data, &cl, &all(&cl)).await;
    let u = meter.usage();
    assert_eq!(u.count(Counter::BytesHashed), data.len() as u64);
    assert!(!u.busy(Activity::Hash).is_zero());
    assert!(!u.busy(Activity::Write).is_zero());
}

#[tokio::test]
async fn received_counts_only_chunks_from_the_sender() {
    let e = env();
    let data = content(5);
    let cl = chunk_list(&data);
    assert!(cl.chunks.len() >= 2, "want several chunks");
    let mut w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    let first = cl.chunks[0].len as u64;
    w.write_copied(0, piece(&data, &cl, 0)).await.unwrap();
    assert_eq!(w.received(), 0);
    let rest: Vec<u32> = (1..cl.chunks.len() as u32).collect();
    write_all(&mut w, &data, &cl, &rest).await;
    // A chunk written again (a re-sent stream) is not counted twice.
    w.write_chunk(1, piece(&data, &cl, 1)).await.unwrap();
    assert_eq!(w.received(), data.len() as u64 - first);
}

#[tokio::test]
async fn finalize_rejects_hash_mismatch_and_leaves_no_final_file() {
    let e = env();
    let data = content(11);
    let mut cl = chunk_list(&data);
    cl.hash = [0xAB; 32];
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
    let written = w.bytes_written();
    let err = w.write_chunk(2, piece(&data, &cl, 2)).await.unwrap_err();
    assert_eq!(err.raw_os_error(), Some(nix::libc::ENOSPC));
    // The failed write put nothing into the `.part`.
    assert_eq!(w.bytes_written(), written);
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
    assert!(part_path(&e.root).exists());
    assert!(e.index.lock().unwrap().partial(&rel()).unwrap().is_some());
}

#[tokio::test]
async fn partial_dir_and_copy_local() {
    let e = env();
    let data = content(41);
    let cl = chunk_list(&data);
    let src = e.root.join("src.bin");
    fs::write(&src, &data).unwrap();
    let pd = e.root.join(".partials");
    let pdir = irsync::recv::PartialDir::new(&pd);
    let mut w = FileWriter::open(&e.root, &rel(), &cl, Some(&pdir), e.index.clone()).unwrap();
    // A wrong source offset is caught by the hash check.
    let err = w.copy_local(0, &src, 1).await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    for (idx, c) in cl.chunks.iter().enumerate() {
        w.copy_local(idx as u32, &src, c.offset).await.unwrap();
    }
    assert!(pd.join("dir/.irsync.f.bin.part").exists());
    w.finalize(&entry(data.len() as u64), &OPTS).await.unwrap();
    assert_eq!(fs::read(final_path(&e.root)).unwrap(), data);
    assert!(!pd.join("dir/.irsync.f.bin.part").exists());
}

#[tokio::test]
async fn missing_directory_is_created_at_the_first_write() {
    let e = env();
    let data = content(43);
    let cl = chunk_list(&data);
    let rel = RelPath::new(b"new/sub/f.bin").unwrap();
    let mut w = FileWriter::open(&e.root, &rel, &cl, None, e.index.clone()).unwrap();
    // Nothing is created until the `.part` is opened.
    assert!(!e.root.join("new").exists());
    write_all(&mut w, &data, &cl, &all(&cl)).await;
    assert!(e.root.join("new/sub/.irsync.f.bin.part").exists());
    w.finalize(&entry(data.len() as u64), &OPTS).await.unwrap();
    assert_eq!(fs::read(e.root.join("new/sub/f.bin")).unwrap(), data);
}

#[tokio::test]
async fn read_only_mode_with_dir_dest_stays_resumable() {
    let e = env();
    let data = content(43);
    let cl = chunk_list(&data);
    fs::create_dir_all(final_path(&e.root)).unwrap();
    let mut w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    write_all(&mut w, &data, &cl, &all(&cl)).await;
    let mut ent = entry(data.len() as u64);
    ent.mode = 0o100444;
    let err = w.finalize(&ent, &OPTS).await.unwrap_err();
    assert!(matches!(err, FinalizeError::Io(_)), "{err:?}");
    // The failure came after the read-only mode was applied: owner
    // read/write is restored on the `.part` before anything reopens it.
    let mode = fs::metadata(part_path(&e.root))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o644, "{mode:o}");
    let w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    assert!(w.missing_chunks().is_empty());
    // Even a .part left read-only by an older failure is reopened.
    drop(w);
    fs::set_permissions(part_path(&e.root), fs::Permissions::from_mode(0o444)).unwrap();
    let w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    assert!(w.missing_chunks().is_empty());
}

#[tokio::test]
async fn record_covering_part_of_a_chunk_does_not_adopt_it() {
    let e = env();
    let data = content(47);
    let cl = chunk_list(&data);
    fs::write(part_path(&e.root), &data).unwrap();
    let c1 = &cl.chunks[1];
    let ranges = v1::RangeSet {
        ranges: vec![v1::Range {
            offset: 0,
            length: c1.offset + u64::from(c1.len) - 1,
        }],
    };
    e.index
        .lock()
        .unwrap()
        .put_partial(
            &rel(),
            &irsync::index::Partial {
                expected_hash: *blake3::hash(&data).as_bytes(),
                size: data.len() as u64,
                ranges,
                updated_at: 0,
            },
        )
        .unwrap();
    let w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    assert_eq!(
        w.missing_chunks(),
        (1..cl.chunks.len() as u32).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn wrong_length_chunk_is_rejected() {
    let e = env();
    let data = content(53);
    let cl = chunk_list(&data);
    let mut w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    let p = piece(&data, &cl, 0);
    let err = w.write_chunk(0, &p[..p.len() - 1]).await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(w.missing_chunks(), all(&cl));
}

#[tokio::test]
async fn oversized_chunk_in_list_is_rejected() {
    let e = env();
    let data = vec![7u8; 2 * 1024 * 1024];
    // One 2 MiB "chunk": tiles the file, but exceeds the 1 MiB maximum.
    let hash = *blake3::hash(&data).as_bytes();
    let cl = FileDigest {
        size: data.len() as u64,
        hash,
        chunks: vec![ChunkRecord {
            offset: 0,
            len: data.len() as u32,
            hash,
        }],
    };
    let err = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone())
        .err()
        .expect("oversized chunk must be refused");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(!part_path(&e.root).exists());
}

#[tokio::test]
async fn symlink_at_destination_is_replaced_and_target_untouched() {
    let e = env();
    let data = content(59);
    let cl = chunk_list(&data);
    let target = e.root.join("target.txt");
    fs::write(&target, b"keep me").unwrap();
    symlink(&target, final_path(&e.root)).unwrap();
    let mut w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    write_all(&mut w, &data, &cl, &all(&cl)).await;
    w.finalize(&entry(data.len() as u64), &OPTS).await.unwrap();
    let m = fs::symlink_metadata(final_path(&e.root)).unwrap();
    assert!(m.file_type().is_file());
    assert_eq!(fs::read(final_path(&e.root)).unwrap(), data);
    assert_eq!(fs::read(&target).unwrap(), b"keep me");
}

#[tokio::test]
async fn zero_size_file_finalizes_empty() {
    let e = env();
    let cl = chunk_list(b"");
    assert!(cl.chunks.is_empty());
    let w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    assert!(w.missing_chunks().is_empty());
    w.finalize(&entry(0), &OPTS).await.unwrap();
    assert_eq!(fs::read(final_path(&e.root)).unwrap(), b"");
    assert!(!part_path(&e.root).exists());
}

/// Random data with an 18 MiB zero run in the middle: at least 16 MiB of it
/// falls in whole zero chunks (FastCDC max is 1 MiB). APFS on a FileVault
/// volume zero-fills holes under 16 MiB on writeback.
fn with_zero_run() -> Vec<u8> {
    let mut data = content(5);
    data.extend(std::iter::repeat_n(0u8, 18 * 1024 * 1024));
    data.extend(content(6));
    data
}

#[tokio::test]
async fn sparse_skips_zero_chunks_of_a_fresh_part() {
    let e = env();
    let data = with_zero_run();
    let cl = chunk_list(&data);
    let mut w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    w.set_sparse(true);
    write_all(&mut w, &data, &cl, &all(&cl)).await;
    w.finalize(&entry(data.len() as u64), &OPTS).await.unwrap();
    assert!(fs::read(final_path(&e.root)).unwrap() == data);
    let m = fs::metadata(final_path(&e.root)).unwrap();
    assert!(
        m.blocks() * 512 < data.len() as u64 - 1024 * 1024,
        "zero run was written: {} bytes allocated",
        m.blocks() * 512
    );
}

#[tokio::test]
async fn sparse_never_leaves_stale_bytes_of_an_old_part() {
    // A `.part` from a different expected file (no record): its bytes lie
    // where the new file has zero chunks, so those must still be written.
    let e = env();
    let data = with_zero_run();
    let stale = content(99).repeat(3);
    fs::write(part_path(&e.root), &stale).unwrap();
    let cl = chunk_list(&data);
    let mut w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    w.set_sparse(true);
    let missing = w.missing_chunks();
    write_all(&mut w, &data, &cl, &missing).await;
    w.finalize(&entry(data.len() as u64), &OPTS).await.unwrap();
    assert!(fs::read(final_path(&e.root)).unwrap() == data);
}

#[tokio::test]
async fn part_symlink_is_refused() {
    let e = env();
    let data = content(71);
    let cl = chunk_list(&data);
    // A symlink planted at the `.part` name, to an existing file and to a
    // name that does not exist yet: the writer must neither write through
    // it, chmod its target, nor create the target.
    let target = e.root.join("victim.txt");
    fs::write(&target, b"keep me").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
    symlink(&target, part_path(&e.root)).unwrap();
    let err = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone())
        .err()
        .expect("open through a symlinked .part must fail");
    assert!(err.to_string().contains("symlink"), "{err}");
    assert_eq!(fs::read(&target).unwrap(), b"keep me");
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o7777,
        0o640
    );
    assert!(!final_path(&e.root).exists());

    fs::remove_file(part_path(&e.root)).unwrap();
    let absent = e.root.join("created-through-link");
    symlink(&absent, part_path(&e.root)).unwrap();
    assert!(FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).is_err());
    assert!(fs::symlink_metadata(&absent).is_err());
    assert!(fs::symlink_metadata(part_path(&e.root))
        .unwrap()
        .file_type()
        .is_symlink());
}

#[tokio::test]
async fn part_that_is_not_a_regular_file_is_refused() {
    let e = env();
    let data = content(73);
    let cl = chunk_list(&data);
    nix::unistd::mkfifo(&part_path(&e.root), nix::sys::stat::Mode::S_IRWXU).unwrap();
    let err = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone())
        .err()
        .expect("a FIFO at the .part name must be refused");
    assert!(err.to_string().contains("not a regular file"), "{err}");
}

#[tokio::test]
async fn part_swapped_before_rename_is_not_placed() {
    let e = env();
    let data = content(79);
    let cl = chunk_list(&data);
    let mut w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    write_all(&mut w, &data, &cl, &all(&cl)).await;
    // Someone replaces the `.part` path while the writer holds the file.
    fs::remove_file(part_path(&e.root)).unwrap();
    fs::write(part_path(&e.root), b"unverified bytes").unwrap();
    let err = w
        .finalize(&entry(data.len() as u64), &OPTS)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("replaced"), "{err}");
    assert!(!final_path(&e.root).exists());
    assert_eq!(fs::read(part_path(&e.root)).unwrap(), b"unverified bytes");
}

#[tokio::test]
async fn temp_named_destination_is_refused() {
    let e = env();
    let data = content(83);
    let cl = chunk_list(&data);
    for name in [&b"dir/.irsync.x.part"[..], b"dir/.irsync.x.tmp"] {
        let rel = RelPath::new(name).unwrap();
        assert!(FileWriter::open(&e.root, &rel, &cl, None, e.index.clone()).is_err());
    }
    assert_eq!(fs::read_dir(e.root.join("dir")).unwrap().count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_writer_waits_and_never_takes_a_placed_file() {
    let e = env();
    let data = content(89);
    let cl = chunk_list(&data);
    let mut w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    // The first write creates and locks the `.part`; a second writer on
    // the same .part then waits for the lock...
    w.write_chunk(0, piece(&data, &cl, 0)).await.unwrap();
    let (root, cl2, index) = (e.root.clone(), cl.clone(), e.index.clone());
    let second =
        tokio::task::spawn_blocking(move || FileWriter::open(&root, &rel(), &cl2, None, index));
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(!second.is_finished(), "the second writer must wait");
    // ...while the first finishes; it then gets a fresh .part, never the
    // file the first one renamed into place.
    write_all(&mut w, &data, &cl, &all(&cl)).await;
    w.finalize(&entry(data.len() as u64), &OPTS).await.unwrap();
    let w2 = second.await.unwrap().unwrap();
    assert_eq!(w2.missing_chunks().len(), cl.chunks.len());
    assert_eq!(fs::read(final_path(&e.root)).unwrap(), data);
    drop(w2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn busy_part_waits_for_previous_holder() {
    let e = env();
    let data = content(97);
    let cl = chunk_list(&data);
    let mut w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    w.write_chunk(0, piece(&data, &cl, 0)).await.unwrap();
    // The previous connection's writer (e.g. a finalize that outlived it)
    // lets go after 2 s.
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        drop(w);
    });
    let start = std::time::Instant::now();
    let (root, index) = (e.root.clone(), e.index.clone());
    let w2 = tokio::task::spawn_blocking(move || FileWriter::open(&root, &rel(), &cl, None, index))
        .await
        .unwrap()
        .expect("the second writer gets the .part once the first lets go");
    let waited = start.elapsed();
    assert!(
        waited >= std::time::Duration::from_millis(1500),
        "{waited:?}"
    );
    assert!(waited < std::time::Duration::from_secs(3), "{waited:?}");
    drop(w2);
}

mod temp_names {
    use irsync::recv::{is_temp_name, temp_name, NAME_MAX};

    #[test]
    fn short_names_are_wrapped_verbatim() {
        assert_eq!(temp_name(b"f.bin", b".part"), b".irsync.f.bin.part");
        assert_eq!(temp_name(b"f.bin", b".tmp"), b".irsync.f.bin.tmp");
    }

    #[test]
    fn longest_plain_form_is_kept() {
        let name = vec![b'a'; NAME_MAX - ".irsync.".len() - ".part".len()];
        let t = temp_name(&name, b".part");
        assert_eq!(t.len(), NAME_MAX);
        assert_eq!(&t[8..8 + name.len()], &name[..]);
        let name = vec![b'a'; NAME_MAX - ".irsync.".len() - ".tmp".len()];
        let t = temp_name(&name, b".tmp");
        assert_eq!(t.len(), NAME_MAX);
        assert_eq!(&t[8..8 + name.len()], &name[..]);
    }

    #[test]
    fn long_names_are_shortened_to_fit_and_stay_distinct() {
        for suffix in [&b".part"[..], &b".tmp"[..]] {
            for len in [NAME_MAX - 12, NAME_MAX - 1, NAME_MAX] {
                let mut a = vec![b'x'; len];
                let mut b = a.clone();
                *a.last_mut().unwrap() = b'1';
                *b.last_mut().unwrap() = b'2';
                let (ta, tb) = (temp_name(&a, suffix), temp_name(&b, suffix));
                assert_eq!(ta.len(), NAME_MAX, "len {len} {suffix:?}");
                assert!(ta.starts_with(b".irsync."));
                assert!(ta.ends_with(suffix));
                assert!(is_temp_name(&ta));
                assert_ne!(ta, tb, "names differing only past the cut must not collide");
                assert!(
                    ta.windows(NAME_MAX - 12 - 40).any(|w| w == &a[..w.len()]),
                    "prefix kept"
                );
            }
        }
    }

    #[test]
    fn shortening_is_deterministic() {
        let name = [0xC3u8, 0xA9].repeat(127);
        assert_eq!(temp_name(&name, b".part"), temp_name(&name, b".part"));
    }

    #[test]
    fn utf8_names_are_cut_on_a_character_boundary() {
        // 127 x "é" (2 bytes) + "a" = 255 bytes of valid UTF-8.
        let mut name = "é".repeat(127).into_bytes();
        name.push(b'a');
        assert_eq!(name.len(), NAME_MAX);
        let t = temp_name(&name, b".part");
        assert!(t.len() <= NAME_MAX);
        assert!(std::str::from_utf8(&t).is_ok(), "{t:?}");
    }

    #[test]
    fn non_utf8_names_are_cut_bytewise() {
        let name = vec![0xFFu8; NAME_MAX];
        let t = temp_name(&name, b".part");
        assert_eq!(t.len(), NAME_MAX);
        assert!(is_temp_name(&t));
    }
}

#[tokio::test]
async fn finalize_does_not_read_the_part_file_back() {
    let e = env();
    let data = content(13);
    let cl = chunk_list(&data);
    let mut w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    write_all(&mut w, &data, &cl, &all(&cl)).await;
    // Every byte was verified in memory and the whole-file hash is folded
    // from those bytes: a byte changed on disk behind the writer's back is
    // not noticed (rsync's guarantee too), and there is no second full pass.
    let f = fs::OpenOptions::new()
        .write(true)
        .open(part_path(&e.root))
        .unwrap();
    std::os::unix::fs::FileExt::write_all_at(&f, &[data[100] ^ 0xFF], 100).unwrap();
    drop(f);
    w.finalize(&entry(data.len() as u64), &OPTS).await.unwrap();
    let placed = fs::read(final_path(&e.root)).unwrap();
    assert_eq!(placed[100], data[100] ^ 0xFF);
    assert_eq!(placed.len(), data.len());
}

/// A temp root under `target/`, where the filesystem may support reflinks
/// (`/tmp` is usually tmpfs, which does not).
fn env_in_target() -> Env {
    let base = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    fs::create_dir_all(base).unwrap();
    let tmp = tempfile::tempdir_in(base).unwrap();
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

/// Whether `dir`'s filesystem clones files (`FICLONE`); tests that need it
/// skip otherwise.
fn reflinks(dir: &Path) -> bool {
    let a = fs::File::create(dir.join(".probe-a")).unwrap();
    std::io::Write::write_all(&mut &a, b"probe").unwrap();
    let b = fs::File::create(dir.join(".probe-b")).unwrap();
    let ok = irsync::recv::reflink(&a, &b).is_ok();
    let _ = fs::remove_file(dir.join(".probe-a"));
    let _ = fs::remove_file(dir.join(".probe-b"));
    if !ok {
        eprintln!("skipping: {} does not support reflinks", dir.display());
    }
    ok
}

fn clone_source<'a>(file: &'a fs::File, digest: &'a FileDigest) -> irsync::recv::CloneSource<'a> {
    irsync::recv::CloneSource { file, digest }
}

#[tokio::test]
async fn clone_of_the_old_file_adopts_its_unchanged_chunks_without_writing_them() {
    let e = env_in_target();
    if !reflinks(&e.root) {
        return;
    }
    let old = content(61);
    let old_cl = chunk_list(&old);
    let mut new = old.clone();
    let changed = 1usize;
    let c = &old_cl.chunks[changed];
    new[c.offset as usize + 10] ^= 0xFF;
    let cl = chunk_list(&new);
    assert_eq!(cl.chunks.len(), old_cl.chunks.len());
    fs::write(final_path(&e.root), &old).unwrap();
    let old_file = fs::File::open(final_path(&e.root)).unwrap();
    let mut w = FileWriter::open_from(
        &e.root,
        &rel(),
        &cl,
        None,
        e.index.clone(),
        Default::default(),
        None,
        Some(clone_source(&old_file, &old_cl)),
    )
    .unwrap();
    assert!(w.cloned());
    // Every chunk the old file holds at the same offset is verified in place.
    assert_eq!(w.missing_chunks(), vec![changed as u32]);
    assert_eq!(w.bytes_written(), 0);
    w.write_chunk(changed as u32, piece(&new, &cl, changed))
        .await
        .unwrap();
    assert_eq!(w.bytes_written(), u64::from(cl.chunks[changed].len));
    let before = fs::metadata(final_path(&e.root)).unwrap().ino();
    w.finalize(&entry(new.len() as u64), &OPTS).await.unwrap();
    assert_eq!(fs::read(final_path(&e.root)).unwrap(), new);
    assert_ne!(fs::metadata(final_path(&e.root)).unwrap().ino(), before);
    assert!(!part_path(&e.root).exists());
}

#[tokio::test]
async fn clone_is_skipped_when_a_part_already_holds_progress() {
    let e = env_in_target();
    if !reflinks(&e.root) {
        return;
    }
    let data = content(62);
    let cl = chunk_list(&data);
    fs::write(final_path(&e.root), content(63)).unwrap();
    let old_file = fs::File::open(final_path(&e.root)).unwrap();
    let old_cl = chunk_list(&content(63));
    {
        let mut w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
        write_all(&mut w, &data, &cl, &[0]).await;
    }
    let w = FileWriter::open_from(
        &e.root,
        &rel(),
        &cl,
        None,
        e.index.clone(),
        Default::default(),
        None,
        Some(clone_source(&old_file, &old_cl)),
    )
    .unwrap();
    assert!(!w.cloned());
    assert_eq!(
        w.missing_chunks(),
        (1..cl.chunks.len() as u32).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn clone_that_the_filesystem_refuses_falls_back_to_an_empty_part() {
    // The default temp dir is usually tmpfs, which cannot clone: nothing
    // may depend on the clone, and a refused one costs only the attempt.
    let e = env();
    let supported = reflinks(&e.root);
    let old = content(64);
    let old_cl = chunk_list(&old);
    let mut new = old.clone();
    new[5] ^= 1;
    let cl = chunk_list(&new);
    fs::write(final_path(&e.root), &old).unwrap();
    let old_file = fs::File::open(final_path(&e.root)).unwrap();
    let mut w = FileWriter::open_from(
        &e.root,
        &rel(),
        &cl,
        None,
        e.index.clone(),
        Default::default(),
        None,
        Some(clone_source(&old_file, &old_cl)),
    )
    .unwrap();
    assert_eq!(w.cloned(), supported);
    if !supported {
        assert_eq!(w.missing_chunks(), all(&cl));
    }
    let missing = w.missing_chunks();
    write_all(&mut w, &new, &cl, &missing).await;
    w.finalize(&entry(new.len() as u64), &OPTS).await.unwrap();
    assert_eq!(fs::read(final_path(&e.root)).unwrap(), new);
}

#[tokio::test]
async fn open_inplace_adopts_the_old_chunks_and_finalizes_without_a_rename() {
    let e = env();
    let old = content(71);
    let old_cl = chunk_list(&old);
    let mut new = old.clone();
    let changed = 2usize;
    new[old_cl.chunks[changed].offset as usize + 3] ^= 0xFF;
    let cl = chunk_list(&new);
    assert_eq!(cl.chunks.len(), old_cl.chunks.len());
    fs::write(final_path(&e.root), &old).unwrap();
    let old_file = fs::File::open(final_path(&e.root)).unwrap();
    let ino = fs::metadata(final_path(&e.root)).unwrap().ino();
    let mut w = FileWriter::open_inplace(
        &e.root,
        &rel(),
        &cl,
        e.index.clone(),
        Default::default(),
        None,
        clone_source(&old_file, &old_cl),
    )
    .unwrap();
    assert_eq!(w.missing_chunks(), vec![changed as u32]);
    assert!(!part_path(&e.root).exists());
    w.write_chunk(changed as u32, piece(&new, &cl, changed))
        .await
        .unwrap();
    assert_eq!(w.bytes_written(), u64::from(cl.chunks[changed].len));
    w.finalize(&entry(new.len() as u64), &OPTS).await.unwrap();
    assert_eq!(fs::read(final_path(&e.root)).unwrap(), new);
    assert_eq!(fs::metadata(final_path(&e.root)).unwrap().ino(), ino);
    assert!(e.index.lock().unwrap().partial(&rel()).unwrap().is_none());
}

#[tokio::test]
async fn open_inplace_shrinks_the_file_only_at_finalize() {
    // Bytes above the new size may still be the source of forward copies:
    // the file keeps its old length until every chunk is in.
    let e = env();
    let old = content(72);
    let old_cl = chunk_list(&old);
    let new = old[..1024 * 1024].to_vec();
    let cl = chunk_list(&new);
    fs::write(final_path(&e.root), &old).unwrap();
    let old_file = fs::File::open(final_path(&e.root)).unwrap();
    let mut w = FileWriter::open_inplace(
        &e.root,
        &rel(),
        &cl,
        e.index.clone(),
        Default::default(),
        None,
        clone_source(&old_file, &old_cl),
    )
    .unwrap();
    assert_eq!(
        fs::metadata(final_path(&e.root)).unwrap().len(),
        old.len() as u64
    );
    let missing = w.missing_chunks();
    write_all(&mut w, &new, &cl, &missing).await;
    w.finalize(&entry(new.len() as u64), &OPTS).await.unwrap();
    assert_eq!(fs::read(final_path(&e.root)).unwrap(), new);
}

#[tokio::test]
async fn open_inplace_refuses_a_destination_that_is_not_the_hashed_file() {
    let e = env();
    let old = content(73);
    let old_cl = chunk_list(&old);
    fs::write(final_path(&e.root), &old).unwrap();
    let other = e.root.join("other.bin");
    fs::write(&other, &old).unwrap();
    let other_file = fs::File::open(&other).unwrap();
    let err = FileWriter::open_inplace(
        &e.root,
        &rel(),
        &old_cl,
        e.index.clone(),
        Default::default(),
        None,
        clone_source(&other_file, &old_cl),
    )
    .err()
    .expect("another inode at the destination must be refused");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[tokio::test]
async fn fresh_writer_creates_the_part_only_on_its_first_chunk() {
    let e = env();
    let data = content(47);
    let cl = chunk_list(&data);
    let mut w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    // Nothing on disk until there is something to write: a file waiting
    // for the sender's stream holds no descriptor.
    assert!(
        !part_path(&e.root).exists(),
        "fresh .part opened before any data"
    );
    assert_eq!(w.missing_chunks().len(), cl.chunks.len());
    w.write_chunk(0, piece(&data, &cl, 0)).await.unwrap();
    assert!(part_path(&e.root).exists());
    let missing = w.missing_chunks();
    write_all(&mut w, &data, &cl, &missing).await;
    w.finalize(&entry(data.len() as u64), &OPTS).await.unwrap();
    assert_eq!(fs::read(final_path(&e.root)).unwrap(), data);
    assert!(!part_path(&e.root).exists());
}

#[tokio::test]
async fn fresh_empty_file_is_placed_without_a_chunk() {
    let e = env();
    let cl = chunk_list(b"");
    assert!(cl.chunks.is_empty());
    let w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    assert!(w.missing_chunks().is_empty());
    w.finalize(&entry(0), &OPTS).await.unwrap();
    assert_eq!(fs::metadata(final_path(&e.root)).unwrap().len(), 0);
    assert!(!part_path(&e.root).exists());
}

/// The time trigger counts from the first chunk not yet persisted, not from
/// opening: a file whose data arrives late, but in one go, never writes a
/// partial record (each one is three index writes under the index lock).
#[tokio::test]
async fn a_late_first_chunk_does_not_persist_a_record() {
    let e = env();
    let data = content(11);
    let cl = chunk_list(&data);
    assert!(cl.chunks.len() >= 2, "want several chunks");
    let mut w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2_100));
    write_all(&mut w, &data, &cl, &[0]).await;
    assert!(e.index.lock().unwrap().partial(&rel()).unwrap().is_none());
    // Progress left unpersisted for the interval is recorded at the next chunk.
    std::thread::sleep(std::time::Duration::from_millis(2_100));
    write_all(&mut w, &data, &cl, &[1]).await;
    let p = e.index.lock().unwrap().partial(&rel()).unwrap().unwrap();
    assert_eq!(p.size, data.len() as u64);
}

/// `--fsync` is opt-in, as in rsync: without `set_sync` finalize flushes
/// nothing, with it the flush is timed.
#[tokio::test]
async fn finalize_flushes_only_when_asked() {
    use irsync::telemetry::meter::{Counter, Meter};
    use irsync::telemetry::{Role, Side};
    for sync in [false, true] {
        let e = env();
        let data = content(5);
        let cl = chunk_list(&data);
        let meter = Meter::new(Side::Client, Role::Receiver, None, true);
        let mut w = FileWriter::open_tracked(
            &e.root,
            &rel(),
            &cl,
            None,
            e.index.clone(),
            Default::default(),
            Some(meter.clone()),
        )
        .unwrap();
        if sync {
            w.set_sync(true);
        }
        write_all(&mut w, &data, &cl, &all(&cl)).await;
        w.finalize(&entry(data.len() as u64), &OPTS).await.unwrap();
        assert_eq!(fs::read(final_path(&e.root)).unwrap(), data);
        let flushed = meter.usage().count(Counter::FsyncNanos);
        assert_eq!(flushed > 0, sync, "sync {sync}: {flushed} ns flushing");
    }
}
