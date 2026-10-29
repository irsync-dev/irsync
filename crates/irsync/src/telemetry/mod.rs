//! Telemetry: a per-session meter of where time and bytes go, and the
//! subscriber and metrics recorder its sinks need.

pub mod init;
pub mod meter;
pub mod process;
pub mod publish;
pub mod sampler;

pub use init::{init, OtlpConfig, Telemetry, TelemetryConfig};

use std::sync::atomic::{AtomicBool, Ordering};

/// Which end of the connection this process is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Client,
    Daemon,
}

impl Side {
    pub fn label(self) -> &'static str {
        match self {
            Side::Client => "client",
            Side::Daemon => "daemon",
        }
    }
}

/// Which direction data flows for this end of the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Sender,
    Receiver,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Role::Sender => "sender",
            Role::Receiver => "receiver",
        }
    }
}

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Turns telemetry collection on or off for the whole process.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Whether telemetry collection is on.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// The labels every irsync metric carries: `side`, `role` and, on the
/// daemon, `export`.
pub fn labels(side: Side, role: Role, export: Option<&str>) -> Vec<metrics::Label> {
    let mut v = vec![
        metrics::Label::from_static_parts("side", side.label()),
        metrics::Label::from_static_parts("role", role.label()),
    ];
    if let Some(e) = export {
        v.push(metrics::Label::new("export", e.to_owned()));
    }
    v
}

/// Keeps a gauge one higher while alive; inert when telemetry is off.
pub struct Held(Option<metrics::Gauge>);

/// Raises gauge `name` with `labels` until the returned guard drops.
pub fn hold(name: &'static str, labels: impl FnOnce() -> Vec<metrics::Label>) -> Held {
    if !enabled() {
        return Held(None);
    }
    let g = metrics::gauge!(name, labels());
    g.increment(1.0);
    Held(Some(g))
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some(g) = &self.0 {
            g.decrement(1.0);
        }
    }
}

/// The phases of a session, as named in the trace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhaseKind {
    Connect,
    Handshake,
    List,
    Plan,
    Transfer,
    Delete,
    Reconnect,
}

impl PhaseKind {
    pub fn label(self) -> &'static str {
        match self {
            PhaseKind::Connect => "connect",
            PhaseKind::Handshake => "handshake",
            PhaseKind::List => "list",
            PhaseKind::Plan => "plan",
            PhaseKind::Transfer => "transfer",
            PhaseKind::Delete => "delete",
            PhaseKind::Reconnect => "reconnect",
        }
    }
}

/// A phase's span; dropping it records the phase's wall time.
pub struct Phase {
    span: tracing::Span,
    kind: PhaseKind,
    /// The metric labels and start, only while telemetry is on.
    timed: Option<(Vec<metrics::Label>, std::time::Instant)>,
}

/// Opens the span for `kind` under the current span. `tracing` needs
/// literal span names, hence one `info_span!` per kind. Fields a phase
/// has (`attempt`, `path`, `mode`, ...) are declared empty for the caller
/// to record. With telemetry off no clock is read and nothing recorded.
pub fn phase(kind: PhaseKind, side: Side, role: Role, export: Option<&str>) -> Phase {
    use tracing::info_span as span;
    let span = match kind {
        PhaseKind::Connect => span!(
            "connect",
            attempt = tracing::field::Empty,
            path = tracing::field::Empty
        ),
        PhaseKind::Handshake => span!("handshake"),
        PhaseKind::List => span!("list"),
        PhaseKind::Plan => span!("plan"),
        PhaseKind::Transfer => span!("transfer"),
        PhaseKind::Delete => span!("delete", mode = tracing::field::Empty),
        PhaseKind::Reconnect => span!(
            "reconnect",
            attempt = tracing::field::Empty,
            reason = tracing::field::Empty
        ),
    };
    Phase {
        span,
        kind,
        timed: enabled().then(|| (labels(side, role, export), std::time::Instant::now())),
    }
}

impl Phase {
    pub fn span(&self) -> &tracing::Span {
        &self.span
    }
}

impl Drop for Phase {
    fn drop(&mut self) {
        let Some((mut labels, started)) = self.timed.take() else {
            return;
        };
        labels.push(metrics::Label::from_static_parts(
            "phase",
            self.kind.label(),
        ));
        metrics::histogram!("irsync_phase_duration_seconds", labels)
            .record(started.elapsed().as_secs_f64());
    }
}
