//! The process's open-file limit (`RLIMIT_NOFILE`). The receiver holds a
//! descriptor per file in flight (two for a `DELTA`), so the soft limit is
//! raised at startup and the in-flight window is sized from what remains.

use std::io;

use nix::sys::resource::{getrlimit, setrlimit, Resource};

/// The soft limit a raise aims for: the hard limit, or on macOS at most
/// `OPEN_MAX`, which the kernel refuses to exceed.
fn nofile_target(hard: u64) -> u64 {
    #[cfg(target_os = "macos")]
    let hard = hard.min(nix::libc::OPEN_MAX as u64);
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
    fn raise_nofile_lifts_the_soft_limit_as_far_as_the_hard_limit_allows() {
        let (soft, hard) = getrlimit(Resource::RLIMIT_NOFILE).unwrap();
        // Lower the soft limit by one (far above what this process uses),
        // then ask for the raise.
        let lowered = soft - 1;
        setrlimit(Resource::RLIMIT_NOFILE, lowered, hard).unwrap();
        let raised = raise_nofile().unwrap();
        assert!(raised > lowered, "{raised} <= {lowered}");
        assert_eq!(raised, nofile_target(hard));
        assert_eq!(nofile_soft(), raised);
    }
}
