//! Wall and busy time per span name, as histograms for `/debug/timing`.

use crate::cli::progress::summary::{ms, short_dur};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::span::Attributes;
use tracing::{Id, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// Wall times in 64 log2 buckets of nanoseconds, plus count, sum and max.
#[derive(Clone)]
pub struct Hist {
    buckets: [u64; 64],
    count: u64,
    sum_ns: u128,
    max_ns: u64,
}

impl Default for Hist {
    fn default() -> Hist {
        Hist {
            buckets: [0; 64],
            count: 0,
            sum_ns: 0,
            max_ns: 0,
        }
    }
}

impl Hist {
    fn record(&mut self, d: Duration) {
        let ns = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX).max(1);
        self.buckets[63 - ns.leading_zeros() as usize] += 1;
        self.count += 1;
        self.sum_ns += ns as u128;
        self.max_ns = self.max_ns.max(ns);
    }

    /// The upper bound of the bucket holding the `q`th quantile.
    fn quantile(&self, q: f64) -> Duration {
        let want = ((self.count as f64 * q).ceil() as u64).max(1);
        let mut seen = 0;
        for (i, n) in self.buckets.iter().enumerate() {
            seen += n;
            if seen >= want {
                let upper = if i == 63 { u64::MAX } else { 1u64 << (i + 1) };
                return Duration::from_nanos(upper);
            }
        }
        Duration::ZERO
    }
}

#[derive(Default)]
struct Entry {
    wall: Hist,
    busy_ns: u128,
}

/// What [`TimingLayer`] collects; shared with the debug server.
#[derive(Default)]
pub struct TimingState {
    spans: Mutex<HashMap<&'static str, Entry>>,
}

/// A duration with `ms` below a second and `short_dur` from there.
fn dur(d: Duration) -> String {
    if d < Duration::from_secs(1) {
        ms(d)
    } else {
        short_dur(d)
    }
}

impl TimingState {
    /// One header line, then a row per span name, sorted by name.
    pub fn render(&self) -> String {
        let spans = self.spans.lock().unwrap();
        let mut names: Vec<_> = spans.keys().copied().collect();
        names.sort_unstable();
        let mut out = String::from("span count p50 p90 p99 max busy_total\n");
        for n in names {
            let e = &spans[n];
            let h = &e.wall;
            let _ = writeln!(
                out,
                "{n} {} {} {} {} {} {}",
                h.count,
                dur(h.quantile(0.50)),
                dur(h.quantile(0.90)),
                dur(h.quantile(0.99)),
                dur(Duration::from_nanos(h.max_ns)),
                dur(Duration::from_nanos(
                    u64::try_from(e.busy_ns).unwrap_or(u64::MAX)
                )),
            );
        }
        out
    }
}

/// Per-span bookkeeping kept in the span's extensions.
struct Clock {
    created: Instant,
    entered: Option<Instant>,
    busy: Duration,
}

/// Records each span's wall time (creation to close) and busy time (the
/// intervals it was entered) under its name.
pub struct TimingLayer {
    state: Arc<TimingState>,
}

impl TimingLayer {
    pub fn new(state: Arc<TimingState>) -> TimingLayer {
        TimingLayer { state }
    }
}

impl<S> Layer<S> for TimingLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, _attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(Clock {
                created: Instant::now(),
                entered: None,
                busy: Duration::ZERO,
            });
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            if let Some(c) = span.extensions_mut().get_mut::<Clock>() {
                c.entered = Some(Instant::now());
            }
        }
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            if let Some(c) = span.extensions_mut().get_mut::<Clock>() {
                if let Some(t) = c.entered.take() {
                    c.busy += t.elapsed();
                }
            }
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let Some(c) = span.extensions_mut().remove::<Clock>() else {
            return;
        };
        let mut spans = self.state.spans.lock().unwrap();
        let e = spans.entry(span.name()).or_default();
        e.wall.record(c.created.elapsed());
        e.busy_ns += c.busy.as_nanos();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn records_count_and_max() {
        let state = Arc::new(TimingState::default());
        let sub = tracing_subscriber::registry().with(TimingLayer::new(state.clone()));
        tracing::subscriber::with_default(sub, || {
            for ms in [1, 2, 4] {
                let s = tracing::info_span!("x");
                let _g = s.enter();
                std::thread::sleep(Duration::from_millis(ms));
            }
        });
        let out = state.render();
        assert!(out.starts_with("span count p50 p90 p99 max busy_total\n"));
        let row: Vec<&str> = out.lines().nth(1).unwrap().split(' ').collect();
        assert_eq!(row[0], "x");
        assert_eq!(row[1], "3");
        let max: f64 = row[5].trim_end_matches("ms").parse().unwrap();
        assert!(row[5].ends_with("ms") && max >= 4.0, "{out}");
    }
}
