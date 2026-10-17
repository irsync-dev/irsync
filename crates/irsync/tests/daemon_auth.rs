mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use common::{client_endpoint, spawn_daemon, within};
use irsync::authz::{Access, Grants, MemoryAuthorizer};
use irsync::net::frame::{read_msg, write_msg};
use irsync_proto::chunk::CHUNK_PARAMS;
use irsync_proto::v1::{error::Code, hello::Direction, Error, Hello, HelloAck};
use irsync_proto::{ALPN, PROTOCOL_VERSION};

fn hello(module: &str, path: &[u8], dir: Direction) -> Hello {
    Hello {
        protocol_version: PROTOCOL_VERSION,
        session_id: vec![],
        module: module.into(),
        path: path.to_vec(),
        direction: dir as i32,
        chunk_min: CHUNK_PARAMS.min,
        chunk_avg: CHUNK_PARAMS.avg,
        chunk_max: CHUNK_PARAMS.max,
        contents_only: false,
    }
}

fn grants(access: Access) -> Grants {
    Grants {
        all: Some(access),
        ..Grants::default()
    }
}

/// Sends `h` from an authorised client with `access`; returns the reply bytes' decode attempt.
async fn exchange(access: Access, h: Hello) -> Result<HelloAckOrError, String> {
    exchange_with(grants(access), h).await
}

async fn exchange_with(g: Grants, h: Hello) -> Result<HelloAckOrError, String> {
    let client = client_endpoint().await;
    let dir = tempfile::tempdir().unwrap();
    let authz = Arc::new(MemoryAuthorizer::single(client.id(), g));
    let d = spawn_daemon(
        vec![
            ("rw", dir.path().to_path_buf(), false),
            ("ro", dir.path().to_path_buf(), true),
        ],
        authz,
    )
    .await;
    let conn = within(client.connect(d.addr.clone(), ALPN)).await.unwrap();
    let (mut send, mut recv) = within(conn.open_bi()).await.unwrap();
    within(write_msg(&mut send, &h)).await.unwrap();
    send.finish().unwrap();
    let raw = within(recv.read_to_end(1 << 20))
        .await
        .map_err(|e| e.to_string())?;
    // HelloAck and Error differ in the wire type of field 1, so at most one decodes.
    let out = match read_msg::<HelloAck, _>(&mut &raw[..]).await {
        Ok(Some(ack)) => HelloAckOrError::Ack(ack),
        _ => HelloAckOrError::Err(read_msg::<Error, _>(&mut &raw[..]).await.unwrap().unwrap()),
    };
    d.shutdown.cancel();
    let _ = within(d.task).await;
    Ok(out)
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
    closed_before_hello(|_| {
        Arc::new(MemoryAuthorizer::single(
            stranger.id(),
            grants(Access::ReadWrite),
        ))
    })
    .await;
}

#[tokio::test]
async fn empty_grants_closed_before_hello() {
    closed_before_hello(|id| Arc::new(MemoryAuthorizer::single(id, Grants::default()))).await;
}

async fn closed_before_hello(
    mk: impl FnOnce(iroh::EndpointId) -> Arc<dyn irsync::authz::Authorizer>,
) {
    let client = client_endpoint().await;
    let dir = tempfile::tempdir().unwrap();
    let authz = mk(client.id());
    let d = spawn_daemon(vec![("rw", dir.path().to_path_buf(), false)], authz).await;
    let conn = within(client.connect(d.addr.clone(), ALPN)).await.unwrap();
    // The stream may open locally and the write may even succeed; reading must fail.
    let res = within(async {
        let (mut send, mut recv) = conn.open_bi().await?;
        let _ = write_msg(&mut send, &hello("rw", b"", Direction::ClientSends)).await;
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
        hello("rw", b"sub/dir", Direction::ClientSends),
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
async fn module_path_escape_rejected() {
    expect_err(
        exchange(
            Access::ReadWrite,
            hello("rw", b"../etc", Direction::ClientSends),
        )
        .await,
        Code::BadPath,
    );
}

#[tokio::test]
async fn read_only_grant_cannot_send() {
    expect_err(
        exchange(Access::ReadOnly, hello("rw", b"", Direction::ClientSends)).await,
        Code::ReadOnly,
    );
}

#[tokio::test]
async fn unknown_module_rejected() {
    expect_err(
        exchange(
            Access::ReadWrite,
            hello("nope", b"", Direction::DaemonSends),
        )
        .await,
        Code::NoSuchModule,
    );
}

#[tokio::test]
async fn rw_grant_cannot_send_to_read_only_module() {
    expect_err(
        exchange(Access::ReadWrite, hello("ro", b"", Direction::ClientSends)).await,
        Code::ReadOnly,
    );
}

#[tokio::test]
async fn grant_lacking_module_cannot_pull() {
    let g = Grants {
        modules: [("ro".to_string(), Access::ReadWrite)].into(),
        all: None,
    };
    expect_err(
        exchange_with(g, hello("rw", b"", Direction::DaemonSends)).await,
        Code::Unauthorized,
    );
}

#[tokio::test]
async fn ro_grant_can_pull() {
    match exchange(Access::ReadOnly, hello("rw", b"", Direction::DaemonSends)).await {
        Ok(HelloAckOrError::Ack(_)) => {}
        _ => panic!("expected HelloAck"),
    }
}

#[tokio::test]
async fn wrong_protocol_version_rejected() {
    let mut h = hello("rw", b"", Direction::ClientSends);
    h.protocol_version = PROTOCOL_VERSION + 1;
    expect_err(exchange(Access::ReadWrite, h).await, Code::VersionMismatch);
}

#[tokio::test]
async fn wrong_chunk_params_rejected() {
    let mut h = hello("rw", b"", Direction::ClientSends);
    h.chunk_avg += 1;
    expect_err(exchange(Access::ReadWrite, h).await, Code::VersionMismatch);
}
