mod common;

use irsync::fs::meta::{apply_metadata, create_special, lstat_entry, ApplyOpts};
use irsync::fs::paths::{is_contained, RelPath};
use irsync_proto::v1::file_entry::Kind;
use nix::sys::stat::{utimensat, UtimensatFlags};
use nix::sys::time::TimeSpec;
use std::fs;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::Path;

fn rp(b: &[u8]) -> RelPath {
    RelPath::new(b).unwrap()
}

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
    let e = lstat_entry(d.path(), &rp(b"f")).unwrap();
    assert_eq!(e.path, b"f");
    assert_eq!(e.kind(), Kind::Regular);
    assert_eq!(e.size, 5);
    assert_eq!(e.mode & 0o7777, 0o640);
    assert_eq!(e.mtime_sec, 1_700_000_000);
    assert_eq!(e.mtime_nsec, 123_456_789);
    assert!(e.whole_hash.is_none());
    let md = fs::symlink_metadata(&p).unwrap();
    assert_eq!(e.uid, nix::unistd::getuid().as_raw());
    assert_eq!(e.gid, nix::unistd::getgid().as_raw());
    assert_eq!(e.nlink, 1);
    assert_ne!(e.inode, 0);
    assert_ne!(e.dev, 0);
    assert_eq!(e.inode, md.ino());
    assert_eq!(e.dev, md.dev());
    assert!(!e.sparse);
}

#[test]
fn lstat_directory_kind() {
    let d = tempfile::tempdir().unwrap();
    fs::create_dir(d.path().join("sub")).unwrap();
    let e = lstat_entry(d.path(), &rp(b"sub")).unwrap();
    assert_eq!(e.kind(), Kind::Directory);
    assert!(!e.sparse);
    let root = lstat_entry(d.path(), &RelPath::root()).unwrap();
    assert_eq!(root.kind(), Kind::Directory);
    assert!(root.path.is_empty());
}

#[test]
fn sparse_only_for_regular_files_with_holes() {
    use std::io::Write;
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("holey");
    let mut f = fs::File::create(&p).unwrap();
    f.set_len(1 << 20).unwrap();
    f.write_all(&[1u8; 4096]).unwrap();
    drop(f);
    assert!(lstat_entry(d.path(), &rp(b"holey")).unwrap().sparse);
    symlink("holey", d.path().join("l")).unwrap();
    let l = lstat_entry(d.path(), &rp(b"l")).unwrap();
    assert_eq!(l.kind(), Kind::Symlink);
    assert!(!l.sparse);
}

#[test]
fn create_special_fifo() {
    let d = tempfile::tempdir().unwrap();
    let mut e = lstat_entry(d.path(), &RelPath::root()).unwrap();
    e.kind = Kind::Fifo as i32;
    e.mode = 0o10644;
    e.rdev = 0;
    create_special(&d.path().join("pipe"), &e).unwrap();
    let got = lstat_entry(d.path(), &rp(b"pipe")).unwrap();
    assert_eq!(got.kind(), Kind::Fifo);
}

#[test]
fn relpath_new_rejects_invalid() {
    let bad: [&[u8]; 8] = [
        b"/a", b"a//b", b"a/", b".", b"..", b"a/./b", b"a/../b", b"a\0b",
    ];
    for b in bad {
        assert!(RelPath::new(b).is_err(), "{b:?}");
    }
    assert!(rp(b"a").join(b"b/c").is_err());
    assert!(rp(b"a").join(b"..").is_err());
    assert!(rp(b"a").join(b"").is_err());
}

#[test]
fn relpath_new_accepts_root_and_nested() {
    assert_eq!(RelPath::new(b"").unwrap(), RelPath::root());
    assert_eq!(rp(b"a/b/c").components().count(), 3);
    assert!(RelPath::new(b"caf\xe9/.hidden/..x").is_ok());
}

#[test]
fn lstat_symlink_has_target_and_kind() {
    let d = tempfile::tempdir().unwrap();
    symlink("some/target", d.path().join("l")).unwrap();
    let e = lstat_entry(d.path(), &rp(b"l")).unwrap();
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
    let e = lstat_entry(d.path(), &rp(b"src")).unwrap();
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
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    set_mtime(&target, 1_500_000_000, 0, true);
    let link = d.path().join("link");
    symlink("target", &link).unwrap();
    set_mtime(&link, 1_400_000_000, 7, false);
    let e = lstat_entry(d.path(), &rp(b"link")).unwrap();

    set_mtime(&link, 1_000_000_000, 0, false);
    apply_metadata(&link, &e, &ALL).unwrap();

    assert_eq!(fs::metadata(&target).unwrap().mtime(), 1_500_000_000);
    assert_eq!(fs::metadata(&target).unwrap().mode() & 0o7777, 0o644);
    let lm = fs::symlink_metadata(&link).unwrap();
    assert_eq!(lm.mtime(), 1_400_000_000);
    assert_eq!(lm.mtime_nsec(), 7);
}

#[test]
fn non_utf8_path_roundtrips() {
    let d = tempfile::tempdir().unwrap();
    let name = b"caf\xe9";
    if common::try_create_non_utf8(d.path(), name).is_none() {
        return;
    }
    let rel = rp(name);
    let e = lstat_entry(d.path(), &rel).unwrap();
    assert_eq!(e.path, name);
    let reopened = d.path().join(rel.to_os());
    assert_eq!(fs::read(reopened).unwrap(), b"x");
}

#[test]
fn relpath_order_is_componentwise() {
    assert!(rp(b"a/b") < rp(b"a-b"));
    assert!(rp(b"a") < rp(b"a/b"));
    assert!(RelPath::root() < rp(b"a"));
    let r = rp(b"a/b").join(b"c").unwrap();
    assert_eq!(r.as_bytes(), b"a/b/c");
    assert_eq!(RelPath::root().join(b"c").unwrap().as_bytes(), b"c");
    assert_eq!(
        r.components().collect::<Vec<_>>(),
        vec![&b"a"[..], b"b", b"c"]
    );
}

#[test]
fn is_contained_rejects_dotdot() {
    let root = Path::new("/tmp/root");
    assert!(is_contained(root, b"a/b"));
    assert!(is_contained(root, b""));
    assert!(!is_contained(root, b"../x"));
    assert!(!is_contained(root, b"a/../../x"));
    assert!(!is_contained(root, b"/etc/passwd"));
}
