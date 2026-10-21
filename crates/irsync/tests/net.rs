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
        module: "photos".into(),
        path: b"2026/oct".to_vec(),
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
