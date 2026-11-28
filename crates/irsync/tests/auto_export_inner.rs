//! A peer with `all_paths` that also has a grant on a read-only export E:
//! on Windows a request by absolute path that spells E's directory (or a
//! place on the way to it, or below it) otherwise than E's configuration
//! does is refused naming E, before an automatic export is made for it:
//! nothing is created and no export is added.
#![cfg(windows)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{client_endpoint, spawn_daemon_full, within, DaemonHandle};
use iroh::Endpoint;
use irsync::authz::{Access, Authorizer, MemoryAuthorizer, Permissions};
use irsync::config::exports::Export;
use irsync::net::frame::{read_body, write_msg};
use irsync::options::Direction;
use irsync::session::SyncHello;
use irsync_proto::v1::{error::Code, hello_reply, Error, Hello, HelloReply, SyncRequest};
use irsync_proto::ALPN;
use prost::Message as _;

fn hello(path: &Path, dir: Direction) -> SyncHello {
    SyncHello::new(
        dir,
        SyncRequest {
            path: irsync::platform::os_to_bytes(path.as_os_str()).to_vec(),
            preserve: Some(Default::default()),
            ..Default::default()
        },
    )
}

/// The daemon's answer to `h`: `Ok(())` for a `HelloAck`, else the error.
async fn ask(d: &DaemonHandle, client: &Endpoint, h: SyncHello) -> Result<(), Error> {
    let h: Hello = h.into();
    let conn = within(client.connect(d.addr.clone(), ALPN)).await.unwrap();
    let (mut send, mut recv) = within(conn.open_bi()).await.unwrap();
    within(write_msg(&mut send, &h)).await.unwrap();
    send.finish().unwrap();
    let body = within(read_body(&mut recv))
        .await
        .unwrap()
        .expect("a reply");
    match HelloReply::decode(&body[..]).unwrap().reply {
        Some(hello_reply::Reply::Ack(_)) => Ok(()),
        Some(hello_reply::Reply::Error(e)) => Err(e),
        r => panic!("not a session's reply: {r:?}"),
    }
}

/// A daemon whose only configured export is the read-only `ro` at the
/// path `ro` (as configured, made or not by the caller), for a client with
/// `all_paths` read-write and a grant on `ro`.
struct Setup {
    authz: Arc<MemoryAuthorizer>,
    daemon: DaemonHandle,
    client: Endpoint,
}

async fn setup(ro: PathBuf) -> Setup {
    let client = client_endpoint().await;
    let export = Export {
        path: ro,
        read_only: true,
        index_at_start: false,
        fsync: false,
        auto: false,
        rw: Vec::new(),
        ro: Vec::new(),
    };
    let grants = Permissions {
        all_paths: Some(Access::ReadWrite),
        exports: [("ro".to_string(), Access::ReadWrite)].into(),
        ..Permissions::default()
    };
    let authz = Arc::new(
        MemoryAuthorizer::single(client.id(), grants)
            .with_exports([("ro".to_string(), export)].into()),
    );
    let daemon = spawn_daemon_full(authz.clone(), tempfile::tempdir().unwrap(), None).await;
    Setup {
        authz,
        daemon,
        client,
    }
}

impl Setup {
    async fn ask(&self, path: &Path, dir: Direction) -> Result<(), Error> {
        ask(&self.daemon, &self.client, hello(path, dir)).await
    }

    /// The exports' names, sorted.
    fn exports(&self) -> Vec<String> {
        self.authz
            .snapshot()
            .unwrap()
            .exports
            .keys()
            .cloned()
            .collect()
    }

    async fn stop(self) {
        self.daemon.shutdown.cancel();
        let _ = within(self.daemon.task).await;
    }
}

fn refused_naming(r: Result<(), Error>, name: &str, what: &str) {
    let e = r.expect_err(&format!("{what}: admitted"));
    assert_eq!(e.code, Code::BadPath as i32, "{what}: {}", e.message);
    assert!(
        e.message.contains(&format!(
            "is inside export {name}; ask for it by that export"
        )),
        "{what}: {}",
        e.message
    );
}

/// The names in `dir`.
fn listed(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// (a) E's root exists as `srv\ro` and is configured as `Srv\Ro`: a push
/// (or pull) by absolute path to a third spelling, `sRv\rO`, is refused
/// naming E, nothing is written and no automatic export is added. (c) So
/// is a push below it, to a name not made yet. (d) E's own spelling still
/// meets its rules.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_granted_export_in_another_spelling_is_not_made_an_automatic_export() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("srv").join("ro");
    std::fs::create_dir_all(&real).unwrap();
    let s = setup(tmp.path().join("Srv").join("Ro")).await;
    let third = tmp.path().join("sRv").join("rO");
    for (what, path, dir) in [
        (
            "a push to a third spelling",
            third.clone(),
            Direction::ClientSends,
        ),
        (
            "a pull of a third spelling",
            third.clone(),
            Direction::DaemonSends,
        ),
        ("a push below it", third.join("sub"), Direction::ClientSends),
        (
            "a push to the disk's spelling",
            real.clone(),
            Direction::ClientSends,
        ),
    ] {
        refused_naming(s.ask(&path, dir).await, "ro", what);
    }
    assert_eq!(s.exports(), ["ro"], "an export was added");
    assert!(listed(&real).is_empty(), "made: {:?}", listed(&real));
    // E's own spelling: E itself, read-only.
    let e = s
        .ask(&tmp.path().join("Srv").join("Ro"), Direction::ClientSends)
        .await
        .expect_err("a push into the read-only export");
    assert_eq!(e.code, Code::ReadOnly as i32, "{}", e.message);
    s.stop().await;
}

/// (b) E's root is not made yet (`data\ro`, below an existing `data`): a
/// push by absolute path to `data\RO` is refused naming E, and `RO` is not
/// created.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_missing_granted_export_is_not_made_by_an_automatic_export() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let s = setup(data.join("ro")).await;
    for (what, path) in [
        ("RO", data.join("RO")),
        ("Ro below", data.join("Ro").join("x")),
        ("DATA/RO", tmp.path().join("DATA").join("RO")),
    ] {
        refused_naming(s.ask(&path, Direction::ClientSends).await, "ro", what);
    }
    assert!(listed(&data).is_empty(), "made: {:?}", listed(&data));
    assert_eq!(s.exports(), ["ro"], "an export was added");
    s.stop().await;
}

/// (e) A path in no granted export still becomes an automatic export and
/// is served, made when it is missing. (f) Another case of that automatic
/// export's existing directory is served by that same export, as is a name
/// not made yet below it typed in another case of the existing part; no
/// second automatic export is added.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_path_outside_granted_exports_still_becomes_an_automatic_export() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("srv").join("ro")).unwrap();
    let s = setup(tmp.path().join("srv").join("ro")).await;
    let elsewhere = tmp.path().join("Elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    s.ask(&elsewhere, Direction::ClientSends)
        .await
        .unwrap_or_else(|e| panic!("{}", e.message));
    let added: Vec<String> = s.exports().into_iter().filter(|n| n != "ro").collect();
    assert_eq!(added.len(), 1, "{added:?}");
    let new = tmp.path().join("made-here");
    s.ask(&new, Direction::ClientSends)
        .await
        .unwrap_or_else(|e| panic!("{}", e.message));
    assert!(new.is_dir());
    assert_eq!(s.exports().len(), 3, "{:?}", s.exports());
    // (f) Served by the automatic export of `Elsewhere`.
    for path in [
        tmp.path().join("ELSEWHERE"),
        tmp.path().join("elsewhere"),
        tmp.path().join("eLSEWHERE").join("not-made-yet"),
        elsewhere.clone(),
        elsewhere.join("x"),
    ] {
        s.ask(&path, Direction::ClientSends)
            .await
            .unwrap_or_else(|e| panic!("{path:?}: {}", e.message));
    }
    assert_eq!(s.exports().len(), 3, "a second export: {:?}", s.exports());
    assert_eq!(
        listed(tmp.path()),
        ["Elsewhere", "made-here", "srv"],
        "another spelling was made"
    );
    s.stop().await;
}

/// E configured in the disk's own spelling (`srv\ro`): a request in
/// another spelling resolves to that real path, which is E's, so it is
/// served under E's rules: a push is refused as read-only (not as
/// "inside export"), and no export is added.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_variant_spelling_of_a_granted_export_is_served_under_its_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("srv").join("ro");
    std::fs::create_dir_all(&real).unwrap();
    let s = setup(real.clone()).await;
    for path in [
        tmp.path().join("SRV").join("RO"),
        tmp.path().join("Srv").join("rO").join("new"),
    ] {
        let e = s
            .ask(&path, Direction::ClientSends)
            .await
            .expect_err(&format!("{path:?}: a push into the read-only export"));
        assert_eq!(e.code, Code::ReadOnly as i32, "{path:?}: {}", e.message);
    }
    // A pull is served by E.
    s.ask(&tmp.path().join("SRV").join("RO"), Direction::DaemonSends)
        .await
        .unwrap_or_else(|e| panic!("{}", e.message));
    assert_eq!(s.exports(), ["ro"], "an export was added");
    assert!(listed(&real).is_empty(), "made: {:?}", listed(&real));
    s.stop().await;
}

/// A push of a directory by name (`irsync RO host:<tmp>\data`): the name
/// the daemon joins below the path is part of the way, so a request that
/// reaches E (`data\ro`) in another spelling is refused before an
/// automatic export is added for `data`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pushed_name_that_reaches_a_granted_export_adds_no_export() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(data.join("ro")).unwrap();
    let s = setup(data.join("ro")).await;
    let mut h = hello(&data, Direction::ClientSends);
    h.req.name = b"RO".to_vec();
    refused_naming(
        ask(&s.daemon, &s.client, h).await,
        "ro",
        "a push of RO into data",
    );
    assert_eq!(s.exports(), ["ro"], "an export was added");
    assert!(listed(&data.join("ro")).is_empty());
    // Another name goes ahead, as an automatic export of `data`.
    let mut h = hello(&data, Direction::ClientSends);
    h.req.name = b"other".to_vec();
    ask(&s.daemon, &s.client, h)
        .await
        .unwrap_or_else(|e| panic!("{}", e.message));
    assert_eq!(s.exports().len(), 2, "{:?}", s.exports());
    s.stop().await;
}
