//! `irsync HOST:` and `irsync HOST:PATH` against a running daemon.

mod common;

use common::{irsync_bin as irsync, spawn_daemon_binary};
use std::path::Path;

fn stderr(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn ok(home: &Path, args: &[&str]) -> String {
    let o = irsync(home, args);
    assert!(o.status.success(), "{args:?}: {}", stderr(&o));
    String::from_utf8(o.stdout).unwrap()
}

/// A server home with exports photos (rw), docs (ro) and secret, the first
/// two granted to the client; returns (server, client, photos, docs, id, address).
struct Setup {
    _dir: tempfile::TempDir,
    _daemon: common::DaemonGuard,
    server: std::path::PathBuf,
    client: std::path::PathBuf,
    photos: std::path::PathBuf,
    docs: std::path::PathBuf,
    daemon_id: String,
    address: String,
}

fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path();
    let server = tmp.join("server/home");
    let client = tmp.join("client/home");
    let photos = tmp.join("srv/photos");
    let docs = tmp.join("srv/docs");
    let secret = tmp.join("srv/secret");
    for d in [&server, &client, &photos.join("a"), &docs, &secret] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(photos.join("a/x.jpg"), b"x").unwrap();
    std::fs::write(photos.join("b.jpg"), b"b").unwrap();
    let (daemon, daemon_id, listen, _log) = spawn_daemon_binary(&server);
    let client_id = ok(&client, &["id", "show"])
        .lines()
        .nth(1)
        .unwrap()
        .to_string();
    ok(
        &server,
        &["export", "add", &format!("photos:{}", photos.display())],
    );
    ok(
        &server,
        &["export", "add", &format!("docs:{}", docs.display()), "--ro"],
    );
    ok(
        &server,
        &["export", "add", &format!("secret:{}", secret.display())],
    );
    ok(&server, &["grant", "add", "photos", &client_id]);
    ok(&server, &["grant", "add", "docs", &client_id]);
    Setup {
        _dir: dir,
        _daemon: daemon,
        server,
        client,
        photos,
        docs,
        daemon_id: daemon_id.to_string(),
        address: format!("127.0.0.1:{}", listen.port()),
    }
}

#[test]
fn host_colon_lists_reachable_exports_with_real_paths() {
    let s = setup();
    let operand = format!("{}:", s.daemon_id);
    let out = ok(&s.client, &["--address", &s.address, &operand]);
    assert_eq!(
        out,
        format!(
            "docs {} ro\nphotos {} rw\n",
            s.docs.display(),
            s.photos.display()
        )
    );
    // A client with no grant learns nothing.
    let other = s.server.parent().unwrap().join("other/home");
    std::fs::create_dir_all(&other).unwrap();
    let o = irsync(&other, &["--address", &s.address, &operand]);
    assert!(!o.status.success());
    assert!(
        stderr(&o).contains("rejected this client's key"),
        "{}",
        stderr(&o)
    );
    assert!(String::from_utf8_lossy(&o.stdout).is_empty());
}

#[test]
fn host_path_lists_the_directory_without_writing() {
    let s = setup();
    for operand in [
        format!("{}:photos/", s.daemon_id),
        format!("{}:{}", s.daemon_id, s.photos.display()),
    ] {
        let out = ok(&s.client, &["--address", &s.address, &operand]);
        assert!(
            out.contains("a/x.jpg") && out.contains("b.jpg"),
            "{operand}: {out:?}"
        );
    }
    // The export is untouched and nothing was pulled anywhere.
    assert_eq!(std::fs::read(s.photos.join("b.jpg")).unwrap(), b"b");
    assert!(!s.client.join("photos").exists());
}

#[test]
fn directory_listing_leaves_no_state() {
    let s = setup();
    let operand = format!("{}:photos", s.daemon_id);
    let out = ok(&s.client, &["--address", &s.address, &operand]);
    assert!(out.contains("b.jpg"), "{out:?}");
    assert!(
        !s.client.join("xdg/irsync").exists(),
        "a listing must not create the client's state directory"
    );
}

#[test]
fn listing_an_unreachable_path_prints_the_hint() {
    let s = setup();
    let client_id = ok(&s.client, &["id", "show"])
        .lines()
        .nth(1)
        .unwrap()
        .to_string();
    let operand = format!("{}:/nowhere", s.daemon_id);
    let o = irsync(&s.client, &["--address", &s.address, &operand]);
    assert!(!o.status.success());
    assert!(
        stderr(&o).contains(&format!("irsync grant list {client_id}"))
            && stderr(&o).contains("no access to"),
        "{}",
        stderr(&o)
    );
}
