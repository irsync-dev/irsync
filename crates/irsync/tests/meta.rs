use irsync::fs::meta::{apply_metadata, lstat_entry, ApplyOpts};
use irsync::fs::paths::{is_contained, RelPath};
use irsync_proto::v1::file_entry::Kind;
use nix::sys::stat::{utimensat, UtimensatFlags};
use nix::sys::time::TimeSpec;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::Path;

fn set_mtime(path: &Path, sec: i64, nsec: i64, follow: bool) {
    let t = TimeSpec::new(sec, nsec);
    let flag = if follow {
        UtimensatFlags::FollowSymlink
    } else {
        UtimensatFlags::NoFollowSymlink
    };
    utimensat(nix::fcntl::AT_FDCWD, path, &t, &t, flag).unwrap();
}

const ALL: ApplyOpts = ApplyOpts {
    perms: true,
    times: true,
    owner: false,
    group: false,
};

#[test]
fn lstat_regular_file_fields() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("f");
    fs::write(&p, b"hello").unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o640)).unwrap();
    set_mtime(&p, 1_700_000_000, 123_456_789, true);
    let e = lstat_entry(d.path(), &RelPath(b"f".to_vec())).unwrap();
    assert_eq!(e.path, b"f");
    assert_eq!(e.kind(), Kind::Regular);
    assert_eq!(e.size, 5);
    assert_eq!(e.mode & 0o7777, 0o640);
    assert_eq!(e.mtime_sec, 1_700_000_000);
    assert_eq!(e.mtime_nsec, 123_456_789);
    assert!(e.whole_hash.is_none());
}

#[test]
fn lstat_symlink_has_target_and_kind() {
    let d = tempfile::tempdir().unwrap();
    symlink("some/target", d.path().join("l")).unwrap();
    let e = lstat_entry(d.path(), &RelPath(b"l".to_vec())).unwrap();
    assert_eq!(e.kind(), Kind::Symlink);
    assert_eq!(e.link_target, b"some/target");
}

#[test]
fn apply_metadata_sets_mode_and_mtime() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("src");
    let dst = d.path().join("dst");
    fs::write(&src, b"x").unwrap();
    fs::write(&dst, b"x").unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(0o751)).unwrap();
    set_mtime(&src, 1_600_000_000, 5, true);
    let e = lstat_entry(d.path(), &RelPath(b"src".to_vec())).unwrap();
    apply_metadata(&dst, &e, &ALL).unwrap();
    let m = fs::metadata(&dst).unwrap();
    assert_eq!(m.mode() & 0o7777, 0o751);
    assert_eq!(m.mtime(), 1_600_000_000);
    assert_eq!(m.mtime_nsec(), 5);
}

#[test]
fn apply_metadata_on_symlink_does_not_follow() {
    let d = tempfile::tempdir().unwrap();
    let target = d.path().join("target");
    fs::write(&target, b"x").unwrap();
    set_mtime(&target, 1_500_000_000, 0, true);
    let link = d.path().join("link");
    symlink("target", &link).unwrap();
    set_mtime(&link, 1_400_000_000, 7, false);
    let e = lstat_entry(d.path(), &RelPath(b"link".to_vec())).unwrap();

    set_mtime(&link, 1_000_000_000, 0, false);
    apply_metadata(&link, &e, &ALL).unwrap();

    assert_eq!(fs::metadata(&target).unwrap().mtime(), 1_500_000_000);
    let lm = fs::symlink_metadata(&link).unwrap();
    assert_eq!(lm.mtime(), 1_400_000_000);
    assert_eq!(lm.mtime_nsec(), 7);
}

#[test]
fn non_utf8_path_roundtrips() {
    let d = tempfile::tempdir().unwrap();
    let name = b"caf\xe9";
    fs::write(d.path().join(OsStr::from_bytes(name)), b"x").unwrap();
    let rel = RelPath(name.to_vec());
    let e = lstat_entry(d.path(), &rel).unwrap();
    assert_eq!(e.path, name);
    let reopened = d.path().join(rel.to_os());
    assert_eq!(fs::read(reopened).unwrap(), b"x");
}

#[test]
fn relpath_order_is_componentwise() {
    assert!(RelPath(b"a/b".to_vec()) < RelPath(b"a-b".to_vec()));
    assert!(RelPath(b"a".to_vec()) < RelPath(b"a/b".to_vec()));
    assert!(RelPath(Vec::new()) < RelPath(b"a".to_vec()));
    let r = RelPath(b"a/b".to_vec()).join(b"c");
    assert_eq!(r.0, b"a/b/c");
    assert_eq!(RelPath(Vec::new()).join(b"c").0, b"c");
    assert_eq!(
        r.components().collect::<Vec<_>>(),
        vec![&b"a"[..], b"b", b"c"]
    );
}

#[test]
fn is_contained_rejects_dotdot() {
    let root = Path::new("/tmp/root");
    assert!(is_contained(root, &RelPath(b"a/b".to_vec())));
    assert!(is_contained(root, &RelPath(Vec::new())));
    assert!(!is_contained(root, &RelPath(b"../x".to_vec())));
    assert!(!is_contained(root, &RelPath(b"a/../../x".to_vec())));
    assert!(!is_contained(root, &RelPath(b"/etc/passwd".to_vec())));
}
