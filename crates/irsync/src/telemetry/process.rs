//! Process-wide resource usage: CPU time, peak resident memory and open file descriptors.

use nix::sys::resource::{getrusage, UsageWho};
use std::time::Duration;

/// One reading of this process's resource usage.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessSample {
    pub cpu_user: Duration,
    pub cpu_sys: Duration,
    pub max_rss_bytes: u64,
    /// `None` where the open descriptors cannot be counted.
    pub open_fds: Option<u64>,
}

/// Converts `ru_maxrss` (KiB on Linux, bytes on macOS) to bytes.
pub fn rss_bytes(raw: i64) -> u64 {
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

/// The number of descriptors this process has open, from the `sysinfo`
/// crate, which knows how each OS reports it (a folder under `/proc` on
/// Linux, a kernel call on macOS and FreeBSD).
fn open_fds() -> Option<u64> {
    use std::sync::{Mutex, OnceLock, PoisonError};
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
    // One `System` for the life of the process, not one per sample: on
    // macOS each new one takes a kernel handle that sysinfo never gives
    // back. `None` if it could not be made.
    static SYSTEM: OnceLock<Option<Mutex<System>>> = OnceLock::new();
    let mut sys = SYSTEM
        .get_or_init(|| System::new().ok().map(Mutex::new))
        .as_ref()?
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let pid = sysinfo::get_current_pid().ok()?;
    // Only finds the process: the count itself is read by `open_files`.
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        false,
        ProcessRefreshKind::nothing(),
    );
    let n = sys.process(pid)?.open_files()? as u64;
    if cfg!(target_os = "linux") {
        // Reading the folder itself holds one descriptor for a moment.
        Some(n.saturating_sub(1))
    } else {
        // On FreeBSD the kept `System` holds one descriptor of its own.
        // It is open for as long as the process runs, so it counts.
        Some(n)
    }
}

/// Reads the current process usage.
pub fn sample() -> ProcessSample {
    let mut s = ProcessSample {
        open_fds: open_fds(),
        ..ProcessSample::default()
    };
    if let Ok(u) = getrusage(UsageWho::RUSAGE_SELF) {
        s.cpu_user = timeval(u.user_time());
        s.cpu_sys = timeval(u.system_time());
        s.max_rss_bytes = rss_bytes(u.max_rss());
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn rss_bytes_units() {
        if cfg!(target_os = "macos") {
            assert_eq!(rss_bytes(2048), 2048);
        } else {
            assert_eq!(rss_bytes(2048), 2048 * 1024);
        }
    }

    /// Counts the open descriptors the slow, sure way: asks about each
    /// number in turn.
    fn direct_fd_count() -> u64 {
        (0..4096)
            // SAFETY: F_GETFD only reads the flags of `fd`, or fails with
            // EBADF when no such descriptor is open.
            .filter(|&fd| unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFD) } != -1)
            .count() as u64
    }

    #[test]
    fn open_fds_matches_a_direct_count() {
        // Other tests in this process open and close descriptors too, even
        // in the middle of a sample. So take many samples, keep those where
        // the direct count was the same before and after, and ask that
        // most of them agree: a count that is off by one never would.
        let (mut same, mut differ) = (0, 0);
        let mut last = None;
        for _ in 0..200 {
            let before = direct_fd_count();
            let got = sample().open_fds;
            if direct_fd_count() != before {
                continue;
            }
            if got == Some(before) {
                same += 1;
            } else {
                differ += 1;
                last = Some((got, before));
            }
        }
        assert!(
            same > differ,
            "agreed {same} times, differed {differ}; last (open_fds, direct count): {last:?}"
        );
    }

    #[test]
    fn sample_sees_cpu_and_fds() {
        let cpu = |s: &ProcessSample| s.cpu_user + s.cpu_sys;
        let before = sample();
        // Spin until the sampled CPU time grows, not for a fixed wall time: on a
        // busy machine the thread may be off the CPU for much of any interval.
        let t = Instant::now();
        let mut x = 1u64;
        while cpu(&sample()) < cpu(&before) + Duration::from_millis(30)
            && t.elapsed() < Duration::from_secs(10)
        {
            for _ in 0..10_000 {
                x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
            }
        }
        let dir = tempfile::tempdir().unwrap();
        // Other tests in this process open and close descriptors too, so retry the count.
        let grew = (0..20).any(|_| {
            let before = sample().open_fds.unwrap();
            let files: Vec<_> = (0..3)
                .map(|i| std::fs::File::create(dir.path().join(format!("f{i}"))).unwrap())
                .collect();
            let after = sample().open_fds.unwrap();
            drop(files);
            after >= before + 3
        });
        assert!(grew, "open_fds never grew by 3");
        let after = sample();
        assert!(
            cpu(&after) >= cpu(&before) + Duration::from_millis(30),
            "{:?} -> {:?}",
            cpu(&before),
            cpu(&after)
        );
    }
}
