//! A daemon started in the background (`irsync ssh-setup`), and told by
//! its pid to reload or to stop.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use anyhow::Context;

/// Tells the daemon `pid` to re-read its settings (SIGHUP).
pub fn send_reload(pid: u32) -> anyhow::Result<()> {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    let raw = i32::try_from(pid).context("daemon pid out of range")?;
    kill(Pid::from_raw(raw), Signal::SIGHUP)
        .with_context(|| format!("telling the daemon (pid {pid}) to reload"))
}

/// Tells the daemon `pid` to stop (SIGTERM). Does not wait for it.
pub fn send_stop(pid: u32) -> anyhow::Result<()> {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    let raw = i32::try_from(pid).context("daemon pid out of range")?;
    kill(Pid::from_raw(raw), Signal::SIGTERM)
        .with_context(|| format!("stopping the old daemon (pid {pid})"))
}

/// Opens the daemon's log `log` to append to, made for the owner only
/// (0600) when it is new.
pub fn open_log(log: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(log)
}

/// Makes `cmd` start its program in a session of its own, so it outlives
/// the terminal and the session it was started from.
pub fn new_session(cmd: &mut Command) {
    // SAFETY: the closure runs in the child between fork and exec, where
    // only async-signal-safe calls are allowed. It only calls setsid,
    // which is, and on failure turns its errno into an `io::Error`, which
    // does not allocate.
    unsafe {
        cmd.pre_exec(|| {
            nix::unistd::setsid()
                .map(|_| ())
                .map_err(std::io::Error::from)
        });
    }
}
