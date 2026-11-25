//! `irsync remote SERVER COMMAND` against a running daemon, with the real
//! binaries on both sides.

mod common;

use std::path::{Path, PathBuf};

use common::{irsync_bin as irsync, irsync_bin_env, spawn_daemon_binary_env, BinaryDaemon};
use irsync::authz::Access;
use irsync_proto::capture::v1::{record::Msg, Direction, Record, Side};
use irsync_proto::v1::{admin_reply, admin_request, hello};
use prost::Message as _;

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
    setup_env(admin, &[])
}

/// [`setup`], the daemon started with the environment `env`.
fn setup_env(admin: Option<Access>, env: &[(&str, &str)]) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let server = dir.path().join("server/home");
    let client = dir.path().join("client/home");
    std::fs::create_dir_all(server.join("Photos")).unwrap();
    std::fs::create_dir_all(&client).unwrap();
    let daemon = spawn_daemon_binary_env(&server, env);
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

/// A stub daemon that speaks the protocol version before this one refuses
/// the admin Hello as an older irsync does, with a version error:
/// `irsync remote` tells the user to update the server.
#[tokio::test(flavor = "multi_thread")]
async fn an_older_daemon_is_told_to_update() {
    use irsync::net::frame::{read_msg, write_msg};
    use irsync_proto::v1::{error::Code, hello_reply, Error, Hello, HelloReply};
    use irsync_proto::PROTOCOL_VERSION;

    let stub = common::loopback_endpoint().await;
    let stub_id = stub.id().to_string();
    let port = common::addr_of(&stub).ip_addrs().next().unwrap().port();
    let serve = tokio::spawn(async move {
        let conn = stub.accept().await.unwrap().await.unwrap();
        let (mut send, mut recv) = conn.accept_bi().await.unwrap();
        let hello: Hello = read_msg(&mut recv).await.unwrap().unwrap();
        let older = PROTOCOL_VERSION - 1;
        let e = Error {
            code: Code::VersionMismatch as i32,
            message: format!(
                "protocol version {} unsupported (daemon speaks {older})",
                hello.protocol_version
            ),
        };
        let refusal = HelloReply {
            reply: Some(hello_reply::Reply::Error(e)),
        };
        write_msg(&mut send, &refusal).await.unwrap();
        send.finish().unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), send.stopped()).await;
        conn.closed().await;
    });
    let dir = tempfile::tempdir().unwrap();
    let client = dir.path().join("client/home");
    std::fs::create_dir_all(&client).unwrap();
    let o = tokio::task::spawn_blocking(move || {
        ok(
            &client,
            &[
                "server",
                "add",
                "myserver",
                &stub_id,
                "--address",
                &format!("127.0.0.1:{port}"),
            ],
        );
        irsync(&client, &["remote", "myserver", "show"])
    })
    .await
    .unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(
        stderr(&o).contains("myserver runs an older irsync: update it there"),
        "{}",
        stderr(&o)
    );
    serve.abort();
}

/// Every record of a capture file.
fn read_capture(path: &Path) -> Vec<Record> {
    let file = std::fs::read(path).unwrap();
    pbcap::Reader::new(&file[..])
        .expect("a capture file")
        .map(|p| Record::decode(&p.expect("a whole block").body[..]).expect("a record"))
        .collect()
}

/// The admin messages of a capture, as (side, direction, message).
fn admin_msgs(recs: &[Record]) -> Vec<(Side, Direction, &Msg)> {
    recs.iter()
        .filter_map(|r| match r.msg.as_ref()? {
            m @ (Msg::AdminRequest(_) | Msg::AdminReply(_)) => Some((r.side(), r.direction(), m)),
            _ => None,
        })
        .collect()
}

#[test]
fn admin_traffic_is_captured_on_both_sides() {
    let dir = tempfile::tempdir().unwrap();
    let dcap = dir.path().join("daemon.pcapng");
    let (c1, c2) = (dir.path().join("c1.pcapng"), dir.path().join("c2.pcapng"));
    let s = setup_env(
        Some(Access::ReadWrite),
        &[("IRSYNC_CAPTURE", dcap.to_str().unwrap())],
    );
    // One change with its commit, then one read: two client processes.
    for (cap, args) in [
        (
            &c1,
            vec!["remote", "myserver", "export", "add", "photos:~/Photos"],
        ),
        (&c2, vec!["remote", "myserver", "export", "list"]),
    ] {
        let o = irsync_bin_env(&s.client, None, &[("IRSYNC_CAPTURE", cap)], &args);
        assert!(o.status.success(), "{args:?}: {}", stderr(&o));
    }
    s.daemon.stop();

    let change = read_capture(&c1);
    let read = read_capture(&c2);
    let daemon = read_capture(&dcap);

    let is_change = |m: &Msg| matches!(m, Msg::AdminRequest(r) if matches!(r.msg, Some(admin_request::Msg::Change(_))));
    let is_commit = |m: &Msg| matches!(m, Msg::AdminRequest(r) if matches!(r.msg, Some(admin_request::Msg::Commit(_))));
    let is_read = |m: &Msg| matches!(m, Msg::AdminRequest(r) if matches!(r.msg, Some(admin_request::Msg::Read(_))));
    let is_ack = |m: &Msg| matches!(m, Msg::AdminReply(r) if matches!(r.msg, Some(admin_reply::Msg::Ack(_))));
    let is_view = |m: &Msg| matches!(m, Msg::AdminReply(r) if matches!(r.msg, Some(admin_reply::Msg::View(_))));
    fn shape(recs: &[Record], me: Side, mine: Direction, theirs: Direction) -> Vec<&Msg> {
        // The client sends requests and receives replies; the daemon the reverse.
        let (req, rep) = if me == Side::Client {
            (mine, theirs)
        } else {
            (theirs, mine)
        };
        let msgs = admin_msgs(recs);
        assert!(msgs.iter().all(|(side, _, _)| *side == me), "{msgs:?}");
        for (_, dir, m) in &msgs {
            let want = if matches!(m, Msg::AdminRequest(_)) {
                req
            } else {
                rep
            };
            assert_eq!(*dir, want, "{m:?}");
        }
        msgs.into_iter().map(|(_, _, m)| m).collect()
    }

    // The client's change: Change, Commit sent; Ack received.
    let m = shape(&change, Side::Client, Direction::Sent, Direction::Received);
    assert_eq!(m.len(), 3, "{m:?}");
    assert!(is_change(m[0]) && is_commit(m[1]) && is_ack(m[2]), "{m:?}");
    // The client's read: Read sent; View received.
    let m = shape(&read, Side::Client, Direction::Sent, Direction::Received);
    assert_eq!(m.len(), 2, "{m:?}");
    assert!(is_read(m[0]) && is_view(m[1]), "{m:?}");
    // Both clients' Hello says ADMIN.
    for recs in [&change, &read] {
        assert!(recs.iter().any(|r| matches!(
            &r.msg,
            Some(Msg::Hello(h)) if matches!(h.request, Some(hello::Request::Admin(_)))
                && r.direction() == Direction::Sent
        )));
    }
    // The daemon saw all five, in the other directions.
    let m = shape(&daemon, Side::Daemon, Direction::Sent, Direction::Received);
    assert_eq!(m.len(), 5, "{m:?}");
    assert!(is_change(m[0]) && is_commit(m[1]) && is_ack(m[2]), "{m:?}");
    assert!(is_read(m[3]) && is_view(m[4]), "{m:?}");
    // What the change carries is in the capture.
    assert!(change.iter().any(|r| matches!(
        &r.msg,
        Some(Msg::AdminRequest(q)) if format!("{q:?}").contains("photos:~/Photos")
    )));
}
