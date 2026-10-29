//! The process's tracing subscriber and metrics recorder, set up for the sinks that are on.

use super::{publish, Side};
use crate::config::settings::{OtlpProtocol, TelemetrySettings};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::Level;
use tracing_subscriber::filter::{EnvFilter, Targets};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Layer, Registry};
use url::Url;

/// How long [`Telemetry::shutdown`] may take in all.
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(2);
/// How often the Prometheus recorder drains its histograms.
const UPKEEP_INTERVAL: Duration = Duration::from_secs(5);

/// Where OTLP export sends traces and metrics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtlpConfig {
    pub endpoint: Url,
    pub protocol: OtlpProtocol,
}

impl OtlpConfig {
    /// `[telemetry]`'s endpoint and protocol (HTTP unless set); `None` without an endpoint.
    pub fn from_settings(t: &TelemetrySettings) -> Option<OtlpConfig> {
        t.otlp_endpoint.clone().map(|endpoint| OtlpConfig {
            endpoint,
            protocol: t.otlp_protocol.unwrap_or(OtlpProtocol::Http),
        })
    }
}

/// What the process's telemetry is set up with.
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    pub side: Side,
    /// The log filter unless `RUST_LOG` is set.
    pub log_default: String,
    /// `-v`: the per-session summary.
    pub summary: bool,
    /// The debug HTTP port.
    pub debug_addr: Option<SocketAddr>,
    pub otlp: Option<OtlpConfig>,
    /// `IRSYNC_TRACE_FILES=1`: per-file spans reach the span sinks.
    pub trace_files: bool,
}

impl TelemetryConfig {
    /// Logs at `log_default` and nothing else.
    pub fn logs_only(side: Side, log_default: &str) -> TelemetryConfig {
        TelemetryConfig {
            side,
            log_default: log_default.to_string(),
            summary: false,
            debug_addr: None,
            otlp: None,
            trace_files: false,
        }
    }
}

/// Whether `IRSYNC_TRACE_FILES=1` is set.
pub fn trace_files_from_env() -> bool {
    std::env::var_os("IRSYNC_TRACE_FILES").is_some_and(|v| v == "1")
}

/// Which metrics recorder the sinks need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecorderKind {
    Prometheus,
    Otel,
    Fanout,
}

/// The recorder for `cfg`: none without a metric sink (the `-v` summary
/// reads the meters directly), the one sink's alone, or a fanout to both.
pub(crate) fn recorder_kind(cfg: &TelemetryConfig) -> Option<RecorderKind> {
    match (cfg.debug_addr.is_some(), cfg.otlp.is_some()) {
        (false, false) => None,
        (true, false) => Some(RecorderKind::Prometheus),
        (false, true) => Some(RecorderKind::Otel),
        (true, true) => Some(RecorderKind::Fanout),
    }
}

/// A span sink layer (timing, flame graph, OTel).
type SpanLayer = Box<dyn Layer<Registry> + Send + Sync>;

/// What the span sinks see: irsync's spans at `info`, per-file ones too with `trace_files`.
fn sink_targets(trace_files: bool) -> Targets {
    let level = if trace_files {
        Level::DEBUG
    } else {
        Level::INFO
    };
    Targets::new().with_target("irsync", level)
}

/// The process's telemetry once [`init`] has installed it.
pub struct Telemetry {
    cfg: TelemetryConfig,
    prometheus: Option<PrometheusHandle>,
    /// A metrics recorder is installed, so the meters are worth publishing.
    recording: bool,
    tasks: Vec<JoinHandle<()>>,
}

/// Installs the global subscriber (the fmt layer filtered by `RUST_LOG`
/// or `cfg.log_default`, plus the span sinks that are on) and, when a
/// metric sink is on, the metrics recorder; turns telemetry collection on
/// when any sink is. Call inside a runtime context.
pub fn init<W>(cfg: TelemetryConfig, writer: W) -> Telemetry
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    let env =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cfg.log_default));
    let fmt = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_filter(env);
    let timing: Option<SpanLayer> = None;
    let flame: Option<SpanLayer> = None;
    let otel: Option<SpanLayer> = None;
    let targets = sink_targets(cfg.trace_files);
    let sinks: Vec<SpanLayer> = [timing, flame, otel]
        .into_iter()
        .flatten()
        .map(|l| l.with_filter(targets.clone()).boxed())
        .collect();
    // `None`, not an empty list, so the fmt filter alone decides interest.
    let sinks = (!sinks.is_empty()).then_some(sinks);
    let _ = tracing_subscriber::registry()
        .with(sinks)
        .with(fmt)
        .try_init();

    let kind = recorder_kind(&cfg);
    let prometheus = matches!(kind, Some(RecorderKind::Prometheus | RecorderKind::Fanout))
        .then(|| PrometheusBuilder::new().build_recorder());
    let handle = prometheus.as_ref().map(|r| r.handle());
    let installed = match (kind, prometheus) {
        (Some(RecorderKind::Prometheus), Some(r)) => metrics::set_global_recorder(r).is_ok(),
        (Some(RecorderKind::Fanout), Some(r)) => {
            let fanout = metrics_util::layers::FanoutBuilder::default()
                .add_recorder(r)
                .build();
            metrics::set_global_recorder(fanout).is_ok()
        }
        // The OTel bridge recorder is not built yet: nothing to install.
        _ => false,
    };
    if installed {
        publish::describe();
    }
    super::set_enabled(cfg.summary || cfg.debug_addr.is_some() || cfg.otlp.is_some());
    Telemetry {
        cfg,
        prometheus: handle.filter(|_| installed),
        recording: installed,
        tasks: Vec::new(),
    }
}

impl Telemetry {
    /// The Prometheus recorder's handle, when the debug port is on.
    pub fn prometheus(&self) -> Option<&PrometheusHandle> {
        self.prometheus.as_ref()
    }

    /// Starts the background work the sinks need: publishing the meters
    /// and the debug server. Returns the debug server's bound address.
    pub async fn start(&mut self) -> Option<SocketAddr> {
        if self.recording {
            self.tasks.push(publish::spawn(self.cfg.side));
        }
        if let Some(h) = self.prometheus.clone() {
            self.tasks.push(tokio::spawn(async move {
                let mut tick = tokio::time::interval(UPKEEP_INTERVAL);
                loop {
                    tick.tick().await;
                    h.run_upkeep();
                }
            }));
        }
        None
    }

    /// Stops the background work and publishes the meters a last time;
    /// takes at most two seconds.
    pub async fn shutdown(self) {
        let flush = async move {
            for t in &self.tasks {
                t.abort();
            }
            if self.recording {
                publish::once(self.cfg.side);
            }
        };
        let _ = tokio::time::timeout(SHUTDOWN_BUDGET, flush).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorder_choice() {
        let mut cfg = TelemetryConfig::logs_only(Side::Client, "warn");
        assert_eq!(recorder_kind(&cfg), None);
        cfg.summary = true;
        assert_eq!(recorder_kind(&cfg), None);
        cfg.debug_addr = Some("127.0.0.1:9464".parse().unwrap());
        assert_eq!(recorder_kind(&cfg), Some(RecorderKind::Prometheus));
        cfg.otlp = Some(OtlpConfig {
            endpoint: "http://c:4318".parse().unwrap(),
            protocol: OtlpProtocol::Http,
        });
        assert_eq!(recorder_kind(&cfg), Some(RecorderKind::Fanout));
        cfg.debug_addr = None;
        assert_eq!(recorder_kind(&cfg), Some(RecorderKind::Otel));
    }
}
