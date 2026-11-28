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
    _daemon: common::BinaryDaemon,
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
    let server = tmp.join("server").join("home");
    let client = tmp.join("client").join("home");
    let photos = tmp.join("srv").join("photos");
    let docs = tmp.join("srv").join("docs");
    let secret = tmp.join("srv").join("secret");
    for d in [&server, &client, &photos.join("a"), &docs, &secret] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(photos.join("a").join("x.jpg"), b"x").unwrap();
    std::fs::write(photos.join("b.jpg"), b"b").unwrap();
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
    ok(
        &server,
        &[
            "export",
            "add",
            &format!("docs:{}", docs.display()),
            "--read-only",
        ],
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
    let other = s.server.parent().unwrap().join("other").join("home");
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
fn host_colon_says_when_nothing_is_shared() {
    let dir = tempfile::tempdir().unwrap();
    let server = dir.path().join("server/home");
    let client = dir.path().join("client/home");
    for d in [&server, &client] {
        std::fs::create_dir_all(d).unwrap();
    }
    let daemon = spawn_daemon_binary(&server);
    let client_id = ok(&client, &["id", "show"])
        .lines()
        .last()
        .unwrap()
        .to_string();
    // Known to the server and let in, but with no export to see.
    ok(&server, &["client", "add", "laptop", &client_id]);
    ok(&server, &["client", "set", "laptop", "all_exports=rw"]);
    let address = format!("127.0.0.1:{}", daemon.listen.port());
    let operand = format!("{}:", daemon.id);
    let o = irsync(&client, &["--address", &address, &operand]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(
        o.stdout.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&o.stdout)
    );
    assert!(
        stderr(&o).contains(&format!(
            "connected to {}; it shares no exports with this client",
            daemon.id
        )),
        "{}",
        stderr(&o)
    );
    // -q leaves the line out.
    let o = irsync(&client, &["-q", "--address", &address, &operand]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(!stderr(&o).contains("shares no exports"), "{}", stderr(&o));
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
        !s.client.join("xdg").join("irsync").exists(),
        "a listing must not create the client's state directory"
    );
}

#[test]
fn listing_an_unreachable_path_prints_the_hint() {
    let s = setup();
    let client_id = ok(&s.client, &["id", "show"])
        .lines()
        .last()
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

#[test]
fn list_command_matches_host_colon() {
    let s = setup();
    let colon = format!("{}:", s.daemon_id);
    let want = ok(&s.client, &["--address", &s.address, &colon]);
    for args in [
        vec!["list", "--address", &s.address, &s.daemon_id],
        vec!["list", "--address", &s.address, &colon],
        vec!["sync", "--address", &s.address, &colon],
    ] {
        assert_eq!(ok(&s.client, &args), want, "{args:?}");
    }
}

#[test]
fn list_command_lists_files() {
    let s = setup();
    let operand = format!("{}:photos/", s.daemon_id);
    let out = ok(&s.client, &["list", "--address", &s.address, &operand]);
    assert!(out.contains("a/x.jpg") && out.contains("b.jpg"), "{out:?}");
    assert!(!s.client.join("photos").exists());
}

#[test]
fn sync_command_pushes() {
    let s = setup();
    let src = s.client.join("up");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("new.jpg"), b"n").unwrap();
    let dest = format!("{}:photos/", s.daemon_id);
    let src_arg = format!("{}/", src.display());
    ok(
        &s.client,
        &["sync", "-a", "--address", &s.address, &src_arg, &dest],
    );
    assert_eq!(std::fs::read(s.photos.join("new.jpg")).unwrap(), b"n");
}
