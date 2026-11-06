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
    /// The thread the current interval runs on.
    thread: Option<std::thread::ThreadId>,
    /// Time children spent nested in the current interval (entered on its
    /// thread while it was): a child running on another thread meanwhile
    /// takes none of this span's time.
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
        iv.thread = Some(std::thread::current().id());
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
                let nested = iv.entered.is_some() && iv.thread == Some(std::thread::current().id());
                if nested {
                    iv.children += total;
                }
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

    fn micros(d: Duration) -> u64 {
        d.as_micros() as u64
    }

    /// How long `sleep` really took: macOS oversleeps short sleeps by
    /// milliseconds, so the tests compare with this, not with the request.
    fn timed_sleep(ms: u64) -> Duration {
        let t = Instant::now();
        std::thread::sleep(Duration::from_millis(ms));
        t.elapsed()
    }

    #[test]
    fn a_child_on_another_thread_takes_no_self_time() {
        let state = Arc::new(FlameState::default());
        let dispatch = tracing::Dispatch::new(
            tracing_subscriber::registry().with(FlameLayer::new(state.clone())),
        );
        let mut alongside = Duration::ZERO;
        let mut slept = Duration::ZERO;
        let start = Instant::now();
        tracing::dispatcher::with_default(&dispatch, || {
            let outer = tracing::info_span!("outer");
            let _o = outer.enter();
            let (d, parent) = (dispatch.clone(), outer.clone());
            let child = std::thread::spawn(move || {
                tracing::dispatcher::with_default(&d, || {
                    let inner = tracing::info_span!(parent: &parent, "inner");
                    let _i = inner.enter();
                    timed_sleep(8)
                })
            });
            alongside = timed_sleep(10);
            slept = child.join().unwrap();
        });
        let wall = micros(start.elapsed());
        let out = state.render();
        // The outer span ran its whole sleep itself, alongside the child.
        let outer = value(&out, "outer");
        assert!(outer >= micros(alongside), "{out}slept {alongside:?}");
        let inner = value(&out, "outer;inner");
        assert!(
            (micros(slept)..=wall).contains(&inner),
            "{out}slept {slept:?}, wall {wall} us"
        );
    }

    #[test]
    fn self_time_excludes_children() {
        let state = Arc::new(FlameState::default());
        let sub = tracing_subscriber::registry().with(FlameLayer::new(state.clone()));
        let (mut first, mut nested) = (Duration::ZERO, Duration::ZERO);
        let start = Instant::now();
        tracing::subscriber::with_default(sub, || {
            let outer = tracing::info_span!("outer");
            let _o = outer.enter();
            first = timed_sleep(4);
            let inner = tracing::info_span!("inner");
            let _i = inner.enter();
            nested = timed_sleep(6);
        });
        let wall = micros(start.elapsed());
        let out = state.render();
        let outer = value(&out, "outer");
        assert!(outer >= micros(first), "{out}slept {first:?}");
        let inner = value(&out, "outer;inner");
        assert!(inner >= micros(nested), "{out}slept {nested:?}");
        // Self time plus the child's time is the outer span's whole time,
        // which fits in the wall time; counted twice, the child's would not.
        assert!(outer + inner <= wall, "{out}wall {wall} us");
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
