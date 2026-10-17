//! Session engine. Stub: the daemon side only acknowledges the Hello for now.

use std::path::PathBuf;

use iroh::endpoint::{Connection, RecvStream, SendStream};
use irsync_proto::v1::{Hello, HelloAck};
use irsync_proto::PROTOCOL_VERSION;
use rand_core::{OsRng, RngCore};

use crate::index::IndexHandle;
use crate::net::frame::write_msg;

pub async fn serve_daemon(
    _conn: Connection,
    mut send: SendStream,
    _recv: RecvStream,
    _module_root: PathBuf,
    _hello: Hello,
    _index: IndexHandle,
) -> anyhow::Result<()> {
    let mut session_id = vec![0u8; 16];
    OsRng.fill_bytes(&mut session_id);
    write_msg(
        &mut send,
        &HelloAck {
            session_id,
            resumed: false,
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await?;
    send.finish()?;
    // Let the peer read the ack before the connection is dropped.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), send.stopped()).await;
    Ok(())
}
