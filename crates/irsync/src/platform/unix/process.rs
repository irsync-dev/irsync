//! The process as a whole: its open-file limit, its resource usage, the
//! machine's name, and the signals that stop or reload it.

use std::io;
use std::time::Duration;

use nix::sys::resource::{getrlimit, getrusage, rlim_t, setrlimit, Resource, UsageWho};

// ---------------------------------------------------------------------------
// Limits: the open-file limit (`RLIMIT_NOFILE`). The receiver holds a
// descriptor per file in flight (two for a `DELTA`), so the soft limit is
// raised at startup and the in-flight window is sized from what remains.

/// `OPEN_MAX` from macOS's `<sys/syslimits.h>`: the soft `RLIMIT_NOFILE`
/// its `setrlimit(2)` man page says to stay within (`min(OPEN_MAX,
/// rlim_max)`); older kernels refused more. The `libc` crate does not
/// export it, so it is spelled out here. It leaves room for the whole
/// in-flight window, so a higher soft limit (which a shell may already
/// have set) is left as it is rather than lowered.
#[cfg(target_os = "macos")]
const OPEN_MAX: rlim_t = 10240;

/// The soft limit a raise aims for: the hard limit, or on macOS at most
/// [`OPEN_MAX`].
fn nofile_target(hard: rlim_t) -> rlim_t {
    #[cfg(target_os = "macos")]
    let hard = hard.min(OPEN_MAX);
    hard
}

/// A limit as a `u64`. `rlim_t` is `u64` on Linux and macOS but `i64` on
/// FreeBSD, where a limit is never negative ("no limit" is `i64::MAX`).
// On Linux and macOS this converts a type to itself.
#[allow(clippy::useless_conversion, clippy::unnecessary_fallible_conversions)]
fn as_u64(limit: rlim_t) -> u64 {
    u64::try_from(limit).unwrap_or(u64::MAX)
}

/// Raises the soft open-file limit to [`nofile_target`] and returns the
/// soft limit in force afterwards (unchanged when it is already there or
/// the raise is refused).
pub fn raise_handle_limit() -> io::Result<u64> {
    let (soft, hard) = getrlimit(Resource::RLIMIT_NOFILE)?;
    let target = nofile_target(hard);
    if target <= soft {
        return Ok(as_u64(soft));
    }
    match setrlimit(Resource::RLIMIT_NOFILE, target, hard) {
        Ok(()) => Ok(as_u64(target)),
        Err(e) => {
            tracing::debug!(soft, hard, error = %e, "open-file limit not raised");
            Ok(as_u64(soft))
        }
    }
}

/// The current soft open-file limit.
pub fn handle_limit() -> u64 {
    getrlimit(Resource::RLIMIT_NOFILE).map_or(u64::MAX, |(soft, _)| as_u64(soft))
}

// ---------------------------------------------------------------------------
// Process usage.

/// This process's resource usage so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// CPU time in user mode.
    pub cpu_user: Duration,
    /// CPU time in the kernel.
    pub cpu_sys: Duration,
    /// The peak resident memory, in bytes.
    pub max_rss: u64,
}

/// Converts `ru_maxrss` (KiB on Linux, bytes on macOS) to bytes.
fn rss_bytes(raw: i64) -> u64 {
    let raw = u64::try_from(raw).unwrap_or(0);
    if cfg!(target_os = "macos") {
        raw
    } else {
        raw * 1024
    }
}

fn timeval(t: nix::sys::time::TimeVal) -> Duration {
    use nix::sys::time::TimeValLike;
    Duration::from_micros(u64::try_from(t.num_microseconds()).unwrap_or(0))
}

/// This process's resource usage (`getrusage`); `None` when it cannot be
/// read.
pub fn process_usage() -> Option<Usage> {
    let u = getrusage(UsageWho::RUSAGE_SELF).ok()?;
    Some(Usage {
        cpu_user: timeval(u.user_time()),
        cpu_sys: timeval(u.system_time()),
        max_rss: rss_bytes(u.max_rss()),
    })
}

// ---------------------------------------------------------------------------
// The machine's name.

/// This machine's host name, as `gethostname` gives it (bytes that are not
/// UTF-8 replaced). The error carries the system library's own text.
pub fn host_name() -> io::Result<String> {
    nix::unistd::gethostname()
        .map(|name| name.to_string_lossy().into_owned())
        .map_err(io::Error::other)
}

// ---------------------------------------------------------------------------
// Signals. Each listener is set up once and kept: a signal that arrives
// while it is set up is never lost. Once tokio listens for a signal, it
// keeps it from the default action for the life of the process.

/// What ends the wait on an open debug port: SIGINT or SIGHUP. SIGTERM
/// keeps its default action.
pub struct ExitSignals(tokio::signal::unix::Signal, tokio::signal::unix::Signal);

/// Listens for SIGINT and SIGHUP; `None` when that cannot be set up.
pub fn exit_signals() -> Option<ExitSignals> {
    use tokio::signal::unix::{signal, SignalKind};
    Some(ExitSignals(
        signal(SignalKind::interrupt()).ok()?,
        signal(SignalKind::hangup()).ok()?,
    ))
}

/// Returns on SIGINT or SIGHUP from `signals`; at once when there are none.
pub async fn wait_for_exit_signal(signals: Option<ExitSignals>) {
    let Some(ExitSignals(mut int, mut hup)) = signals else {
        return;
    };
    tokio::select! {
        _ = int.recv() => {}
        _ = hup.recv() => {}
    }
}

/// Returns when the daemon is told to stop: SIGTERM or SIGINT (Ctrl-C).
/// Where SIGTERM cannot be listened for, SIGINT alone; at once when SIGINT
/// cannot be listened for. The listeners are set up when this is first
/// polled.
pub async fn stop_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    match signal(SignalKind::terminate()) {
        Ok(mut term) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
        }
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

/// Calls `f` each time the daemon is told to re-read its settings
/// (SIGHUP), for as long as the process runs; returns at once when SIGHUP
/// cannot be listened for. The listener is set up when this is first
/// polled.
pub async fn on_each_reload_signal(mut f: impl FnMut()) {
    use tokio::signal::unix::{signal, SignalKind};
    let Ok(mut hangup) = signal(SignalKind::hangup()) else {
        return;
    };
    while hangup.recv().await.is_some() {
        f();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raise_handle_limit_lifts_the_soft_limit_to_the_target() {
        let (soft, hard) = getrlimit(Resource::RLIMIT_NOFILE).unwrap();
        let target = nofile_target(hard);
        // Lower the soft limit to one below the target (far above what
        // this process uses), then ask for the raise. Starting from the
        // current soft limit would not do: on macOS a shell's `ulimit -n`
        // may already sit above `OPEN_MAX`, where there is nothing to raise.
        let lowered = target - 1;
        setrlimit(Resource::RLIMIT_NOFILE, lowered, hard).unwrap();
        assert_eq!(handle_limit(), as_u64(lowered));
        let raised = raise_handle_limit().unwrap();
        assert_eq!(raised, as_u64(target));
        assert_eq!(handle_limit(), as_u64(target));
        // A soft limit already at or above the target is left alone.
        assert_eq!(raise_handle_limit().unwrap(), as_u64(target));
        setrlimit(Resource::RLIMIT_NOFILE, soft, hard).unwrap();
    }

    #[test]
    fn rss_bytes_units() {
        if cfg!(target_os = "macos") {
            assert_eq!(rss_bytes(2048), 2048);
        } else {
            assert_eq!(rss_bytes(2048), 2048 * 1024);
        }
    }
}
