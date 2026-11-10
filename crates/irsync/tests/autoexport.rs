//! Where the daemon puts the automatic export for a path an immediate
//! client reached.

use irsync::autoexport::{auto_root, AutoRootError};

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
fn auto_root_missing_push_uses_parent() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(
        auto_root(&tmp.path().join("new"), true).unwrap(),
        tmp.path()
    );
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
