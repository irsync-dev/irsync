mod common;

use std::io;

use common::{connect_pair, loopback_pair, within};
use iroh::SecretKey;
use irsync::net::frame::{read_msg, write_msg, MAX_FRAME};
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
            session_id: hello.session_id.clone(),
            resumed: false,
            protocol_version: hello.protocol_version,
            build: None,
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
        session_id: vec![7; 16],
        path: b"photos/2026/oct".to_vec(),
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
    assert_eq!(ack.protocol_version, PROTOCOL_VERSION);
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
    let len = u32::try_from(MAX_FRAME + 1).unwrap();
    send.write_all(&len.to_be_bytes()).await.unwrap();
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
    send.write_all(&[0, 0]).await.unwrap();
    send.finish().unwrap();
    let (_s2, mut recv) = within(server.accept_bi()).await.unwrap();
    let err = within(read_msg::<Hello, _>(&mut recv)).await.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);

    // Stream 3: EOF mid-body is an error too.
    let (mut send, _r3) = client.open_bi().await.unwrap();
    send.write_all(&10u32.to_be_bytes()).await.unwrap();
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
    assert_eq!(opts.relay, irsync::net::RelayChoice::Disabled);
    let ep = irsync::net::build_endpoint(opts).await.unwrap();
    assert_eq!(ep.addr().relay_urls().count(), 0, "{:?}", ep.addr());
    assert!(ep.address_lookup().unwrap().is_empty());
    ep.close().await;
}

#[test]
fn client_opts_are_isolated_whenever_addresses_are_given() {
    use irsync::config::hosts::HostEntry;
    use irsync::config::settings::ClientSettings;
    use irsync::net::{client_endpoint_opts, RelayChoice};
    let cs = ClientSettings::default();
    // Addresses from the host entry, with relay/dht/mdns set beside them: isolated wins.
    let he = HostEntry {
        endpoint_id: SecretKey::generate().public(),
        relay: Some("https://r.example".parse().unwrap()),
        relay_mode: None,
        congestion: None,
        address: vec!["127.0.0.1:1".parse().unwrap()],
        parallel: None,
        reconnect: None,
        dht: Some(true),
        mdns: Some(true),
    };
    let o = client_endpoint_opts(
        SecretKey::generate(),
        None,
        false,
        false,
        &cs,
        Some(&he),
        &he.address,
    )
    .unwrap();
    assert_eq!(o.relay, RelayChoice::Disabled);
    assert!(!o.dht && !o.dns && !o.mdns && !o.portmap && !o.publish);
    // `--relay` with the host entry's addresses is refused.
    let e = client_endpoint_opts(
        SecretKey::generate(),
        Some("https://c.example"),
        false,
        false,
        &cs,
        Some(&he),
        &he.address,
    )
    .unwrap_err();
    assert!(e.to_string().contains("--relay"), "{e}");
    // Addresses from the command line only.
    let cli = vec!["localhost:1".parse().unwrap()];
    let o =
        client_endpoint_opts(SecretKey::generate(), None, false, false, &cs, None, &cli).unwrap();
    assert_eq!(o.relay, RelayChoice::Disabled);
    assert!(!o.dns);
    // No addresses: the pre-existing defaults.
    let o = client_endpoint_opts(SecretKey::generate(), None, false, true, &cs, None, &[]).unwrap();
    assert_eq!(o.relay, RelayChoice::N0Default);
    assert!(!o.dht && o.dns && o.mdns && o.portmap && !o.publish);
    let o = client_endpoint_opts(
        SecretKey::generate(),
        Some("https://c.example"),
        false,
        false,
        &cs,
        None,
        &[],
    )
    .unwrap();
    assert_eq!(
        o.relay,
        RelayChoice::Custom("https://c.example".parse().unwrap())
    );
    assert!(o.dht);
    assert!(client_endpoint_opts(
        SecretKey::generate(),
        Some("not a url"),
        false,
        false,
        &cs,
        None,
        &[]
    )
    .is_err());
}

#[test]
fn daemon_opts_follow_the_isolated_config() {
    use irsync::config::settings::Settings;
    use irsync::net::{daemon_endpoint_opts, RelayChoice};
    let parse = |t: &str| Settings::parse(t, std::path::Path::new("irsync.toml")).unwrap();
    let cfg = parse(
        "[daemon]\nlisten = [\"127.0.0.1:0\"]\nrelay_mode = \"disabled\"\nportmap = false\n\
         [daemon.discovery]\ndht = false\ndns = false\nmdns = false\n",
    );
    let bind = vec!["127.0.0.1:0".parse().unwrap()];
    let o = daemon_endpoint_opts(&cfg.daemon, SecretKey::generate(), bind.clone());
    assert_eq!(o.relay, RelayChoice::Disabled);
    assert!(!o.dht && !o.dns && !o.mdns && !o.portmap && o.publish);
    assert_eq!(o.bind_addrs, bind);
    let o = daemon_endpoint_opts(&parse("").daemon, SecretKey::generate(), vec![]);
    assert_eq!(o.relay, RelayChoice::N0Default);
    assert!(o.dht && o.dns && o.mdns && o.portmap && o.publish && o.bind_addrs.is_empty());
    let o = daemon_endpoint_opts(
        &parse("[daemon]\nrelay = \"https://r.example\"\n").daemon,
        SecretKey::generate(),
        vec![],
    );
    assert_eq!(
        o.relay,
        RelayChoice::Custom("https://r.example".parse().unwrap())
    );
}

/// The congestion controller: the host entry's, else the client settings',
/// else BBR, with or without direct addresses; the daemon's from its own.
#[test]
fn opts_carry_the_congestion_controller() {
    use irsync::config::hosts::HostEntry;
    use irsync::config::settings::{ClientSettings, Congestion, DaemonSettings};
    use irsync::net::{client_endpoint_opts, daemon_endpoint_opts};
    let addr: Vec<irsync::net::addr::HostPort> = vec!["127.0.0.1:1".parse().unwrap()];
    let cs = ClientSettings::default();
    let cubic = ClientSettings {
        congestion: Some(Congestion::Cubic),
        ..ClientSettings::default()
    };
    let mut he = HostEntry {
        endpoint_id: SecretKey::generate().public(),
        relay: None,
        relay_mode: None,
        congestion: None,
        address: vec![],
        parallel: None,
        reconnect: None,
        dht: None,
        mdns: None,
    };
    for a in [&[][..], &addr[..]] {
        let o = client_endpoint_opts(SecretKey::generate(), None, false, false, &cs, None, a);
        assert_eq!(o.unwrap().congestion, Congestion::Bbr);
        let o = client_endpoint_opts(SecretKey::generate(), None, false, false, &cubic, None, a);
        assert_eq!(o.unwrap().congestion, Congestion::Cubic);
        he.congestion = Some(Congestion::Bbr);
        let o = client_endpoint_opts(
            SecretKey::generate(),
            None,
            false,
            false,
            &cubic,
            Some(&he),
            a,
        );
        assert_eq!(o.unwrap().congestion, Congestion::Bbr);
        he.congestion = Some(Congestion::Cubic);
        let o = client_endpoint_opts(SecretKey::generate(), None, false, false, &cs, Some(&he), a);
        assert_eq!(o.unwrap().congestion, Congestion::Cubic);
    }
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

#[test]
fn client_opts_disable_the_relay_but_keep_the_lookups() {
    use irsync::config::hosts::HostEntry;
    use irsync::config::settings::{ClientSettings, RelayMode};
    use irsync::net::{client_endpoint_opts, RelayChoice};
    let cs = ClientSettings::default();
    // --no-relay: no relay, lookups and the port mapper as before.
    let o = client_endpoint_opts(SecretKey::generate(), None, true, false, &cs, None, &[]).unwrap();
    assert_eq!(o.relay, RelayChoice::Disabled);
    assert!(o.dht && o.dns && o.mdns && o.portmap);
    // [client] relay_mode = "disabled", even with a relay URL beside it.
    let off = ClientSettings {
        relay: Some("https://c.example".parse().unwrap()),
        relay_mode: Some(RelayMode::Disabled),
        ..ClientSettings::default()
    };
    let o =
        client_endpoint_opts(SecretKey::generate(), None, false, false, &off, None, &[]).unwrap();
    assert_eq!(o.relay, RelayChoice::Disabled);
    // The host entry decides over the client settings, either way round.
    let mut he = HostEntry {
        endpoint_id: SecretKey::generate().public(),
        relay: None,
        relay_mode: Some(RelayMode::Default),
        congestion: None,
        address: vec![],
        parallel: None,
        reconnect: None,
        dht: None,
        mdns: None,
    };
    let o = client_endpoint_opts(
        SecretKey::generate(),
        None,
        false,
        false,
        &off,
        Some(&he),
        &[],
    )
    .unwrap();
    assert_eq!(
        o.relay,
        RelayChoice::Custom("https://c.example".parse().unwrap())
    );
    he.relay_mode = Some(RelayMode::Disabled);
    let o = client_endpoint_opts(
        SecretKey::generate(),
        None,
        false,
        false,
        &cs,
        Some(&he),
        &[],
    )
    .unwrap();
    assert_eq!(o.relay, RelayChoice::Disabled);
    // --relay on the command line beats a file's "disabled".
    let o = client_endpoint_opts(
        SecretKey::generate(),
        Some("https://cli.example"),
        false,
        false,
        &cs,
        Some(&he),
        &[],
    )
    .unwrap();
    assert_eq!(
        o.relay,
        RelayChoice::Custom("https://cli.example".parse().unwrap())
    );
}
