//! Signal flag behaviour. A test binary of its own: raising signals in the
//! library's test process would interrupt its concurrent runs. One test,
//! so nothing else in this process races the handlers.
#![cfg(unix)]

use std::sync::atomic::Ordering;

use filegen::platform::{self, testutil};
use testutil::{SIGINT, SIGTERM};

#[test]
fn first_signal_wins_install_clears_and_syscalls_restart() {
    let flag = platform::install_interrupt();
    assert_eq!(flag.load(Ordering::SeqCst), 0);

    // SA_RESTART: a step in flight finishes instead of failing with EINTR.
    // This is the only test in this binary, so nothing else in the process
    // races the probe.
    for sig in [SIGINT, SIGTERM] {
        assert!(testutil::interrupt_restarts_syscalls(sig), "{sig}");
    }

    testutil::raise_interrupt(SIGTERM);
    assert_eq!(flag.load(Ordering::SeqCst), SIGTERM);
    testutil::raise_interrupt(SIGTERM);
    assert_eq!(flag.load(Ordering::SeqCst), SIGTERM);
    assert_eq!(platform::interrupt_name(SIGTERM), "SIGTERM");
    // Reinstalling clears the flag; the handler keeps the same flag.
    let again = platform::install_interrupt();
    assert_eq!(flag.load(Ordering::SeqCst), 0);
    testutil::raise_interrupt(SIGINT);
    assert_eq!(again.load(Ordering::SeqCst), SIGINT);
    // A second SIGINT would `_exit(130)`. The binary-level tests
    // `cli_sigint_resumable` and `cli_sigterm_resumable` cover the first
    // signal's orderly shutdown; the immediate exit on a second SIGINT is
    // not exercised (two signals sent back to back can coalesce into one).
}
