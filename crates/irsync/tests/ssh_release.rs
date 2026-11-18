//! The release download, against a fake release in a local directory.
//! Nothing here touches the network.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use irsync::ssh::release::fetch;

#[path = "common/release.rs"]
mod release;

/// Tests that set process-wide env must not overlap.
static ENV: Mutex<()> = Mutex::new(());

struct World {
    /// Holds `tmpdir`.
    _outer: tempfile::TempDir,
    /// What `TMPDIR` points at; its name holds a space.
    tmpdir: PathBuf,
    /// The fake release directory.
    rel: tempfile::TempDir,
    target: &'static str,
    _guard: std::sync::MutexGuard<'static, ()>,
}

fn world() -> World {
    let guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: serialized by ENV; no other thread reads these concurrently.
    unsafe { std::env::remove_var("TMPDIR") };
    let outer = tempfile::tempdir().unwrap();
    let rel = tempfile::tempdir().unwrap();
    let tmpdir = outer.path().join("tmp dir");
    std::fs::create_dir(&tmpdir).unwrap();
    // SAFETY: as above.
    unsafe {
        std::env::set_var("TMPDIR", &tmpdir);
        std::env::set_var("IRSYNC_TEST_RELEASE_URL", release::url_of(rel.path()));
        std::env::set_var("IRSYNC_TEST_RELEASE_KEYS", release::keys_file());
    }
    World {
        _outer: outer,
        tmpdir,
        rel,
        target: release::other_platform().1,
        _guard: guard,
    }
}

impl World {
    fn tarball(&self) -> String {
        format!("irsync-{}.tar.xz", self.target)
    }

    fn left_in_tmpdir(&self) -> usize {
        std::fs::read_dir(&self.tmpdir).unwrap().count()
    }
}

fn this_irsync() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_irsync"))
}

#[test]
fn fetch_unpacks_the_binary() {
    let w = world();
    release::fake_release(w.rel.path(), w.target, this_irsync());
    let got = fetch(&release::tag(), w.target).unwrap();
    let bin = got.dir().join("irsync");
    assert_eq!(
        std::fs::read(&bin).unwrap(),
        std::fs::read(this_irsync()).unwrap()
    );
    assert!(bin.starts_with(&w.tmpdir), "{bin:?}");
    let path = got.dir().to_path_buf();
    drop(got);
    assert!(!path.exists());
    assert_eq!(w.left_in_tmpdir(), 0);
}

#[test]
fn fetch_refuses_a_damaged_download() {
    let w = world();
    release::fake_release(w.rel.path(), w.target, this_irsync());
    let tarball = w.rel.path().join(w.tarball());
    let mut bytes = std::fs::read(&tarball).unwrap();
    bytes.push(0);
    std::fs::write(&tarball, bytes).unwrap();
    let e = fetch(&release::tag(), w.target).unwrap_err().to_string();
    assert!(e.contains("SHA-256 does not match"), "{e}");
    assert_eq!(w.left_in_tmpdir(), 0);
}

#[test]
fn fetch_refuses_a_tarball_missing_from_the_sums() {
    let w = world();
    release::fake_release(w.rel.path(), w.target, this_irsync());
    std::fs::write(w.rel.path().join("sha256.sum"), "abcd *other.tar.xz\n").unwrap();
    release::sign_sums(w.rel.path(), "a", &format!("irsync {}", release::tag()));
    let e = fetch(&release::tag(), w.target).unwrap_err().to_string();
    assert!(e.contains("sha256.sum does not list"), "{e}");
    assert_eq!(w.left_in_tmpdir(), 0);
}

#[test]
fn fetch_reports_a_failed_download() {
    let w = world();
    let e = fetch(&release::tag(), w.target).unwrap_err().to_string();
    assert!(e.contains("could not download"), "{e}");
    assert!(e.contains(&w.tarball()), "{e}");
    assert_eq!(w.left_in_tmpdir(), 0);
}

#[test]
fn fetch_refuses_a_tarball_without_the_binary() {
    let w = world();
    release::pack(w.rel.path(), w.target, |inner| {
        std::fs::write(inner.join("README.md"), "no binary here\n").unwrap();
    });
    let e = fetch(&release::tag(), w.target).unwrap_err().to_string();
    assert!(e.contains("does not hold"), "{e}");
    assert_eq!(w.left_in_tmpdir(), 0);
}

#[test]
fn fetch_refuses_a_symlink_for_the_binary() {
    let w = world();
    release::pack(w.rel.path(), w.target, |inner| {
        std::os::unix::fs::symlink("/bin/sh", inner.join("irsync")).unwrap();
    });
    let e = fetch(&release::tag(), w.target).unwrap_err().to_string();
    assert!(e.contains("does not hold"), "{e}");
    assert_eq!(w.left_in_tmpdir(), 0);
}

#[test]
fn fetch_accepts_signed_release() {
    let w = world();
    release::fake_release(w.rel.path(), w.target, this_irsync());
    assert!(w.rel.path().join("sha256.sum.minisig").exists());
    let got = fetch(&release::tag(), w.target).unwrap();
    assert!(got.dir().join("irsync").is_file());
}

#[test]
fn fetch_fails_without_signature() {
    let w = world();
    release::fake_release(w.rel.path(), w.target, this_irsync());
    std::fs::remove_file(w.rel.path().join("sha256.sum.minisig")).unwrap();
    let tag = release::tag();
    let e = fetch(&tag, w.target).unwrap_err().to_string();
    assert_eq!(
        e,
        format!("the release {tag} has no signature for sha256.sum; refusing to use the download")
    );
    assert_eq!(w.left_in_tmpdir(), 0);
}

#[test]
fn fetch_rejects_sums_signed_for_other_tag() {
    let w = world();
    release::fake_release(w.rel.path(), w.target, this_irsync());
    release::sign_sums(w.rel.path(), "a", "irsync v1.2.2");
    let e = fetch("v1.2.3", w.target).unwrap_err().to_string();
    assert!(
        e.starts_with("the release v1.2.3 failed the signature check: "),
        "{e}"
    );
    assert!(
        e.contains("the signature is for \"irsync v1.2.2\", not \"irsync v1.2.3\""),
        "{e}"
    );
    assert_eq!(w.left_in_tmpdir(), 0);
}

#[test]
fn fetch_rejects_unknown_key() {
    let w = world();
    release::fake_release(w.rel.path(), w.target, this_irsync());
    let tag = release::tag();
    release::sign_sums(w.rel.path(), "b", &format!("irsync {tag}"));
    let e = fetch(&tag, w.target).unwrap_err().to_string();
    assert!(
        e.starts_with(&format!("the release {tag} failed the signature check: ")),
        "{e}"
    );
    assert!(e.contains("which is not an irsync release key"), "{e}");
    assert_eq!(w.left_in_tmpdir(), 0);
}

#[test]
fn fetch_rejects_a_changed_sums_file() {
    let w = world();
    release::fake_release(w.rel.path(), w.target, this_irsync());
    let sums = w.rel.path().join("sha256.sum");
    let mut text = std::fs::read_to_string(&sums).unwrap();
    text.push_str("abcd *other.tar.xz\n");
    std::fs::write(&sums, text).unwrap();
    let tag = release::tag();
    let e = fetch(&tag, w.target).unwrap_err().to_string();
    assert!(
        e.contains("failed the signature check: the signature does not match the file"),
        "{e}"
    );
    assert_eq!(w.left_in_tmpdir(), 0);
}
