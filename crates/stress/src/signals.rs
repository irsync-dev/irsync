//! SIGINT and SIGTERM handling: pass SIGINT on to the daemon, then exit 130.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, OnceLock};

use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};

/// The process-wide daemon pid slot: 0 while no daemon is running.
static DAEMON_PID: OnceLock<Arc<AtomicI32>> = OnceLock::new();

extern "C" fn handler(_sig: libc::c_int) {
    // Only an atomic load (`OnceLock::get` once initialised), `kill` and
    // `_exit`: all async-signal-safe.
    if let Some(slot) = DAEMON_PID.get() {
        let pid = slot.load(Ordering::SeqCst);
        if pid != 0 {
            // SAFETY: `kill` is async-signal-safe.
            unsafe { libc::kill(pid, libc::SIGINT) };
        }
    }
    // SAFETY: `_exit` is async-signal-safe and never returns.
    unsafe { libc::_exit(130) };
}

/// Installs the SIGINT and SIGTERM handlers and returns the daemon pid
/// slot they read. The handler sends SIGINT to the pid in the slot (if
/// non-zero), then exits the process with status 130. `run` stores the
/// pid once the daemon is up and clears it after stopping the daemon.
pub fn install() -> Arc<AtomicI32> {
    let slot = DAEMON_PID.get_or_init(|| Arc::new(AtomicI32::new(0)));
    slot.store(0, Ordering::SeqCst);
    let action = SigAction::new(
        SigHandler::Handler(handler),
        SaFlags::empty(),
        SigSet::empty(),
    );
    for sig in [Signal::SIGINT, Signal::SIGTERM] {
        // SAFETY: the handler only loads an atomic, calls `kill` and `_exit`.
        unsafe { sigaction(sig, &action) }.expect("installing a signal handler");
    }
    Arc::clone(slot)
}
