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

fn open_fds() -> Option<u64> {
    let dir = if cfg!(target_os = "macos") {
        "/dev/fd"
    } else {
        "/proc/self/fd"
    };
    let n = std::fs::read_dir(dir).ok()?.count() as u64;
    // The directory read itself holds one descriptor.
    Some(n.saturating_sub(1))
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
        let before = sample();
        let t = Instant::now();
        let mut x = 1u64;
        while t.elapsed() < Duration::from_millis(50) {
            x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
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
        let cpu = |s: &ProcessSample| s.cpu_user + s.cpu_sys;
        assert!(
            cpu(&after) >= cpu(&before) + Duration::from_millis(30),
            "{:?} -> {:?}",
            cpu(&before),
            cpu(&after)
        );
    }
}
