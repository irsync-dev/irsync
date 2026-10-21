//! Spawning child processes, surviving the Linux ETXTBSY fork race.

use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Output};
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread;
use std::time::Duration;

use crate::signals;

const MAX_ATTEMPTS: u32 = 100;
const RETRY_DELAY: Duration = Duration::from_millis(10);

/// Spawns `cmd`, retrying when the kernel reports ETXTBSY (a script that
/// another thread's fork still holds open for writing) for up to about
/// one second. Every other error is returned at once.
///
/// The child starts with SIGINT and SIGTERM unblocked even when the caller
/// has them blocked (see `signals::block`): std passes the signal mask on
/// to the child, and a daemon that inherited the block would never see the
/// SIGINT that stops it.
///
/// `spawn` alone leaves the signal handler's foreground child slot alone
/// (the daemon has its own slot); `status` and `output` set it.
pub fn spawn(cmd: &mut Command) -> io::Result<Child> {
    // SAFETY: between fork and exec the closure only calls `sigemptyset`,
    // `sigaddset` and `pthread_sigmask`, which are async-signal-safe and
    // do not allocate. Registered once, outside the ETXTBSY retry loop; a
    // caller passing the same `Command` again only stacks an identical,
    // harmless unblock.
    unsafe {
        cmd.pre_exec(|| {
            let mut set = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
            libc::sigemptyset(set.as_mut_ptr());
            libc::sigaddset(set.as_mut_ptr(), libc::SIGINT);
            libc::sigaddset(set.as_mut_ptr(), libc::SIGTERM);
            match libc::pthread_sigmask(libc::SIG_UNBLOCK, set.as_ptr(), std::ptr::null_mut()) {
                0 => Ok(()),
                e => Err(io::Error::from_raw_os_error(e)),
            }
        });
    }
    let mut attempt = 1;
    loop {
        match cmd.spawn() {
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && attempt < MAX_ATTEMPTS => {
                attempt += 1;
                thread::sleep(RETRY_DELAY);
            }
            other => return other,
        }
    }
}

/// `spawn`, then `wait` on the child, with its pid in `slot` meanwhile.
/// SIGINT and SIGTERM are blocked from before the spawn until the pid is
/// stored, so a signal handler reading `slot` never misses a child that
/// exists. The slot is cleared only if it still holds this child's pid.
fn tracked<T>(
    slot: &AtomicI32,
    cmd: &mut Command,
    wait: impl FnOnce(Child) -> io::Result<T>,
) -> io::Result<T> {
    let blocked = signals::block();
    let child = spawn(cmd)?;
    let pid = child.id() as i32;
    slot.store(pid, Ordering::SeqCst);
    drop(blocked); // a pending signal is delivered here, with the pid set
    let result = wait(child);
    let _ = slot.compare_exchange(pid, 0, Ordering::SeqCst, Ordering::SeqCst);
    result
}

fn status_in(slot: &AtomicI32, cmd: &mut Command) -> io::Result<ExitStatus> {
    tracked(slot, cmd, |mut c| c.wait())
}

/// `spawn` followed by `wait_with_output`; the caller has set the stdio it
/// wants. The child is the signal handler's foreground child while it runs.
pub fn output(cmd: &mut Command) -> io::Result<Output> {
    tracked(signals::child_slot(), cmd, Child::wait_with_output)
}

/// `spawn` followed by `wait`. The child is the signal handler's foreground
/// child while it runs.
pub fn status(cmd: &mut Command) -> io::Result<ExitStatus> {
    status_in(signals::child_slot(), cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_until_the_script_is_no_longer_busy() {
        // Keep a script open for writing in this thread, spawn it from another
        // thread (which fails with ETXTBSY until the handle is dropped), then
        // drop the handle: the spawn must succeed.
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("busy.sh");
        let mut f = std::fs::File::create(&path).unwrap();
        use std::io::Write as _;
        f.write_all(b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let p = path.clone();
        let h = std::thread::spawn(move || status(&mut Command::new(p)));
        std::thread::sleep(std::time::Duration::from_millis(100));
        drop(f);
        assert!(h.join().unwrap().unwrap().success());
    }

    #[test]
    #[cfg(target_os = "linux")] // reads /proc
    fn the_child_starts_with_sigint_and_sigterm_unblocked() {
        // `grep` itself reads its mask: a shell in between might reset it.
        let _blocked = crate::signals::block();
        let out = output(
            Command::new("grep")
                .args(["SigBlk", "/proc/self/status"])
                .stdout(std::process::Stdio::piped()),
        )
        .unwrap();
        let line = String::from_utf8(out.stdout).unwrap();
        let hex = line.trim().strip_prefix("SigBlk:").unwrap().trim();
        let mask = u64::from_str_radix(hex, 16).unwrap();
        let bit = |sig: i32| 1u64 << (sig - 1);
        assert_eq!(mask & (bit(libc::SIGINT) | bit(libc::SIGTERM)), 0, "{line}");
    }

    #[test]
    fn a_waited_for_child_is_in_the_slot_until_it_exits() {
        // A local slot: parallel tests share the global one. The shell
        // waits on a background sleep so a trapped SIGINT ends it at once,
        // and touches `ready` once its trap is set.
        use std::sync::Arc;
        use std::time::Instant;
        let t = tempfile::tempdir().unwrap();
        let ready = t.path().join("ready");
        let slot = Arc::new(AtomicI32::new(0));
        let s = Arc::clone(&slot);
        let r = ready.clone();
        let h = thread::spawn(move || {
            status_in(
                &s,
                Command::new("sh")
                    .args([
                        "-c",
                        "trap 'kill $!; exit 7' INT; sleep 30 & : > \"$1\"; wait",
                        "sh",
                    ])
                    .arg(r),
            )
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let pid = loop {
            let pid = slot.load(Ordering::SeqCst);
            if pid != 0 && ready.exists() {
                break pid;
            }
            assert!(Instant::now() < deadline, "child never in the slot");
            thread::sleep(Duration::from_millis(10));
        };
        let p = nix::unistd::Pid::from_raw(pid);
        nix::sys::signal::kill(p, nix::sys::signal::Signal::SIGINT).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !h.is_finished() {
            if Instant::now() >= deadline {
                let _ = nix::sys::signal::kill(p, nix::sys::signal::Signal::SIGKILL);
                panic!("child still running 5 s after SIGINT");
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(h.join().unwrap().unwrap().code(), Some(7));
        assert_eq!(slot.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn other_errors_are_not_retried() {
        let t = std::time::Instant::now();
        assert!(spawn(&mut Command::new("/nonexistent/program")).is_err());
        assert!(t.elapsed() < std::time::Duration::from_millis(500));
    }
}
