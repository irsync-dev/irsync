mod common;

use std::io;

use common::{connect_pair, loopback_pair, within};
use iroh::SecretKey;
use irsync::net::frame::{encode_prefix, read_msg, write_msg, MAX_FRAME};
use irsync::net::EndpointOpts;
use irsync_proto::v1::{Hello, HelloAck};
use irsync_proto::{ALPN, PROTOCOL_VERSION};

#[tokio::test(flavor = "multi_thread")]
async fn two_local_endpoints_exchange_hello() {
    let (a, b) = loopback_pair().await;
    let (client, server) = connect_pair(&b, &a).await;

    assert_eq!(server.alpn(), ALPN);
    assert_eq!(client.alpn(), ALPN);
    assert_eq!(server.remote_id(), b.id());
    assert_eq!(client.remote_id(), a.id());

    let responder = tokio::spawn(async move {
        let (mut send, mut recv) = within(server.accept_bi()).await.expect("accept_bi");
        let hello: Hello = within(read_msg(&mut recv))
            .await
            .unwrap()
            .expect("hello frame");
        let ack = HelloAck {
            session_id: vec![7; 16],
            resumed: false,
            build: None,
            single_name: vec![],
        };
        write_msg(&mut send, &ack).await.unwrap();
        send.finish().unwrap();
        // Client finishes its side after reading the ack: clean EOF.
        assert!(within(read_msg::<Hello, _>(&mut recv))
            .await
            .unwrap()
            .is_none());
        hello
    });

    let hello = Hello {
        protocol_version: PROTOCOL_VERSION,
        request: Some(irsync_proto::v1::hello::Request::Probe(
            irsync_proto::v1::Probe {
                path: b"photos/2026/oct".to_vec(),
            },
        )),
        ..Default::default()
    };
    let (mut send, mut recv) = client.open_bi().await.expect("open_bi");
    write_msg(&mut send, &hello).await.unwrap();
    let ack: HelloAck = within(read_msg(&mut recv))
        .await
        .unwrap()
        .expect("ack frame");
    send.finish().unwrap();
    assert!(within(read_msg::<HelloAck, _>(&mut recv))
        .await
        .unwrap()
        .is_none());

    assert_eq!(ack.session_id, vec![7; 16]);
    assert_eq!(within(responder).await.unwrap(), hello);

    client.close(0u32.into(), b"done");
    a.close().await;
    b.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn oversize_frame_rejected() {
    let (a, b) = loopback_pair().await;
    let (client, server) = connect_pair(&b, &a).await;

    let (mut send, _recv) = client.open_bi().await.unwrap();
    let mut prefix = Vec::new();
    encode_prefix(MAX_FRAME + 1, &mut prefix);
    send.write_all(&prefix).await.unwrap();
    send.write_all(&[0u8; 64]).await.unwrap();

    let (_send, mut recv) = within(server.accept_bi()).await.unwrap();
    let err = within(read_msg::<Hello, _>(&mut recv)).await.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);

    client.close(0u32.into(), b"done");
    a.close().await;
    b.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn eof_returns_none() {
    let (a, b) = loopback_pair().await;
    let (client, server) = connect_pair(&b, &a).await;

    // Stream 1: one complete frame, then a clean finish -> Some, then None.
    let (mut send, _r1) = client.open_bi().await.unwrap();
    let hello = Hello {
        protocol_version: PROTOCOL_VERSION,
        ..Default::default()
    };
    write_msg(&mut send, &hello).await.unwrap();
    send.finish().unwrap();
    let (_s1, mut recv) = within(server.accept_bi()).await.unwrap();
    assert_eq!(
        within(read_msg::<Hello, _>(&mut recv)).await.unwrap(),
        Some(hello)
    );
    assert_eq!(within(read_msg::<Hello, _>(&mut recv)).await.unwrap(), None);

    // Stream 2: EOF mid-prefix is an error, not a clean end.
    let (mut send, _r2) = client.open_bi().await.unwrap();
    send.write_all(&[0x80]).await.unwrap();
    send.finish().unwrap();
    let (_s2, mut recv) = within(server.accept_bi()).await.unwrap();
    let err = within(read_msg::<Hello, _>(&mut recv)).await.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);

    // Stream 3: EOF mid-body is an error too.
    let (mut send, _r3) = client.open_bi().await.unwrap();
    let mut prefix = Vec::new();
    encode_prefix(10, &mut prefix);
    send.write_all(&prefix).await.unwrap();
    send.write_all(&[1, 2, 3]).await.unwrap();
    send.finish().unwrap();
    let (_s3, mut recv) = within(server.accept_bi()).await.unwrap();
    let err = within(read_msg::<Hello, _>(&mut recv)).await.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);

    client.close(0u32.into(), b"done");
    a.close().await;
    b.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn binds_each_requested_socket_and_reports_real_ports() {
    let mut opts = EndpointOpts::isolated(SecretKey::generate());
    opts.bind_addrs = vec!["127.0.0.1:0".parse().unwrap(), "[::1]:0".parse().unwrap()];
    let ep = irsync::net::build_endpoint(opts).await.unwrap();
    let bound = ep.bound_sockets();
    assert_eq!(bound.len(), 2, "{bound:?}");
    assert!(
        bound.iter().all(|a| a.ip().is_loopback() && a.port() != 0),
        "{bound:?}"
    );
    assert!(bound.iter().any(|a| a.is_ipv4()) && bound.iter().any(|a| a.is_ipv6()));
    ep.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn isolated_opts_have_no_relay_and_no_lookup() {
    let opts = EndpointOpts::isolated(SecretKey::generate());
    assert!(!opts.dht && !opts.dns && !opts.mdns && !opts.publish && !opts.portmap);
    assert_eq!(opts.relay, irsync::config::settings::Relay::Disabled);
    let ep = irsync::net::build_endpoint(opts).await.unwrap();
    assert_eq!(ep.addr().relay_urls().count(), 0, "{:?}", ep.addr());
    assert!(ep.address_lookup().unwrap().is_empty());
    ep.close().await;
}

#[test]
fn daemon_opts_follow_the_isolated_config() {
    use irsync::config::settings::Relay;
    use irsync::config::settings::Settings;
    use irsync::net::daemon_endpoint_opts;
    let parse = |t: &str| Settings::parse(t, std::path::Path::new("irsync.toml")).unwrap();
    let cfg = parse(
        "[daemon]\nlisten = [\"127.0.0.1:0\"]\nrelay = \"disabled\"\nportmap = false\n\
         dht = false\ndns = false\nmdns = false\n",
    );
    let bind = vec!["127.0.0.1:0".parse().unwrap()];
    let o = daemon_endpoint_opts(&cfg.daemon, SecretKey::generate(), bind.clone());
    assert_eq!(o.relay, Relay::Disabled);
    assert!(!o.dht && !o.dns && !o.mdns && !o.portmap && o.publish);
    assert_eq!(o.bind_addrs, bind);
    let o = daemon_endpoint_opts(&parse("").daemon, SecretKey::generate(), vec![]);
    assert_eq!(o.relay, Relay::Default);
    assert!(o.dht && o.dns && o.mdns && o.portmap && o.publish && o.bind_addrs.is_empty());
    let o = daemon_endpoint_opts(
        &parse("[daemon]\nrelay = \"https://r.example\"\n").daemon,
        SecretKey::generate(),
        vec![],
    );
    assert_eq!(o.relay, Relay::Url("https://r.example".parse().unwrap()));
}

/// The daemon's congestion controller is its own.
#[test]
fn daemon_opts_carry_the_congestion_controller() {
    use irsync::config::settings::{Congestion, DaemonSettings};
    use irsync::net::daemon_endpoint_opts;
    let ds = DaemonSettings {
        congestion: Congestion::Cubic,
        ..DaemonSettings::default()
    };
    let o = daemon_endpoint_opts(&ds, SecretKey::generate(), vec![]);
    assert_eq!(o.congestion, Congestion::Cubic);
    assert_eq!(
        daemon_endpoint_opts(&DaemonSettings::default(), SecretKey::generate(), vec![]).congestion,
        Congestion::Bbr
    );
}

/// Endpoints on Cubic carry data like the default ones.
#[tokio::test(flavor = "multi_thread")]
async fn endpoints_on_cubic_carry_data() {
    use irsync::config::settings::Congestion;
    let mut opts = [common::offline_opts(), common::offline_opts()];
    for o in &mut opts {
        o.congestion = Congestion::Cubic;
    }
    let [a, b] = opts;
    let a = irsync::net::build_endpoint(a).await.unwrap();
    let b = irsync::net::build_endpoint(b).await.unwrap();
    let (client, server) = connect_pair(&b, &a).await;
    let data: Vec<u8> = (0..4u32 << 20).map(|i| (i % 251) as u8).collect();
    let sent = data.clone();
    let writer = tokio::spawn(async move {
        let mut send = client.open_uni().await.unwrap();
        send.write_all(&sent).await.unwrap();
        send.finish().unwrap();
        send.stopped().await.unwrap();
    });
    let mut recv = within(server.accept_uni()).await.unwrap();
    let got = within(recv.read_to_end(8 << 20)).await.unwrap();
    assert_eq!(got, data);
    writer.await.unwrap();
}

/// Two irsync endpoints agree on the hybrid key exchange, and both ends
/// can tell.
#[tokio::test(flavor = "multi_thread")]
async fn two_irsync_endpoints_use_the_hybrid_key_exchange() {
    let (a, b) = loopback_pair().await;
    let (client, server) = connect_pair(&b, &a).await;
    assert_eq!(
        irsync::net::key_exchange(&client).as_deref(),
        Some("X25519MLKEM768")
    );
    assert_eq!(
        irsync::net::key_exchange(&server).as_deref(),
        Some("X25519MLKEM768")
    );
    client.close(0u32.into(), b"done");
    a.close().await;
    b.close().await;
}

/// A peer without the hybrid still connects, in either direction, with
/// X25519.
#[tokio::test(flavor = "multi_thread")]
async fn a_peer_without_the_hybrid_falls_back_to_x25519() {
    let ours = common::loopback_endpoint().await;
    let old = common::classical_endpoint().await;
    for (dialler, listener) in [(&old, &ours), (&ours, &old)] {
        let (client, server) = connect_pair(dialler, listener).await;
        assert_eq!(
            irsync::net::key_exchange(&client).as_deref(),
            Some("X25519")
        );
        assert_eq!(
            irsync::net::key_exchange(&server).as_deref(),
            Some("X25519")
        );
        client.close(0u32.into(), b"done");
        within(server.closed()).await;
    }
    ours.close().await;
    old.close().await;
}
