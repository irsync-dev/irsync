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
