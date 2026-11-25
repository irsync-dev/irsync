//! `irsync remote SERVER COMMAND` against a running daemon, with the real
//! binaries on both sides.

mod common;

use std::path::{Path, PathBuf};

use common::{irsync_bin as irsync, spawn_daemon_binary, BinaryDaemon};
use irsync::authz::Access;

fn stderr(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn stdout(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn ok(home: &Path, args: &[&str]) -> String {
    let o = irsync(home, args);
    assert!(o.status.success(), "{args:?}: {}", stderr(&o));
    stdout(&o)
}

struct Setup {
    _dir: tempfile::TempDir,
    daemon: BinaryDaemon,
    server: PathBuf,
    client: PathBuf,
    client_id: String,
}

/// A running daemon, and a client that has saved it as `myserver`. The
/// client holds `admin` as `admin` says.
fn setup(admin: Option<Access>) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let server = dir.path().join("server/home");
    let client = dir.path().join("client/home");
    std::fs::create_dir_all(server.join("Photos")).unwrap();
    std::fs::create_dir_all(&client).unwrap();
    let daemon = spawn_daemon_binary(&server);
    let client_id = ok(&client, &["id", "show"])
        .lines()
        .last()
        .unwrap()
        .to_string();
    ok(
        &client,
        &[
            "server",
            "add",
            "myserver",
            &daemon.id.to_string(),
            "--address",
            &format!("127.0.0.1:{}", daemon.listen.port()),
        ],
    );
    // The server's own command line writes the file, as its owner would.
    ok(&server, &["client", "add", "granted", &client_id]);
    match admin {
        Some(a) => ok(
            &server,
            &["client", "set", "granted", &format!("admin={}", a.as_str())],
        ),
        // Known to the server, with a right that is not admin.
        None => ok(&server, &["client", "set", "granted", "all_paths=rw"]),
    };
    Setup {
        _dir: dir,
        daemon,
        server,
        client,
        client_id,
    }
}

#[test]
fn remote_export_add_then_list() {
    let s = setup(Some(Access::ReadWrite));
    let o = irsync(
        &s.client,
        &["remote", "myserver", "export", "add", "photos:~/Photos"],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stderr(&o).contains("added export photos"), "{}", stderr(&o));
    let remote = ok(&s.client, &["remote", "myserver", "export", "list"]);
    let local = ok(&s.server, &["export", "list"]);
    assert!(remote.contains("photos: "), "{remote}");
    assert_eq!(remote, local);
}

#[test]
fn remote_grant_list_matches_local() {
    let s = setup(Some(Access::ReadWrite));
    let other = iroh::SecretKey::from_bytes(&[7; 32]).public().to_string();
    ok(
        &s.client,
        &[
            "remote",
            "myserver",
            "export",
            "add",
            "photos:~/Photos",
            "--ro",
        ],
    );
    ok(
        &s.client,
        &["remote", "myserver", "client", "add", "bob", &other],
    );
    ok(
        &s.client,
        &[
            "remote", "myserver", "grant", "add", "photos", "bob", "--ro",
        ],
    );
    let remote = ok(&s.client, &["remote", "myserver", "grant", "list"]);
    assert!(remote.contains("bob"), "{remote}");
    assert_eq!(remote, ok(&s.server, &["grant", "list"]));
    let remote = ok(&s.client, &["remote", "myserver", "grant", "list", "bob"]);
    assert_eq!(remote, ok(&s.server, &["grant", "list", "bob"]));
    assert_eq!(
        ok(&s.client, &["remote", "myserver", "client", "list"]),
        ok(&s.server, &["client", "list"])
    );
}

#[test]
fn remote_client_set_admin_on_self_is_refused() {
    let s = setup(Some(Access::ReadWrite));
    let o = irsync(
        &s.client,
        &["remote", "myserver", "client", "set", "granted", "admin=ro"],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("own admin right"), "{}", stderr(&o));
    // The file is as it was.
    assert!(ok(&s.server, &["client", "list"]).contains("admin=rw"));
}

#[test]
fn remote_without_admin_is_refused() {
    let s = setup(None);
    let o = irsync(&s.client, &["remote", "myserver", "export", "list"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains(
            "irsync remote: myserver does not let this client manage it \
             (it needs admin = \"rw\" or \"ro\" in its [clients] entry)"
        ),
        "{}",
        stderr(&o)
    );
    let o = irsync(
        &s.client,
        &["remote", "myserver", "export", "remove", "photos"],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains("does not let this client manage it"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn read_only_admin_reads_but_does_not_change() {
    let s = setup(Some(Access::ReadOnly));
    ok(&s.client, &["remote", "myserver", "export", "list"]);
    let o = irsync(
        &s.client,
        &["remote", "myserver", "export", "add", "p:~/Photos"],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("not allowed"), "{}", stderr(&o));
}

#[test]
fn an_expanded_home_path_is_refused_before_dialling() {
    let s = setup(Some(Access::ReadWrite));
    let target = format!("photos:{}/Photos", s.client.display());
    let o = irsync(&s.client, &["remote", "myserver", "export", "add", &target]);
    assert_eq!(o.status.code(), Some(1));
    let err = stderr(&o);
    assert!(err.contains("the shell expanded ~"), "{err}");
    assert!(
        err.contains("irsync remote myserver export add 'photos:~/Photos'"),
        "{err}"
    );
    // Nothing reached the server: its config has no export.
    assert_eq!(ok(&s.server, &["export", "list"]), "");
    let log = std::fs::read_to_string(&s.daemon.log).unwrap();
    assert!(!log.contains("admin"), "{log}");
}

#[test]
fn config_set_is_not_a_remote_command() {
    let s = setup(Some(Access::ReadWrite));
    let o = irsync(
        &s.client,
        &["remote", "myserver", "config", "set", "x", "y"],
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(
        stderr(&o).contains("unrecognized subcommand 'set'"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn remote_config_get_show_and_the_help() {
    let s = setup(Some(Access::ReadOnly));
    let o = irsync(
        &s.client,
        &["remote", "myserver", "config", "get", "daemon.idle_timeout"],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(
        stdout(&o),
        ok(&s.server, &["config", "get", "daemon.idle_timeout"])
    );
    // A key that is unset and has no default: no output, exit 1.
    let local = irsync(&s.server, &["config", "get", "daemon.allow_all_paths"]);
    let o = irsync(
        &s.client,
        &[
            "remote",
            "myserver",
            "config",
            "get",
            "daemon.allow_all_paths",
        ],
    );
    assert_eq!(o.status.code(), local.status.code());
    assert_eq!(stdout(&o), stdout(&local));
    assert_eq!(
        ok(&s.client, &["remote", "myserver", "config", "list"]),
        ok(&s.server, &["config", "list"])
    );
    let shown = ok(&s.client, &["remote", "myserver", "show"]);
    assert!(
        shown.contains(&format!("daemon identity: {}", s.daemon.id)),
        "{shown}"
    );
    assert!(shown.contains("granted"), "{shown}");
    let help = ok(&s.client, &["remote", "--help"]);
    for word in ["export", "grant", "client", "show", "config"] {
        assert!(help.contains(word), "{help}");
    }
    let _ = &s.client_id;
}
