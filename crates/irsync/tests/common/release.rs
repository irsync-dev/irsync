//! A fake release directory for the tests of the release download: a
//! `.tar.xz`, a `sha256.sum` and its signature `sha256.sum.minisig`, laid
//! out like a GitHub release, served through a `file://` URL. Needs the
//! system `tar` with xz support.
//!
//! The signatures are made here, with the test keys in
//! `tests/fixtures/minisign`. The download trusts the keys in the file
//! `IRSYNC_TEST_RELEASE_KEYS` names: set it to [`keys_file`].
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

/// A supported platform that is not this machine's: the `uname -sm` text,
/// the release target, and "os cpu" as messages print it.
pub fn other_platform() -> (&'static str, &'static str, &'static str) {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        (
            "Linux aarch64",
            "aarch64-unknown-linux-musl",
            "linux aarch64",
        )
    } else {
        ("Linux x86_64", "x86_64-unknown-linux-musl", "linux x86_64")
    }
}

/// The tag the client asks for: its own version. The fake releases are
/// signed for it.
pub fn tag() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/minisign")
        .join(name)
}

/// The public key file the download should trust: test key `a`.
pub fn keys_file() -> PathBuf {
    fixture("a.pub")
}

/// Sign `dir/sha256.sum` into `dir/sha256.sum.minisig` with test key
/// `key` (`"a"` or `"b"`), with the trusted comment `comment`.
pub fn sign_sums(dir: &Path, key: &str, comment: &str) {
    use ct_codecs::{Base64, Decoder};
    let text = std::fs::read_to_string(fixture(&format!("{key}.key"))).unwrap();
    // The minisign tool writes a password-less key with an all-zero
    // checksum, which the minisign crate's key loader rejects. Read the
    // key's bytes directly instead: that skips only the checksum.
    let line = text.lines().nth(1).unwrap();
    let sk = minisign::SecretKey::from_bytes(&Base64::decode_to_vec(line, None).unwrap()).unwrap();
    let sums = std::fs::read(dir.join("sha256.sum")).unwrap();
    let sig = minisign::sign(None, &sk, &sums[..], Some(comment), None).unwrap();
    std::fs::write(dir.join("sha256.sum.minisig"), sig.into_string()).unwrap();
}

/// The `file://` URL of `dir`.
pub fn url_of(dir: &Path) -> String {
    format!("file://{}", dir.display())
}

/// Write `irsync-<target>.tar.xz` into `dir`, holding the directory
/// `irsync-<target>/` as `fill` leaves it, and a matching, signed
/// `sha256.sum`.
/// Returns the `file://` URL of `dir`.
pub fn pack(dir: &Path, target: &str, fill: impl FnOnce(&Path)) -> String {
    let stage = tempfile::tempdir().unwrap();
    let top = format!("irsync-{target}");
    let inner = stage.path().join(&top);
    std::fs::create_dir(&inner).unwrap();
    fill(&inner);
    let name = format!("{top}.tar.xz");
    // The fastest xz level: a debug irsync is over 100 MB, and the default
    // level takes many seconds on it. bsdtar (macOS) and GNU tar spell
    // the setting differently.
    let version = Command::new("tar").arg("--version").output().unwrap();
    let bsdtar = String::from_utf8_lossy(&version.stdout).contains("bsdtar");
    let mut tar = Command::new("tar");
    if bsdtar {
        tar.args(["--options", "xz:compression-level=0"]);
    } else {
        tar.env("XZ_OPT", "-0");
    }
    let status = tar
        .arg("-cJf")
        .arg(dir.join(&name))
        .arg("-C")
        .arg(stage.path())
        .arg(&top)
        .status()
        .unwrap();
    assert!(status.success(), "tar -cJf failed");
    write_sums(dir, &name);
    url_of(dir)
}

/// (Re)write `dir/sha256.sum` for the file `name` in it, and sign it with
/// test key `a` for [`tag`].
pub fn write_sums(dir: &Path, name: &str) {
    let digest = Sha256::digest(std::fs::read(dir.join(name)).unwrap());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(dir.join("sha256.sum"), format!("{hex} *{name}\n")).unwrap();
    sign_sums(dir, "a", &format!("irsync {}", tag()));
}

/// A release holding a copy of `bin` as `irsync-<target>/irsync`.
pub fn fake_release(dir: &Path, target: &str, bin: &Path) -> String {
    pack(dir, target, |inner| {
        // `copy` keeps the mode, so the binary stays executable.
        std::fs::copy(bin, inner.join("irsync")).unwrap();
    })
}

/// The three man pages a release holds, relative to its directory.
pub const PAGES: [&str; 3] = [
    "man/man1/irsync.1",
    "man/man1/fileset.1",
    "man/man5/irsync.toml.5",
];

/// A release as the real ones are: a copy of `bin` as `irsync`, a stand-in
/// `fileset` and the three man pages.
pub fn full_release(dir: &Path, target: &str, bin: &Path) -> String {
    pack(dir, target, |inner| {
        std::fs::copy(bin, inner.join("irsync")).unwrap();
        put_extras(inner);
    })
}

/// A stand-in `fileset` and the three man pages, in `dir`.
pub fn put_extras(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let fileset = dir.join("fileset");
    std::fs::write(&fileset, "#!/bin/sh\necho 'fileset (stand-in)'\n").unwrap();
    std::fs::set_permissions(&fileset, std::fs::Permissions::from_mode(0o755)).unwrap();
    for page in PAGES {
        let p = dir.join(page);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, ".TH STAND-IN 1\n").unwrap();
    }
}
