//! Telemetry: a per-session meter of where time and bytes go.

pub mod meter;
pub mod process;
pub mod sampler;

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
    side: Side,
    role: Option<Role>,
    started: std::time::Instant,
}

/// Opens the span for `kind` under the current span. `tracing` needs
/// literal span names, hence one `info_span!` per kind. Fields a phase
/// has (`attempt`, `path`, `mode`, ...) are declared empty for the caller
/// to record.
pub fn phase(kind: PhaseKind, side: Side, role: Option<Role>) -> Phase {
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
        side,
        role,
        started: std::time::Instant::now(),
    }
}

impl Phase {
    pub fn span(&self) -> &tracing::Span {
        &self.span
    }
}

impl Drop for Phase {
    fn drop(&mut self) {
        metrics::histogram!(
            "irsync_phase_duration_seconds",
            "phase" => self.kind.label(),
            "side" => self.side.label(),
            "role" => self.role.map_or("none", Role::label),
        )
        .record(self.started.elapsed().as_secs_f64());
    }
}
