mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use common::{
    client_endpoint, spawn_daemon, spawn_daemon_full, spawn_daemon_home, spawn_daemon_with,
    spawn_daemon_with_state, within, DaemonHandle,
};
use iroh::Endpoint;
use irsync::authz::{Access, FileAuthorizer, Grants, MemoryAuthorizer};
use irsync::config::paths::Paths;
use irsync::net::frame::{read_msg, write_msg};
use irsync_proto::chunk::CHUNK_PARAMS;
use irsync_proto::v1::{error::Code, hello::Direction, Error, Hello, HelloAck};
use irsync_proto::{ALPN, PROTOCOL_VERSION};

fn hello(path: &[u8], dir: Direction) -> Hello {
    Hello {
        protocol_version: PROTOCOL_VERSION,
        session_id: vec![],
        path: path.to_vec(),
        direction: dir as i32,
        chunk_min: CHUNK_PARAMS.min,
        chunk_avg: CHUNK_PARAMS.avg,
        chunk_max: CHUNK_PARAMS.max,
        contents_only: false,
        ..Default::default()
    }
}

fn grants(access: Access) -> Grants {
    Grants {
        all: Some(access),
        immediate: false,
        ..Grants::default()
    }
}

/// Sends `h` from an authorised client with `access`; returns the reply bytes' decode attempt.
async fn exchange(access: Access, h: Hello) -> Result<HelloAckOrError, String> {
    exchange_with(grants(access), h).await
}

async fn exchange_with(g: Grants, h: Hello) -> Result<HelloAckOrError, String> {
    let dir = tempfile::tempdir().unwrap();
    exchange_in(dir.path(), g, h).await
}

/// A fresh temp dir: itself, its file name (home-relative from its parent)
/// and its absolute path, as bytes.
fn export_dir() -> (tempfile::TempDir, Vec<u8>, Vec<u8>) {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    let rel = dir.path().file_name().unwrap().as_bytes().to_vec();
    let abs = dir.path().as_os_str().as_bytes().to_vec();
    (dir, rel, abs)
}

fn no_access(path: &[u8]) -> String {
    format!("no access to {}", irsync::fs::paths::escape_path(path))
}

fn reply(r: Result<HelloAckOrError, String>) -> (i32, String) {
    match r.unwrap() {
        HelloAckOrError::Err(e) => (e.code, e.message),
        HelloAckOrError::Ack(_) => panic!("expected an error, got ack"),
    }
}

/// Sends `h` to a daemon serving `rw` = `dir` and `ro` = `dir` (read-only)
/// whose home is `dir`'s parent.
async fn exchange_in(
    dir: &std::path::Path,
    g: Grants,
    h: Hello,
) -> Result<HelloAckOrError, String> {
    let client = client_endpoint().await;
    let d = spawn_daemon_home(
        vec![
            ("rw", dir.to_path_buf(), false),
            ("ro", dir.to_path_buf(), true),
        ],
        MemoryAuthorizer::single(client.id(), g),
        dir.parent().map(|p| p.to_path_buf()),
    )
    .await;
    let out = exchange_against(&d, &client, h).await;
    d.shutdown.cancel();
    let _ = within(d.task).await;
    out
}

/// Sends `h` over a fresh connection to `d`; any failure (including the
/// daemon closing the connection before the Hello) is an `Err`.
async fn exchange_against(
    d: &DaemonHandle,
    client: &Endpoint,
    h: Hello,
) -> Result<HelloAckOrError, String> {
    let conn = within(client.connect(d.addr.clone(), ALPN))
        .await
        .map_err(|e| e.to_string())?;
    let (mut send, mut recv) = within(conn.open_bi()).await.map_err(|e| e.to_string())?;
    within(write_msg(&mut send, &h))
        .await
        .map_err(|e| e.to_string())?;
    send.finish().map_err(|e| e.to_string())?;
    // Only the reply: after a HelloAck the daemon waits for the listing
    // stream, which this client never opens.
    let mut raw = vec![0u8; 4];
    within(recv.read_exact(&mut raw))
        .await
        .map_err(|e| e.to_string())?;
    let len = u32::from_be_bytes(raw[..4].try_into().unwrap()) as usize;
    raw.resize(4 + len, 0);
    within(recv.read_exact(&mut raw[4..]))
        .await
        .map_err(|e| e.to_string())?;
    // HelloAck and Error differ in the wire type of field 1, so at most one decodes.
    Ok(match read_msg::<HelloAck, _>(&mut &raw[..]).await {
        Ok(Some(ack)) => HelloAckOrError::Ack(ack),
        _ => HelloAckOrError::Err(read_msg::<Error, _>(&mut &raw[..]).await.unwrap().unwrap()),
    })
}

enum HelloAckOrError {
    Ack(HelloAck),
    Err(Error),
}

fn expect_err(r: Result<HelloAckOrError, String>, code: Code) {
    match r.unwrap() {
        HelloAckOrError::Err(e) => assert_eq!(e.code, code as i32, "{}", e.message),
        HelloAckOrError::Ack(_) => panic!("expected {code:?}, got ack"),
    }
}

#[tokio::test]
async fn unauthorized_key_connection_closed_before_hello() {
    let stranger = client_endpoint().await;
    closed_before_hello(|_| MemoryAuthorizer::single(stranger.id(), grants(Access::ReadWrite)))
        .await;
}

#[tokio::test]
async fn empty_grants_closed_before_hello() {
    closed_before_hello(|id| MemoryAuthorizer::single(id, Grants::default())).await;
}

async fn closed_before_hello(mk: impl FnOnce(iroh::EndpointId) -> MemoryAuthorizer) {
    let client = client_endpoint().await;
    let dir = tempfile::tempdir().unwrap();
    let authz = mk(client.id());
    let d = spawn_daemon(vec![("rw", dir.path().to_path_buf(), false)], authz).await;
    let conn = within(client.connect(d.addr.clone(), ALPN)).await.unwrap();
    // The stream may open locally and the write may even succeed; reading must fail.
    let res = within(async {
        let (mut send, mut recv) = conn.open_bi().await?;
        let _ = write_msg(&mut send, &hello(b"rw", Direction::ClientSends)).await;
        let _ = send.finish();
        recv.read_to_end(1 << 20)
            .await
            .map(|_| ())
            .map_err(anyhow::Error::from)
    })
    .await;
    assert!(res.is_err(), "unauthorized peer got a clean reply: {res:?}");
    within(conn.closed()).await;
    assert_eq!(d.hellos.load(Ordering::SeqCst), 0, "daemon parsed a Hello");
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

#[tokio::test]
async fn authorized_key_gets_hello_ack() {
    match exchange(
        Access::ReadWrite,
        hello(b"rw/sub/dir", Direction::ClientSends),
    )
    .await
    {
        Ok(HelloAckOrError::Ack(a)) => {
            assert_eq!(a.session_id.len(), 16);
            assert!(!a.resumed);
            assert_eq!(a.protocol_version, PROTOCOL_VERSION);
        }
        _ => panic!("expected HelloAck"),
    }
}

#[tokio::test]
async fn read_only_grant_cannot_send() {
    expect_err(
        exchange(Access::ReadOnly, hello(b"rw", Direction::ClientSends)).await,
        Code::ReadOnly,
    );
}

/// `rw` and `ro` share a directory: a key granted only `ro` reaches that
/// directory under the name `rw` too (the grant is on the directory, and
/// `ro` is read-only, so it still cannot write there).
#[tokio::test]
async fn an_ungranted_name_reaches_the_granted_twin_on_the_same_path() {
    let g = || Grants {
        exports: [("ro".to_string(), Access::ReadWrite)].into(),
        all: None,
        immediate: false,
    };
    assert!(matches!(
        exchange_with(g(), hello(b"rw", Direction::DaemonSends))
            .await
            .unwrap(),
        HelloAckOrError::Ack(_)
    ));
    expect_err(
        exchange_with(g(), hello(b"rw", Direction::ClientSends)).await,
        Code::ReadOnly,
    );
}

/// A grant on an outer export reaches everything below it, an ungranted
/// inner export included: the reply is the same as for any other path
/// under the outer one, so a key cannot map where other exports sit.
#[tokio::test]
async fn outer_grant_reaches_paths_inside_an_ungranted_inner_export() {
    use std::os::unix::ffi::OsStrExt;
    let tmp = tempfile::tempdir().unwrap();
    let (srv, inner) = (tmp.path().join("srv"), tmp.path().join("srv/photos"));
    std::fs::create_dir_all(&inner).unwrap();
    let client = client_endpoint().await;
    let d = spawn_daemon_home(
        vec![("srv", srv.clone(), false), ("photos", inner.clone(), true)],
        MemoryAuthorizer::single(
            client.id(),
            Grants {
                exports: [("srv".to_string(), Access::ReadWrite)].into(),
                all: None,
                immediate: false,
            },
        ),
        None,
    )
    .await;
    for path in [
        inner.as_os_str().as_bytes(),
        &b"srv/photos"[..],
        &b"srv/other"[..],
    ] {
        let r = exchange_against(&d, &client, hello(path, Direction::ClientSends)).await;
        assert!(matches!(r.unwrap(), HelloAckOrError::Ack(_)), "{path:?}");
    }
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

#[tokio::test]
async fn ro_grant_can_pull() {
    match exchange(Access::ReadOnly, hello(b"rw", Direction::DaemonSends)).await {
        Ok(HelloAckOrError::Ack(_)) => {}
        _ => panic!("expected HelloAck"),
    }
}

#[tokio::test]
async fn wrong_protocol_version_rejected() {
    let mut h = hello(b"rw", Direction::ClientSends);
    h.protocol_version = PROTOCOL_VERSION + 1;
    expect_err(exchange(Access::ReadWrite, h).await, Code::VersionMismatch);
}

#[tokio::test]
async fn wrong_chunk_params_rejected() {
    let mut h = hello(b"rw", Direction::ClientSends);
    h.chunk_avg += 1;
    expect_err(exchange(Access::ReadWrite, h).await, Code::VersionMismatch);
}

/// A daemon started with an empty exports file serves an export added
/// afterwards, and stops serving a key revoked afterwards, with no restart.
#[tokio::test]
async fn exports_file_is_read_on_every_connection() {
    let d = tempfile::tempdir().unwrap();
    let exports = d.path().join("irsync.toml");
    let client = client_endpoint().await;
    let authz = Arc::new(FileAuthorizer::new(Paths::resolve(Some(&exports))));
    let daemon = spawn_daemon_with(authz).await;
    let h = hello(b"data", Direction::DaemonSends);
    assert!(
        exchange_against(&daemon, &client, h.clone()).await.is_err(),
        "rejected before Hello: no grant"
    );
    let grant = format!(
        "[exports.data]\npath = {:?}\nro = [\"{}\"]\n",
        d.path().join("data"),
        client.id()
    );
    std::fs::write(&exports, &grant).unwrap();
    std::fs::create_dir_all(d.path().join("data")).unwrap();
    assert!(matches!(
        exchange_against(&daemon, &client, h.clone()).await,
        Ok(HelloAckOrError::Ack(_))
    ));
    std::fs::write(&exports, "[x\n").unwrap(); // broken: fail closed, daemon alive
    assert!(exchange_against(&daemon, &client, h.clone()).await.is_err());
    std::fs::write(&exports, &grant).unwrap(); // fixed again: still serving
    assert!(matches!(
        exchange_against(&daemon, &client, h.clone()).await,
        Ok(HelloAckOrError::Ack(_))
    ));
    std::fs::write(
        &exports,
        format!("[exports.data]\npath = {:?}\n", d.path().join("data")),
    )
    .unwrap();
    assert!(
        exchange_against(&daemon, &client, h).await.is_err(),
        "revoked"
    );
    daemon.shutdown.cancel();
    let _ = within(daemon.task).await;
}

/// An export whose index cannot be opened gets an INTERNAL error reply, not
/// a dropped connection the client would retry.
#[tokio::test]
async fn index_open_failure_is_an_internal_error_reply() {
    let d = tempfile::tempdir().unwrap();
    let exports = d.path().join("irsync.toml");
    std::fs::create_dir_all(d.path().join("data")).unwrap();
    let client = client_endpoint().await;
    let state = tempfile::tempdir().unwrap();
    // The index directory cannot be created: a file is in its place.
    std::fs::write(state.path().join("index"), b"").unwrap();
    let authz = Arc::new(FileAuthorizer::new(Paths::resolve(Some(&exports))));
    let daemon = spawn_daemon_with_state(authz, state).await;
    std::fs::write(
        &exports,
        format!(
            "[exports.data]\npath = {:?}\nro = [\"{}\"]\n",
            d.path().join("data"),
            client.id()
        ),
    )
    .unwrap();
    let h = hello(b"data", Direction::DaemonSends);
    match exchange_against(&daemon, &client, h).await {
        Ok(HelloAckOrError::Err(e)) => {
            assert_eq!(e.code, Code::Internal as i32, "{}", e.message);
            assert!(
                e.message.contains("cannot open index for export \"data\""),
                "{}",
                e.message
            );
        }
        Ok(HelloAckOrError::Ack(_)) => panic!("expected INTERNAL, got ack"),
        Err(e) => panic!("expected an INTERNAL error reply, got a dropped connection: {e}"),
    }
    daemon.shutdown.cancel();
    let _ = within(daemon.task).await;
}

#[tokio::test]
async fn short_name_home_and_absolute_all_reach_the_export() {
    let (dir, home_rel, abs) = export_dir();
    for path in [&b"rw"[..], &b"rw/"[..], &home_rel, &abs] {
        let r = exchange_in(
            dir.path(),
            grants(Access::ReadWrite),
            hello(path, Direction::ClientSends),
        )
        .await;
        assert!(matches!(r.unwrap(), HelloAckOrError::Ack(_)), "{path:?}");
    }
}

#[tokio::test]
async fn path_outside_every_export_is_unauthorized_with_the_path_as_typed() {
    let (dir, ..) = export_dir();
    for path in [&b"/etc"[..], b"nope", b"", b"~", b"/"] {
        let e = reply(
            exchange_in(
                dir.path(),
                grants(Access::ReadWrite),
                hello(path, Direction::DaemonSends),
            )
            .await,
        );
        assert_eq!(e, (Code::Unauthorized as i32, no_access(path)), "{path:?}");
    }
}

#[tokio::test]
async fn dotdot_is_bad_path_for_a_granted_key() {
    let (dir, ..) = export_dir();
    for path in [&b"rw/../x"[..], b"/a/../b", b"a\0b"] {
        let e = reply(
            exchange_in(
                dir.path(),
                grants(Access::ReadWrite),
                hello(path, Direction::DaemonSends),
            )
            .await,
        );
        assert_eq!(e.0, Code::BadPath as i32, "{path:?}: {}", e.1);
    }
}

/// The reply for a path the key may not use is the same whether or not
/// the path exists, so a key cannot probe the daemon's layout.
#[tokio::test]
async fn ungranted_reply_does_not_reveal_whether_a_path_exists() {
    let (dir, _, abs) = export_dir();
    let none = Grants {
        exports: [("other".to_string(), Access::ReadWrite)].into(),
        all: None,
        immediate: false,
    };
    let mut existing = abs.clone();
    existing.extend_from_slice(b"/sub");
    let a = reply(
        exchange_in(
            dir.path(),
            none.clone(),
            hello(&existing, Direction::DaemonSends),
        )
        .await,
    );
    let b = reply(exchange_in(dir.path(), none, hello(b"/no/such", Direction::DaemonSends)).await);
    assert_eq!(a, (Code::Unauthorized as i32, no_access(&existing)));
    assert_eq!(b, (Code::Unauthorized as i32, no_access(b"/no/such")));
}

#[tokio::test]
async fn push_basename_is_joined_after_resolution() {
    let tmp = tempfile::tempdir().unwrap();
    let (srv, home) = (tmp.path().join("srv/photos"), tmp.path().join("home"));
    std::fs::create_dir_all(&srv).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let client = client_endpoint().await;
    let d = spawn_daemon_home(
        vec![
            ("photos", srv.clone(), false),
            ("home", home.clone(), false),
        ],
        MemoryAuthorizer::single(client.id(), grants(Access::ReadWrite)),
        Some(home.clone()),
    )
    .await;
    let mut h = hello(b"", Direction::ClientSends);
    h.name = b"photos".to_vec();
    let r = exchange_against(&d, &client, h).await;
    assert!(matches!(r.unwrap(), HelloAckOrError::Ack(_)));
    assert!(home.join("photos").is_dir(), "the push lands in the home");
    assert!(
        !srv.join("photos").exists(),
        "not in the export of that name"
    );
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

#[tokio::test]
async fn read_only_export_and_ro_grant_still_refuse_sends() {
    let (dir, _, abs) = export_dir();
    // `ro` is read_only: a rw grant cannot send to it by short name.
    expect_err(
        exchange_in(
            dir.path(),
            grants(Access::ReadWrite),
            hello(b"ro", Direction::ClientSends),
        )
        .await,
        Code::ReadOnly,
    );
    // The same directory by absolute path: rw and ro share it; the best effective access (rw) wins.
    assert!(matches!(
        exchange_in(
            dir.path(),
            grants(Access::ReadWrite),
            hello(&abs, Direction::ClientSends)
        )
        .await
        .unwrap(),
        HelloAckOrError::Ack(_)
    ));
    expect_err(
        exchange_in(
            dir.path(),
            grants(Access::ReadOnly),
            hello(&abs, Direction::ClientSends),
        )
        .await,
        Code::ReadOnly,
    );
}

/// `LIST_EXPORTS` answers with the exports the key may use, by name, with
/// the real path and the effective access.
async fn list_against(
    d: &DaemonHandle,
    client: &Endpoint,
) -> Result<Vec<(String, Vec<u8>, i32)>, String> {
    use irsync_proto::v1::ExportList;
    let mut h = hello(b"", Direction::ListExports);
    h.direction = Direction::ListExports as i32;
    let conn = within(client.connect(d.addr.clone(), ALPN))
        .await
        .map_err(|e| e.to_string())?;
    let (mut send, mut recv) = within(conn.open_bi()).await.map_err(|e| e.to_string())?;
    within(write_msg(&mut send, &h))
        .await
        .map_err(|e| e.to_string())?;
    send.finish().map_err(|e| e.to_string())?;
    let list: ExportList = within(read_msg(&mut recv))
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no reply")?;
    Ok(list
        .exports
        .into_iter()
        .map(|e| (e.name, e.path, e.access))
        .collect())
}

#[tokio::test]
async fn list_exports_names_only_what_the_key_may_use() {
    use irsync_proto::v1::export_entry::Access as A;
    use std::os::unix::ffi::OsStrExt;
    let (dir, ..) = export_dir();
    let path = dir.path().as_os_str().as_bytes().to_vec();
    let client = client_endpoint().await;
    let spawn = |g: Grants| {
        spawn_daemon_home(
            vec![
                ("rw", dir.path().to_path_buf(), false),
                ("ro", dir.path().to_path_buf(), true),
            ],
            MemoryAuthorizer::single(client.id(), g),
            None,
        )
    };
    // Granted on rw only: one entry.
    let d = spawn(Grants {
        exports: [("rw".to_string(), Access::ReadWrite)].into(),
        all: None,
        immediate: false,
    })
    .await;
    assert_eq!(
        list_against(&d, &client).await.unwrap(),
        vec![("rw".to_string(), path.clone(), A::Rw as i32)]
    );
    d.shutdown.cancel();
    let _ = within(d.task).await;
    // Granted everywhere: both, sorted by name, read_only capping to RO.
    let d = spawn(grants(Access::ReadWrite)).await;
    assert_eq!(
        list_against(&d, &client).await.unwrap(),
        vec![
            ("ro".to_string(), path.clone(), A::Ro as i32),
            ("rw".to_string(), path.clone(), A::Rw as i32),
        ]
    );
    d.shutdown.cancel();
    let _ = within(d.task).await;
    // No grants at all: closed before Hello, as for a sync.
    let d = spawn(Grants::default()).await;
    assert!(list_against(&d, &client).await.is_err());
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

fn immediate() -> Grants {
    Grants {
        immediate: true,
        ..Grants::default()
    }
}

fn bytes(p: &std::path::Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    p.as_os_str().as_bytes().to_vec()
}

/// A daemon with no home whose exports and grants are `authz`'s, kept
/// so the test can look at what the daemon added.
async fn spawn_memory(authz: &Arc<MemoryAuthorizer>) -> DaemonHandle {
    spawn_daemon_full(authz.clone(), tempfile::tempdir().unwrap(), None).await
}

#[tokio::test]
async fn immediate_push_creates_auto_export() {
    use irsync::authz::Authorizer;
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("dest");
    std::fs::create_dir(&dest).unwrap();
    let client = client_endpoint().await;
    let authz = Arc::new(MemoryAuthorizer::single(client.id(), immediate()));
    let d = spawn_memory(&authz).await;
    let r = exchange_against(&d, &client, hello(&bytes(&dest), Direction::ClientSends)).await;
    assert!(matches!(r.unwrap(), HelloAckOrError::Ack(_)));
    let snap = authz.snapshot().unwrap();
    assert_eq!(snap.exports.len(), 1, "{:?}", snap.exports);
    let (name, e) = snap.exports.iter().next().unwrap();
    assert!(e.auto);
    assert_eq!(e.path, dest);
    assert_eq!(e.rw, vec![client.id().to_string()]);
    assert_eq!(
        snap.grants(&client.id()).access(name),
        Some(Access::ReadWrite)
    );
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

#[tokio::test]
async fn immediate_reuses_containing_auto_export() {
    use irsync::authz::Authorizer;
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("dest");
    std::fs::create_dir(&dest).unwrap();
    let client = client_endpoint().await;
    let authz = Arc::new(MemoryAuthorizer::single(client.id(), immediate()));
    let d = spawn_memory(&authz).await;
    for path in [dest.clone(), dest.join("sub")] {
        let r = exchange_against(&d, &client, hello(&bytes(&path), Direction::ClientSends)).await;
        assert!(matches!(r.unwrap(), HelloAckOrError::Ack(_)), "{path:?}");
    }
    let snap = authz.snapshot().unwrap();
    assert_eq!(snap.exports.len(), 1, "{:?}", snap.exports);
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

#[tokio::test]
async fn immediate_off_rejects_unexported_path() {
    use irsync::authz::Authorizer;
    let tmp = tempfile::tempdir().unwrap();
    let (exported, elsewhere) = (tmp.path().join("exported"), tmp.path().join("elsewhere"));
    std::fs::create_dir(&exported).unwrap();
    std::fs::create_dir(&elsewhere).unwrap();
    let client = client_endpoint().await;
    let g = Grants {
        exports: [("other".to_string(), Access::ReadWrite)].into(),
        all: None,
        immediate: false,
    };
    let export = irsync::config::exports::Export {
        path: exported,
        read_only: false,
        warm_index: false,
        fsync: false,
        auto: false,
        rw: Vec::new(),
        ro: Vec::new(),
    };
    let authz = Arc::new(
        MemoryAuthorizer::single(client.id(), g)
            .with_exports([("other".to_string(), export)].into()),
    );
    let d = spawn_memory(&authz).await;
    let path = bytes(&elsewhere);
    let e = reply(exchange_against(&d, &client, hello(&path, Direction::ClientSends)).await);
    assert_eq!(e, (Code::Unauthorized as i32, no_access(&path)));
    let snap = authz.snapshot().unwrap();
    assert_eq!(
        snap.exports.keys().collect::<Vec<_>>(),
        vec!["other"],
        "an export was added"
    );
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

#[tokio::test]
async fn immediate_push_to_missing_dir_creates_and_exports_it() {
    use irsync::authz::Authorizer;
    let tmp = tempfile::tempdir().unwrap();
    let new = tmp.path().join("new");
    let client = client_endpoint().await;
    let authz = Arc::new(MemoryAuthorizer::single(client.id(), immediate()));
    let d = spawn_memory(&authz).await;
    let r = exchange_against(&d, &client, hello(&bytes(&new), Direction::ClientSends)).await;
    assert!(matches!(r.unwrap(), HelloAckOrError::Ack(_)));
    assert!(new.is_dir());
    let snap = authz.snapshot().unwrap();
    let paths: Vec<_> = snap.exports.values().map(|e| e.path.clone()).collect();
    assert_eq!(paths, vec![new]);
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

#[tokio::test]
async fn immediate_refuses_root_and_non_utf8_roots() {
    use irsync::authz::Authorizer;
    let tmp = tempfile::tempdir().unwrap();
    let mut bad = bytes(tmp.path());
    bad.extend_from_slice(b"/caf\xe9");
    let client = client_endpoint().await;
    let authz = Arc::new(MemoryAuthorizer::single(client.id(), immediate()));
    let d = spawn_memory(&authz).await;
    let cases = [
        (
            &b"/"[..],
            Direction::DaemonSends,
            "cannot export / automatically; add an export by hand",
        ),
        (
            &bad,
            Direction::ClientSends,
            "automatic exports need a UTF-8 path",
        ),
    ];
    for (path, dir, message) in cases {
        let e = reply(exchange_against(&d, &client, hello(path, dir)).await);
        assert_eq!(e, (Code::BadPath as i32, message.to_string()), "{path:?}");
    }
    assert!(authz.snapshot().unwrap().exports.is_empty());
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

#[tokio::test]
async fn immediate_missing_path_is_bad_path() {
    use irsync::authz::Authorizer;
    let tmp = tempfile::tempdir().unwrap();
    let client = client_endpoint().await;
    let authz = Arc::new(MemoryAuthorizer::single(client.id(), immediate()));
    let d = spawn_memory(&authz).await;
    let path = bytes(&tmp.path().join("nope"));
    let e = reply(exchange_against(&d, &client, hello(&path, Direction::DaemonSends)).await);
    assert_eq!(
        e,
        (
            Code::BadPath as i32,
            format!(
                "no such file or directory: {}",
                irsync::fs::paths::escape_path(&path)
            )
        )
    );
    assert!(authz.snapshot().unwrap().exports.is_empty());
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

fn export_at(
    path: &std::path::Path,
    auto: bool,
    rw: Vec<String>,
) -> irsync::config::exports::Export {
    irsync::config::exports::Export {
        path: path.to_path_buf(),
        read_only: false,
        warm_index: false,
        fsync: false,
        auto,
        rw,
        ro: Vec::new(),
    }
}

/// `[all]` covers the exports written by hand only: an automatic export
/// belongs to the peer it was made for, so an `[all] rw` key can neither
/// use nor list it.
#[tokio::test]
async fn all_grant_does_not_reach_auto_exports() {
    use irsync_proto::v1::export_entry::Access as A;
    let tmp = tempfile::tempdir().unwrap();
    let (normal, auto) = (tmp.path().join("normal"), tmp.path().join("auto"));
    std::fs::create_dir(&normal).unwrap();
    std::fs::create_dir(&auto).unwrap();
    let client = client_endpoint().await;
    let owner = client_endpoint().await;
    let authz = Arc::new(
        MemoryAuthorizer::single(client.id(), grants(Access::ReadWrite)).with_exports(
            [
                ("normal".to_string(), export_at(&normal, false, Vec::new())),
                (
                    "home-auto".to_string(),
                    export_at(&auto, true, vec![owner.id().to_string()]),
                ),
            ]
            .into(),
        ),
    );
    let d = spawn_memory(&authz).await;
    for path in [
        bytes(&auto),
        bytes(&auto.join("sub")),
        b"home-auto".to_vec(),
    ] {
        for dir in [Direction::ClientSends, Direction::DaemonSends] {
            let e = reply(exchange_against(&d, &client, hello(&path, dir)).await);
            assert_eq!(e, (Code::Unauthorized as i32, no_access(&path)), "{path:?}");
        }
    }
    let r = exchange_against(&d, &client, hello(&bytes(&normal), Direction::ClientSends)).await;
    assert!(matches!(r.unwrap(), HelloAckOrError::Ack(_)));
    assert_eq!(
        list_against(&d, &client).await.unwrap(),
        vec![("normal".to_string(), bytes(&normal), A::Rw as i32)]
    );
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

/// An immediate peer's relative path names another client's export: the
/// short name only counts for exports this peer may use, so the path
/// resolves under the home instead and the shared export is untouched.
#[tokio::test]
async fn immediate_relative_path_does_not_take_another_clients_export() {
    use irsync::authz::Authorizer;
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let (home, shared) = (base.join("home"), base.join("srv/shared/photos"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&shared).unwrap();
    let client = client_endpoint().await;
    let other = client_endpoint().await;
    let authz = Arc::new(
        MemoryAuthorizer::single(client.id(), immediate())
            .with(
                other.id(),
                Grants {
                    exports: [("photos".to_string(), Access::ReadWrite)].into(),
                    ..Grants::default()
                },
            )
            .with_exports(
                [(
                    "photos".to_string(),
                    export_at(&shared, false, vec![other.id().to_string()]),
                )]
                .into(),
            ),
    );
    let d = spawn_daemon_full(
        authz.clone(),
        tempfile::tempdir().unwrap(),
        Some(home.clone()),
    )
    .await;
    let r = exchange_against(&d, &client, hello(b"photos", Direction::ClientSends)).await;
    assert!(matches!(r.unwrap(), HelloAckOrError::Ack(_)));
    assert!(home.join("photos").is_dir());
    let snap = authz.snapshot().unwrap();
    let autos: Vec<_> = snap
        .exports
        .values()
        .filter(|e| e.auto)
        .map(|e| e.path.clone())
        .collect();
    assert_eq!(autos, vec![home.join("photos")]);
    let at_shared = snap.exports.values().filter(|e| e.path == shared).count();
    assert_eq!(at_shared, 1, "{:?}", snap.exports);
    assert!(!snap.grants(&client.id()).exports.contains_key("photos"));
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

/// A push to a new directory whose parent this account may not write
/// says why it failed, as the OS put it, not "no such file".
#[tokio::test]
async fn immediate_push_under_unwritable_parent_says_why() {
    use irsync::authz::Authorizer;
    use std::os::unix::fs::PermissionsExt;
    if nix::unistd::geteuid().is_root() {
        eprintln!("skipped: root may write anywhere");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path().join("srv");
    std::fs::create_dir(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();
    let new = parent.join("photos");
    let client = client_endpoint().await;
    let authz = Arc::new(MemoryAuthorizer::single(client.id(), immediate()));
    let d = spawn_memory(&authz).await;
    let e = reply(exchange_against(&d, &client, hello(&bytes(&new), Direction::ClientSends)).await);
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        e,
        (
            Code::BadPath as i32,
            format!(
                "cannot create {}: Permission denied (os error 13)",
                new.display()
            )
        )
    );
    assert!(authz.snapshot().unwrap().exports.is_empty());
    d.shutdown.cancel();
    let _ = within(d.task).await;
}
