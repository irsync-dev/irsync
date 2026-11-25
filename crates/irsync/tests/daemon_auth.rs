mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use common::{
    client_endpoint, spawn_daemon, spawn_daemon_full, spawn_daemon_home, spawn_daemon_with,
    spawn_daemon_with_state, within, DaemonHandle,
};
use iroh::Endpoint;
use irsync::authz::{Access, FileAuthorizer, MemoryAuthorizer, Permissions};
use irsync::config::paths::Paths;
use irsync::net::frame::{read_body, read_msg, write_msg};
use irsync::options::Direction;
use irsync::session::SyncHello;
use irsync_proto::v1::{
    error::Code, hello as wire_hello, hello_reply, Error, Hello, HelloAck, HelloReply, ListExports,
    SyncRequest,
};
use irsync_proto::{ALPN, PROTOCOL_VERSION};
use prost::Message as _;

fn hello(path: &[u8], dir: Direction) -> SyncHello {
    SyncHello::new(
        dir,
        SyncRequest {
            path: path.to_vec(),
            preserve: Some(Default::default()),
            ..Default::default()
        },
    )
}

/// The Hello of an export listing.
fn listing_hello() -> Hello {
    Hello {
        protocol_version: PROTOCOL_VERSION,
        request: Some(wire_hello::Request::ListExports(ListExports {})),
        ..Default::default()
    }
}

fn grants(access: Access) -> Permissions {
    Permissions {
        all_exports: Some(access),
        ..Permissions::default()
    }
}

/// Sends `h` from an authorised client with `access`; returns the reply bytes' decode attempt.
async fn exchange(access: Access, h: impl Into<Hello>) -> Result<HelloAckOrError, String> {
    exchange_with(grants(access), h).await
}

async fn exchange_with(g: Permissions, h: impl Into<Hello>) -> Result<HelloAckOrError, String> {
    let dir = tempfile::tempdir().unwrap();
    exchange_in(dir.path(), g, h).await
}

/// A fresh temp dir: itself, its file name (home-relative from its parent)
/// and its absolute path, as bytes.
fn export_dir() -> (tempfile::TempDir, Vec<u8>, Vec<u8>) {
    use irsync::platform::os_to_bytes;
    let dir = tempfile::tempdir().unwrap();
    let rel = os_to_bytes(dir.path().file_name().unwrap());
    let abs = os_to_bytes(dir.path().as_os_str());
    (dir, rel, abs)
}

fn no_access(path: &[u8]) -> String {
    format!("no access to {}", irsync::fs::paths::escape_os_path(path))
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
    g: Permissions,
    h: impl Into<Hello>,
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
    h: impl Into<Hello>,
) -> Result<HelloAckOrError, String> {
    let h: Hello = h.into();
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
    let body = within(read_body(&mut recv))
        .await
        .map_err(|e| e.to_string())?
        .ok_or("end of stream before reply")?;
    Ok(match HelloReply::decode(&body[..]).unwrap().reply {
        Some(hello_reply::Reply::Ack(ack)) => HelloAckOrError::Ack(ack),
        Some(hello_reply::Reply::Error(e)) => HelloAckOrError::Err(e),
        r => panic!("not a session's reply: {r:?}"),
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
    closed_before_hello(|id| MemoryAuthorizer::single(id, Permissions::default())).await;
}

async fn closed_before_hello(mk: impl FnOnce(iroh::EndpointId) -> MemoryAuthorizer) {
    let client = client_endpoint().await;
    let dir = tempfile::tempdir().unwrap();
    let authz = mk(client.id());
    let d = spawn_daemon(vec![("rw", dir.path().to_path_buf(), false)], authz).await;
    assert_closed_before_hello(&d, &client).await;
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

/// `d` closes a connection from `client` as `unauthorized` before it
/// reads a Hello.
async fn assert_closed_before_hello(d: &DaemonHandle, client: &Endpoint) {
    let conn = within(client.connect(d.addr.clone(), ALPN)).await.unwrap();
    // The stream may open locally and the write may even succeed; reading must fail.
    let res = within(async {
        let (mut send, mut recv) = conn.open_bi().await?;
        let _ = write_msg(&mut send, &hello(b"rw", Direction::ClientSends).to_hello()).await;
        let _ = send.finish();
        recv.read_to_end(1 << 20)
            .await
            .map(|_| ())
            .map_err(anyhow::Error::from)
    })
    .await;
    assert!(res.is_err(), "unauthorized peer got a clean reply: {res:?}");
    let closed = format!("{:?}", within(conn.closed()).await);
    assert!(closed.contains("unauthorized"), "{closed}");
    assert_eq!(d.hellos.load(Ordering::SeqCst), 0, "daemon parsed a Hello");
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

/// `rw` and `ro` share a directory. To a key granted only `ro` the name
/// `rw` is not an export's: the answer is the one for any name that is no
/// export, so the key cannot tell that an export `rw` exists. It reaches
/// the directory under the name it has a grant for.
#[tokio::test]
async fn an_ungranted_name_is_no_export_even_with_a_granted_twin_on_the_same_path() {
    let g = || Permissions {
        exports: [("ro".to_string(), Access::ReadWrite)].into(),
        ..Permissions::default()
    };
    for name in [&b"rw"[..], b"no-such-export"] {
        for direction in [Direction::DaemonSends, Direction::ClientSends] {
            expect_err(
                exchange_with(g(), hello(name, direction)).await,
                Code::Unauthorized,
            );
        }
    }
    assert!(matches!(
        exchange_with(g(), hello(b"ro", Direction::DaemonSends))
            .await
            .unwrap(),
        HelloAckOrError::Ack(_)
    ));
}

/// A grant on an outer export reaches everything below it, an ungranted
/// inner export included: the reply is the same as for any other path
/// under the outer one, so a key cannot map where other exports sit.
#[tokio::test]
async fn outer_grant_reaches_paths_inside_an_ungranted_inner_export() {
    let tmp = tempfile::tempdir().unwrap();
    let (srv, inner) = (tmp.path().join("srv"), tmp.path().join("srv/photos"));
    std::fs::create_dir_all(&inner).unwrap();
    let client = client_endpoint().await;
    let d = spawn_daemon_home(
        vec![("srv", srv.clone(), false), ("photos", inner.clone(), true)],
        MemoryAuthorizer::single(
            client.id(),
            Permissions {
                exports: [("srv".to_string(), Access::ReadWrite)].into(),
                ..Permissions::default()
            },
        ),
        None,
    )
    .await;
    for path in [
        &irsync::platform::os_to_bytes(inner.as_os_str())[..],
        &b"srv/photos"[..],
        &b"srv/other"[..],
    ] {
        let r = exchange_against(&d, &client, hello(path, Direction::ClientSends)).await;
        assert!(matches!(r.unwrap(), HelloAckOrError::Ack(_)), "{path:?}");
    }
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

/// Whether names in `dir` ignore case: the volume finds `probe` under
/// `PROBE`. The macOS default does; Linux by default does not.
#[cfg(unix)]
fn names_ignore_case(dir: &std::path::Path) -> bool {
    std::fs::create_dir(dir.join("probe")).unwrap();
    dir.join("PROBE").exists()
}

/// On a volume that ignores case, another spelling of an inner export's
/// path names the same directory. The daemon must still apply the inner
/// export's rules: its `read_only`, and a client's read-only access to it,
/// hold whichever way the client spells the path, and a push is refused.
/// An outer export the client may write to must not open a way around
/// them: another spelling is refused, naming the export to ask by.
#[tokio::test]
#[cfg(unix)]
async fn another_spelling_of_an_inner_export_keeps_its_rules() {
    use irsync::platform::testutil::name_bytes;
    let tmp = tempfile::tempdir().unwrap();
    if !names_ignore_case(tmp.path()) {
        eprintln!(
            "skipping: {} keeps case apart, so other spellings are other directories",
            tmp.path().display()
        );
        return;
    }
    let (srv, inner) = (tmp.path().join("srv"), tmp.path().join("srv/ro"));
    std::fs::create_dir_all(&inner).unwrap();
    // The inner export is read-only, and the client may write to both.
    let read_only_export = (
        vec![("srv", srv.clone(), false), ("ro", inner.clone(), true)],
        Permissions {
            exports: [
                ("srv".to_string(), Access::ReadWrite),
                ("ro".to_string(), Access::ReadWrite),
            ]
            .into(),
            ..Permissions::default()
        },
    );
    // The client may only read the inner export, and write to the outer one.
    let read_only_grant = (
        vec![("srv", srv.clone(), false), ("ro", inner.clone(), false)],
        Permissions {
            exports: [
                ("srv".to_string(), Access::ReadWrite),
                ("ro".to_string(), Access::ReadOnly),
            ]
            .into(),
            ..Permissions::default()
        },
    );
    let mut wrong = Vec::new();
    for (case, (exports, grants)) in [
        ("read-only export", read_only_export),
        ("read-only grant", read_only_grant),
    ] {
        let client = client_endpoint().await;
        let d =
            spawn_daemon_home(exports, MemoryAuthorizer::single(client.id(), grants), None).await;
        for spelling in ["ro", "RO", "Ro"] {
            let path = name_bytes(srv.join(spelling).join("x").as_os_str()).to_vec();
            let r = exchange_against(&d, &client, hello(&path, Direction::ClientSends)).await;
            match r.unwrap() {
                // The exact spelling meets the inner export's own rules;
                // another spelling is refused naming the export to ask by.
                HelloAckOrError::Err(e) if spelling == "ro" && e.code == Code::ReadOnly as i32 => {}
                HelloAckOrError::Err(e)
                    if spelling != "ro"
                        && e.code == Code::BadPath as i32
                        && e.message
                            .ends_with("is inside export ro; ask for it by that export") => {}
                HelloAckOrError::Err(e) => wrong.push(format!(
                    "{case}, srv/{spelling}/x: {:?}: {}",
                    e.code, e.message
                )),
                HelloAckOrError::Ack(_) => {
                    wrong.push(format!("{case}, srv/{spelling}/x: push accepted"))
                }
            }
        }
        d.shutdown.cancel();
        let _ = within(d.task).await;
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// On Windows, where the daemon also checks by identity that a request
/// does not pass through another export's directory, that check knows only
/// the exports the key has a grant for: through the outer export the key
/// reaches the ungranted inner one's directory in any spelling, and no
/// reply names the inner export.
#[tokio::test]
#[cfg(windows)]
async fn the_identity_check_knows_only_granted_exports() {
    let tmp = tempfile::tempdir().unwrap();
    let (srv, inner) = (
        tmp.path().join("srv"),
        tmp.path().join("srv").join("photos"),
    );
    std::fs::create_dir_all(inner.join("d")).unwrap();
    std::fs::write(inner.join("f"), b"f").unwrap();
    let client = client_endpoint().await;
    let d = spawn_daemon_home(
        vec![
            ("srv", srv.clone(), false),
            ("hidden-name", inner.clone(), true),
        ],
        MemoryAuthorizer::single(
            client.id(),
            Permissions {
                exports: [("srv".to_string(), Access::ReadWrite)].into(),
                ..Permissions::default()
            },
        ),
        None,
    )
    .await;
    for path in [
        &b"srv/photos"[..],
        b"srv/PHOTOS",
        b"srv/Photos/d",
        b"srv/photos/new",
    ] {
        for dir in [Direction::ClientSends, Direction::DaemonSends] {
            match exchange_against(&d, &client, hello(path, dir))
                .await
                .unwrap()
            {
                HelloAckOrError::Ack(_) => {}
                HelloAckOrError::Err(e) => {
                    assert!(
                        !e.message.contains("hidden-name"),
                        "{path:?}: {}",
                        e.message
                    );
                    // Only a pull of a path that is not there may fail.
                    assert!(
                        dir == Direction::DaemonSends && path.ends_with(b"new"),
                        "{path:?} {dir:?}: {}",
                        e.message
                    );
                }
            }
        }
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
    let mut h: Hello = hello(b"rw", Direction::ClientSends).into();
    h.protocol_version = PROTOCOL_VERSION + 1;
    expect_err(exchange(Access::ReadWrite, h).await, Code::VersionMismatch);
}

#[tokio::test]
async fn wrong_chunk_params_rejected() {
    let mut h = hello(b"rw", Direction::ClientSends);
    h.req.chunk_avg += 1;
    expect_err(exchange(Access::ReadWrite, h).await, Code::VersionMismatch);
}

/// A push or a pull must say what to preserve; a probe need not.
#[tokio::test]
async fn hello_without_preserve_flags_rejected() {
    for dir in [Direction::ClientSends, Direction::DaemonSends] {
        let mut h = hello(b"rw", dir);
        h.req.preserve = None;
        expect_err(exchange(Access::ReadWrite, h).await, Code::VersionMismatch);
    }
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
    irsync::fs::private::write(&exports, &grant).unwrap();
    std::fs::create_dir_all(d.path().join("data")).unwrap();
    assert!(matches!(
        exchange_against(&daemon, &client, h.clone()).await,
        Ok(HelloAckOrError::Ack(_))
    ));
    irsync::fs::private::write(&exports, "[x\n").unwrap(); // broken: fail closed, daemon alive
    assert!(exchange_against(&daemon, &client, h.clone()).await.is_err());
    irsync::fs::private::write(&exports, &grant).unwrap(); // fixed again: still serving
    assert!(matches!(
        exchange_against(&daemon, &client, h.clone()).await,
        Ok(HelloAckOrError::Ack(_))
    ));
    irsync::fs::private::write(
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
    irsync::fs::private::write(
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
    let none = Permissions {
        exports: [("other".to_string(), Access::ReadWrite)].into(),
        ..Permissions::default()
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
    h.req.name = b"photos".to_vec();
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
    let h = listing_hello();
    let conn = within(client.connect(d.addr.clone(), ALPN))
        .await
        .map_err(|e| e.to_string())?;
    let (mut send, mut recv) = within(conn.open_bi()).await.map_err(|e| e.to_string())?;
    within(write_msg(&mut send, &h))
        .await
        .map_err(|e| e.to_string())?;
    send.finish().map_err(|e| e.to_string())?;
    let reply: HelloReply = within(read_msg(&mut recv))
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no reply")?;
    let Some(hello_reply::Reply::Exports(list)) = reply.reply else {
        return Err(format!("not an export list: {reply:?}"));
    };
    Ok(list
        .exports
        .into_iter()
        .map(|e| (e.name, e.path, e.access))
        .collect())
}

#[tokio::test]
async fn list_exports_names_only_what_the_key_may_use() {
    use irsync_proto::v1::export_entry::Access as A;
    let (dir, ..) = export_dir();
    let path = irsync::platform::os_to_bytes(dir.path().as_os_str());
    let client = client_endpoint().await;
    let spawn = |g: Permissions| {
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
    let d = spawn(Permissions {
        exports: [("rw".to_string(), Access::ReadWrite)].into(),
        ..Permissions::default()
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
    let d = spawn(Permissions::default()).await;
    assert!(list_against(&d, &client).await.is_err());
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

/// A refused listing reaches the client as a refusal, not as an empty
/// list: the reply's oneof says which it is.
#[tokio::test]
async fn a_refused_listing_reaches_the_client_as_a_refusal() {
    use irsync::session::engine::{list_exports, SessionRefused};
    let (dir, ..) = export_dir();
    let client = client_endpoint().await;
    let d = spawn_daemon_home(
        vec![("rw", dir.path().to_path_buf(), false)],
        MemoryAuthorizer::single(client.id(), grants(Access::ReadWrite)),
        None,
    )
    .await;
    let conn = within(client.connect(d.addr.clone(), ALPN)).await.unwrap();
    let mut h = listing_hello();
    h.protocol_version = PROTOCOL_VERSION + 1;
    let err = within(list_exports(&conn, h)).await.unwrap_err();
    let refused = err
        .downcast_ref::<SessionRefused>()
        .unwrap_or_else(|| panic!("not a refusal: {err:#}"));
    assert_eq!(refused.code, Code::VersionMismatch, "{}", refused.message);
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

fn all_paths() -> Permissions {
    Permissions {
        all_paths: Some(Access::ReadWrite),
        ..Permissions::default()
    }
}

fn bytes(p: &std::path::Path) -> Vec<u8> {
    irsync::platform::os_to_bytes(p.as_os_str())
}

/// A daemon with no home whose exports and grants are `authz`'s, kept
/// so the test can look at what the daemon added.
async fn spawn_memory(authz: &Arc<MemoryAuthorizer>) -> DaemonHandle {
    spawn_daemon_full(authz.clone(), tempfile::tempdir().unwrap(), None).await
}

#[tokio::test]
async fn all_paths_push_creates_auto_export() {
    use irsync::authz::Authorizer;
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("dest");
    std::fs::create_dir(&dest).unwrap();
    let client = client_endpoint().await;
    let authz = Arc::new(MemoryAuthorizer::single(client.id(), all_paths()));
    let d = spawn_memory(&authz).await;
    let r = exchange_against(&d, &client, hello(&bytes(&dest), Direction::ClientSends)).await;
    assert!(matches!(r.unwrap(), HelloAckOrError::Ack(_)));
    let snap = authz.snapshot().unwrap();
    assert_eq!(snap.exports.len(), 1, "{:?}", snap.exports);
    let (name, e) = snap.exports.iter().next().unwrap();
    assert!(e.auto);
    // The daemon records the real path (macOS: /var is a link to /private/var).
    assert_eq!(e.path, dest.canonicalize().unwrap());
    assert_eq!(e.rw, vec![client.id().to_string()]);
    assert_eq!(snap.access(&client.id(), name), Some(Access::ReadWrite));
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

#[tokio::test]
async fn all_paths_reuses_containing_auto_export() {
    use irsync::authz::Authorizer;
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("dest");
    std::fs::create_dir(&dest).unwrap();
    let client = client_endpoint().await;
    let authz = Arc::new(MemoryAuthorizer::single(client.id(), all_paths()));
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
async fn all_paths_off_rejects_unexported_path() {
    use irsync::authz::Authorizer;
    let tmp = tempfile::tempdir().unwrap();
    let (exported, elsewhere) = (tmp.path().join("exported"), tmp.path().join("elsewhere"));
    std::fs::create_dir(&exported).unwrap();
    std::fs::create_dir(&elsewhere).unwrap();
    let client = client_endpoint().await;
    let g = Permissions {
        exports: [("other".to_string(), Access::ReadWrite)].into(),
        ..Permissions::default()
    };
    let export = irsync::config::exports::Export {
        path: exported,
        read_only: false,
        index_at_start: false,
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
async fn all_paths_push_to_missing_dir_creates_and_exports_it() {
    use irsync::authz::Authorizer;
    let tmp = tempfile::tempdir().unwrap();
    let new = tmp.path().join("new");
    let client = client_endpoint().await;
    let authz = Arc::new(MemoryAuthorizer::single(client.id(), all_paths()));
    let d = spawn_memory(&authz).await;
    let r = exchange_against(&d, &client, hello(&bytes(&new), Direction::ClientSends)).await;
    assert!(matches!(r.unwrap(), HelloAckOrError::Ack(_)));
    assert!(new.is_dir());
    let snap = authz.snapshot().unwrap();
    let paths: Vec<_> = snap.exports.values().map(|e| e.path.clone()).collect();
    assert_eq!(paths, vec![new.canonicalize().unwrap()]);
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

#[tokio::test]
async fn all_paths_refuses_root_and_non_utf8_roots() {
    use irsync::authz::Authorizer;
    let tmp = tempfile::tempdir().unwrap();
    let mut bad = bytes(tmp.path());
    // A name that is not UTF-8: any bytes on Unix; on Windows, where names
    // are UTF-16 and stored as WTF-8, an unpaired surrogate (U+D800).
    bad.extend_from_slice(if cfg!(windows) {
        &b"/caf\xed\xa0\x80"[..]
    } else {
        &b"/caf\xe9"[..]
    });
    let client = client_endpoint().await;
    let authz = Arc::new(MemoryAuthorizer::single(client.id(), all_paths()));
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
async fn all_paths_missing_path_is_bad_path() {
    use irsync::authz::Authorizer;
    let tmp = tempfile::tempdir().unwrap();
    let client = client_endpoint().await;
    let authz = Arc::new(MemoryAuthorizer::single(client.id(), all_paths()));
    let d = spawn_memory(&authz).await;
    let path = bytes(&tmp.path().join("nope"));
    let e = reply(exchange_against(&d, &client, hello(&path, Direction::DaemonSends)).await);
    assert_eq!(
        e,
        (
            Code::BadPath as i32,
            format!(
                "no such file or directory: {}",
                irsync::fs::paths::escape_os_path(&path)
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
        index_at_start: false,
        fsync: false,
        auto,
        rw,
        ro: Vec::new(),
    }
}

/// `all_exports` covers the exports written by hand only: an automatic
/// export belongs to the client it was made for, so a client with
/// `all_exports = "rw"` can neither use nor list it.
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

/// An `all_paths` client's relative path names another client's export: the
/// short name only counts for exports this client may use, so the path
/// resolves under the home instead and the shared export is untouched.
#[tokio::test]
async fn all_paths_relative_path_does_not_take_another_clients_export() {
    use irsync::authz::Authorizer;
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let (home, shared) = (base.join("home"), base.join("srv/shared/photos"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&shared).unwrap();
    let client = client_endpoint().await;
    let other = client_endpoint().await;
    let authz = Arc::new(
        MemoryAuthorizer::single(client.id(), all_paths())
            .with(
                other.id(),
                Permissions {
                    exports: [("photos".to_string(), Access::ReadWrite)].into(),
                    ..Permissions::default()
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
    assert!(!snap
        .permissions(&client.id())
        .exports
        .contains_key("photos"));
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

/// A push to a new directory whose parent this account may not write
/// says why it failed, as the OS put it, not "no such file".
#[tokio::test]
#[cfg(unix)]
async fn all_paths_push_under_unwritable_parent_says_why() {
    use irsync::authz::Authorizer;
    use irsync::platform::testutil::set_mode;
    if irsync::platform::is_root() {
        eprintln!("skipped: root may write anywhere");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path().join("srv");
    std::fs::create_dir(&parent).unwrap();
    set_mode(&parent, 0o555).unwrap();
    let new = parent.join("photos");
    let client = client_endpoint().await;
    let authz = Arc::new(MemoryAuthorizer::single(client.id(), all_paths()));
    let d = spawn_memory(&authz).await;
    let e = reply(exchange_against(&d, &client, hello(&bytes(&new), Direction::ClientSends)).await);
    set_mode(&parent, 0o755).unwrap();
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

/// A daemon whose exports and grants come from `dir/irsync.toml`, written
/// as `config` after a `state_dir` in `dir` (so saving the file looks for
/// a daemon there, not in the default state folder), with home `home`.
async fn spawn_from_file(
    dir: &std::path::Path,
    home: &std::path::Path,
    config: &str,
) -> (DaemonHandle, std::path::PathBuf) {
    let path = dir.join("irsync.toml");
    let state = dir.join("state").display().to_string();
    irsync::fs::private::write(&path, format!("state_dir = {state:?}\n{config}")).unwrap();
    let authz = Arc::new(FileAuthorizer::new(Paths::resolve(Some(&path))));
    let d = spawn_daemon_full(
        authz,
        tempfile::tempdir().unwrap(),
        Some(home.to_path_buf()),
    )
    .await;
    (d, path)
}

/// The table of a client `laptop` with `id` and the lines `rights`.
fn laptop(id: iroh::EndpointId, rights: &str) -> String {
    format!("[clients.laptop]\nclient_id = \"{id}\"\n{rights}")
}

/// A home directory in a fresh temp dir, by its real path (macOS: /var is
/// a link to /private/var).
fn temp_home() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(tmp.path()).unwrap().join("home");
    std::fs::create_dir(&home).unwrap();
    (tmp, home)
}

fn read_only_reply() -> (i32, String) {
    (
        Code::ReadOnly as i32,
        "export is read-only for this key".to_string(),
    )
}

#[tokio::test]
async fn all_paths_ro_pulls_any_path_and_makes_a_ro_export() {
    use irsync::authz::Authorizer;
    let (tmp, home) = temp_home();
    std::fs::create_dir(home.join("data")).unwrap();
    let client = client_endpoint().await;
    let (d, config) = spawn_from_file(
        tmp.path(),
        &home,
        &laptop(client.id(), "all_paths = \"ro\"\n"),
    )
    .await;
    let r = exchange_against(&d, &client, hello(b"~/data", Direction::DaemonSends)).await;
    assert!(matches!(r.unwrap(), HelloAckOrError::Ack(_)));
    let snap = FileAuthorizer::new(Paths::resolve(Some(&config)))
        .snapshot()
        .unwrap();
    let autos: Vec<_> = snap.exports.values().filter(|e| e.auto).collect();
    assert_eq!(autos.len(), 1, "{:?}", snap.exports);
    assert_eq!(autos[0].path, home.join("data"));
    assert_eq!(autos[0].ro, ["laptop"]);
    assert!(autos[0].rw.is_empty(), "{:?}", autos[0]);
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

/// A push by a read-only client is refused before the daemon makes the
/// directory or the export.
#[tokio::test]
async fn all_paths_ro_cannot_push_and_creates_nothing() {
    let (tmp, home) = temp_home();
    let client = client_endpoint().await;
    let (d, config) = spawn_from_file(
        tmp.path(),
        &home,
        &laptop(client.id(), "all_paths = \"ro\"\n"),
    )
    .await;
    let before = std::fs::read(&config).unwrap();
    let e = reply(exchange_against(&d, &client, hello(b"~/new", Direction::ClientSends)).await);
    assert_eq!(e, read_only_reply());
    assert!(!home.join("new").exists(), "the push target was created");
    assert_eq!(
        std::fs::read(&config).unwrap(),
        before,
        "the config changed"
    );
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

/// An automatic export made while the client was `rw` gives it `ro` once
/// its `all_paths` is lowered, without a restart.
#[tokio::test]
async fn lowering_all_paths_takes_effect_on_existing_auto_exports() {
    let (tmp, home) = temp_home();
    let client = client_endpoint().await;
    let (d, config) = spawn_from_file(
        tmp.path(),
        &home,
        &laptop(client.id(), "all_paths = \"rw\"\n"),
    )
    .await;
    let push = hello(b"~/dest", Direction::ClientSends);
    let r = exchange_against(&d, &client, push.clone()).await;
    assert!(matches!(r.unwrap(), HelloAckOrError::Ack(_)));
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(text.contains("auto = true"), "{text}");
    let lowered = text.replace("all_paths = \"rw\"", "all_paths = \"ro\"");
    assert_ne!(lowered, text);
    irsync::fs::private::write(&config, &lowered).unwrap();
    let e = reply(exchange_against(&d, &client, push).await);
    assert_eq!(e, read_only_reply());
    let r = exchange_against(&d, &client, hello(b"~/dest", Direction::DaemonSends)).await;
    assert!(
        matches!(r.unwrap(), HelloAckOrError::Ack(_)),
        "a pull still works"
    );
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

#[tokio::test]
async fn all_paths_alone_is_refused_when_not_allowed() {
    let (tmp, home) = temp_home();
    let client = client_endpoint().await;
    let (d, _config) = spawn_from_file(
        tmp.path(),
        &home,
        &format!(
            "[daemon]\nallow_all_paths = false\n\n{}",
            laptop(client.id(), "all_paths = \"rw\"\n")
        ),
    )
    .await;
    assert_closed_before_hello(&d, &client).await;
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

/// `admin` alone is a right: the client is not closed before the Hello.
#[tokio::test]
async fn a_client_with_only_admin_passes_the_gate() {
    let (dir, ..) = export_dir();
    let client = client_endpoint().await;
    let only_admin = Permissions {
        admin: Some(Access::ReadOnly),
        ..Permissions::default()
    };
    let d = spawn_daemon_home(
        vec![("rw", dir.path().to_path_buf(), false)],
        MemoryAuthorizer::single(client.id(), only_admin),
        None,
    )
    .await;
    assert_eq!(list_against(&d, &client).await.unwrap(), vec![]);
    d.shutdown.cancel();
    let _ = within(d.task).await;
    // No rights at all: still closed.
    let d = spawn_daemon_home(
        vec![("rw", dir.path().to_path_buf(), false)],
        MemoryAuthorizer::single(client.id(), Permissions::default()),
        None,
    )
    .await;
    assert!(list_against(&d, &client).await.is_err());
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

// The daemon's last answer on a connection (#298). Closing a connection
// throws away what has not reached the client yet, so after its last answer
// the daemon waits for the client to close, up to `ANSWER_WAIT`. With this
// QUIC library, data that has arrived stays readable after the close: the
// two late-read tests pass either way, and are there in case that changes.

/// Sends `h` over a fresh connection, then waits `delay` before reading the
/// reply, as a slow client would. Closes the connection once it has read it.
async fn exchange_read_late(
    d: &DaemonHandle,
    client: &Endpoint,
    h: Hello,
    delay: std::time::Duration,
) -> Result<HelloReply, String> {
    let conn = within(client.connect(d.addr.clone(), ALPN))
        .await
        .map_err(|e| e.to_string())?;
    let (mut send, mut recv) = within(conn.open_bi()).await.map_err(|e| e.to_string())?;
    within(write_msg(&mut send, &h))
        .await
        .map_err(|e| e.to_string())?;
    send.finish().map_err(|e| e.to_string())?;
    tokio::time::sleep(delay).await;
    let body = within(read_body(&mut recv))
        .await
        .map_err(|e| e.to_string())?
        .ok_or("end of stream before reply")?;
    conn.close(0u32.into(), b"done");
    Ok(HelloReply::decode(&body[..]).unwrap())
}

#[tokio::test]
async fn a_client_that_reads_late_still_gets_the_refusal() {
    let client = client_endpoint().await;
    let dir = tempfile::tempdir().unwrap();
    let d = spawn_daemon(
        vec![("rw", dir.path().to_path_buf(), false)],
        MemoryAuthorizer::single(client.id(), grants(Access::ReadWrite)),
    )
    .await;
    let h = hello(b"/no/such", Direction::ClientSends).to_hello();
    let late = std::time::Duration::from_millis(500);
    let got = exchange_read_late(&d, &client, h, late).await;
    match got.map(|r| r.reply) {
        Ok(Some(hello_reply::Reply::Error(e))) => {
            assert_eq!(e.code, Code::Unauthorized as i32, "{}", e.message);
            assert_eq!(e.message, no_access(b"/no/such"));
        }
        other => panic!("expected the refusal, got {other:?}"),
    }
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

#[tokio::test]
async fn a_client_that_reads_late_still_gets_the_listing() {
    let client = client_endpoint().await;
    let dir = tempfile::tempdir().unwrap();
    let d = spawn_daemon(
        vec![("rw", dir.path().to_path_buf(), false)],
        MemoryAuthorizer::single(client.id(), grants(Access::ReadWrite)),
    )
    .await;
    let late = std::time::Duration::from_millis(500);
    let got = exchange_read_late(&d, &client, listing_hello(), late).await;
    match got.map(|r| r.reply) {
        Ok(Some(hello_reply::Reply::Exports(list))) => {
            let names: Vec<_> = list.exports.iter().map(|e| e.name.as_str()).collect();
            assert_eq!(names, ["rw"]);
        }
        other => panic!("expected the listing, got {other:?}"),
    }
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

/// The wait ends as soon as the client closes: a refused sync, daemon side
/// included, takes nowhere near the bound.
#[tokio::test]
async fn a_refused_sync_ends_promptly() {
    let h = common::Harness::new().await;
    let refused = hello(b"nope/", Direction::ClientSends);
    let t = std::time::Instant::now();
    let (out, _) = h
        .run_reconnect_hello(
            &common::archive_opts(),
            refused,
            irsync::session::reconnect::ReconnectOpts::default(),
            h.local_side(),
        )
        .await;
    let took = t.elapsed();
    assert!(out.is_err(), "a sync of an unknown export was not refused");
    assert!(
        took < std::time::Duration::from_secs(3),
        "the refused sync took {took:?}: the daemon waited out its bound"
    );
}

/// A client that reads the refusal and never closes: the daemon keeps the
/// connection open for it up to `ANSWER_WAIT`, then closes it itself. On
/// tokio's paused clock, through the in-process fault layer.
#[cfg(feature = "netsim")]
#[tokio::test(start_paused = true)]
async fn the_daemon_waits_for_the_client_to_close_up_to_a_bound() {
    use irsync::net::netsim::ProfileName;
    use irsync::net::ANSWER_WAIT;
    common::with_netsim(ProfileName::TestWan, 1, async {
        let client = client_endpoint().await;
        let dir = tempfile::tempdir().unwrap();
        let d = spawn_daemon(
            vec![("rw", dir.path().to_path_buf(), false)],
            MemoryAuthorizer::single(client.id(), grants(Access::ReadWrite)),
        )
        .await;
        let conn = client.connect(d.addr.clone(), ALPN).await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        let h = hello(b"/no/such", Direction::ClientSends).to_hello();
        write_msg(&mut send, &h).await.unwrap();
        send.finish().unwrap();
        let reply: HelloReply = read_msg(&mut recv).await.unwrap().expect("a reply");
        assert!(
            matches!(reply.reply, Some(hello_reply::Reply::Error(_))),
            "{reply:?}"
        );
        let start = tokio::time::Instant::now();
        let closed = tokio::time::timeout(ANSWER_WAIT * 2, conn.closed())
            .await
            .expect("the daemon never closed the connection");
        let open = start.elapsed();
        assert!(format!("{closed:?}").contains("rejected"), "{closed:?}");
        assert!(
            open >= ANSWER_WAIT - std::time::Duration::from_secs(1),
            "closed after {open:?}"
        );
        assert!(
            open < ANSWER_WAIT + std::time::Duration::from_secs(5),
            "closed after {open:?}"
        );
        d.shutdown.cancel();
        let _ = d.task.await;
    })
    .await;
}

/// A client with `all_paths` that spells an export's path in another case
/// gets that export's rules, not an automatic export over the same
/// directory: a push into a read-only export stays refused, and nothing is
/// added.
#[tokio::test]
#[cfg(unix)]
async fn all_paths_in_another_spelling_keeps_an_exports_rules() {
    use irsync::authz::Authorizer;
    use irsync::config::exports::Export;
    let tmp = tempfile::tempdir().unwrap();
    if !names_ignore_case(tmp.path()) {
        eprintln!(
            "skipping: {} keeps case apart, so other spellings are other directories",
            tmp.path().display()
        );
        return;
    }
    let ro = tmp.path().join("srv/ro");
    std::fs::create_dir_all(&ro).unwrap();
    let client = client_endpoint().await;
    let export = Export {
        path: ro.clone(),
        read_only: true,
        index_at_start: false,
        fsync: false,
        auto: false,
        rw: Vec::new(),
        ro: Vec::new(),
    };
    let mut g = all_paths();
    g.exports = [("ro".to_string(), Access::ReadWrite)].into();
    let authz = Arc::new(
        MemoryAuthorizer::single(client.id(), g).with_exports([("ro".to_string(), export)].into()),
    );
    let d = spawn_memory(&authz).await;
    let mut wrong = Vec::new();
    for spelling in ["srv/ro/x", "srv/RO/x", "SRV/Ro", "srv/RO"] {
        let path = bytes(&tmp.path().join(spelling));
        let r = exchange_against(&d, &client, hello(&path, Direction::ClientSends)).await;
        match r.unwrap() {
            HelloAckOrError::Err(e)
                if e.code == Code::ReadOnly as i32
                    || (e.code == Code::BadPath as i32
                        && e.message
                            .ends_with("is inside export ro; ask for it by that export")) => {}
            HelloAckOrError::Err(e) => {
                wrong.push(format!("{spelling}: {:?}: {}", e.code, e.message))
            }
            HelloAckOrError::Ack(_) => wrong.push(format!("{spelling}: push accepted")),
        }
    }
    let snap = authz.snapshot().unwrap();
    assert_eq!(snap.exports.len(), 1, "{:?}", snap.exports);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

/// Below a directory that keeps case apart, another case is another name:
/// an inner export whose root is not made yet does not refuse a push to a
/// name that differs from its root's only in case, which is another
/// directory there. The exact name still meets the inner export's rules.
#[tokio::test]
#[cfg(unix)]
async fn another_case_below_a_case_sensitive_directory_is_another_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let srv = tmp.path().join("srv");
    std::fs::create_dir(&srv).unwrap();
    let said = irsync::platform::Dir::open_root(&srv)
        .unwrap()
        .ignores_case();
    if said != Some(false) {
        eprintln!(
            "skipping: {} is not known to keep case apart ({said:?})",
            srv.display()
        );
        return;
    }
    let client = client_endpoint().await;
    let d = spawn_daemon_home(
        vec![("srv", srv.clone(), false), ("ro", srv.join("ro"), true)],
        MemoryAuthorizer::single(
            client.id(),
            Permissions {
                exports: [
                    ("srv".to_string(), Access::ReadWrite),
                    ("ro".to_string(), Access::ReadWrite),
                ]
                .into(),
                ..Permissions::default()
            },
        ),
        None,
    )
    .await;
    let push = |p: &str| hello(&bytes(&srv.join(p)), Direction::ClientSends);
    let r = exchange_against(&d, &client, push("RO/x")).await;
    assert!(matches!(r.unwrap(), HelloAckOrError::Ack(_)), "srv/RO/x");
    expect_err(
        exchange_against(&d, &client, push("ro/x")).await,
        Code::ReadOnly,
    );
    d.shutdown.cancel();
    let _ = within(d.task).await;
}
