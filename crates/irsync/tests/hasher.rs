use irsync::fs::meta::lstat_entry_with_ctime;
use irsync::fs::paths::RelPath;
use irsync::hasher::Hasher;
use irsync::index::Index;
use irsync::scan::ScanItem;
use nix::sys::stat::{utimensat, UtimensatFlags};
use nix::sys::time::TimeSpec;
use std::fs;
use std::path::Path;
use std::sync::atomic::Ordering::Relaxed;

fn set_mtime(path: &Path, sec: i64, nsec: i64) {
    let t = TimeSpec::new(sec, nsec);
    utimensat(
        nix::fcntl::AT_FDCWD,
        path,
        &t,
        &t,
        UtimensatFlags::FollowSymlink,
    )
    .unwrap();
}

fn item(root: &Path, name: &[u8]) -> ScanItem {
    let path = RelPath::new(name).unwrap();
    let (entry, ctime) = lstat_entry_with_ctime(root, &path).unwrap();
    ScanItem { path, entry, ctime }
}

fn setup() -> (tempfile::TempDir, tempfile::TempDir, Hasher) {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let idx = Index::open(state.path(), root.path())
        .unwrap()
        .into_handle();
    let h = Hasher::new(idx, root.path().to_path_buf(), 2);
    (root, state, h)
}

#[tokio::test]
async fn first_digest_reads_second_uses_cache() {
    let (root, _s, h) = setup();
    fs::write(root.path().join("f"), b"hello world").unwrap();
    set_mtime(&root.path().join("f"), 1_700_000_000, 5);
    let it = item(root.path(), b"f");
    let d1 = h.digest(&it).await.unwrap();
    h.flush();
    assert_eq!(h.stats().files_hashed.load(Relaxed), 1);
    assert_eq!(h.stats().bytes_hashed.load(Relaxed), 11);
    let d2 = h.digest(&it).await.unwrap();
    assert_eq!(h.stats().files_hashed.load(Relaxed), 1);
    assert_eq!(d1, d2);
}

#[tokio::test]
async fn changed_content_same_mtime_rehashed() {
    let (root, _s, h) = setup();
    let p = root.path().join("f");
    fs::write(&p, b"aaaa").unwrap();
    set_mtime(&p, 1_700_000_000, 5);
    let before = item(root.path(), b"f");
    let d1 = h.digest(&before).await.unwrap();
    // The row is in the index: only the ctime can tell the rewrite apart.
    h.flush();
    fs::write(&p, b"bbbb").unwrap();
    set_mtime(&p, 1_700_000_000, 5);
    // A kernel may keep ctime in coarse ticks: touch again until it moves.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut after = item(root.path(), b"f");
    while after.ctime == before.ctime {
        assert!(std::time::Instant::now() < deadline, "ctime never moved");
        std::thread::sleep(std::time::Duration::from_millis(5));
        set_mtime(&p, 1_700_000_000, 5);
        after = item(root.path(), b"f");
    }
    assert_eq!(
        (after.entry.size, after.entry.inode, after.entry.mtime_nsec),
        (
            before.entry.size,
            before.entry.inode,
            before.entry.mtime_nsec
        )
    );
    let d2 = h.digest(&after).await.unwrap();
    assert_eq!(h.stats().files_hashed.load(Relaxed), 2);
    assert_ne!(d1.digest.hash, d2.digest.hash);
}

#[tokio::test]
async fn force_rehashes() {
    let (root, _s, h) = setup();
    let h = h.with_force(true);
    fs::write(root.path().join("f"), b"data").unwrap();
    let it = item(root.path(), b"f");
    h.digest(&it).await.unwrap();
    h.digest(&it).await.unwrap();
    assert_eq!(h.stats().files_hashed.load(Relaxed), 2);
}

#[tokio::test]
async fn cached_returns_none_before_first_digest() {
    let (root, _s, h) = setup();
    fs::write(root.path().join("f"), b"data").unwrap();
    let it = item(root.path(), b"f");
    assert!(h.cached(&it).unwrap().is_none());
    let d = h.digest(&it).await.unwrap();
    h.flush();
    assert_eq!(h.cached(&it).unwrap(), Some(d.digest));
    assert_eq!(h.stats().files_hashed.load(Relaxed), 1);
}

#[tokio::test]
async fn hasher_refuses_symlink() {
    let (root, _s, h) = setup();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret"), b"private").unwrap();
    fs::write(root.path().join("f"), b"data").unwrap();
    let it = item(root.path(), b"f");
    // Swapped for a symlink after the walk saw a regular file.
    fs::remove_file(root.path().join("f")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret"), root.path().join("f")).unwrap();
    let e = h.digest(&it).await.unwrap_err();
    assert!(e.to_string().contains("refusing to follow symlink"), "{e}");
    assert_eq!(h.stats().files_hashed.load(Relaxed), 0);
}

#[tokio::test]
async fn a_forced_hasher_reports_no_cached_digest() {
    let (root, _s, h) = setup();
    fs::write(root.path().join("f"), b"data").unwrap();
    let it = item(root.path(), b"f");
    let d = h.digest(&it).await.unwrap();
    h.flush();
    assert_eq!(h.cached(&it).unwrap(), Some(d.digest));
    // `-c`: a fresh row is not evidence; the file has to be read.
    let forced = h.with_force(true);
    assert!(forced.cached(&it).unwrap().is_none());
}

/// Many small files are read at once, but at most `LARGE_WORKERS` large ones
/// (a spinning disk seeks between long sequential reads).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn large_files_are_read_a_few_at_a_time() {
    use irsync::hasher::{HashEvent, LARGE_FILE, LARGE_WORKERS};
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    const WORKERS: usize = 16;
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    for i in 0..WORKERS {
        fs::write(root.path().join(format!("small{i}")), b"x").unwrap();
        let big = fs::File::create(root.path().join(format!("big{i}"))).unwrap();
        big.set_len(LARGE_FILE).unwrap();
    }
    // Per kind: files being read now, and the most at once. Each read
    // lingers so that reads allowed to overlap do.
    let counts: Arc<[AtomicUsize; 4]> = Arc::default();
    let c = counts.clone();
    let progress = Arc::new(move |p: &RelPath, e: HashEvent| {
        let k = if p.as_bytes().starts_with(b"big") {
            0
        } else {
            2
        };
        match e {
            HashEvent::Start { .. } => {
                let now = c[k].fetch_add(1, Relaxed) + 1;
                c[k + 1].fetch_max(now, Relaxed);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            HashEvent::Done => {
                c[k].fetch_sub(1, Relaxed);
            }
        }
    });
    let idx = Index::open(state.path(), root.path())
        .unwrap()
        .into_handle();
    let h = Hasher::new(idx, root.path().to_path_buf(), WORKERS).with_progress(progress);
    for kind in ["big", "small"] {
        let items: Vec<_> = (0..WORKERS)
            .map(|i| item(root.path(), format!("{kind}{i}").as_bytes()))
            .collect();
        let digests = futures::future::join_all(items.iter().map(|it| h.digest(it))).await;
        assert!(digests.iter().all(Result::is_ok));
    }
    let (big, small) = (counts[1].load(Relaxed), counts[3].load(Relaxed));
    assert!(big <= LARGE_WORKERS, "{big} large files at once");
    assert!(small > LARGE_WORKERS, "only {small} small files at once");
}
