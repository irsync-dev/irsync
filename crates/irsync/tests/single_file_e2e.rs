//! One file as the source of a copy, through the `irsync` binary: pushes
//! and pulls placed by rsync's rules, several sources, a listing of one
//! file, and the same placement as `rsync -a` (skipped without GNU rsync).

mod common;

use common::{irsync_bin as irsync, irsync_bin_in, spawn_daemon_binary};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn ok(home: &Path, args: &[&str]) -> String {
    let o = irsync(home, args);
    assert!(o.status.success(), "{args:?}: {}", stderr(&o));
    String::from_utf8(o.stdout).unwrap()
}

/// A server home with the rw export `photos` granted to the client, and a
/// work directory for the client's files.
struct Setup {
    _dir: tempfile::TempDir,
    _daemon: common::BinaryDaemon,
    client: PathBuf,
    photos: PathBuf,
    work: PathBuf,
    daemon_id: String,
    address: String,
}

fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path();
    let server = tmp.join("server/home");
    let client = tmp.join("client/home");
    let photos = tmp.join("srv/photos");
    let work = tmp.join("work");
    for d in [&server, &client, &photos.join("a"), &work] {
        fs::create_dir_all(d).unwrap();
    }
    fs::write(photos.join("a/x.jpg"), b"x").unwrap();
    fs::write(photos.join("b.jpg"), b"b").unwrap();
    let daemon = spawn_daemon_binary(&server);
    let (daemon_id, listen) = (daemon.id, daemon.listen);
    let client_id = ok(&client, &["id", "show"])
        .lines()
        .last()
        .unwrap()
        .to_string();
    ok(
        &server,
        &["export", "add", &format!("photos:{}", photos.display())],
    );
    ok(&server, &["grant", "add", "photos", &client_id]);
    Setup {
        _dir: dir,
        _daemon: daemon,
        client,
        photos,
        work,
        daemon_id: daemon_id.to_string(),
        address: format!("127.0.0.1:{}", listen.port()),
    }
}

impl Setup {
    /// `ID:path`.
    fn remote(&self, path: &str) -> String {
        format!("{}:{path}", self.daemon_id)
    }

    /// `irsync --address ADDR -a ARGS...` run in `cwd`.
    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        let mut all = vec!["--address", &self.address, "-a"];
        all.extend_from_slice(args);
        irsync_bin_in(&self.client, Some(cwd), &all)
    }

    fn run_ok(&self, cwd: &Path, args: &[&str]) -> Output {
        let o = self.run(cwd, args);
        assert!(o.status.success(), "{args:?}: {}", stderr(&o));
        o
    }
}

#[test]
fn push_bare_name_from_cwd() {
    let s = setup();
    fs::write(s.work.join("f.iso"), b"image").unwrap();
    s.run_ok(&s.work, &["f.iso", &s.remote("photos/tmp")]);
    assert_eq!(fs::read(s.photos.join("tmp")).unwrap(), b"image");
}

#[test]
fn pull_file_into_dot() {
    let s = setup();
    s.run_ok(&s.work, &[&s.remote("photos/b.jpg"), "."]);
    assert_eq!(fs::read(s.work.join("b.jpg")).unwrap(), b"b");
    // Only the file: nothing else from the export came along.
    let names: Vec<_> = fs::read_dir(&s.work)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["b.jpg"]);
}

#[test]
fn delete_keeps_everything_else() {
    let s = setup();
    fs::write(s.work.join("other"), b"mine").unwrap();
    s.run_ok(&s.work, &["--delete", &s.remote("photos/b.jpg"), "."]);
    assert_eq!(fs::read(s.work.join("b.jpg")).unwrap(), b"b");
    assert_eq!(fs::read(s.work.join("other")).unwrap(), b"mine");
    s.run_ok(&s.work, &["--delete", "other", &s.remote("photos")]);
    assert_eq!(fs::read(s.photos.join("other")).unwrap(), b"mine");
    assert_eq!(fs::read(s.photos.join("a/x.jpg")).unwrap(), b"x");
}

#[test]
fn several_sources_mix_files_and_dirs() {
    let s = setup();
    fs::write(s.work.join("a.iso"), b"a").unwrap();
    fs::create_dir_all(s.work.join("dir/sub")).unwrap();
    fs::write(s.work.join("dir/sub/y"), b"y").unwrap();
    s.run_ok(&s.work, &["a.iso", "dir", &s.remote("photos/T")]);
    let t = s.photos.join("T");
    assert_eq!(fs::read(t.join("a.iso")).unwrap(), b"a");
    assert_eq!(fs::read(t.join("dir/sub/y")).unwrap(), b"y");
}

#[test]
fn file_source_with_slash_fails() {
    let s = setup();
    fs::write(s.work.join("f.iso"), b"image").unwrap();
    fs::write(s.work.join("g.iso"), b"other").unwrap();
    // The failed source is reported and the next one still goes: exit 23,
    // as for any failed source among several.
    let o = s.run(&s.work, &["f.iso/", "g.iso", &s.remote("photos/T")]);
    assert_eq!(o.status.code(), Some(23), "{}", stderr(&o));
    assert!(stderr(&o).contains("not a directory"), "{}", stderr(&o));
    assert!(!s.photos.join("T/f.iso").exists());
    assert_eq!(fs::read(s.photos.join("T/g.iso")).unwrap(), b"other");
    // Alone it is the only source and it failed: nothing got through.
    let o = s.run(&s.work, &["f.iso/", &s.remote("photos/U")]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("not a directory"), "{}", stderr(&o));
    assert!(!s.photos.join("U").exists());
}

#[test]
fn listing_one_file() {
    let s = setup();
    let o = irsync_bin_in(
        &s.client,
        Some(&s.work),
        &["--address", &s.address, &s.remote("photos/b.jpg")],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("b.jpg"), "{out:?}");
    assert!(!out.contains("x.jpg"), "{out:?}");
    assert_eq!(fs::read_dir(&s.work).unwrap().count(), 0);
    assert!(!s.client.join("b.jpg").exists());
}

#[test]
fn names_with_spaces_and_unicode() {
    let s = setup();
    let name = "ä b.iso";
    fs::write(s.work.join(name), b"umlaut").unwrap();
    s.run_ok(&s.work, &[name, &s.remote("photos")]);
    assert_eq!(fs::read(s.photos.join(name)).unwrap(), b"umlaut");
    let back = s.work.join("back");
    fs::create_dir(&back).unwrap();
    s.run_ok(&back, &[&s.remote(&format!("photos/{name}")), "."]);
    assert_eq!(fs::read(back.join(name)).unwrap(), b"umlaut");
}

/// True for GNU rsync >= 3.1, as in `rsync_compare.rs`.
fn rsync_present() -> bool {
    let Ok(out) = Command::new("rsync").arg("--version").output() else {
        return false;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.lines().next().unwrap_or("");
    let ver = first.split("version").nth(1).unwrap_or("").trim();
    let mut parts = ver.split(|c: char| !c.is_ascii_digit());
    let major: u32 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    out.status.success() && !first.contains("openrsync") && (major, minor) >= (3, 1)
}

/// Called when rsync is unusable: under CI the comparison must not be skipped.
fn skip_allowed(ci: bool) {
    assert!(
        !ci,
        "CI is set but GNU rsync >= 3.1 is not on PATH: the comparison must not be skipped"
    );
}

/// Kind, mode, mtime (files only) and contents, by path.
type Tree = BTreeMap<PathBuf, (&'static str, u32, Option<(i64, i64)>, Vec<u8>)>;

/// What `-a` keeps, per path under `root`. Directory mtimes are left out:
/// placing a file in a directory moves its mtime (the kernel does that, for
/// both tools, at different moments).
fn snapshot(root: &Path) -> Tree {
    fn walk(root: &Path, dir: &Path, out: &mut Tree) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let m = fs::symlink_metadata(&p).unwrap();
            let rel = p.strip_prefix(root).unwrap().to_path_buf();
            if m.is_dir() {
                out.insert(rel, ("dir", m.mode() & 0o7777, None, vec![]));
                walk(root, &p, out);
            } else {
                let mtime = Some((m.mtime(), m.mtime_nsec()));
                out.insert(
                    rel,
                    ("file", m.mode() & 0o7777, mtime, fs::read(&p).unwrap()),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// One row of the placement table: a label, what the destination
/// directory holds first, the destination (`T` or `T/`) and whether the
/// copy succeeds.
type Row = (&'static str, fn(&Path), &'static str, bool);

/// The placement table, for both directions: `T` a directory, missing, a
/// file, or a directory in the way, with and without a trailing slash.
fn placement_rows() -> [Row; 7] {
    [
        (
            "into_dir",
            |r| fs::create_dir_all(r.join("T/other")).unwrap(),
            "T",
            true,
        ),
        (
            "into_dir_slash",
            |r| fs::create_dir_all(r.join("T/other")).unwrap(),
            "T/",
            true,
        ),
        ("missing", |_| {}, "T", true),
        ("missing_slash", |_| {}, "T/", true),
        (
            "over_file",
            |r| fs::write(r.join("T"), b"old").unwrap(),
            "T",
            true,
        ),
        (
            "file_slash",
            |r| fs::write(r.join("T"), b"old").unwrap(),
            "T/",
            false,
        ),
        (
            // Not empty: rsync replaces an empty directory in the way, which
            // irsync never does.
            "dir_in_the_way",
            |r| fs::create_dir_all(r.join("T/f.iso/keep")).unwrap(),
            "T",
            false,
        ),
    ]
}

/// `f.iso` at `src` with fixed contents, mode and mtime.
fn placement_source(src: &Path) {
    fs::write(src, b"new image").unwrap();
    fs::set_permissions(src, fs::Permissions::from_mode(0o640)).unwrap();
    set_mtime(src, 1_500_000_000);
}

/// For each row: prepares `rsync_root/LABEL` and `irsync_root/LABEL` the
/// same way, copies `src` there with `rsync -a` and with `irsync` (run by
/// `irsync(label, dest)`), and compares the two trees and outcomes.
fn compare_placement(
    src: &Path,
    rsync_root: &Path,
    irsync_root: &Path,
    irsync: impl Fn(&str, &str) -> Output,
) {
    for (label, prep, dest, succeeds) in placement_rows() {
        let (a, b) = (rsync_root.join(label), irsync_root.join(label));
        for d in [&a, &b] {
            fs::create_dir_all(d).unwrap();
            prep(d);
            // An old file in the way gets the same mtime in both trees.
            if d.join("T").is_file() {
                set_mtime(&d.join("T"), 1_400_000_000);
            }
        }
        let rsync_dest = format!("{}/{dest}", a.display());
        let st = Command::new("rsync")
            .args(["-a", src.to_str().unwrap(), &rsync_dest])
            .output()
            .unwrap();
        assert_eq!(
            st.status.success(),
            succeeds,
            "{label}: rsync: {}",
            stderr(&st)
        );
        let o = irsync(label, dest);
        assert_eq!(
            o.status.success(),
            succeeds,
            "{label}: irsync: {}",
            stderr(&o)
        );
        assert_eq!(snapshot(&a), snapshot(&b), "{label}: trees differ");
    }
}

/// Each row of the placement table, pushed by `rsync -a` locally and by
/// irsync to the daemon from the same starting tree, gives the same tree
/// (or fails in both).
#[test]
fn matches_rsync_placement() {
    if !rsync_present() {
        skip_allowed(std::env::var_os("CI").is_some());
        eprintln!("skipping: rsync not on PATH (or older than 3.1)");
        return;
    }
    let s = setup();
    let src = s.work.join("f.iso");
    placement_source(&src);
    compare_placement(&src, &s.work.join("rsync"), &s.photos, |label, dest| {
        s.run(
            &s.work,
            &["f.iso", &s.remote(&format!("photos/{label}/{dest}"))],
        )
    });
}

/// The same table pulled: `rsync -a` copies the daemon's file locally, and
/// irsync pulls it from the daemon, into the same starting trees.
#[test]
fn matches_rsync_placement_on_pull() {
    if !rsync_present() {
        skip_allowed(std::env::var_os("CI").is_some());
        eprintln!("skipping: rsync not on PATH (or older than 3.1)");
        return;
    }
    let s = setup();
    fs::create_dir(s.photos.join("src")).unwrap();
    let src = s.photos.join("src/f.iso");
    placement_source(&src);
    let (rsync_root, irsync_root) = (s.work.join("rsync"), s.work.join("irsync"));
    compare_placement(&src, &rsync_root, &irsync_root, |label, dest| {
        let local = format!("{}/{label}/{dest}", irsync_root.display());
        s.run(&s.work, &[&s.remote("photos/src/f.iso"), &local])
    });
}

/// Sets `p`'s mtime to `secs` (both tools must copy it).
fn set_mtime(p: &Path, secs: u64) {
    let f = fs::File::options().write(true).open(p).unwrap();
    f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
        .unwrap();
}
