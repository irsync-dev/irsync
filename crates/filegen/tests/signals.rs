//! Signal flag behaviour. A test binary of its own: raising signals in the
//! library's test process would interrupt its concurrent runs.

use std::sync::atomic::Ordering;

use nix::sys::signal::{raise, Signal};

#[test]
fn first_signal_wins_and_install_clears() {
    let flag = filegen::signals::install();
    assert_eq!(flag.load(Ordering::SeqCst), 0);
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
    // A second SIGINT would `_exit(130)`; that is covered at binary level.
}
