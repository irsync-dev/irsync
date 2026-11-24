//! A daemon started in the background (`irsync ssh-setup`), and told by
//! its pid to reload or to stop: not available on Windows yet. Each entry
//! point that can fail does, with a plain message.

use std::fs::File;
use std::path::Path;
use std::process::Command;

/// What every entry point says.
const NOT_YET: &str =
    "starting and signalling a background daemon (ssh setup) is not available on Windows yet";

/// Not on Windows yet.
pub fn send_reload(_pid: u32) -> anyhow::Result<()> {
    anyhow::bail!(NOT_YET)
}

/// Not on Windows yet.
pub fn send_stop(_pid: u32) -> anyhow::Result<()> {
    anyhow::bail!(NOT_YET)
}

/// Not on Windows yet (`Unsupported`): the daemon is never started, so
/// no log is opened for it.
pub fn open_log(_log: &Path) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        NOT_YET,
    ))
}

/// Does nothing: on Windows [`open_log`] fails first, so no daemon is
/// started.
pub fn new_session(_cmd: &mut Command) {}
