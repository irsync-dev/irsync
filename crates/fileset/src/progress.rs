//! Progress reporting hooks for long-running engine work.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub trait Progress: Sync {
    /// One entry was recorded or compared.
    fn entry(&self);
    /// `n` more bytes were hashed.
    fn hashed_bytes(&self, n: u64);
    fn finish(&self);
}

/// Reports nothing.
pub struct NoProgress;

impl Progress for NoProgress {
    fn entry(&self) {}
    fn hashed_bytes(&self, _n: u64) {}
    fn finish(&self) {}
}

/// Spinner on stderr showing `entries=N hashed=X MiB`.
///
/// The message is refreshed from `entry()`/`hashed_bytes()` at most once
/// every 100 ms; the counters themselves are updated on every call.
pub struct TermProgress {
    bar: indicatif::ProgressBar,
    entries: AtomicU64,
    bytes: AtomicU64,
    start: Instant,
    last_ms: AtomicU64,
}

impl TermProgress {
    const INTERVAL_MS: u64 = 100;

    pub fn new() -> Self {
        let bar = indicatif::ProgressBar::new_spinner();
        bar.set_draw_target(indicatif::ProgressDrawTarget::stderr());
        bar.set_style(
            indicatif::ProgressStyle::with_template("{spinner} {msg}")
                .expect("valid progress template"),
        );
        bar.enable_steady_tick(Duration::from_millis(100));
        TermProgress {
            bar,
            entries: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
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
        self.bar.set_message(format!(
            "entries={} hashed={} MiB",
            self.entries.load(Ordering::Relaxed),
            self.bytes.load(Ordering::Relaxed) >> 20
        ));
    }
}

impl Default for TermProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl Progress for TermProgress {
    fn entry(&self) {
        self.entries.fetch_add(1, Ordering::Relaxed);
        self.refresh();
    }

    fn hashed_bytes(&self, n: u64) {
        self.bytes.fetch_add(n, Ordering::Relaxed);
        self.refresh();
    }

    fn finish(&self) {
        self.bar.finish_and_clear();
    }
}
