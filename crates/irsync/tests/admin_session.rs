//! The daemon's admin sessions, over a real connection: a client with
//! `admin` changes or reads the server's config file.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{client_endpoint, spawn_daemon_full, within, DaemonHandle};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::Endpoint;
use irsync::authz::{FileAuthorizer, LOCK_WAIT};
use irsync::config::paths::Paths;
use irsync::daemon::ADMIN_MAX_CHANGES;
use irsync::net::frame::{read_msg, write_msg};
use irsync_proto::v1::{
    admin_reply, admin_request, change::Op, error::Code, hello::Direction, read, AdminReply,
    AdminRequest, Change, Commit, ExportAdd, ExportRemove, GrantAdd, Hello, Nack, Read, ReadConfig,
    ReadView,
};
use irsync_proto::{ALPN, PROTOCOL_VERSION};

/// A daemon whose config is `dir/irsync.toml`, written as `config` after a
/// `state_dir` in `dir` (so a save looks for a daemon there).
async fn spawn_from_file(dir: &Path, config: &str) -> (DaemonHandle, PathBuf) {
    let path = dir.join("irsync.toml");
    let state = dir.join("state").display().to_string();
    irsync::fs::private::write(&path, format!("state_dir = {state:?}\n{config}")).unwrap();
    let authz = Arc::new(FileAuthorizer::new(Paths::resolve(Some(&path))));
    let d = spawn_daemon_full(authz, tempfile::tempdir().unwrap(), Some(dir.to_path_buf())).await;
    (d, path)
}

/// The table of a client `laptop` with `id` and the lines `rights`.
fn laptop(id: iroh::EndpointId, rights: &str) -> String {
    format!("[clients.laptop]\nclient_id = \"{id}\"\n{rights}")
}

/// An admin session: the control stream of one connection.
struct Session {
    _conn: Connection,
    send: SendStream,
    recv: RecvStream,
}

impl Session {
    async fn open(d: &DaemonHandle, client: &Endpoint) -> Session {
        let conn = within(client.connect(d.addr.clone(), ALPN)).await.unwrap();
        let (mut send, recv) = within(conn.open_bi()).await.unwrap();
        let hello = Hello {
            protocol_version: PROTOCOL_VERSION,
            direction: Direction::Admin as i32,
            ..Default::default()
        };
        within(write_msg(&mut send, &hello)).await.unwrap();
        Session {
            _conn: conn,
            send,
            recv,
        }
    }

    async fn send(&mut self, msg: admin_request::Msg) {
        let req = AdminRequest { msg: Some(msg) };
        within(write_msg(&mut self.send, &req)).await.unwrap();
    }

    async fn change(&mut self, op: Op) {
        self.send(admin_request::Msg::Change(Change { op: Some(op) }))
            .await;
    }

    /// The next reply; `None` when the daemon ended the stream.
    async fn reply(&mut self) -> Option<admin_reply::Msg> {
        let r: Option<AdminReply> = within(read_msg(&mut self.recv)).await.unwrap();
        r.map(|r| r.msg.expect("a reply with a message"))
    }

    async fn commit(&mut self) -> admin_reply::Msg {
        self.send(admin_request::Msg::Commit(Commit {})).await;
        self.reply().await.expect("a reply to COMMIT")
    }

    async fn read_view(&mut self) -> admin_reply::Msg {
        self.send(admin_request::Msg::Read(Read {
            what: Some(read::What::View(ReadView {})),
        }))
        .await;
        self.reply().await.expect("a reply to READ")
    }

    /// The daemon ends the stream: no more replies.
    async fn assert_closed(&mut self) {
        let r = within(read_msg::<AdminReply, _>(&mut self.recv)).await;
        assert!(
            matches!(r, Ok(None) | Err(_)),
            "the stream is still open: {r:?}"
        );
    }
}

fn export_add(name: &str, path: &Path) -> Op {
    Op::ExportAdd(ExportAdd {
        target: format!("{name}:{}", path.display()),
        read_only: false,
        warm_index: false,
    })
}

fn export_remove(name: &str) -> Op {
    Op::ExportRemove(ExportRemove {
        name: name.to_string(),
    })
}

fn grant_add(export: &str, who: &str) -> Op {
    Op::GrantAdd(GrantAdd {
        export: export.to_string(),
        who: who.to_string(),
        ro: false,
    })
}

fn nack(m: admin_reply::Msg) -> Nack {
    match m {
        admin_reply::Msg::Nack(n) => n,
        other => panic!("expected a NACK, got {other:?}"),
    }
}

fn ack(m: admin_reply::Msg) -> Vec<String> {
    match m {
        admin_reply::Msg::Ack(a) => a.results,
        other => panic!("expected an ACK, got {other:?}"),
    }
}

fn assert_not_allowed(m: Option<admin_reply::Msg>) {
    match m {
        Some(admin_reply::Msg::Refused(e)) => {
            assert_eq!(e.code, Code::Unauthorized as i32, "{}", e.message);
            assert_eq!(e.message, "not allowed");
        }
        other => panic!("expected the plain refusal, got {other:?}"),
    }
}

/// A temp dir by its real path (macOS: /var is a link to /private/var),
/// with a directory `photos` in it to export.
fn temp_dir() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::create_dir(dir.join("photos")).unwrap();
    (tmp, dir)
}

async fn stop(d: DaemonHandle) {
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

#[tokio::test]
async fn an_admin_adds_an_export_and_a_grant_in_one_commit() {
    let (_tmp, dir) = temp_dir();
    let client = client_endpoint().await;
    let (d, path) = spawn_from_file(&dir, &laptop(client.id(), "admin = \"rw\"\n")).await;
    let mut s = Session::open(&d, &client).await;
    s.change(export_add("photos", &dir.join("photos"))).await;
    s.change(grant_add("photos", "laptop")).await;
    let lines = ack(s.commit().await);
    assert_eq!(lines.len(), 2, "{lines:?}");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("[exports.photos]"), "{text}");
    assert!(text.contains("rw = [\"laptop\"]"), "{text}");
    stop(d).await;
}

#[tokio::test]
async fn a_nack_leaves_the_file_unchanged() {
    let (_tmp, dir) = temp_dir();
    let client = client_endpoint().await;
    let (d, path) = spawn_from_file(&dir, &laptop(client.id(), "admin = \"rw\"\n")).await;
    let before = std::fs::read(&path).unwrap();
    let mut s = Session::open(&d, &client).await;
    s.change(export_add("photos", &dir.join("photos"))).await;
    s.change(export_remove("nobody")).await;
    let n = nack(s.commit().await);
    assert_eq!(n.change, 2, "{}", n.reason);
    assert!(n.reason.contains("nobody"), "{}", n.reason);
    assert_eq!(std::fs::read(&path).unwrap(), before, "the file changed");
    stop(d).await;
}

#[tokio::test]
async fn the_session_goes_on_after_ack_and_nack() {
    let (_tmp, dir) = temp_dir();
    let client = client_endpoint().await;
    let (d, path) = spawn_from_file(&dir, &laptop(client.id(), "admin = \"rw\"\n")).await;
    let mut s = Session::open(&d, &client).await;
    s.change(export_remove("nobody")).await;
    assert_eq!(nack(s.commit().await).change, 1);
    s.change(export_add("photos", &dir.join("photos"))).await;
    assert_eq!(ack(s.commit().await).len(), 1);
    s.change(export_remove("photos")).await;
    assert_eq!(ack(s.commit().await).len(), 1);
    s.change(export_add("photos", &dir.join("photos"))).await;
    assert_eq!(ack(s.commit().await).len(), 1);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("[exports.photos]"), "{text}");
    stop(d).await;
}

#[tokio::test]
async fn closing_without_commit_changes_nothing() {
    let (_tmp, dir) = temp_dir();
    let client = client_endpoint().await;
    let (d, path) = spawn_from_file(&dir, &laptop(client.id(), "admin = \"rw\"\n")).await;
    let before = std::fs::read(&path).unwrap();
    let mut s = Session::open(&d, &client).await;
    s.change(export_add("photos", &dir.join("photos"))).await;
    s.send.finish().unwrap();
    s.assert_closed().await;
    assert_eq!(std::fs::read(&path).unwrap(), before, "the file changed");
    stop(d).await;
}

#[tokio::test]
async fn a_client_without_admin_is_refused() {
    let (_tmp, dir) = temp_dir();
    let client = client_endpoint().await;
    let config = format!(
        "{}\n[exports.photos]\npath = {:?}\nrw = [\"laptop\"]\n",
        laptop(client.id(), ""),
        dir.join("photos")
    );
    let (d, _path) = spawn_from_file(&dir, &config).await;
    let mut s = Session::open(&d, &client).await;
    assert_not_allowed(s.reply().await);
    s.assert_closed().await;
    stop(d).await;
}

#[tokio::test]
async fn a_wrong_protocol_version_is_a_version_mismatch() {
    let (_tmp, dir) = temp_dir();
    let client = client_endpoint().await;
    let (d, _path) = spawn_from_file(&dir, &laptop(client.id(), "admin = \"rw\"\n")).await;
    let conn = within(client.connect(d.addr.clone(), ALPN)).await.unwrap();
    let (mut send, mut recv) = within(conn.open_bi()).await.unwrap();
    let hello = Hello {
        protocol_version: PROTOCOL_VERSION - 1,
        direction: Direction::Admin as i32,
        ..Default::default()
    };
    within(write_msg(&mut send, &hello)).await.unwrap();
    let e: irsync_proto::v1::Error = within(read_msg(&mut recv)).await.unwrap().unwrap();
    assert_eq!(e.code, Code::VersionMismatch as i32, "{}", e.message);
    stop(d).await;
}

#[tokio::test]
async fn admin_ro_reads_but_cannot_commit() {
    let (_tmp, dir) = temp_dir();
    let client = client_endpoint().await;
    let (d, path) = spawn_from_file(&dir, &laptop(client.id(), "admin = \"ro\"\n")).await;
    let before = std::fs::read(&path).unwrap();
    let mut s = Session::open(&d, &client).await;
    match s.read_view().await {
        admin_reply::Msg::View(v) => {
            assert_eq!(v.daemon_id, d.id.as_bytes().to_vec());
            assert_eq!(v.clients.len(), 1, "{v:?}");
            assert_eq!(v.clients[0].name, "laptop");
        }
        other => panic!("expected a View, got {other:?}"),
    }
    s.send(admin_request::Msg::Read(Read {
        what: Some(read::What::Config(ReadConfig {
            key: "state_dir".to_string(),
        })),
    }))
    .await;
    match s.reply().await {
        Some(admin_reply::Msg::Config(c)) => {
            assert_eq!(c.lines.len(), 1, "{c:?}");
            assert!(c.lines[0].contains("state"), "{c:?}");
        }
        other => panic!("expected config lines, got {other:?}"),
    }
    s.change(export_add("photos", &dir.join("photos"))).await;
    let n = nack(s.commit().await);
    assert_eq!((n.change, n.reason.as_str()), (0, "not allowed"));
    // A COMMIT with nothing pending checks the right too.
    let n = nack(s.commit().await);
    assert_eq!((n.change, n.reason.as_str()), (0, "not allowed"));
    assert_eq!(std::fs::read(&path).unwrap(), before, "the file changed");
    stop(d).await;
}

/// Review Focus 2: the right is checked again at commit and at each read.
#[tokio::test]
async fn losing_admin_mid_session() {
    let (_tmp, dir) = temp_dir();
    let client = client_endpoint().await;
    let (d, path) = spawn_from_file(&dir, &laptop(client.id(), "admin = \"rw\"\n")).await;
    let mut s = Session::open(&d, &client).await;
    assert!(matches!(s.read_view().await, admin_reply::Msg::View(_)));
    s.change(export_add("photos", &dir.join("photos"))).await;
    let text = std::fs::read_to_string(&path).unwrap();
    let without = text.replace("admin = \"rw\"\n", "");
    assert_ne!(without, text);
    irsync::fs::private::write(&path, &without).unwrap();
    let n = nack(s.commit().await);
    assert_eq!(n.change, 0, "{}", n.reason);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), without);
    s.send(admin_request::Msg::Read(Read {
        what: Some(read::What::View(ReadView {})),
    }))
    .await;
    assert_not_allowed(s.reply().await);
    s.assert_closed().await;
    stop(d).await;
}

#[tokio::test]
async fn too_many_changes_drop_the_transaction() {
    let (_tmp, dir) = temp_dir();
    let client = client_endpoint().await;
    let (d, path) = spawn_from_file(&dir, &laptop(client.id(), "admin = \"rw\"\n")).await;
    let before = std::fs::read(&path).unwrap();
    let mut s = Session::open(&d, &client).await;
    for i in 0..=ADMIN_MAX_CHANGES {
        s.change(export_add(&format!("e{i}"), &dir.join("photos")))
            .await;
    }
    let n = nack(s.reply().await.expect("a NACK for one change too many"));
    assert_eq!(n.change, 0, "{}", n.reason);
    assert!(n.reason.contains("100"), "{}", n.reason);
    assert!(ack(s.commit().await).is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), before, "the file changed");
    stop(d).await;
}

#[tokio::test]
async fn a_held_lock_makes_commit_busy() {
    let (_tmp, dir) = temp_dir();
    let client = client_endpoint().await;
    let (d, path) = spawn_from_file(&dir, &laptop(client.id(), "admin = \"rw\"\n")).await;
    let before = std::fs::read(&path).unwrap();
    let lock = irsync::manage::lock_config(&path).unwrap();
    let mut s = Session::open(&d, &client).await;
    s.change(export_add("photos", &dir.join("photos"))).await;
    let start = std::time::Instant::now();
    let n = nack(s.commit().await);
    let waited = start.elapsed();
    assert_eq!(n.change, 0, "{}", n.reason);
    assert!(n.reason.starts_with("busy"), "{}", n.reason);
    assert!(
        waited >= LOCK_WAIT - Duration::from_millis(100),
        "answered after {waited:?}"
    );
    drop(lock);
    assert_eq!(std::fs::read(&path).unwrap(), before, "the file changed");
    // The lock is free again: the same session commits.
    s.change(export_add("photos", &dir.join("photos"))).await;
    assert_eq!(ack(s.commit().await).len(), 1);
    stop(d).await;
}

/// On tokio's paused clock, through the in-process fault layer with a
/// clean link: the daemon ends a session idle for `ADMIN_IDLE`.
#[cfg(feature = "netsim")]
#[tokio::test(start_paused = true)]
async fn an_idle_session_is_closed() {
    use irsync::daemon::ADMIN_IDLE;
    use irsync::net::netsim::ProfileName;
    common::with_netsim(ProfileName::TestWan, 1, async {
        let (_tmp, dir) = temp_dir();
        let client = client_endpoint().await;
        let (d, _path) = spawn_from_file(&dir, &laptop(client.id(), "admin = \"ro\"\n")).await;
        let mut s = Session::open(&d, &client).await;
        assert!(matches!(s.read_view().await, admin_reply::Msg::View(_)));
        let start = tokio::time::Instant::now();
        let r = tokio::time::timeout(ADMIN_IDLE * 2, read_msg::<AdminReply, _>(&mut s.recv))
            .await
            .expect("the daemon kept an idle session open");
        assert!(matches!(r, Ok(None) | Err(_)), "{r:?}");
        let idle = start.elapsed();
        assert!(idle >= ADMIN_IDLE, "closed after {idle:?}");
        assert!(
            idle < ADMIN_IDLE + Duration::from_secs(30),
            "closed after {idle:?}"
        );
        stop(d).await;
    })
    .await;
}
