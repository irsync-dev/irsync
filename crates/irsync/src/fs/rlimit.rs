//! The process's open-file limit (`RLIMIT_NOFILE`). The receiver holds a
//! descriptor per file in flight (two for a `DELTA`), so the soft limit is
//! raised at startup and the in-flight window is sized from what remains.

use std::io;

use nix::sys::resource::{getrlimit, setrlimit, Resource};

/// `OPEN_MAX` from macOS's `<sys/syslimits.h>`: the soft `RLIMIT_NOFILE`
/// its `setrlimit(2)` man page says to stay within (`min(OPEN_MAX,
/// rlim_max)`); older kernels refused more. The `libc` crate does not
/// export it, so it is spelled out here. It leaves room for the whole
/// in-flight window, so a higher soft limit (which a shell may already
/// have set) is left as it is rather than lowered.
#[cfg(target_os = "macos")]
const OPEN_MAX: u64 = 10240;

/// The soft limit a raise aims for: the hard limit, or on macOS at most
/// [`OPEN_MAX`].
fn nofile_target(hard: u64) -> u64 {
    #[cfg(target_os = "macos")]
    let hard = hard.min(OPEN_MAX);
    hard
}

/// Raises the soft open-file limit to [`nofile_target`] and returns the
/// soft limit in force afterwards (unchanged when it is already there or
/// the raise is refused).
pub fn raise_nofile() -> io::Result<u64> {
    let (soft, hard) = getrlimit(Resource::RLIMIT_NOFILE)?;
    let target = nofile_target(hard);
    if target <= soft {
        return Ok(soft);
    }
    match setrlimit(Resource::RLIMIT_NOFILE, target, hard) {
        Ok(()) => Ok(target),
        Err(e) => {
            tracing::debug!(soft, hard, error = %e, "open-file limit not raised");
            Ok(soft)
        }
    }
}

/// The current soft open-file limit.
pub fn nofile_soft() -> u64 {
    getrlimit(Resource::RLIMIT_NOFILE).map_or(u64::MAX, |(soft, _)| soft)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raise_nofile_lifts_the_soft_limit_to_the_target() {
        let (soft, hard) = getrlimit(Resource::RLIMIT_NOFILE).unwrap();
        let target = nofile_target(hard);
        // Lower the soft limit to one below the target (far above what
        // this process uses), then ask for the raise. Starting from the
        // current soft limit would not do: on macOS a shell's `ulimit -n`
        // may already sit above `OPEN_MAX`, where there is nothing to raise.
        let lowered = target - 1;
        setrlimit(Resource::RLIMIT_NOFILE, lowered, hard).unwrap();
        assert_eq!(nofile_soft(), lowered);
        let raised = raise_nofile().unwrap();
        assert_eq!(raised, target);
        assert_eq!(nofile_soft(), target);
        // A soft limit already at or above the target is left alone.
        assert_eq!(raise_nofile().unwrap(), target);
        setrlimit(Resource::RLIMIT_NOFILE, soft, hard).unwrap();
    }
}
