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
    // A second writer on the same .part waits for the lock...
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
    let w = FileWriter::open(&e.root, &rel(), &cl, None, e.index.clone()).unwrap();
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
