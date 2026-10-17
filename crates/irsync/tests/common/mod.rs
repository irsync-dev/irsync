//! Shared helpers for integration tests that need real iroh endpoints.
#![allow(dead_code)]

use std::future::Future;
use std::time::Duration;

use iroh::{Endpoint, EndpointAddr, SecretKey};
use irsync::net::{build_endpoint, EndpointOpts, RelayChoice};

/// Awaits `fut`, panicking after 10 s so a regression fails instead of hanging.
pub async fn within<F: Future>(fut: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(10), fut)
        .await
        .expect("timed out after 10 s")
}

/// Options for a fully offline endpoint: no relay, no address lookup, loopback bind.
pub fn offline_opts() -> EndpointOpts {
    EndpointOpts {
        secret: SecretKey::generate(),
        relay: RelayChoice::Disabled,
        dht: false,
        dns: false,
        mdns: false,
        publish: false,
        bind_addr: Some("127.0.0.1:0".parse().unwrap()),
    }
}

/// Binds one offline endpoint on loopback.
pub async fn loopback_endpoint() -> Endpoint {
    build_endpoint(offline_opts())
        .await
        .expect("bind loopback endpoint")
}

/// Binds two offline endpoints on loopback. Dial one from the other with [`addr_of`].
pub async fn loopback_pair() -> (Endpoint, Endpoint) {
    (loopback_endpoint().await, loopback_endpoint().await)
}

/// The endpoint's id plus its direct (loopback) addresses: enough to dial it offline.
pub fn addr_of(ep: &Endpoint) -> EndpointAddr {
    let addr = ep.addr();
    assert!(
        addr.ip_addrs().next().is_some(),
        "endpoint has no direct addresses"
    );
    assert!(
        addr.ip_addrs().all(|a| a.ip().is_loopback()),
        "offline endpoint bound beyond loopback: {addr:?}"
    );
    addr
}

/// Connects `client` to `server` over loopback and returns both ends of the connection
/// as `(client_side, server_side)`.
pub async fn connect_pair(
    client: &Endpoint,
    server: &Endpoint,
) -> (iroh::endpoint::Connection, iroh::endpoint::Connection) {
    let addr = addr_of(server);
    let accept = async {
        server
            .accept()
            .await
            .expect("incoming connection")
            .await
            .expect("server handshake")
    };
    let connect = async {
        client
            .connect(addr, irsync_proto::ALPN)
            .await
            .expect("client connect")
    };
    let (c, s) = within(async { tokio::join!(connect, accept) }).await;
    (c, s)
}

/// A running in-process daemon: its dialable address, its id, and the task running it.
pub struct DaemonHandle {
    pub addr: EndpointAddr,
    pub id: iroh::EndpointId,
    pub task: tokio::task::JoinHandle<anyhow::Result<()>>,
    pub shutdown: tokio_util::sync::CancellationToken,
    pub hellos: std::sync::Arc<std::sync::atomic::AtomicU64>,
    pub _state: tempfile::TempDir,
}

/// Starts a daemon on a loopback endpoint serving `modules` (name -> (path, read_only)).
pub async fn spawn_daemon(
    modules: Vec<(&str, std::path::PathBuf, bool)>,
    authz: std::sync::Arc<dyn irsync::authz::Authorizer>,
) -> DaemonHandle {
    use irsync::config::daemon::{DaemonConfig, Module};
    let state = tempfile::tempdir().unwrap();
    let mut cfg = DaemonConfig {
        state_dir: state.path().to_path_buf(),
        ..DaemonConfig::default()
    };
    for (name, path, read_only) in modules {
        cfg.modules
            .insert(name.to_string(), Module { path, read_only });
    }
    let ep = loopback_endpoint().await;
    let (addr, id) = (addr_of(&ep), ep.id());
    let daemon = irsync::daemon::Daemon::new(cfg, authz, ep).expect("daemon");
    let hellos = daemon.hello_counter();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let task = tokio::spawn(daemon.run(shutdown.clone()));
    DaemonHandle {
        addr,
        id,
        task,
        shutdown,
        hellos,
        _state: state,
    }
}

/// A client endpoint with its own fresh identity, offline on loopback.
pub async fn client_endpoint() -> Endpoint {
    loopback_endpoint().await
}
