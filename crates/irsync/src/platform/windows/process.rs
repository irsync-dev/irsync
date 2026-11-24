//! The process as a whole: its handle window, its resource usage, the
//! machine's name, and the console events that stop it.

use std::io;

use crate::platform::Usage;

// ---------------------------------------------------------------------------
// Limits. Windows has no open-file limit to raise: a process can hold far
// more handles than the receiver's window needs. The window is fixed.

/// What [`handle_limit`] reports: the in-flight window is sized from it as
/// on Unix (two handles per file in flight, a reserve for the rest).
const HANDLE_WINDOW: u64 = 8192;

/// Nothing to raise on Windows: the fixed [`HANDLE_WINDOW`].
pub fn raise_handle_limit() -> io::Result<u64> {
    Ok(HANDLE_WINDOW)
}

/// The fixed [`HANDLE_WINDOW`].
pub fn handle_limit() -> u64 {
    HANDLE_WINDOW
}

// ---------------------------------------------------------------------------
// Process usage.

/// `None`: the `sysinfo` crate irsync uses gives neither the user and
/// kernel CPU times apart nor the peak resident memory on Windows, only
/// their sum and the current memory, so the figures are left out.
pub fn process_usage() -> Option<Usage> {
    None
}

// ---------------------------------------------------------------------------
// The machine's name.

/// This machine's name, from the `COMPUTERNAME` environment variable
/// (anything that is not UTF-16 replaced).
pub fn host_name() -> io::Result<String> {
    std::env::var_os("COMPUTERNAME")
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| io::Error::other("the COMPUTERNAME environment variable is not set"))
}

// ---------------------------------------------------------------------------
// Console events, through tokio. Each listener is set up once and kept, as
// on Unix: an event that arrives while it is set up is never lost.

/// What ends the wait on an open debug port: Ctrl-C or Ctrl-Break.
pub struct ExitSignals(
    tokio::signal::windows::CtrlC,
    tokio::signal::windows::CtrlBreak,
);

/// Listens for Ctrl-C and Ctrl-Break; `None` when that cannot be set up.
pub fn exit_signals() -> Option<ExitSignals> {
    use tokio::signal::windows::{ctrl_break, ctrl_c};
    Some(ExitSignals(ctrl_c().ok()?, ctrl_break().ok()?))
}

/// Returns on Ctrl-C or Ctrl-Break from `signals`; at once when there are
/// none.
pub async fn wait_for_exit_signal(signals: Option<ExitSignals>) {
    let Some(ExitSignals(mut c, mut brk)) = signals else {
        return;
    };
    tokio::select! {
        _ = c.recv() => {}
        _ = brk.recv() => {}
    }
}

/// Returns when the daemon is told to stop: Ctrl-C, Ctrl-Break, or its
/// console closing. Where Ctrl-Break or the close cannot be listened for,
/// the others alone; at once when Ctrl-C cannot be listened for, as on
/// Unix. The listeners are set up when this is first polled.
pub async fn stop_signal() {
    use tokio::signal::windows::{ctrl_break, ctrl_close};
    let brk = async {
        match ctrl_break() {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    let close = async {
        match ctrl_close() {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        () = brk => {}
        () = close => {}
    }
}

/// Never returns and never calls `f`: Windows has no signal to re-read the
/// settings; the daemon is restarted instead. It waits without polling.
pub async fn on_each_reload_signal(f: impl FnMut()) {
    drop(f);
    std::future::pending::<()>().await
}
