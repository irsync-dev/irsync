//! A client session that outlives its connections: dial, run the session,
//! and when the connection fails underneath it dial again (with backoff)
//! and resume the same session until `ReconnectOpts::max` has elapsed. A
//! session stored by an earlier client process with the same key is resumed
//! on start unless `--no-resume` was given.

use std::time::{Duration, Instant};

use futures::StreamExt;
use iroh::endpoint::{Connection, ConnectionError};
use iroh::{Endpoint, EndpointAddr};
use irsync_proto::v1::{error::Code, Hello};
use rand_core::{OsRng, RngCore};

use super::engine::{
    client_session_key, run_client_inner, PeerError, SessionRefused, TransportError,
};
use super::{control::hex, LocalSide, Outcome, SessionRecord, CLOSE_SUPERSEDED};
use crate::cli::SyncOptions;

/// How long to keep trying, and how long to wait between attempts.
#[derive(Debug, Clone, Copy)]
pub struct ReconnectOpts {
    /// Give up once this much time has passed since the first attempt
    /// (zero: never reconnect).
    pub max: Duration,
    /// The first wait; it doubles per attempt up to `backoff_max`, jittered.
    pub backoff_min: Duration,
    pub backoff_max: Duration,
}

impl Default for ReconnectOpts {
    fn default() -> Self {
        ReconnectOpts {
            max: Duration::from_secs(24 * 3600),
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
        }
    }
}

impl ReconnectOpts {
    /// The wait before reconnect `attempt` (1-based): exponential, capped,
    /// plus up to a quarter of jitter, never below `backoff_min`.
    fn delay(&self, attempt: u32) -> Duration {
        let exp = self
            .backoff_min
            .saturating_mul(1u32 << attempt.saturating_sub(1).min(16));
        let base = exp.min(self.backoff_max).max(self.backoff_min);
        let jitter = base.mul_f64(f64::from(OsRng.next_u32() % 1000) / 4000.0);
        (base + jitter).min(self.backoff_max.max(self.backoff_min))
    }
}

/// The connection's state, as reported to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    Connecting,
    /// The selected path is a direct (IP) path.
    Direct,
    /// The selected path goes through a relay.
    Relay,
    /// The connection was lost; dialling again.
    Reconnecting {
        attempt: u32,
    },
}

/// What to do after a failed attempt.
enum Next {
    /// The connection failed underneath the session: dial again.
    Retry,
    /// The daemon does not know the session: start a fresh one.
    Fresh,
    Fatal,
}

/// Close codes that are a final answer: unauthorized, rejected, and
/// superseded (another client process resumed this session; two runs must
/// not keep taking it from each other).
const FINAL_CLOSE_CODES: [u32; 3] = [1, 2, CLOSE_SUPERSEDED];

fn classify(err: &anyhow::Error, conn: Option<&Connection>) -> Next {
    if let Some(r) = err.chain().find_map(|e| e.downcast_ref::<SessionRefused>()) {
        return if r.code == Code::BadSession {
            Next::Fresh
        } else {
            Next::Fatal
        };
    }
    if err.chain().any(|e| e.is::<PeerError>()) {
        return Next::Fatal;
    }
    if let Some(ConnectionError::ApplicationClosed(c)) = conn.and_then(|c| c.close_reason()) {
        if FINAL_CLOSE_CODES
            .iter()
            .any(|&code| c.error_code == code.into())
        {
            return Next::Fatal;
        }
    }
    if err.chain().any(|e| e.is::<TransportError>()) {
        Next::Retry
    } else {
        Next::Fatal
    }
}

/// `secs` since the epoch as local time, `YYYY-MM-DD HH:MM:SS`.
fn local_time(secs: i64) -> String {
    let t = secs as libc::time_t;
    // SAFETY: an all-zero `tm` is a valid value; `localtime_r` only writes
    // into the `tm` it is given and reads `t`.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&t, &mut tm).is_null() };
    if !ok {
        return format!("@{secs}");
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// The session an earlier run left for this key, if any.
async fn stored_session(local: &LocalSide, key: Vec<u8>) -> Option<SessionRecord> {
    let index = local.index.clone();
    let row = tokio::task::spawn_blocking(move || {
        index
            .lock()
            .map_err(|_| anyhow::anyhow!("index poisoned"))?
            .load_session_by_key(&key)
            .map_err(anyhow::Error::from)
    })
    .await;
    match row {
        Ok(Ok(Some(row))) => SessionRecord::from_row(&row).ok(),
        Ok(Ok(None)) => None,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "looking up a stored session failed");
            None
        }
        Err(e) => {
            tracing::warn!(error = %e, "looking up a stored session failed");
            None
        }
    }
}

/// Runs `fut` while reporting the connection's selected path (direct or
/// relayed) whenever it changes.
async fn watch_paths<F: std::future::Future>(
    conn: &Connection,
    fut: F,
    on_state: &mut impl FnMut(ConnState),
) -> F::Output {
    let mut fut = std::pin::pin!(fut);
    let mut paths = std::pin::pin!(conn.paths_stream());
    let (mut last, mut paths_done) = (None, false);
    loop {
        tokio::select! {
            out = &mut fut => return out,
            list = paths.next(), if !paths_done => match list {
                Some(list) => {
                    let state = list.iter().find(|p| p.is_selected()).map(|p| {
                        if p.is_relay() {
                            ConnState::Relay
                        } else {
                            ConnState::Direct
                        }
                    });
                    if state.is_some() && state != last {
                        last = state;
                        on_state(state.unwrap_or(ConnState::Connecting));
                    }
                }
                None => paths_done = true,
            },
        }
    }
}

/// Runs one client session to completion across connections. `hello_for`
/// builds the `Hello` for a session id (`None`: a new session). Transport
/// failures are retried with backoff until `opts.max` has elapsed; a daemon
/// that no longer knows the session gets one immediate fresh attempt; every
/// other error is returned as is.
pub async fn run_with_reconnect(
    endpoint: &Endpoint,
    target: impl Into<EndpointAddr>,
    hello_for: impl Fn(Option<[u8; 16]>) -> Hello,
    local: LocalSide,
    sync_opts: &SyncOptions,
    opts: ReconnectOpts,
    mut on_state: impl FnMut(ConnState),
) -> anyhow::Result<Outcome> {
    let target: EndpointAddr = target.into();
    let key = client_session_key(target.id, &hello_for(None), &local, sync_opts).await?;
    let mut session = match stored_session(&local, key).await {
        Some(rec) if !sync_opts.no_resume => {
            eprintln!(
                "resuming session {} from {}",
                hex(&rec.id[..4]),
                local_time(rec.updated_at)
            );
            Some(rec.id)
        }
        Some(rec) => {
            tracing::info!(session = %hex(&rec.id), "--no-resume: starting a new session");
            None
        }
        None => None,
    };
    // `max` bounds one outage: it starts when a connection that carried the
    // session is lost and ends when a new one is acknowledged.
    let mut outage_start = Instant::now();
    // Reconnects so far (reported) and failures since the last acknowledged
    // connection (sets the backoff).
    let (mut attempt, mut failures) = (0u32, 0u32);
    loop {
        on_state(match attempt {
            0 => ConnState::Connecting,
            n => ConnState::Reconnecting { attempt: n },
        });
        let mut acked = None;
        let (res, conn) = match endpoint.connect(target.clone(), irsync_proto::ALPN).await {
            Ok(conn) => {
                let run = run_client_inner(
                    conn.clone(),
                    hello_for(session),
                    local.clone(),
                    sync_opts,
                    &mut acked,
                );
                let res = watch_paths(&conn, run, &mut on_state).await;
                (res, Some(conn))
            }
            Err(e) => (
                Err(anyhow::Error::new(TransportError(format!(
                    "connecting to {}: {e}",
                    target.id
                )))),
                None,
            ),
        };
        let next = res.as_ref().err().map(|e| classify(e, conn.as_ref()));
        if let Some(c) = &conn {
            c.close(0u32.into(), b"done");
        }
        let err = match res {
            Ok(out) => return Ok(out),
            Err(e) => e,
        };
        if acked.is_some() {
            session = acked;
            outage_start = Instant::now();
            failures = 0;
        }
        match next {
            Some(Next::Fresh) if session.is_some() => {
                // Gone on the daemon (finished or expired): start over at once.
                tracing::warn!(error = %format!("{err:#}"), "the daemon does not know the session; starting a new one");
                session = None;
                continue;
            }
            Some(Next::Retry) => {}
            _ => return Err(err),
        }
        if outage_start.elapsed() >= opts.max {
            return Err(err);
        }
        attempt += 1;
        failures += 1;
        let wait = opts.delay(failures);
        tracing::warn!(
            error = %format!("{err:#}"),
            attempt,
            wait_ms = wait.as_millis() as u64,
            "connection lost; reconnecting"
        );
        tokio::time::sleep(wait).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_to_the_cap_and_never_drops_below_the_floor() {
        let o = ReconnectOpts::default();
        for attempt in 1..40 {
            let d = o.delay(attempt);
            assert!(d >= Duration::from_secs(1), "{attempt}: {d:?}");
            assert!(d <= Duration::from_secs(60), "{attempt}: {d:?}");
        }
        assert!(o.delay(1) < Duration::from_millis(1300));
        assert!(o.delay(20) >= Duration::from_secs(60));
    }

    #[test]
    fn classifies_errors() {
        let refused = |code| {
            anyhow::Error::new(SessionRefused {
                code,
                message: String::new(),
            })
        };
        assert!(matches!(
            classify(&refused(Code::BadSession), None),
            Next::Fresh
        ));
        assert!(matches!(
            classify(&refused(Code::ReadOnly), None),
            Next::Fatal
        ));
        let lost = anyhow::Error::new(TransportError("lost".into())).context("session");
        assert!(matches!(classify(&lost, None), Next::Retry));
        let peer = anyhow::Error::new(PeerError("disk full".into()));
        assert!(matches!(classify(&peer, None), Next::Fatal));
        assert!(matches!(
            classify(&anyhow::anyhow!("local trouble"), None),
            Next::Fatal
        ));
    }

    #[test]
    fn local_time_is_formatted() {
        let s = local_time(1_500_000_000);
        assert_eq!(s.len(), 19, "{s}");
        assert!(s.starts_with("2017-07-1"), "{s}");
    }
}
