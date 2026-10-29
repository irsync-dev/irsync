//! Self time per span stack, in folded-stack form for `/debug/flame`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{Id, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// Distinct stacks kept; the rest are summed under [`OTHER`].
const MAX_KEYS: usize = 10_000;
const OTHER: &str = "[other]";

/// What [`FlameLayer`] collects; shared with the debug server.
#[derive(Default)]
pub struct FlameState {
    micros: Mutex<HashMap<String, u64>>,
}

impl FlameState {
    /// Adds `micros` to the stack `key` (`root;...;leaf`).
    pub fn add(&self, key: &str, micros: u64) {
        let mut m = self.micros.lock().unwrap();
        if let Some(v) = m.get_mut(key) {
            *v += micros;
        } else if m.len() < MAX_KEYS {
            m.insert(key.to_string(), micros);
        } else {
            *m.entry(OTHER.to_string()).or_default() += micros;
        }
    }

    /// `key value` lines, sorted.
    pub fn render(&self) -> String {
        let m = self.micros.lock().unwrap();
        let mut keys: Vec<_> = m.iter().collect();
        keys.sort();
        let mut out = String::new();
        for (k, v) in keys {
            let _ = writeln!(out, "{k} {v}");
        }
        out
    }
}

/// Per-span bookkeeping kept in the span's extensions.
#[derive(Default)]
struct Interval {
    entered: Option<Instant>,
    /// Time children were entered within the current interval.
    children: Duration,
}

/// On each exit, adds the interval's self time to the span's stack.
pub struct FlameLayer {
    state: Arc<FlameState>,
}

impl FlameLayer {
    pub fn new(state: Arc<FlameState>) -> FlameLayer {
        FlameLayer { state }
    }
}

impl<S> Layer<S> for FlameLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut ext = span.extensions_mut();
        if ext.get_mut::<Interval>().is_none() {
            ext.insert(Interval::default());
        }
        let iv = ext.get_mut::<Interval>().unwrap();
        iv.entered = Some(Instant::now());
        iv.children = Duration::ZERO;
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let (total, own) = {
            let mut ext = span.extensions_mut();
            let Some(iv) = ext.get_mut::<Interval>() else {
                return;
            };
            let Some(t) = iv.entered.take() else { return };
            let total = t.elapsed();
            (total, total.saturating_sub(iv.children))
        };
        let mut key = String::new();
        for s in span.scope().from_root() {
            if !key.is_empty() {
                key.push(';');
            }
            key.push_str(s.name());
        }
        self.state.add(&key, own.as_micros() as u64);
        if let Some(parent) = span.parent() {
            if let Some(iv) = parent.extensions_mut().get_mut::<Interval>() {
                iv.children += total;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    fn value(out: &str, key: &str) -> u64 {
        out.lines()
            .find_map(|l| l.strip_prefix(&format!("{key} ")))
            .unwrap_or_else(|| panic!("no {key} in {out}"))
            .parse()
            .unwrap()
    }

    #[test]
    fn self_time_excludes_children() {
        let state = Arc::new(FlameState::default());
        let sub = tracing_subscriber::registry().with(FlameLayer::new(state.clone()));
        tracing::subscriber::with_default(sub, || {
            let outer = tracing::info_span!("outer");
            let _o = outer.enter();
            std::thread::sleep(Duration::from_millis(4));
            let inner = tracing::info_span!("inner");
            let _i = inner.enter();
            std::thread::sleep(Duration::from_millis(6));
        });
        let out = state.render();
        let outer = value(&out, "outer");
        assert!((2_000..=6_000).contains(&outer), "{out}");
        let inner = value(&out, "outer;inner");
        assert!((5_000..=8_000).contains(&inner), "{out}");
    }

    #[test]
    fn caps_distinct_stacks() {
        let state = FlameState::default();
        for i in 0..10_050 {
            state.add(&format!("k{i}"), 1);
        }
        let out = state.render();
        assert_eq!(out.lines().count(), 10_001);
        assert!(out.lines().any(|l| l == "[other] 50"));
    }
}
