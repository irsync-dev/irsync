//! A `metrics::Recorder` that forwards every value to an OpenTelemetry `Meter`.

use metrics::{
    Counter, CounterFn, Gauge, GaugeFn, Histogram, HistogramFn, Key, KeyName, Metadata, Recorder,
    SharedString, Unit,
};
use opentelemetry::metrics::Meter;
use opentelemetry::KeyValue;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Bucket bounds for durations in seconds: 1 ms to 5 min.
const SECONDS_BOUNDS: [f64; 16] = [
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 300.0,
];

/// Forwards `metrics` counters, gauges and histograms to OTel instruments,
/// one per key, created on first use. Labels become attributes.
pub struct OtelRecorder {
    meter: Meter,
    /// Unit and help from the `describe_*` calls, by metric name.
    described: Mutex<HashMap<String, (Option<Unit>, SharedString)>>,
    counters: Mutex<HashMap<Key, Arc<OtelCounter>>>,
    gauges: Mutex<HashMap<Key, Arc<OtelGauge>>>,
    histograms: Mutex<HashMap<Key, Arc<OtelHistogram>>>,
}

impl OtelRecorder {
    pub fn new(meter: Meter) -> OtelRecorder {
        OtelRecorder {
            meter,
            described: Mutex::default(),
            counters: Mutex::default(),
            gauges: Mutex::default(),
            histograms: Mutex::default(),
        }
    }

    fn describe(&self, key: KeyName, unit: Option<Unit>, help: SharedString) {
        lock(&self.described).insert(key.as_str().to_owned(), (unit, help));
    }

    /// The unit and help described for `name`.
    fn description(&self, name: &str) -> (Option<Unit>, String) {
        match lock(&self.described).get(name) {
            Some((unit, help)) => (*unit, help.to_string()),
            None => (None, String::new()),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `unit` as a UCUM code, where OTel has one.
fn ucum(unit: Unit) -> Option<&'static str> {
    Some(match unit {
        Unit::Seconds => "s",
        Unit::Milliseconds => "ms",
        Unit::Microseconds => "us",
        Unit::Nanoseconds => "ns",
        Unit::Bytes => "By",
        _ => return None,
    })
}

fn attributes(key: &Key) -> Vec<KeyValue> {
    key.labels()
        .map(|l| KeyValue::new(l.key().to_owned(), l.value().to_owned()))
        .collect()
}

/// The cached handle for `key`, made with `make` the first time.
fn cached<T>(map: &Mutex<HashMap<Key, Arc<T>>>, key: &Key, make: impl FnOnce() -> T) -> Arc<T> {
    lock(map)
        .entry(key.clone())
        .or_insert_with(|| Arc::new(make()))
        .clone()
}

impl Recorder for OtelRecorder {
    fn describe_counter(&self, key: KeyName, unit: Option<Unit>, help: SharedString) {
        self.describe(key, unit, help);
    }

    fn describe_gauge(&self, key: KeyName, unit: Option<Unit>, help: SharedString) {
        self.describe(key, unit, help);
    }

    fn describe_histogram(&self, key: KeyName, unit: Option<Unit>, help: SharedString) {
        self.describe(key, unit, help);
    }

    fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
        let c = cached(&self.counters, key, || {
            let (unit, help) = self.description(key.name());
            let mut b = self
                .meter
                .u64_counter(key.name().to_owned())
                .with_description(help);
            if let Some(u) = unit.and_then(ucum) {
                b = b.with_unit(u);
            }
            OtelCounter {
                inner: b.build(),
                attributes: attributes(key),
                last: AtomicU64::new(0),
            }
        });
        Counter::from_arc(c)
    }

    fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
        let g = cached(&self.gauges, key, || {
            let (unit, help) = self.description(key.name());
            let mut b = self
                .meter
                .f64_gauge(key.name().to_owned())
                .with_description(help);
            if let Some(u) = unit.and_then(ucum) {
                b = b.with_unit(u);
            }
            OtelGauge {
                inner: b.build(),
                attributes: attributes(key),
                value: Mutex::new(0.0),
            }
        });
        Gauge::from_arc(g)
    }

    fn register_histogram(&self, key: &Key, _: &Metadata<'_>) -> Histogram {
        let h = cached(&self.histograms, key, || {
            let (unit, help) = self.description(key.name());
            let mut b = self
                .meter
                .f64_histogram(key.name().to_owned())
                .with_description(help);
            if let Some(u) = unit.and_then(ucum) {
                b = b.with_unit(u);
            }
            match unit {
                Some(Unit::Seconds) => b = b.with_boundaries(SECONDS_BOUNDS.to_vec()),
                // 1 KiB to 1 GiB in powers of four.
                Some(Unit::Bytes) => {
                    b = b.with_boundaries((5..=15).map(|i| 4f64.powi(i)).collect());
                }
                _ => {}
            }
            OtelHistogram {
                inner: b.build(),
                attributes: attributes(key),
            }
        });
        Histogram::from_arc(h)
    }
}

/// A `u64` counter; `absolute` adds the rise since the last value seen.
struct OtelCounter {
    inner: opentelemetry::metrics::Counter<u64>,
    attributes: Vec<KeyValue>,
    last: AtomicU64,
}

impl CounterFn for OtelCounter {
    fn increment(&self, value: u64) {
        self.last.fetch_add(value, Ordering::Relaxed);
        self.inner.add(value, &self.attributes);
    }

    fn absolute(&self, value: u64) {
        let prev = self.last.fetch_max(value, Ordering::Relaxed);
        if value > prev {
            self.inner.add(value - prev, &self.attributes);
        }
    }
}

/// An `f64` gauge; keeps the value so `increment` and `decrement` can move it.
struct OtelGauge {
    inner: opentelemetry::metrics::Gauge<f64>,
    attributes: Vec<KeyValue>,
    value: Mutex<f64>,
}

impl OtelGauge {
    fn update(&self, f: impl FnOnce(f64) -> f64) {
        let mut v = lock(&self.value);
        *v = f(*v);
        self.inner.record(*v, &self.attributes);
    }
}

impl GaugeFn for OtelGauge {
    fn increment(&self, value: f64) {
        self.update(|v| v + value);
    }

    fn decrement(&self, value: f64) {
        self.update(|v| v - value);
    }

    fn set(&self, value: f64) {
        self.update(|_| value);
    }
}

struct OtelHistogram {
    inner: opentelemetry::metrics::Histogram<f64>,
    attributes: Vec<KeyValue>,
}

impl HistogramFn for OtelHistogram {
    fn record(&self, value: f64) {
        self.inner.record(value, &self.attributes);
    }
}
