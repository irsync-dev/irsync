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
    let d1 = h.digest(&item(root.path(), b"f")).await.unwrap();
    fs::write(&p, b"bbbb").unwrap();
    set_mtime(&p, 1_700_000_000, 5);
    let d2 = h.digest(&item(root.path(), b"f")).await.unwrap();
    assert_eq!(h.stats().files_hashed.load(Relaxed), 2);
    assert_ne!(d1.hash, d2.hash);
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
    assert_eq!(h.cached(&it).unwrap(), Some(d));
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
    assert_eq!(h.cached(&it).unwrap(), Some(d));
    // `-c`: a fresh row is not evidence; the file has to be read.
    let forced = h.with_force(true);
    assert!(forced.cached(&it).unwrap().is_none());
}
