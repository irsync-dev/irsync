//! Progress reporting hooks for long-running engine work.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::cpu::Cores;

pub trait Progress: Sync {
    /// One entry was recorded or compared.
    fn entry(&self);
    /// One file of `bytes` bytes was hashed, using `cpu` of a hasher's CPU time.
    fn hashed(&self, bytes: u64, cpu: Duration);
    fn finish(&self);
}

/// Reports nothing.
pub struct NoProgress;

impl Progress for NoProgress {
    fn entry(&self) {}
    fn hashed(&self, _bytes: u64, _cpu: Duration) {}
    fn finish(&self) {}
}

/// Spinner on stderr showing `entries=N hashed=X MiB`, then how fast hashing
/// goes, for example
/// `2048.0 MiB/s, 11.2 of 16 cores busy (12P+4E), 182.9 MiB/s/core`.
///
/// MiB/s is the average since the spinner started. The walk feeds the hashers
/// as it goes, so hashing starts almost at once; time spent on entries that
/// need no hashing (reused by `update`) still counts.
///
/// Cores busy is the hashers' CPU time divided by the time elapsed: how many
/// cores were hashing on average. A hasher waiting on the disk uses no CPU
/// time, so fewer busy cores than threads means hashing waits on reads.
/// MiB/s/core is bytes per second of hasher CPU time, including the kernel's
/// share for reading the files. `(12P+4E)` is the machine's performance and
/// efficiency cores, shown where the system reports them.
///
/// The message is refreshed from `entry()`/`hashed()` at most once every
/// 100 ms; the counters themselves are updated on every call.
pub struct TermProgress {
    bar: indicatif::ProgressBar,
    jobs: usize,
    cores: Cores,
    entries: AtomicU64,
    bytes: AtomicU64,
    cpu_ns: AtomicU64,
    start: Instant,
    last_ms: AtomicU64,
}

impl TermProgress {
    const INTERVAL_MS: u64 = 100;

    /// `jobs` is the number of hasher threads.
    pub fn new(jobs: usize) -> Self {
        let bar = indicatif::ProgressBar::new_spinner();
        bar.set_draw_target(indicatif::ProgressDrawTarget::stderr());
        bar.set_style(
            indicatif::ProgressStyle::with_template("{spinner} {msg}")
                .expect("valid progress template"),
        );
        bar.enable_steady_tick(Duration::from_millis(100));
        TermProgress {
            bar,
            jobs: jobs.max(1),
            cores: Cores::detect(),
            entries: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            cpu_ns: AtomicU64::new(0),
            start: Instant::now(),
            last_ms: AtomicU64::new(0),
        }
    }

    fn refresh(&self) {
        let now = self.start.elapsed().as_millis() as u64;
        let last = self.last_ms.load(Ordering::Relaxed);
        if now.saturating_sub(last) < Self::INTERVAL_MS
            || self
                .last_ms
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
        {
            return;
        }
        self.bar.set_message(message(&Stats {
            entries: self.entries.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
            cpu: Duration::from_nanos(self.cpu_ns.load(Ordering::Relaxed)),
            elapsed: self.start.elapsed(),
            jobs: self.jobs,
            cores: self.cores,
        }));
    }
}

struct Stats {
    entries: u64,
    bytes: u64,
    /// Hasher CPU time; zero if it could not be measured.
    cpu: Duration,
    elapsed: Duration,
    jobs: usize,
    cores: Cores,
}

/// `entries=N hashed=X MiB`, plus the rates once there are any to show.
fn message(s: &Stats) -> String {
    const MIB: f64 = (1u64 << 20) as f64;
    let mut msg = format!("entries={} hashed={} MiB", s.entries, s.bytes >> 20);
    let secs = s.elapsed.as_secs_f64();
    if s.bytes == 0 || secs <= 0.0 {
        return msg;
    }
    let mib = s.bytes as f64 / MIB;
    msg.push_str(&format!(" {:.1} MiB/s", mib / secs));
    let cpu = s.cpu.as_secs_f64();
    if cpu > 0.0 {
        msg.push_str(&format!(
            ", {:.1} of {} cores busy",
            cpu / secs,
            s.cores.logical
        ));
        if let Some((p, e)) = s.cores.split {
            msg.push_str(&format!(" ({p}P+{e}E)"));
        }
        msg.push_str(&format!(", {:.1} MiB/s/core", mib / cpu));
    } else {
        let threads = if s.jobs == 1 { "thread" } else { "threads" };
        msg.push_str(&format!(
            ", {} {threads}, {:.1} MiB/s/thread",
            s.jobs,
            mib / secs / s.jobs as f64
        ));
    }
    msg
}

impl Progress for TermProgress {
    fn entry(&self) {
        self.entries.fetch_add(1, Ordering::Relaxed);
        self.refresh();
    }

    fn hashed(&self, bytes: u64, cpu: Duration) {
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
        self.cpu_ns.fetch_add(
            u64::try_from(cpu.as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        self.refresh();
    }

    fn finish(&self) {
        self.bar.finish_and_clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{message, Stats};
    use crate::cpu::Cores;
    use std::time::Duration;

    fn stats(bytes: u64, cpu_ms: u64, elapsed_ms: u64, jobs: usize) -> Stats {
        Stats {
            entries: 7,
            bytes,
            cpu: Duration::from_millis(cpu_ms),
            elapsed: Duration::from_millis(elapsed_ms),
            jobs,
            cores: Cores {
                logical: 16,
                split: Some((12, 4)),
            },
        }
    }

    #[test]
    fn message_without_hashing_shows_no_rate() {
        assert_eq!(message(&stats(0, 0, 0, 8)), "entries=7 hashed=0 MiB");
        assert_eq!(message(&stats(1 << 20, 5, 0, 8)), "entries=7 hashed=1 MiB");
    }

    #[test]
    fn message_shows_busy_cores_split_and_rate_per_core() {
        assert_eq!(
            message(&stats(800 << 20, 5000, 2000, 4)),
            "entries=7 hashed=800 MiB 400.0 MiB/s, \
             2.5 of 16 cores busy (12P+4E), 160.0 MiB/s/core"
        );
    }

    #[test]
    fn message_leaves_out_the_split_when_unknown() {
        let mut s = stats(3 << 20, 1000, 2000, 1);
        s.cores.split = None;
        assert_eq!(
            message(&s),
            "entries=7 hashed=3 MiB 1.5 MiB/s, \
             0.5 of 16 cores busy, 3.0 MiB/s/core"
        );
    }

    #[test]
    fn message_falls_back_to_per_thread_without_cpu_time() {
        assert_eq!(
            message(&stats(800 << 20, 0, 2000, 4)),
            "entries=7 hashed=800 MiB 400.0 MiB/s, 4 threads, 100.0 MiB/s/thread"
        );
    }
}
