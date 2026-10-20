//! SIGINT and SIGTERM handling: the run loop polls a flag between steps.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, OnceLock};

use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};

/// The process-wide flag: 0, or the number of the first signal received.
static FLAG: OnceLock<Arc<AtomicI32>> = OnceLock::new();

extern "C" fn handler(sig: libc::c_int) {
    // Only an atomic load (`OnceLock::get` once initialised), a CAS and
    // `_exit`: all async-signal-safe.
    let Some(flag) = FLAG.get() else {
        return;
    };
    if flag
        .compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
        && sig == libc::SIGINT
    {
        // A second Ctrl-C while shutting down: leave now.
        // SAFETY: `_exit` is async-signal-safe and never returns.
        unsafe { libc::_exit(130) };
    }
}

/// Installs the SIGINT and SIGTERM handlers and returns the flag they
/// set: 0 until a signal arrives, then that signal's number (the first
/// one wins). A second SIGINT while the flag is set exits the process at
/// once with status 130. Each call clears the flag. With `SA_RESTART`,
/// so the step in flight finishes rather than failing with `EINTR`.
pub fn install() -> Arc<AtomicI32> {
    let flag = FLAG.get_or_init(|| Arc::new(AtomicI32::new(0)));
    flag.store(0, Ordering::SeqCst);
    let action = SigAction::new(
        SigHandler::Handler(handler),
        SaFlags::SA_RESTART,
        SigSet::empty(),
    );
    for sig in [Signal::SIGINT, Signal::SIGTERM] {
        // SAFETY: the handler only touches an atomic and calls `_exit`.
        unsafe { sigaction(sig, &action) }.expect("installing a signal handler");
    }
    Arc::clone(flag)
}

/// The name of a signal number for the log, e.g. `SIGTERM`.
pub fn name(sig: i32) -> String {
    Signal::try_from(sig).map_or_else(|_| format!("signal {sig}"), |s| s.as_str().to_string())
}
