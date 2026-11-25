//! Local operands and path options through the `irsync` binary, which
//! reads them as this system means them before the session starts: `.`
//! and `..` as a source still send only the contents, a relative
//! `--partial-dir` stays relative to the destination, and on Windows a
//! destination typed with a trailing dot is the directory Windows names.

mod common;

use common::{irsync_bin as irsync, irsync_bin_in, spawn_daemon_binary};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn ok(home: &Path, args: &[&str]) -> String {
    let o = irsync(home, args);
    assert!(o.status.success(), "{args:?}: {}", stderr(&o));
    String::from_utf8(o.stdout).unwrap()
}

/// A server with the rw export `photos` (holding `b.jpg`) granted to the
/// client, and a work directory for the client's files.
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
    for d in [&server, &client, &photos, &work] {
        fs::create_dir_all(d).unwrap();
    }
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

    /// `irsync --address ADDR -a ARGS...` run in `cwd`, which must succeed.
    fn run_ok(&self, cwd: &Path, args: &[&str]) {
        let mut all = vec!["--address", &self.address, "-a"];
        all.extend_from_slice(args);
        let o = irsync_bin_in(&self.client, Some(cwd), &all);
        assert!(o.status.success(), "{args:?}: {}", stderr(&o));
    }
}

/// The names in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// `irsync . host:photos/d`, `irsync .. host:…` and `irsync inner/..
/// host:…` send the contents of the directory they name, as rsync does:
/// the files land in `d` itself, not in `d/<that directory's name>`.
#[test]
fn dot_and_dot_dot_as_the_source_send_the_contents() {
    let s = setup();
    let src = s.work.join("src");
    fs::create_dir_all(src.join("inner")).unwrap();
    fs::write(src.join("f"), b"f").unwrap();
    fs::write(src.join("inner/g"), b"g").unwrap();
    for (cwd, typed, dst) in [
        (src.clone(), ".", "d1"),
        (src.join("inner"), "..", "d2"),
        (src.clone(), "inner/..", "d3"),
    ] {
        s.run_ok(&cwd, &[typed, &s.remote(&format!("photos/{dst}"))]);
        let got = s.photos.join(dst);
        assert_eq!(names(&got), ["f", "inner"], "{typed}");
        assert_eq!(fs::read(got.join("inner/g")).unwrap(), b"g", "{typed}");
    }
}

/// `--partial-dir .p` is relative to the destination, not to the current
/// directory: the binary leaves a relative one as it is.
#[test]
fn a_relative_partial_dir_stays_relative_to_the_destination() {
    let s = setup();
    s.run_ok(
        &s.work,
        &["--partial-dir", ".p", &s.remote("photos/"), "dst"],
    );
    assert_eq!(fs::read(s.work.join("dst/b.jpg")).unwrap(), b"b");
    assert!(
        s.work.join("dst/.p").is_dir(),
        "the partial dir was not made under the destination"
    );
    assert!(
        !s.work.join(".p").exists(),
        "the partial dir left the destination"
    );
}

/// `irsync host:photos/ dst.` from the binary: one directory `dst`, no
/// `dst.`.
#[cfg(windows)]
#[test]
fn a_destination_typed_with_a_trailing_dot_is_the_directory_windows_names() {
    let s = setup();
    s.run_ok(&s.work, &[&s.remote("photos/"), "dst."]);
    let verbatim = PathBuf::from(format!(r"\\?\{}", s.work.display()));
    assert_eq!(names(&verbatim), ["dst"]);
    assert_eq!(fs::read(s.work.join("dst/b.jpg")).unwrap(), b"b");
}
