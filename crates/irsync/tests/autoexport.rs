//! Where the daemon puts the automatic export for a path an immediate
//! client reached.

use std::path::Path;
use std::time::Duration;

use irsync::authz::{Authorizer, FileAuthorizer};
use irsync::autoexport::{auto_root, check_root, AutoRootError};
use irsync::config::paths::Paths;

#[test]
fn auto_root_dir_is_itself() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(auto_root(tmp.path(), false).unwrap(), tmp.path());
    assert_eq!(auto_root(tmp.path(), true).unwrap(), tmp.path());
}

#[test]
fn auto_root_file_uses_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("hostname");
    std::fs::write(&file, b"x").unwrap();
    assert_eq!(auto_root(&file, false).unwrap(), tmp.path());
    assert_eq!(auto_root(&file, true).unwrap(), tmp.path());
}

#[test]
fn auto_root_missing_push_creates_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let new = tmp.path().join("new");
    assert_eq!(auto_root(&new, true).unwrap(), new);
    assert!(new.is_dir(), "the push target was not created");
}

#[test]
fn auto_root_is_never_the_filesystem_root() {
    assert_eq!(auto_root(Path::new("/"), false), Err(AutoRootError::Root));
    assert_eq!(auto_root(Path::new("/"), true), Err(AutoRootError::Root));
    assert_eq!(check_root(Path::new("/")), Err(AutoRootError::Root));
}

#[test]
fn auto_root_refuses_non_utf8_before_creating() {
    use std::os::unix::ffi::OsStrExt;
    let tmp = tempfile::tempdir().unwrap();
    let bad = tmp.path().join(std::ffi::OsStr::from_bytes(b"caf\xe9"));
    assert_eq!(auto_root(&bad, true), Err(AutoRootError::NotUtf8));
    assert!(!bad.exists(), "a refused root was created");
}

fn id(n: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
}

/// Another editor holding the config lock briefly does not fail the add:
/// it waits for the lock.
#[test]
fn file_authorizer_waits_for_the_config_lock() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("irsync.toml");
    std::fs::write(&config, format!("[immediate]\nrw = [\"{}\"]\n", id(1))).unwrap();
    let lock = dir.path().join("irsync.toml.lock");
    std::fs::write(&lock, b"").unwrap();
    let release = std::thread::spawn({
        let lock = lock.clone();
        move || {
            std::thread::sleep(Duration::from_millis(300));
            std::fs::remove_file(lock).unwrap();
        }
    });
    let authz = FileAuthorizer::new(Paths::resolve(Some(&config)));
    let root = dir.path().join("data");
    std::fs::create_dir(&root).unwrap();
    authz
        .add_auto_export(&root, id(1))
        .expect("add after the lock is released");
    release.join().unwrap();
    let snap = authz.snapshot().unwrap();
    let auto: Vec<_> = snap.exports.values().filter(|e| e.auto).collect();
    assert_eq!(auto.len(), 1, "{:?}", snap.exports);
    assert_eq!(auto[0].path, root);
    assert!(!lock.exists());
}

#[test]
fn auto_root_missing_pull_is_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(
        auto_root(&tmp.path().join("nope"), false),
        Err(AutoRootError::NotFound)
    );
}

#[test]
fn auto_root_missing_parent_is_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let deep = tmp.path().join("no/such");
    assert_eq!(auto_root(&deep, true), Err(AutoRootError::NotFound));
    assert_eq!(auto_root(&deep, false), Err(AutoRootError::NotFound));
    // A parent that is a file is no directory to export either.
    let file = tmp.path().join("f");
    std::fs::write(&file, b"x").unwrap();
    assert_eq!(
        auto_root(&file.join("x"), true),
        Err(AutoRootError::NotFound)
    );
}
