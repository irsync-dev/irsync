//! One file as the source of a copy: the receiving side places it by
//! rsync's rules (into a directory, under a new name, or over a file), the
//! session lists only that file in the directory that holds it, and nothing
//! else is touched. A pull first probes what the remote path is.

mod common;

use std::fs;
use std::os::unix::fs::{FileExt, PermissionsExt};
use std::path::Path;
use std::time::{Duration, SystemTime};

use common::{archive_opts, Flow, Harness};
use irsync::cli::{Args, SyncOptions};
use irsync::fs::paths::RelPath;
use irsync::session::single::SingleFile;
use irsync::session::SessionKey;
use irsync_proto::v1::path_info::Kind;

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

// ---- Renamed pushes: the wire name differs from the sender's name. ----

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_to_new_name_again_transfers_nothing() {
    within30(async {
        let h = Harness::new().await;
        let f = source(&h, &data(64 * KIB, 1));
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
        assert_eq!(
            fs::read(h.export.path().join("T")).unwrap(),
            fs::read(&f).unwrap()
        );
    })
    .await;
}

/// Changes 4 KiB in the middle of `f` and gives it a new mtime.
fn change_middle(f: &Path) {
    fs::OpenOptions::new()
        .write(true)
        .open(f)
        .unwrap()
        .write_all_at(&data(4 * KIB, 2), 4 * MIB as u64)
        .unwrap();
    set_mtime(
        f,
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_to_new_name_uses_delta() {
    within30(async {
        let h = Harness::new().await;
        let f = source(&h, &data(8 * MIB, 1));
        h.run_single_push(&archive_opts(), &f, b"m/T", false)
            .await
            .unwrap();
        change_middle(&f);
        let out = h
            .run_single_push(&archive_opts(), &f, b"m/T", false)
            .await
            .unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(out.bytes_transferred < MIB as u64, "{out:?}");
        assert_eq!(
            fs::read(h.export.path().join("T")).unwrap(),
            fs::read(&f).unwrap()
        );
    })
    .await;
}

// ---- Symlinks inside the export are never walked through. ----

/// `m/link` points at a directory outside the export holding the file
/// `secret` and the directory `sub/f.iso`.
fn outside_link(h: &Harness) -> tempfile::TempDir {
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret"), b"outside").unwrap();
    fs::create_dir_all(outside.path().join("sub/f.iso")).unwrap();
    std::os::unix::fs::symlink(outside.path(), h.export.path().join("link")).unwrap();
    outside
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn probe_through_symlink_is_missing() {
    within30(async {
        let h = Harness::new().await;
        let _outside = outside_link(&h);
        assert_eq!(h.probe(b"m/link/secret").await.unwrap(), Kind::Missing);
        assert_eq!(h.probe(b"m/link/sub").await.unwrap(), Kind::Missing);
        assert_eq!(h.probe(b"m/link/nope").await.unwrap(), Kind::Missing);
        // The link itself is not followed: it is not a directory.
        assert_eq!(h.probe(b"m/link").await.unwrap(), Kind::File);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_through_symlink_fails() {
    within30(async {
        let h = Harness::new().await;
        let outside = outside_link(&h);
        let f = source(&h, b"image");
        // Through the link: refused before anything below it is looked at
        // (`sub/f.iso` being a directory is not revealed).
        for (dest, co) in [
            (&b"m/link/secret"[..], false),
            (b"m/link/new", false),
            (b"m/link/sub", false),
            (b"m/link/sub", true),
        ] {
            let err = h
                .run_single_push(&archive_opts(), &f, dest, co)
                .await
                .unwrap_err();
            let err = format!("{err:#}");
            assert!(err.contains("bad session path"), "{dest:?}: {err}");
            assert!(err.contains("is not a directory"), "{dest:?}: {err}");
        }
        // Into the link: it is not followed.
        let err = h
            .run_single_push(&archive_opts(), &f, b"m/link", true)
            .await
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("is a symlink to a directory"),
            "{err:#}"
        );
        assert_eq!(fs::read(outside.path().join("secret")).unwrap(), b"outside");
        let mut names: Vec<_> = fs::read_dir(outside.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, ["secret", "sub"]);
        assert_eq!(
            fs::read_dir(outside.path().join("sub/f.iso"))
                .unwrap()
                .count(),
            0
        );
    })
    .await;
}

/// A push to `m/link`, a symlink to a directory outside the export, is
/// refused: the link stays and nothing is written where it points.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_onto_symlink_to_dir_is_refused() {
    within30(async {
        let h = Harness::new().await;
        let outside = outside_link(&h);
        let f = source(&h, b"image");
        let err = h
            .run_single_push(&archive_opts(), &f, b"m/link", false)
            .await
            .unwrap_err();
        let err = format!("{err:#}");
        let want = format!(
            "bad session path: {} is a symlink to a directory",
            h.export.path().join("link").display()
        );
        assert!(err.contains(&want), "{err}");
        let link = h.export.path().join("link");
        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(fs::read_link(&link).unwrap(), outside.path());
        let mut names: Vec<_> = fs::read_dir(outside.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, ["secret", "sub"]);
        assert_eq!(fs::read(outside.path().join("secret")).unwrap(), b"outside");
    })
    .await;
}

// ---- Probe and pull. ----

/// `f.iso` at the export's root, with mode 0o640.
fn remote_source(h: &Harness, bytes: &[u8]) -> std::path::PathBuf {
    let f = h.export.path().join("f.iso");
    fs::write(&f, bytes).unwrap();
    fs::set_permissions(&f, fs::Permissions::from_mode(0o640)).unwrap();
    f
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn probe_kinds() {
    within30(async {
        let h = Harness::new().await;
        remote_source(&h, b"image");
        assert_eq!(h.probe(b"m/f.iso").await.unwrap(), Kind::File);
        assert_eq!(h.probe(b"m").await.unwrap(), Kind::Directory);
        assert_eq!(h.probe(b"m/nope").await.unwrap(), Kind::Missing);
        assert_eq!(h.probe(b"m/nope/deeper").await.unwrap(), Kind::Missing);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pull_into_dir() {
    within30(async {
        let h = Harness::new().await;
        remote_source(&h, b"image");
        let l = h.local.path().join("L");
        fs::create_dir(&l).unwrap();
        let out = h
            .run_single_pull(&archive_opts(), b"m/f.iso", &l, false)
            .await
            .unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(l.join("f.iso")).unwrap(), b"image");
        assert_eq!(mode(&l.join("f.iso")), 0o640);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pull_to_new_name() {
    within30(async {
        let h = Harness::new().await;
        remote_source(&h, b"image");
        let l = h.local.path().join("L");
        let out = h
            .run_single_pull(&archive_opts(), b"m/f.iso", &l, false)
            .await
            .unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(fs::symlink_metadata(&l).unwrap().is_file());
        assert_eq!(fs::read(&l).unwrap(), b"image");
        assert!(fs::symlink_metadata(h.local.path().join("f.iso")).is_err());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pull_with_slash_creates_dir() {
    within30(async {
        let h = Harness::new().await;
        remote_source(&h, b"image");
        let l = h.local.path().join("L");
        let out = h
            .run_single_pull(&archive_opts(), b"m/f.iso", &l, true)
            .await
            .unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(l.is_dir());
        assert_eq!(fs::read(l.join("f.iso")).unwrap(), b"image");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pull_onto_file_updates_it() {
    within30(async {
        let h = Harness::new().await;
        remote_source(&h, b"image");
        let l = h.local.path().join("L");
        fs::write(&l, b"old contents").unwrap();
        let out = h
            .run_single_pull(&archive_opts(), b"m/f.iso", &l, false)
            .await
            .unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(fs::symlink_metadata(&l).unwrap().is_file());
        assert_eq!(fs::read(&l).unwrap(), b"image");
        assert_eq!(mode(&l), 0o640);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pull_into_busy_dir_touches_nothing_else() {
    within30(async {
        let h = Harness::new().await;
        remote_source(&h, b"image");
        // The remote holding directory has metadata that must not travel.
        fs::set_permissions(h.export.path(), fs::Permissions::from_mode(0o755)).unwrap();
        set_mtime(
            h.export.path(),
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000),
        );
        let l = h.local.path().join("L");
        fs::create_dir_all(l.join("b")).unwrap();
        fs::write(l.join("a"), b"keep a").unwrap();
        fs::write(l.join("b/c"), b"keep c").unwrap();
        fs::set_permissions(&l, fs::Permissions::from_mode(0o750)).unwrap();
        let o = opts(&["-a", "--delete"]);

        // A new file: written into L (the kernel moves L's mtime, as with
        // rsync), but nothing else changes.
        let out = h.run_single_pull(&o, b"m/f.iso", &l, false).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(out.deleted.is_empty(), "{:?}", out.deleted);
        assert_eq!(fs::read(l.join("f.iso")).unwrap(), b"image");
        assert_eq!(fs::read(l.join("a")).unwrap(), b"keep a");
        assert_eq!(fs::read(l.join("b/c")).unwrap(), b"keep c");
        assert_eq!(mode(&l), 0o750);

        // Nothing to write: L's metadata stays exactly as it is.
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_500_000_000);
        set_mtime(&l, old);
        let out = h.run_single_pull(&o, b"m/f.iso", &l, false).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(out.deleted.is_empty(), "{:?}", out.deleted);
        assert_eq!(fs::read(l.join("a")).unwrap(), b"keep a");
        assert_eq!(fs::read(l.join("b/c")).unwrap(), b"keep c");
        assert_eq!(mode(&l), 0o750);
        assert_eq!(mtime(&l), old);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pull_dry_run_creates_nothing() {
    within30(async {
        let h = Harness::new().await;
        remote_source(&h, b"image");
        let l = h.local.path().join("L");
        let o = opts(&["-a", "--dry-run"]);
        let out = h.run_single_pull(&o, b"m/f.iso", &l, true).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(fs::symlink_metadata(&l).is_err());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pull_dir_in_the_way_fails() {
    within30(async {
        let h = Harness::new().await;
        remote_source(&h, b"image");
        let l = h.local.path().join("L");
        fs::create_dir_all(l.join("f.iso")).unwrap();
        let err = h
            .run_single_pull(&archive_opts(), b"m/f.iso", &l, false)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("is a directory"), "{err:#}");
        assert!(l.join("f.iso").is_dir());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pull_of_missing_path_creates_nothing() {
    within30(async {
        let h = Harness::new().await;
        let l = h.local.path().join("L");
        let err = h
            .run_single_pull(&archive_opts(), b"m/nope", &l, true)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("Missing"), "{err:#}");
        assert!(fs::symlink_metadata(&l).is_err());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pull_to_new_name_uses_delta() {
    within30(async {
        let h = Harness::new().await;
        let f = remote_source(&h, &data(8 * MIB, 1));
        let l = h.local.path().join("L");
        h.run_single_pull(&archive_opts(), b"m/f.iso", &l, false)
            .await
            .unwrap();
        assert_eq!(fs::read(&l).unwrap(), fs::read(&f).unwrap());
        change_middle(&f);
        let out = h
            .run_single_pull(&archive_opts(), b"m/f.iso", &l, false)
            .await
            .unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(out.bytes_transferred < MIB as u64, "{out:?}");
        assert_eq!(fs::read(&l).unwrap(), fs::read(&f).unwrap());
    })
    .await;
}

/// A single-file pull `Hello` for `path`, with the receiver's name `name`.
fn pull_hello(path: &[u8], name: &[u8]) -> irsync_proto::v1::Hello {
    let mut hello = Harness::hello(Flow::Pull);
    hello.path = path.to_vec();
    hello.contents_only = false;
    hello.single_file = true;
    hello.name = name.to_vec();
    hello
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pull_with_bad_name_is_refused() {
    within30(async {
        let h = Harness::new().await;
        remote_source(&h, b"image");
        for name in [&b""[..], b"..", b"a/b"] {
            let local = h.local_side().with_single(b"x".to_vec());
            let err = h
                .run_hello(&archive_opts(), pull_hello(b"m/f.iso", name), local)
                .await
                .unwrap_err();
            assert!(format!("{err:#}").contains("invalid name"), "{err:#}");
        }
    })
    .await;
}

/// A raw single-file pull whose parent goes through a symlink inside the
/// export is refused like any path through a link, and nothing comes back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn single_pull_through_symlinked_parent_is_refused() {
    within30(async {
        let h = Harness::new().await;
        let _outside = outside_link(&h);
        let local = h.local_side().with_single(b"secret".to_vec());
        let err = h
            .run_hello(
                &archive_opts(),
                pull_hello(b"m/link/secret", b"secret"),
                local,
            )
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("bad session path"), "{err:#}");
        assert_eq!(fs::read_dir(h.local.path()).unwrap().count(), 0);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn single_pull_of_a_directory_or_nothing_is_refused() {
    within30(async {
        let h = Harness::new().await;
        fs::create_dir(h.export.path().join("d")).unwrap();
        let local = h.local_side().with_single(b"d".to_vec());
        let err = h
            .run_hello(&archive_opts(), pull_hello(b"m/d", b"d"), local)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("is a directory"), "{err:#}");
        // The export root itself, and a file that is not there.
        let local = h.local_side().with_single(b"m".to_vec());
        let err = h
            .run_hello(&archive_opts(), pull_hello(b"m", b"m"), local)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("is a directory"), "{err:#}");
        let local = h.local_side().with_single(b"nope".to_vec());
        let err = h
            .run_hello(&archive_opts(), pull_hello(b"m/nope", b"nope"), local)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("does not exist"), "{err:#}");
    })
    .await;
}

/// A stand-in daemon on loopback that answers each probe connection with
/// what `reply` says for it (by its number, from 0): `None` drops the
/// connection after reading the Hello, `Some(body)` sends `body` framed.
fn fake_daemon(
    server: iroh::Endpoint,
    reply: impl Fn(usize) -> Option<Vec<u8>> + Send + 'static,
) -> tokio::task::JoinHandle<usize> {
    tokio::spawn(async move {
        let mut n = 0;
        while let Some(incoming) = server.accept().await {
            let Ok(conn) = incoming.await else { continue };
            let Ok((mut send, mut recv)) = conn.accept_bi().await else {
                continue;
            };
            let _ = recv.read_to_end(1 << 20).await;
            match reply(n) {
                None => conn.close(0u32.into(), b"gone"),
                Some(body) => {
                    let len = (body.len() as u32).to_be_bytes();
                    send.write_all(&len).await.unwrap();
                    send.write_all(&body).await.unwrap();
                    send.finish().unwrap();
                    conn.closed().await;
                }
            }
            n += 1;
            if n == 2 {
                break;
            }
        }
        n
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn probe_retries_a_connection_lost_before_the_reply() {
    use irsync::session::reconnect::{probe_path, ReconnectOpts};
    use irsync_proto::v1::PathInfo;
    use prost::Message;
    within30(async {
        let (client, server) = common::loopback_pair().await;
        let addr = common::addr_of(&server);
        let file = PathInfo {
            kind: Some(Kind::File as i32),
        }
        .encode_to_vec();
        let daemon = fake_daemon(server, move |n| (n > 0).then(|| file.clone()));
        let opts = ReconnectOpts {
            dial_retries: 3,
            backoff_min: Duration::from_millis(10),
            backoff_max: Duration::from_millis(10),
            ..ReconnectOpts::default()
        };
        let kind = probe_path(&client, addr, b"m/f.iso", "host:m/f.iso", opts)
            .await
            .unwrap();
        assert_eq!(kind, Kind::File);
        assert_eq!(daemon.await.unwrap(), 2, "the lost probe was tried again");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn probe_reports_an_empty_reply_as_malformed() {
    within30(async {
        let (client, server) = common::loopback_pair().await;
        let addr = common::addr_of(&server);
        let _daemon = fake_daemon(server, |_| Some(Vec::new()));
        let conn = client.connect(addr, irsync_proto::ALPN).await.unwrap();
        let err = irsync::session::single::probe(&conn, b"m/f.iso")
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("malformed reply"), "{err:#}");
        assert!(
            err.downcast_ref::<irsync::session::engine::SessionRefused>()
                .is_none(),
            "{err:#}"
        );
        conn.close(0u32.into(), b"done");
    })
    .await;
}
