//! Ending a connection after its last answer.
//!
//! A refusal, a listing, a probe result or an admin reply is the last
//! thing the daemon writes on a connection. Closing the connection throws
//! away what the client has not received yet, so the daemon closes only
//! once the client has the answer: when the client closes the connection
//! itself, which every client does as soon as it has read the answer.
//! Until then the answer can still be on its way, on a slow or lossy link
//! or to a starved machine, so the wait is long; it is bounded only so
//! that a client that never closes cannot hold the connection for ever.

use std::time::Duration;

use iroh::endpoint::{Connection, SendStream};

use super::tap::StreamTap;

/// How long [`finish_answer`] waits for the client to close.
pub const ANSWER_WAIT: Duration = Duration::from_secs(30);

/// Finishes `send` after its last frame, waits (at most [`ANSWER_WAIT`])
/// for the client to acknowledge the data and then to close the
/// connection, and closes it with `code` and `reason`.
pub async fn finish_answer(
    conn: &Connection,
    send: &mut SendStream,
    tap: &StreamTap,
    code: u32,
    reason: &[u8],
) -> anyhow::Result<()> {
    send.finish()?;
    tap.finished();
    let _ = tokio::time::timeout(ANSWER_WAIT, async {
        let _ = send.stopped().await;
        conn.closed().await;
    })
    .await;
    conn.close(code.into(), reason);
    Ok(())
}
