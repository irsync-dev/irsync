//! Signal flag behaviour. A test binary of its own: raising signals in the
//! library's test process would interrupt its concurrent runs. One test,
//! so nothing else in this process races the handlers.

use std::sync::atomic::Ordering;

use nix::sys::signal::{raise, sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};

#[test]
fn first_signal_wins_install_clears_and_syscalls_restart() {
    let flag = filegen::signals::install();
    assert_eq!(flag.load(Ordering::SeqCst), 0);

    // SA_RESTART: a step in flight finishes instead of failing with EINTR.
    for sig in [Signal::SIGINT, Signal::SIGTERM] {
        let probe = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
        // SAFETY: the installed action is put straight back.
        let installed = unsafe { sigaction(sig, &probe) }.unwrap();
        unsafe { sigaction(sig, &installed) }.unwrap();
        assert!(installed.flags().contains(SaFlags::SA_RESTART), "{sig:?}");
    }

    raise(Signal::SIGTERM).unwrap();
    assert_eq!(flag.load(Ordering::SeqCst), libc::SIGTERM);
    raise(Signal::SIGTERM).unwrap();
    assert_eq!(flag.load(Ordering::SeqCst), libc::SIGTERM);
    assert_eq!(filegen::signals::name(libc::SIGTERM), "SIGTERM");
    // Reinstalling clears the flag; the handler keeps the same flag.
    let again = filegen::signals::install();
    assert_eq!(flag.load(Ordering::SeqCst), 0);
    raise(Signal::SIGINT).unwrap();
    assert_eq!(again.load(Ordering::SeqCst), libc::SIGINT);
    // A second SIGINT would `_exit(130)`. The binary-level tests
    // `cli_sigint_resumable` and `cli_sigterm_resumable` cover the first
    // signal's orderly shutdown; the immediate exit on a second SIGINT is
    // not exercised (two signals sent back to back can coalesce into one).
}
