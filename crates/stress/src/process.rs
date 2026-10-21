//! Spawning child processes, surviving the Linux ETXTBSY fork race.

use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Output};
use std::thread;
use std::time::Duration;

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
pub fn spawn(cmd: &mut Command) -> io::Result<Child> {
    // SAFETY: between fork and exec the closure only calls `sigemptyset`,
    // `sigaddset` and `pthread_sigmask`, which are async-signal-safe and
    // do not allocate.
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

/// `spawn` followed by `wait_with_output`; the caller has set the stdio it wants.
pub fn output(cmd: &mut Command) -> io::Result<Output> {
    spawn(cmd)?.wait_with_output()
}

/// `spawn` followed by `wait`.
pub fn status(cmd: &mut Command) -> io::Result<ExitStatus> {
    spawn(cmd)?.wait()
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
    fn other_errors_are_not_retried() {
        let t = std::time::Instant::now();
        assert!(spawn(&mut Command::new("/nonexistent/program")).is_err());
        assert!(t.elapsed() < std::time::Duration::from_millis(500));
    }
}
