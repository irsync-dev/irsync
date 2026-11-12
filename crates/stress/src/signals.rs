//! SIGINT and SIGTERM handling: pass SIGINT on to the current foreground
//! child and to the daemon, SIGTERM to a background filegen, then exit 130.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, OnceLock};

use nix::sys::signal::{
    pthread_sigmask, sigaction, SaFlags, SigAction, SigHandler, SigSet, SigmaskHow, Signal,
};

/// The process-wide daemon pid slot: 0 while no daemon is running.
static DAEMON_PID: OnceLock<Arc<AtomicI32>> = OnceLock::new();

/// The process-wide foreground child slot: the pid of the child that
/// `process::status` or `process::output` is waiting for, 0 while none is.
static CHILD_PID: AtomicI32 = AtomicI32::new(0);

/// The background child slot: the pid of a filegen running alongside a
/// sync (see `filegen::spawn`), 0 while none is.
static BACKGROUND_PID: AtomicI32 = AtomicI32::new(0);

/// The foreground child slot the handler reads (see `process::status`).
pub fn child_slot() -> &'static AtomicI32 {
    &CHILD_PID
}

/// The background child slot the handler reads; it sends that child
/// SIGTERM, which filegen takes as "finish the step, save, exit".
pub fn background_slot() -> &'static AtomicI32 {
    &BACKGROUND_PID
}

extern "C" fn handler(_sig: libc::c_int) {
    // Only atomic loads (`OnceLock::get` once initialised), `kill` and
    // `_exit`: all async-signal-safe.
    let child = CHILD_PID.load(Ordering::SeqCst);
    if child != 0 {
        // SAFETY: `kill` is async-signal-safe.
        unsafe { libc::kill(child, libc::SIGINT) };
    }
    let background = BACKGROUND_PID.load(Ordering::SeqCst);
    if background != 0 {
        // SAFETY: `kill` is async-signal-safe.
        unsafe { libc::kill(background, libc::SIGTERM) };
    }
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
/// slot they read. The handler sends SIGINT to the foreground child (see
/// `child_slot`) and to the pid in the daemon slot, each if non-zero, then
/// exits the process with status 130. `run` stores the
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
        // SAFETY: the handler only loads atomics, calls `kill` and `_exit`.
        unsafe { sigaction(sig, &action) }.expect("installing a signal handler");
    }
    Arc::clone(slot)
}

fn handled() -> SigSet {
    let mut set = SigSet::empty();
    set.add(Signal::SIGINT);
    set.add(Signal::SIGTERM);
    set
}

/// SIGINT and SIGTERM blocked on the calling thread until dropped; a signal
/// that arrives meanwhile stays pending and is delivered on drop. Threads
/// spawned meanwhile inherit the block. Child processes would too (std
/// passes the mask on), but `process::spawn`'s `pre_exec` unblocks both
/// signals in every child.
#[must_use]
pub struct Blocked {
    old: SigSet,
}

/// Blocks SIGINT and SIGTERM on the calling thread until the guard drops.
/// `run` holds it from before the daemon is spawned until the daemon's pid
/// is in the slot, so the handler never misses a daemon that exists;
/// `process::status` and `process::output` do the same for their child.
pub fn block() -> Blocked {
    let mut old = SigSet::empty();
    pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&handled()), Some(&mut old))
        .expect("blocking SIGINT and SIGTERM");
    Blocked { old }
}

impl Drop for Blocked {
    fn drop(&mut self) {
        let _ = pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&self.old), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current_mask() -> SigSet {
        let mut cur = SigSet::empty();
        pthread_sigmask(SigmaskHow::SIG_BLOCK, None, Some(&mut cur)).unwrap();
        cur
    }

    #[test]
    fn block_masks_both_signals_until_dropped() {
        // Each test runs on its own thread, so the mask change stays local.
        let before = current_mask();
        assert!(!before.contains(Signal::SIGINT) && !before.contains(Signal::SIGTERM));
        {
            let _blocked = block();
            let m = current_mask();
            assert!(m.contains(Signal::SIGINT) && m.contains(Signal::SIGTERM));
        }
        let after = current_mask();
        assert!(!after.contains(Signal::SIGINT) && !after.contains(Signal::SIGTERM));
    }
}
