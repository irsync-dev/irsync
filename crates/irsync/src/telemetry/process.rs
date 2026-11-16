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
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
    let pid = sysinfo::get_current_pid().ok()?;
    let mut sys = System::new().ok()?;
    // Only finds the process: the count itself is read by `open_files`.
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        false,
        ProcessRefreshKind::nothing(),
    );
    let n = sys.process(pid)?.open_files()? as u64;
    if cfg!(target_os = "linux") {
        // Reading the folder itself holds one descriptor.
        Some(n.saturating_sub(1))
    } else {
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
