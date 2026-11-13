//! One file as the source of a push: the daemon places it by rsync's rules
//! (into a directory, under a new name, or over a file), lists only that
//! file in the directory that holds it, and never touches anything else.

mod common;

use std::fs;
use std::os::unix::fs::{FileExt, PermissionsExt};
use std::path::Path;
use std::time::{Duration, SystemTime};

use common::{archive_opts, Harness};
use irsync::cli::{Args, SyncOptions};
use irsync::fs::paths::RelPath;
use irsync::session::single::SingleFile;
use irsync::session::SessionKey;

const KIB: usize = 1024;
const MIB: usize = 1024 * 1024;

async fn within30<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(30), f)
        .await
        .expect("test timed out after 30 s")
}

fn opts(args: &[&str]) -> SyncOptions {
    let mut v = vec!["irsync"];
    v.extend_from_slice(args);
    v.extend_from_slice(&["src", "host:m/"]);
    SyncOptions::from_args(&Args::try_parse_from(v).unwrap())
}

/// Deterministic pseudo-random bytes.
fn data(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2654435761).wrapping_add(1);
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

fn mode(p: &Path) -> u32 {
    fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777
}

fn mtime(p: &Path) -> SystemTime {
    fs::symlink_metadata(p).unwrap().modified().unwrap()
}

fn set_mtime(p: &Path, t: SystemTime) {
    fs::File::open(p).unwrap().set_modified(t).unwrap();
}

/// `f.iso` in the client's local dir, with mode 0o640.
fn source(h: &Harness, bytes: &[u8]) -> std::path::PathBuf {
    let f = h.local.path().join("f.iso");
    fs::write(&f, bytes).unwrap();
    fs::set_permissions(&f, fs::Permissions::from_mode(0o640)).unwrap();
    f
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_into_dir() {
    within30(async {
        let h = Harness::new().await;
        let f = source(&h, b"image");
        fs::create_dir(h.export.path().join("T")).unwrap();
        let out = h
            .run_single_push(&archive_opts(), &f, b"m/T", false)
            .await
            .unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let got = h.export.path().join("T/f.iso");
        assert_eq!(fs::read(&got).unwrap(), b"image");
        assert_eq!(mode(&got), 0o640);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_to_new_name() {
    within30(async {
        let h = Harness::new().await;
        let f = source(&h, b"image");
        let out = h
            .run_single_push(&archive_opts(), &f, b"m/T", false)
            .await
            .unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let t = h.export.path().join("T");
        assert!(fs::symlink_metadata(&t).unwrap().is_file());
        assert_eq!(fs::read(&t).unwrap(), b"image");
        assert!(fs::symlink_metadata(h.export.path().join("f.iso")).is_err());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_with_slash_creates_dir() {
    within30(async {
        let h = Harness::new().await;
        let f = source(&h, b"image");
        let out = h
            .run_single_push(&archive_opts(), &f, b"m/T", true)
            .await
            .unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(h.export.path().join("T").is_dir());
        assert_eq!(fs::read(h.export.path().join("T/f.iso")).unwrap(), b"image");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_with_slash_onto_file_fails() {
    within30(async {
        let h = Harness::new().await;
        let f = source(&h, b"image");
        let t = h.export.path().join("T");
        fs::write(&t, b"old").unwrap();
        let err = h
            .run_single_push(&archive_opts(), &f, b"m/T", true)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("not a directory"), "{err:#}");
        assert_eq!(fs::read(&t).unwrap(), b"old");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_dir_in_the_way_fails() {
    within30(async {
        let h = Harness::new().await;
        let f = source(&h, b"image");
        fs::create_dir_all(h.export.path().join("T/f.iso")).unwrap();
        let err = h
            .run_single_push(&archive_opts(), &f, b"m/T", false)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("is a directory"), "{err:#}");
        assert!(h.export.path().join("T/f.iso").is_dir());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_again_transfers_nothing() {
    within30(async {
        let h = Harness::new().await;
        let f = source(&h, &data(64 * KIB, 1));
        fs::create_dir(h.export.path().join("T")).unwrap();
        let first = h
            .run_single_push(&archive_opts(), &f, b"m/T", false)
            .await
            .unwrap();
        assert!(first.bytes_transferred > 0, "{first:?}");
        let again = h
            .run_single_push(&archive_opts(), &f, b"m/T", false)
            .await
            .unwrap();
        assert!(again.errors.is_empty(), "{:?}", again.errors);
        assert_eq!(again.bytes_transferred, 0, "{again:?}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_changed_file_uses_delta() {
    within30(async {
        let h = Harness::new().await;
        let f = source(&h, &data(8 * MIB, 1));
        fs::create_dir(h.export.path().join("T")).unwrap();
        h.run_single_push(&archive_opts(), &f, b"m/T", false)
            .await
            .unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&f)
            .unwrap()
            .write_all_at(&data(4 * KIB, 2), 4 * MIB as u64)
            .unwrap();
        set_mtime(
            &f,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000),
        );
        let out = h
            .run_single_push(&archive_opts(), &f, b"m/T", false)
            .await
            .unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(out.bytes_transferred < MIB as u64, "{out:?}");
        assert_eq!(
            fs::read(h.export.path().join("T/f.iso")).unwrap(),
            fs::read(&f).unwrap()
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_leaves_siblings_and_dir_metadata() {
    within30(async {
        let h = Harness::new().await;
        let f = source(&h, b"image");
        // The source's holding directory has metadata that must not travel.
        fs::set_permissions(h.local.path(), fs::Permissions::from_mode(0o755)).unwrap();
        set_mtime(
            h.local.path(),
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000),
        );
        let t = h.export.path().join("T");
        fs::create_dir(&t).unwrap();
        fs::write(t.join("other"), b"keep me").unwrap();
        fs::set_permissions(&t, fs::Permissions::from_mode(0o700)).unwrap();
        let o = opts(&["-a", "--delete"]);

        // A new file: written into T (the kernel moves T's mtime, as with
        // rsync), but T's mode is not the source directory's.
        let out = h.run_single_push(&o, &f, b"m/T", false).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(t.join("f.iso")).unwrap(), b"image");
        assert_eq!(fs::read(t.join("other")).unwrap(), b"keep me");
        assert_eq!(mode(&t), 0o700);

        // Nothing to write: T's metadata stays exactly as it is.
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_500_000_000);
        set_mtime(&t, old);
        let out = h.run_single_push(&o, &f, b"m/T", false).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(out.deleted.is_empty(), "{:?}", out.deleted);
        assert_eq!(fs::read(t.join("other")).unwrap(), b"keep me");
        assert_eq!(mode(&t), 0o700);
        assert_eq!(mtime(&t), old);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_excluded_file_copies_nothing() {
    within30(async {
        let h = Harness::new().await;
        let f = source(&h, b"image");
        fs::create_dir(h.export.path().join("T")).unwrap();
        let o = opts(&["-a", "--exclude", "f.iso"]);
        let out = h.run_single_push(&o, &f, b"m/T", false).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(fs::symlink_metadata(h.export.path().join("T/f.iso")).is_err());
    })
    .await;
}

/// `f.iso` in the local dir as a symlink to `target`.
fn link_source(h: &Harness) -> std::path::PathBuf {
    fs::write(h.local.path().join("target"), b"pointed at").unwrap();
    let f = h.local.path().join("f.iso");
    std::os::unix::fs::symlink("target", &f).unwrap();
    f
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_symlink_source_without_l_skips() {
    within30(async {
        let h = Harness::new().await;
        let f = link_source(&h);
        fs::create_dir(h.export.path().join("T")).unwrap();
        let out = h
            .run_single_push(&opts(&["-rt"]), &f, b"m/T", false)
            .await
            .unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(fs::symlink_metadata(h.export.path().join("T/f.iso")).is_err());
        assert!(fs::symlink_metadata(h.export.path().join("T/target")).is_err());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_symlink_source_with_l_copies_link() {
    within30(async {
        let h = Harness::new().await;
        let f = link_source(&h);
        fs::create_dir(h.export.path().join("T")).unwrap();
        let out = h
            .run_single_push(&archive_opts(), &f, b"m/T", false)
            .await
            .unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let got = h.export.path().join("T/f.iso");
        assert!(fs::symlink_metadata(&got).unwrap().is_symlink());
        assert_eq!(fs::read_link(&got).unwrap(), Path::new("target"));
        assert!(fs::symlink_metadata(h.export.path().join("T/target")).is_err());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_onto_symlink_replaces_the_link() {
    within30(async {
        let h = Harness::new().await;
        let f = source(&h, b"image");
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim");
        fs::write(&victim, b"untouched").unwrap();
        let t = h.export.path().join("T");
        std::os::unix::fs::symlink(&victim, &t).unwrap();
        let out = h
            .run_single_push(&archive_opts(), &f, b"m/T", false)
            .await
            .unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(fs::symlink_metadata(&t).unwrap().is_file());
        assert_eq!(fs::read(&t).unwrap(), b"image");
        assert_eq!(fs::read(&victim).unwrap(), b"untouched");
    })
    .await;
}

#[test]
fn single_and_dir_sessions_have_different_keys() {
    let client = iroh::SecretKey::from_bytes(&[7; 32]).public();
    let hello = Harness::hello(common::Flow::Push);
    let rel = RelPath::new(b"T").unwrap();
    let name = RelPath::new(b"f.iso").unwrap();
    let single = SingleFile {
        wire: name.clone(),
        local: name,
    };
    let dir = SessionKey::for_daemon(client, "m", &rel, &hello, None);
    let one = SessionKey::for_daemon(client, "m", &rel, &hello, Some(&single));
    assert_ne!(dir.as_bytes(), one.as_bytes());
}
