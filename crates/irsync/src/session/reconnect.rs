//! A client session that outlives its connections: dial, run the session,
//! and when the connection fails underneath it dial again (with backoff)
//! and resume the same session until `ReconnectOpts::max` has elapsed. A
//! session stored by an earlier client process with the same key is resumed
//! on start unless `--no-resume` was given.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use futures::StreamExt;
use iroh::endpoint::{Connection, ConnectionError};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use irsync_proto::v1::{error::Code, Hello};
use rand_core::{OsRng, RngCore};
use tracing::Instrument;

use super::engine::{
    client_session_key, record_outcome, role_of, run_client_inner, session_span, Attempt,
    PeerError, SessionRefused, TransportError,
};
use super::{control::hex, LocalSide, Outcome, SessionRecord, CLOSE_SUPERSEDED};
use crate::cli::SyncOptions;
use crate::telemetry::{phase, Phase, PhaseKind, Side};

/// How long to keep trying, and how long to wait between attempts.
#[derive(Debug, Clone, Copy)]
pub struct ReconnectOpts {
    /// Give up once an outage (time since the session last made progress)
    /// lasts this long (zero: never reconnect).
    pub max: Duration,
    /// The first wait; it doubles per attempt up to `backoff_max`, jittered.
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    /// Which source of the command line this session is (for its trace).
    pub source_index: usize,
}

impl Default for ReconnectOpts {
    fn default() -> Self {
        ReconnectOpts {
            max: Duration::from_secs(24 * 3600),
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
            source_index: 0,
        }
    }
}

impl ReconnectOpts {
    /// The wait before reconnect `attempt` (1-based): exponential, capped
    /// at `backoff_max`, then up to a quarter of jitter on top (so clients
    /// at the cap do not synchronise); never below `backoff_min`.
    fn delay(&self, attempt: u32) -> Duration {
        let exp = self
            .backoff_min
            .saturating_mul(1u32 << attempt.saturating_sub(1).min(16));
        let base = exp.min(self.backoff_max).max(self.backoff_min);
        base + base.mul_f64(f64::from(OsRng.next_u32() % 1000) / 4000.0)
    }
}

/// One outage: from the last connection that made progress (or the start)
/// until the session moves again. A connection the daemon acknowledged but
/// that moved no data and placed no file does not end it, so a daemon that
/// accepts and then drops every connection cannot keep the client retrying
/// past `max`.
#[derive(Debug, Clone, Copy)]
struct Outage {
    start: Instant,
    /// Failed connections since the last progress (sets the backoff).
    failures: u32,
}

impl Outage {
    fn new(now: Instant) -> Outage {
        Outage {
            start: now,
            failures: 0,
        }
    }

    /// A connection failed at `now`; `progress`: it moved data or placed a file.
    fn failed(&mut self, now: Instant, progress: bool) {
        if progress {
            *self = Outage::new(now);
        }
        self.failures += 1;
    }

    fn remaining(&self, now: Instant, max: Duration) -> Duration {
        max.saturating_sub(now.saturating_duration_since(self.start))
    }

    fn expired(&self, now: Instant, max: Duration) -> bool {
        now.saturating_duration_since(self.start) >= max
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
    /// The daemon closed the connection with a final answer (see
    /// [`FINAL_CLOSE_CODES`]); `reason` is its close reason, as text.
    ClosedByDaemon {
        code: u32,
        reason: String,
    },
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
        if let Some(&code) = FINAL_CLOSE_CODES
            .iter()
            .find(|&&code| c.error_code == code.into())
        {
            return Next::ClosedByDaemon {
                code,
                reason: String::from_utf8_lossy(&c.reason).into_owned(),
            };
        }
    }
    if err.chain().any(|e| e.is::<TransportError>()) {
        Next::Retry
    } else {
        Next::Fatal
    }
}

/// What to do about a key the daemon will not let use `remote` (`HOST:PATH`
/// as typed). The daemon cannot say which export is missing a grant, or
/// whether the path is exported at all, so the hint names both remedies.
fn no_access_hint(remote: &str, client: &EndpointId) -> String {
    format!(
        "the daemon rejected this client's key: no access to {remote}. On the daemon's \
         machine, irsync grant list {client} shows what this key may reach; \
         irsync grant add EXPORT {client} grants an export, irsync export add NAME:PATH \
         exports a directory (irsync id show prints this id)"
    )
}

/// The message for a connection the daemon closed with a final code. An
/// unauthorized key (code 1: no grants at all) is the likeliest first-run
/// mistake, so that one is the hint.
fn describe_close(code: u32, reason: &str, remote: &str, client: &EndpointId) -> String {
    match code {
        1 => no_access_hint(remote, client),
        _ => format!("the daemon closed the connection: {reason}"),
    }
}

/// A failed one-shot exchange (a listing) explained the way a session's
/// final failure is: the daemon's close code or its refusal become the
/// no-access hint; anything else is returned as is.
pub fn explain(
    err: anyhow::Error,
    conn: Option<&Connection>,
    remote: &str,
    client: &EndpointId,
) -> anyhow::Error {
    match classify(&err, conn) {
        Next::ClosedByDaemon { code, reason } => {
            anyhow::anyhow!(describe_close(code, &reason, remote, client))
        }
        _ => match err
            .chain()
            .find_map(|e| e.downcast_ref::<SessionRefused>())
            .and_then(|r| describe_refusal(r, remote, client))
        {
            Some(hint) => anyhow::anyhow!(hint),
            None => err,
        },
    }
}

/// The hint for a `Hello` the daemon refused as unauthorized (a key with
/// grants, for a path it may not use); `None` for any other refusal.
fn describe_refusal(r: &SessionRefused, remote: &str, client: &EndpointId) -> Option<String> {
    (r.code == Code::Unauthorized).then(|| no_access_hint(remote, client))
}

/// `secs` since the epoch as local time, `YYYY-MM-DD HH:MM:SS`.
fn local_time(secs: i64) -> String {
    let t = secs as nix::libc::time_t;
    // SAFETY: an all-zero `tm` is a valid value; `localtime_r` only writes
    // into the `tm` it is given and reads `t`.
    let mut tm: nix::libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !nix::libc::localtime_r(&t, &mut tm).is_null() };
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
/// other error is returned as is. The whole run is one `session` span; each
/// attempt is a `connect` span under it.
#[allow(clippy::too_many_arguments)]
pub async fn run_with_reconnect(
    endpoint: &Endpoint,
    target: impl Into<EndpointAddr>,
    remote: &str,
    hello_for: impl Fn(Option<[u8; 16]>) -> Hello,
    local: LocalSide,
    sync_opts: &SyncOptions,
    opts: ReconnectOpts,
    on_state: impl FnMut(ConnState),
) -> anyhow::Result<Outcome> {
    let target: EndpointAddr = target.into();
    let span = session_span(
        Side::Client,
        &hello_for(None),
        target.id.fmt_short().to_string(),
        Some(opts.source_index),
        &endpoint.id().to_string(),
    );
    let res = attempts(
        endpoint, target, remote, hello_for, local, sync_opts, opts, &span, on_state,
    )
    .instrument(span.clone())
    .await;
    if let Ok(out) = &res {
        record_outcome(&span, out);
    }
    res
}

#[allow(clippy::too_many_arguments)]
async fn attempts(
    endpoint: &Endpoint,
    target: impl Into<EndpointAddr>,
    remote: &str,
    hello_for: impl Fn(Option<[u8; 16]>) -> Hello,
    local: LocalSide,
    sync_opts: &SyncOptions,
    opts: ReconnectOpts,
    span: &tracing::Span,
    mut on_state: impl FnMut(ConnState),
) -> anyhow::Result<Outcome> {
    let bus = local.hooks.bus.clone();
    // How the latest connection was made, for the outcome.
    let kind = std::sync::Arc::new(std::sync::Mutex::new(None));
    // The attempt's `connect` phase, open until the connection reports its path.
    let connecting: std::sync::Arc<std::sync::Mutex<Option<Phase>>> = Default::default();
    let mut on_state = {
        let (kind, connecting) = (kind.clone(), connecting.clone());
        move |s: ConnState| {
            if matches!(s, ConnState::Direct | ConnState::Relay) {
                *kind.lock().unwrap() = Some(s);
                if let Some(p) = connecting.lock().unwrap().take() {
                    p.span().record(
                        "path",
                        if s == ConnState::Direct {
                            "direct"
                        } else {
                            "relay"
                        },
                    );
                }
            }
            bus.emit(super::Event::Conn(s));
            on_state(s);
        }
    };
    let target: EndpointAddr = target.into();
    let key = client_session_key(target.id, &hello_for(None), &local, sync_opts).await?;
    let mut session = match stored_session(&local, key).await {
        Some(rec) if !sync_opts.no_resume => {
            if !sync_opts.quiet {
                tracing::warn!(
                    "resuming session {} from {}",
                    hex(&rec.id[..4]),
                    local_time(rec.updated_at)
                );
            }
            Some(rec.id)
        }
        Some(rec) => {
            tracing::info!(session = %hex(&rec.id), "--no-resume: starting a new session");
            None
        }
        None => None,
    };
    let role = role_of(Side::Client, hello_for(None).direction);
    let mut outage = Outage::new(Instant::now());
    let mut attempt = 0u32;
    loop {
        on_state(match attempt {
            0 => ConnState::Connecting,
            n => ConnState::Reconnecting { attempt: n },
        });
        let mut tried = Attempt::default();
        let connect = phase(PhaseKind::Connect, Side::Client, role, None);
        connect.span().record("attempt", attempt);
        let dialled = endpoint
            .connect(target.clone(), irsync_proto::ALPN)
            .instrument(connect.span().clone())
            .await;
        *connecting.lock().unwrap() = Some(connect);
        let (res, conn) = match dialled {
            Ok(conn) => {
                let run = run_client_inner(
                    conn.clone(),
                    hello_for(session),
                    local.clone(),
                    sync_opts,
                    span,
                    &mut tried,
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
        connecting.lock().unwrap().take();
        let next = res.as_ref().err().map(|e| classify(e, conn.as_ref()));
        // Why a retry would dial again: the dial failed, or a connection dropped.
        let reason = if conn.is_some() { "lost" } else { "dial" };
        if let Some(c) = &conn {
            c.close(0u32.into(), b"done");
        }
        let err = match res {
            Ok(mut out) => {
                out.reconnects = attempt;
                out.conn = *kind.lock().unwrap();
                return Ok(out);
            }
            Err(e) => e,
        };
        if tried.acked.is_some() {
            session = tried.acked;
        }
        let progress = tried.progress.load(Ordering::Relaxed);
        match next {
            Some(Next::Fresh) if session.is_some() => {
                // Gone on the daemon (finished or expired): start over at once.
                tracing::warn!(error = %format!("{err:#}"), "the daemon does not know the session; starting a new one");
                session = None;
                continue;
            }
            Some(Next::Retry) => {}
            Some(Next::ClosedByDaemon { code, reason }) => {
                return Err(anyhow::anyhow!(describe_close(
                    code,
                    &reason,
                    remote,
                    &endpoint.id()
                )));
            }
            _ => {
                let refused = err
                    .chain()
                    .find_map(|e| e.downcast_ref::<SessionRefused>())
                    .and_then(|r| describe_refusal(r, remote, &endpoint.id()));
                return Err(match refused {
                    Some(hint) => anyhow::anyhow!(hint),
                    None => err,
                });
            }
        }
        let now = Instant::now();
        outage.failed(now, progress);
        if outage.expired(now, opts.max) {
            return Err(err);
        }
        attempt += 1;
        // Kept current on the `session` span, so a session that fails in the
        // end still says how often it reconnected.
        span.record("reconnects", u64::from(attempt));
        // Never sleep past the end of the outage budget: one last try then.
        let wait = opts
            .delay(outage.failures)
            .min(outage.remaining(now, opts.max));
        tracing::warn!(
            error = %format!("{err:#}"),
            attempt,
            wait_ms = wait.as_millis() as u64,
            "connection lost; reconnecting"
        );
        if crate::telemetry::enabled() {
            let mut labels = crate::telemetry::labels(Side::Client, role, None);
            labels.push(metrics::Label::from_static_parts("reason", reason));
            metrics::counter!("irsync_reconnects_total", labels).increment(1);
        }
        let reconnect = phase(PhaseKind::Reconnect, Side::Client, role, None);
        reconnect.span().record("attempt", attempt);
        reconnect.span().record("reason", format!("{err:#}"));
        tokio::time::sleep(wait)
            .instrument(reconnect.span().clone())
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unauthorized_close_and_refusal_share_the_hint() {
        let id = iroh::SecretKey::from_bytes(&[7; 32]).public();
        let close = describe_close(1, "unauthorized", "theoden:/srv/x", &id);
        let refused = describe_refusal(
            &SessionRefused {
                code: Code::Unauthorized,
                message: "no access to \"/srv/x\"".into(),
            },
            "theoden:/srv/x",
            &id,
        )
        .unwrap();
        for msg in [&close, &refused] {
            assert!(msg.contains("no access to theoden:/srv/x"), "{msg}");
            assert!(msg.contains(&format!("irsync grant list {id}")), "{msg}");
            assert!(
                msg.contains(&format!("irsync grant add EXPORT {id}")),
                "{msg}"
            );
            assert!(msg.contains("irsync export add NAME:PATH"), "{msg}");
            assert!(msg.contains("irsync id show"), "{msg}");
        }
        assert!(describe_refusal(
            &SessionRefused {
                code: Code::ReadOnly,
                message: "x".into()
            },
            "t:/p",
            &id
        )
        .is_none());
        let other = describe_close(2, "hello timeout", "t:/p", &id);
        assert!(
            other.contains("hello timeout") && !other.contains("grant"),
            "{other}"
        );
    }

    #[test]
    fn backoff_grows_to_the_cap_and_never_drops_below_the_floor() {
        let o = ReconnectOpts::default();
        for attempt in 1..40 {
            let d = o.delay(attempt);
            assert!(d >= Duration::from_secs(1), "{attempt}: {d:?}");
            assert!(d <= Duration::from_secs(75), "{attempt}: {d:?}");
        }
        assert!(o.delay(1) < Duration::from_millis(1300));
        assert!(o.delay(20) >= Duration::from_secs(60));
    }

    #[test]
    fn acked_without_progress_does_not_reset_outage() {
        let max = Duration::from_secs(3);
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut o = Outage::new(t0);
        // Every connection is acknowledged and dropped before moving data.
        for i in 1..=20u64 {
            o.failed(at(i * 200), false);
        }
        assert_eq!(o.failures, 20);
        assert!(o.expired(at(4000), max), "acks alone kept the outage open");
        // A connection that moved data ends the outage and the backoff.
        o.failed(at(4000), true);
        assert_eq!(o.failures, 1);
        assert!(!o.expired(at(5000), max));
        assert!(o.expired(at(7000), max));
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
