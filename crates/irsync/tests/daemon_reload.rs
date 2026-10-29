//! `SIGHUP` reloads the daemon's `[daemon]` settings: a new endpoint takes
//! over, the old one finishes what it was serving and then refuses, and a
//! reload that cannot be applied changes nothing.

mod common;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{irsync_bin, loopback_endpoint, offline_opts, spawn_daemon_binary, within};
use iroh::{Endpoint, EndpointAddr, EndpointId, TransportAddr};
use irsync::authz::FileAuthorizer;
use irsync::config::file::Config;
use irsync::config::paths::Paths;
use irsync::daemon::{Daemon, Reloader};
use irsync::identity::PassphraseSource;
use irsync::net::build_endpoint;
use irsync::net::frame::{read_msg, write_msg};
use irsync_proto::chunk::CHUNK_PARAMS;
use irsync_proto::v1::{hello::Direction, ExportList, Hello};

fn config_text(identity: &Path, state: &Path, data: &Path, clients: &[EndpointId]) -> String {
    let ro: Vec<String> = clients.iter().map(|c| format!("\"{c}\"")).collect();
    format!(
        "state_dir = {state:?}\n\
         [daemon]\nidentity = {identity:?}\nlisten = [\"127.0.0.1:0\"]\n\
         relay_mode = \"disabled\"\nportmap = false\nidle_timeout = \"5m\"\n\
         [daemon.discovery]\ndht = false\ndns = false\nmdns = false\n\
         [exports.data]\npath = {data:?}\nro = [{ro}]\n",
        state = state.display().to_string(),
        identity = identity.display().to_string(),
        data = data.display().to_string(),
        ro = ro.join(", "),
    )
}

fn dial(id: EndpointId, addr: SocketAddr) -> EndpointAddr {
    EndpointAddr::from_parts(id, [TransportAddr::Ip(addr)])
}

/// Asks the daemon for the export list over `conn` and returns the names.
async fn list_exports(conn: &iroh::endpoint::Connection) -> Vec<String> {
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    let hello = Hello {
        protocol_version: irsync_proto::PROTOCOL_VERSION,
        direction: Direction::ListExports as i32,
        chunk_min: CHUNK_PARAMS.min,
        chunk_avg: CHUNK_PARAMS.avg,
        chunk_max: CHUNK_PARAMS.max,
        ..Default::default()
    };
    write_msg(&mut send, &hello).await.unwrap();
    send.finish().unwrap();
    let list: ExportList = read_msg(&mut recv).await.unwrap().expect("an export list");
    list.exports.into_iter().map(|e| e.name).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reload_binds_anew_and_retires_the_old_endpoint_after_its_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = dir.path().join("cfg");
    let data = dir.path().join("data");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("f"), b"x").unwrap();
    let paths = Paths::resolve(Some(&cfg_dir.join("irsync.toml")));
    let identity = cfg_dir.join("daemon_ed25519");
    // Three clients: an iroh endpoint remembers the daemon's addresses by id,
    // so one that reached the old address would try it again. A client that
    // starts after the reload is the realistic case for the new address.
    let client = loopback_endpoint().await;
    let client2 = loopback_endpoint().await;
    let client3 = loopback_endpoint().await;
    let text = config_text(
        &identity,
        &dir.path().join("state"),
        &data,
        &[client.id(), client2.id(), client3.id()],
    );
    std::fs::write(&paths.config, &text).unwrap();

    // The daemon starts on an endpoint bound by the test, under the identity
    // the file names, so a reload keeps the id.
    let secret = irsync::identity::generate_openssh(&identity, "irsync-daemon").unwrap();
    let first = build_endpoint(irsync::net::EndpointOpts {
        secret,
        ..offline_opts()
    })
    .await
    .unwrap();
    let id = first.id();
    let old_addr = first.bound_sockets()[0];
    let settings = Config::load(&paths).unwrap().settings;
    let notify = Arc::new(tokio::sync::Notify::new());
    let (bound_tx, mut bound_rx) =
        tokio::sync::mpsc::unbounded_channel::<(EndpointId, Vec<SocketAddr>)>();
    let daemon = Daemon::with_home(
        &settings,
        Arc::new(FileAuthorizer::new(paths.clone())),
        first,
        None,
    )
    .unwrap()
    .with_reload(Reloader {
        paths: paths.clone(),
        pass: PassphraseSource::default(),
        listen: vec![],
        notify: notify.clone(),
        announce: Arc::new(move |ep: &Endpoint| {
            let _ = bound_tx.send((ep.id(), ep.bound_sockets()));
        }),
    });
    let shutdown = tokio_util::sync::CancellationToken::new();
    let run = tokio::spawn(daemon.run(shutdown.clone()));

    // A connection accepted before the reload, not yet past its hello.
    let early = within(client.connect(dial(id, old_addr), irsync_proto::ALPN))
        .await
        .unwrap();

    notify.notify_one();
    let (new_id, new_addrs) = within(bound_rx.recv()).await.expect("a new endpoint");
    assert_eq!(new_id, id, "the identity is kept across a reload");
    let new_addr = new_addrs[0];
    assert_ne!(new_addr, old_addr);

    // The new endpoint serves.
    let fresh = within(client2.connect(dial(id, new_addr), irsync_proto::ALPN))
        .await
        .unwrap();
    assert_eq!(within(list_exports(&fresh)).await, ["data"]);
    // The old one refuses newcomers while it still has `early` to finish.
    let refused = tokio::time::timeout(
        Duration::from_secs(5),
        client.connect(dial(id, old_addr), irsync_proto::ALPN),
    )
    .await;
    assert!(
        !matches!(refused, Ok(Ok(_))),
        "a retired endpoint accepted a connection"
    );
    // ... and `early` completes its exchange on it.
    assert_eq!(within(list_exports(&early)).await, ["data"]);

    // A reload with a broken file is refused and changes nothing.
    std::fs::write(&paths.config, "[daemon\nnot toml").unwrap();
    notify.notify_one();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        bound_rx.try_recv().is_err(),
        "a failed reload announced an endpoint"
    );
    std::fs::write(&paths.config, &text).unwrap();
    let again = within(client3.connect(dial(id, new_addr), irsync_proto::ALPN))
        .await
        .unwrap();
    assert_eq!(within(list_exports(&again)).await, ["data"]);

    shutdown.cancel();
    within(run).await.unwrap().unwrap();
    for ep in [client, client2, client3] {
        ep.close().await;
    }
}

#[test]
fn sighup_makes_the_daemon_listen_anew_and_serve_there() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path();
    let server = tmp.join("server/home");
    let client = tmp.join("client/home");
    let data = tmp.join("server/data");
    let dst = tmp.join("client/dst");
    for d in [&server, &client, &data, &dst] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(data.join("a.txt"), b"hello").unwrap();
    let daemon = spawn_daemon_binary(&server);
    let ok = |home: &Path, args: &[&str]| {
        let o = irsync_bin(home, args);
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8(o.stdout).unwrap()
    };
    let client_id = ok(&client, &["id", "show"])
        .lines()
        .nth(1)
        .unwrap()
        .to_string();
    ok(
        &server,
        &["export", "add", &format!("data:{}", data.display())],
    );
    ok(&server, &["grant", "add", "data", &client_id, "--ro"]);
    let from = format!("{}:data/", daemon.id);
    let to = format!("{}/", dst.display());
    let sync = |address: &str| {
        irsync_bin(
            &client,
            &["-a", "--reconnect", "0", "--address", address, &from, &to],
        )
    };
    let old = format!("127.0.0.1:{}", daemon.listen.port());
    let o = sync(&old);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    // A setting changes, and SIGHUP applies it: the daemon binds anew
    // (`--listen 127.0.0.1:0` picks a fresh port) and says so.
    ok(&server, &["config", "set", "daemon.idle_timeout", "1h"]);
    daemon.reload();
    let new = daemon.next_listen();
    let log = std::fs::read_to_string(&daemon.log).unwrap();
    assert!(log.contains("settings reloaded"), "{log}");
    std::fs::write(data.join("b.txt"), b"more").unwrap();
    let o = sync(&format!("127.0.0.1:{}", new.port()));
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(std::fs::read(dst.join("b.txt")).unwrap(), b"more");
    // The old socket is closed once its sessions ended (a dial there now waits
    // out the handshake timeout, as for any stopped daemon; the in-process
    // test covers the refusal while it is retired).
    let log = std::fs::read_to_string(&daemon.log).unwrap();
    assert!(log.contains("closing the retired endpoint"), "{log}");
}
