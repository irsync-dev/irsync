use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use irsync::session::single::{place_file, source_is_single, split_last, PlaceError};

fn parent_name(t: &Path) -> (PathBuf, Vec<u8>) {
    split_last(t)
}

#[test]
fn dir_dest_takes_source_name() {
    let d = tempfile::tempdir().unwrap();
    let t = d.path().join("T");
    fs::create_dir(&t).unwrap();
    let p = place_file(&t, false, b"f.iso", true).unwrap();
    assert_eq!((p.root, p.name), (t, b"f.iso".to_vec()));
}

#[test]
fn dir_dest_with_slash() {
    let d = tempfile::tempdir().unwrap();
    let t = d.path().join("T");
    fs::create_dir(&t).unwrap();
    let p = place_file(&t, true, b"f.iso", true).unwrap();
    assert_eq!((p.root, p.name), (t, b"f.iso".to_vec()));
}

#[test]
fn missing_dest_is_the_file_name() {
    let d = tempfile::tempdir().unwrap();
    let t = d.path().join("T");
    let p = place_file(&t, false, b"f.iso", true).unwrap();
    assert_eq!((p.root, p.name), (parent_name(&t).0, b"T".to_vec()));
}

#[test]
fn missing_dest_with_slash_is_a_dir() {
    let d = tempfile::tempdir().unwrap();
    let t = d.path().join("T");
    let p = place_file(&t, true, b"f.iso", true).unwrap();
    assert_eq!((p.root, p.name), (t, b"f.iso".to_vec()));
}

#[test]
fn file_dest_is_updated() {
    let d = tempfile::tempdir().unwrap();
    let t = d.path().join("T");
    fs::write(&t, b"x").unwrap();
    let p = place_file(&t, false, b"f.iso", true).unwrap();
    assert_eq!((p.root, p.name), (d.path().to_path_buf(), b"T".to_vec()));
}

#[test]
fn file_dest_with_slash_is_an_error() {
    let d = tempfile::tempdir().unwrap();
    let t = d.path().join("T");
    fs::write(&t, b"x").unwrap();
    match place_file(&t, true, b"f.iso", true) {
        Err(PlaceError::NotADirectory(p)) => assert_eq!(p, t),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn dir_in_the_way_is_an_error() {
    let d = tempfile::tempdir().unwrap();
    let t = d.path().join("T");
    fs::create_dir_all(t.join("f.iso")).unwrap();
    match place_file(&t, false, b"f.iso", true) {
        Err(PlaceError::IsADirectory(p)) => assert_eq!(p, t.join("f.iso")),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn symlink_dest_followed_on_client() {
    let d = tempfile::tempdir().unwrap();
    let real = d.path().join("real");
    fs::create_dir(&real).unwrap();
    let t = d.path().join("T");
    symlink(&real, &t).unwrap();
    let p = place_file(&t, false, b"f.iso", true).unwrap();
    assert_eq!((p.root, p.name), (t, b"f.iso".to_vec()));
}

#[test]
fn symlink_dest_is_a_file_on_daemon() {
    let d = tempfile::tempdir().unwrap();
    let real = d.path().join("real");
    fs::create_dir(&real).unwrap();
    let t = d.path().join("T");
    symlink(&real, &t).unwrap();
    let p = place_file(&t, false, b"f.iso", false).unwrap();
    assert_eq!((p.root, p.name), (d.path().to_path_buf(), b"T".to_vec()));
}

#[test]
fn source_is_single_rules() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("dir");
    let file = d.path().join("file");
    fs::create_dir(&dir).unwrap();
    fs::write(&file, b"x").unwrap();
    let lf = d.path().join("lf");
    let ld = d.path().join("ld");
    symlink(&file, &lf).unwrap();
    symlink(&dir, &ld).unwrap();
    assert!(!source_is_single(&dir, false).unwrap());
    assert!(source_is_single(&file, false).unwrap());
    assert!(matches!(
        source_is_single(&file, true),
        Err(PlaceError::NotADirectory(_))
    ));
    assert!(source_is_single(&lf, false).unwrap());
    assert!(!source_is_single(&ld, false).unwrap());
    assert!(!source_is_single(&d.path().join("missing"), false).unwrap());
}

#[test]
fn split_last_bare_name() {
    assert_eq!(
        split_last(Path::new("f.iso")),
        (PathBuf::from("."), b"f.iso".to_vec())
    );
}

#[test]
fn one_component_names() {
    use irsync::session::single::is_one_component as ok;
    assert!(ok(b"a"));
    assert!(!ok(b""));
    assert!(!ok(b"."));
    assert!(!ok(b".."));
    assert!(!ok(b"a/b"));
    assert!(!ok(b"a\0b"));
}
