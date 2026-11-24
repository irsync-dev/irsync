//! `pq_key_exchange = "require"`: a daemon or a client refuses a connection
//! that did not get the post-quantum hybrid key exchange.
mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use common::{classical_endpoint, client_endpoint, spawn_daemon_settings, within, DaemonHandle};
use iroh::endpoint::{Connection, ConnectionError, ZeroRttStatus};
use iroh::Endpoint;
use irsync::authz::{Access, Grants, MemoryAuthorizer};
use irsync::config::settings::{DaemonSettings, PqKeyExchange};
use irsync::session::engine::list_exports;
use irsync::session::reconnect::{check_pq_key_exchange, probe_path, ReconnectOpts};
use irsync_proto::chunk::CHUNK_PARAMS;
use irsync_proto::v1::{hello::Direction, Hello};
use irsync_proto::{ALPN, PROTOCOL_VERSION};

/// A daemon that grants `client` everything, with `pq_key_exchange` as given.
async fn daemon(client: iroh::EndpointId, pq_key_exchange: PqKeyExchange) -> DaemonHandle {
    let grants = Grants {
        all_exports: Some(Access::ReadWrite),
        ..Grants::default()
    };
    spawn_daemon_settings(
        Arc::new(MemoryAuthorizer::single(client, grants)),
        tempfile::tempdir().unwrap(),
        None,
        DaemonSettings {
            pq_key_exchange,
            ..DaemonSettings::default()
        },
    )
    .await
}

async fn stop(d: DaemonHandle) {
    d.shutdown.cancel();
    let _ = within(d.task).await;
}

/// The daemon closes a granted client that has no hybrid, before it reads
/// a Hello, with the final "rejected" code and a reason that says what to do.
#[tokio::test(flavor = "multi_thread")]
async fn a_requiring_daemon_closes_a_client_without_the_hybrid() {
    let old = classical_endpoint().await;
    let d = daemon(old.id(), PqKeyExchange::Require).await;
    let conn = within(old.connect(d.addr.clone(), ALPN)).await.unwrap();
    let why = within(conn.closed()).await;
    let ConnectionError::ApplicationClosed(close) = why else {
        panic!("not closed by the daemon: {why:?}");
    };
    assert_eq!(close.error_code, 2u32.into());
    assert_eq!(
        String::from_utf8_lossy(&close.reason),
        "this daemon requires post-quantum key exchange; upgrade irsync on the client"
    );
    assert_eq!(d.hellos.load(Ordering::SeqCst), 0, "daemon parsed a Hello");
    stop(d).await;
}

/// Asks for the list of exports, as `irsync list HOST:` does.
async fn hello_is_answered(conn: &iroh::endpoint::Connection) -> bool {
    let hello = Hello {
        protocol_version: PROTOCOL_VERSION,
        direction: Direction::ListExports as i32,
        chunk_min: CHUNK_PARAMS.min,
        chunk_avg: CHUNK_PARAMS.avg,
        chunk_max: CHUNK_PARAMS.max,
        ..Hello::default()
    };
    match within(list_exports(conn, hello)).await {
        Ok(_) => true,
        Err(e) => panic!("the daemon did not answer: {e:#}"),
    }
}

/// Without the setting the same client is served, and a requiring daemon
/// serves a client that has the hybrid.
#[tokio::test(flavor = "multi_thread")]
async fn prefer_serves_everyone_and_require_serves_the_hybrid() {
    let old = classical_endpoint().await;
    let d = daemon(old.id(), PqKeyExchange::Prefer).await;
    let conn = within(old.connect(d.addr.clone(), ALPN)).await.unwrap();
    assert!(hello_is_answered(&conn).await);
    conn.close(0u32.into(), b"done");
    stop(d).await;

    let new = client_endpoint().await;
    let d = daemon(new.id(), PqKeyExchange::Require).await;
    let conn = within(new.connect(d.addr.clone(), ALPN)).await.unwrap();
    assert!(hello_is_answered(&conn).await);
    conn.close(0u32.into(), b"done");
    stop(d).await;
}

/// Dials `d` from `ep`, which has been there before, so that the dial
/// resumes the earlier TLS session. `into_0rtt` succeeds only with a
/// session ticket from that daemon in hand, which is what makes the
/// handshake a resumption. No data is sent before it completes.
async fn resume(ep: &Endpoint, d: &DaemonHandle) -> Connection {
    let connecting = within(ep.connect_with_opts(d.addr.clone(), ALPN, Default::default()))
        .await
        .unwrap();
    let Ok(resuming) = connecting.into_0rtt() else {
        panic!("no session ticket from the first connection: this dial would not resume");
    };
    match within(resuming.handshake_completed()).await.unwrap() {
        ZeroRttStatus::Accepted(conn) | ZeroRttStatus::Rejected(conn) => conn,
    }
}

/// A client that resumes its session with a requiring daemon gets a fresh
/// hybrid key exchange (TLS 1.3 does one on resumption too), and is served
/// as on its first connection.
#[tokio::test(flavor = "multi_thread")]
async fn a_resumed_connection_gets_the_hybrid_and_is_served() {
    let new = client_endpoint().await;
    let d = daemon(new.id(), PqKeyExchange::Require).await;
    let conn = within(new.connect(d.addr.clone(), ALPN)).await.unwrap();
    assert!(hello_is_answered(&conn).await);
    conn.close(0u32.into(), b"done");

    let conn = resume(&new, &d).await;
    assert_eq!(
        irsync::net::key_exchange(&conn).as_deref(),
        Some("X25519MLKEM768")
    );
    assert!(hello_is_answered(&conn).await);
    conn.close(0u32.into(), b"done");
    stop(d).await;
}

/// A classical client is refused on a resumed connection as on its first:
/// resuming does not skip the check.
#[tokio::test(flavor = "multi_thread")]
async fn a_requiring_daemon_closes_a_resumed_client_without_the_hybrid() {
    let old = classical_endpoint().await;
    let d = daemon(old.id(), PqKeyExchange::Require).await;
    for attempt in ["first", "resumed"] {
        let conn = match attempt {
            "first" => within(old.connect(d.addr.clone(), ALPN)).await.unwrap(),
            _ => resume(&old, &d).await,
        };
        let why = within(conn.closed()).await;
        let ConnectionError::ApplicationClosed(close) = why else {
            panic!("{attempt}: not closed by the daemon: {why:?}");
        };
        assert_eq!(close.error_code, 2u32.into(), "{attempt}");
    }
    assert_eq!(d.hellos.load(Ordering::SeqCst), 0, "daemon parsed a Hello");
    stop(d).await;
}

/// A stranger learns nothing about the setting: it is turned away as
/// unauthorized, as from any daemon.
#[tokio::test(flavor = "multi_thread")]
async fn a_requiring_daemon_tells_a_stranger_nothing() {
    let (old, granted) = (classical_endpoint().await, client_endpoint().await);
    let d = daemon(granted.id(), PqKeyExchange::Require).await;
    let conn = within(old.connect(d.addr.clone(), ALPN)).await.unwrap();
    let why = within(conn.closed()).await;
    let ConnectionError::ApplicationClosed(close) = why else {
        panic!("not closed by the daemon: {why:?}");
    };
    assert_eq!(close.error_code, 1u32.into());
    assert_eq!(&close.reason[..], b"unauthorized");
    stop(d).await;
}

/// The client's check: nothing with `prefer` or with the hybrid; with
/// `require` and a classical daemon it closes the connection and says why.
#[tokio::test(flavor = "multi_thread")]
async fn a_requiring_client_refuses_a_daemon_without_the_hybrid() {
    let (old, new, ours) = (
        classical_endpoint().await,
        common::loopback_endpoint().await,
        common::loopback_endpoint().await,
    );
    let (conn, _server) = common::connect_pair(&ours, &new).await;
    check_pq_key_exchange(&conn, true, &new.id()).unwrap();

    let (conn, server) = common::connect_pair(&ours, &old).await;
    check_pq_key_exchange(&conn, false, &old.id()).unwrap();
    let e = check_pq_key_exchange(&conn, true, &old.id()).unwrap_err();
    let msg = e.to_string();
    assert!(
        msg.contains("does not offer post-quantum key exchange")
            && msg.contains("X25519")
            && msg.contains("--pq-key-exchange prefer"),
        "{msg}"
    );
    // Closed before any stream was opened.
    let why = within(server.closed()).await;
    assert!(
        matches!(why, ConnectionError::ApplicationClosed(_)),
        "{why:?}"
    );
}

/// A dial that is refused for the key exchange is final: it is not retried,
/// and the caller gets the reason.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_key_exchange_is_not_retried() {
    let (old, ours) = (
        classical_endpoint().await,
        common::loopback_endpoint().await,
    );
    // Counts the dials that reach the old daemon.
    let dials = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let listener = tokio::spawn({
        let (old, dials) = (old.clone(), dials.clone());
        async move {
            while let Some(incoming) = old.accept().await {
                dials.fetch_add(1, Ordering::SeqCst);
                if let Ok(conn) = incoming.await {
                    conn.closed().await;
                }
            }
        }
    });
    let opts = ReconnectOpts {
        require_pq: true,
        ..ReconnectOpts::default()
    };
    let e = within(probe_path(
        &ours,
        common::addr_of(&old),
        b"x",
        "old:x",
        opts,
        None,
    ))
    .await
    .unwrap_err();
    assert!(
        format!("{e:#}").contains("does not offer post-quantum key exchange"),
        "{e:#}"
    );
    assert_eq!(dials.load(Ordering::SeqCst), 1);
    listener.abort();
}

/// The real binaries: the flag is accepted, a wrong value is refused before
/// anything is dialled, and two current builds sync with `require` on
/// both sides.
#[test]
fn the_binaries_sync_with_require_on_both_sides() {
    use common::{irsync_bin, spawn_daemon_binary};
    let dir = tempfile::tempdir().unwrap();
    let (server, client) = (dir.path().join("server"), dir.path().join("client"));
    let (src, dst) = (dir.path().join("src"), dir.path().join("dst"));
    for d in [&server, &client, &src, &dst] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(src.join("f"), b"data").unwrap();
    let text = |o: &std::process::Output| String::from_utf8_lossy(&o.stderr).into_owned();

    let daemon = spawn_daemon_binary(&server);
    let o = irsync_bin(&client, &["id", "show"]);
    let client_id = String::from_utf8_lossy(&o.stdout)
        .lines()
        .last()
        .unwrap()
        .to_string();
    for args in [
        &["export", "add", &format!("data:{}", src.display())][..],
        &["grant", "add", "data", &client_id],
        &["config", "set", "daemon.pq_key_exchange", "require"],
    ] {
        let o = irsync_bin(&server, args);
        assert!(o.status.success(), "{args:?}: {}", text(&o));
    }
    daemon.reload();
    let address = format!("127.0.0.1:{}", daemon.next_listen().port());
    let remote = format!("{}:data/", daemon.id);
    let dest = format!("{}/", dst.display());

    let o = irsync_bin(
        &client,
        &[
            "-a",
            "--pq-key-exchange",
            "sometimes",
            "--address",
            &address,
            &remote,
            &dest,
        ],
    );
    assert!(
        !o.status.success() && text(&o).contains("expected prefer or require"),
        "{}",
        text(&o)
    );

    let o = irsync_bin(
        &client,
        &[
            "-a",
            "--pq-key-exchange",
            "require",
            "--address",
            &address,
            &remote,
            &dest,
        ],
    );
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(std::fs::read(dst.join("f")).unwrap(), b"data");
}
