//! A fake release directory for the tests of the release download: a
//! `.tar.xz` and a `sha256.sum`, laid out like a GitHub release, served
//! through a `file://` URL. Needs the system `tar` with xz support.
#![allow(dead_code)]

use std::path::Path;
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

/// The `file://` URL of `dir`.
pub fn url_of(dir: &Path) -> String {
    format!("file://{}", dir.display())
}

/// Write `irsync-<target>.tar.xz` into `dir`, holding the directory
/// `irsync-<target>/` as `fill` leaves it, and a matching `sha256.sum`.
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

/// (Re)write `dir/sha256.sum` for the file `name` in it.
pub fn write_sums(dir: &Path, name: &str) {
    let digest = Sha256::digest(std::fs::read(dir.join(name)).unwrap());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(dir.join("sha256.sum"), format!("{hex} *{name}\n")).unwrap();
}

/// A release holding a copy of `bin` as `irsync-<target>/irsync`.
pub fn fake_release(dir: &Path, target: &str, bin: &Path) -> String {
    pack(dir, target, |inner| {
        // `copy` keeps the mode, so the binary stays executable.
        std::fs::copy(bin, inner.join("irsync")).unwrap();
    })
}
