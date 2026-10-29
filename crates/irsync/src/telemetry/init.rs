//! The process's tracing subscriber and metrics recorder, set up for the sinks that are on.

use super::flame::{FlameLayer, FlameState};
use super::http::{self, Routes};
use super::timing::{TimingLayer, TimingState};
use super::{publish, Side};
use crate::config::settings::{OtlpProtocol, TelemetrySettings};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
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

/// The variables that turn OTLP export on; the SDK reads them itself.
const OTLP_ENDPOINT_VARS: [&str; 3] = [
    "OTEL_EXPORTER_OTLP_ENDPOINT",
    "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
    "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
];

/// Where OTLP export sends traces and metrics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtlpConfig {
    /// `[telemetry] otlp_endpoint`, a base URL; `None` when the
    /// environment names the endpoints.
    pub endpoint: Option<Url>,
    pub protocol: OtlpProtocol,
}

impl OtlpConfig {
    /// OTLP export from the environment, else `[telemetry]`; `None` when
    /// neither names an endpoint.
    pub fn resolve(t: &TelemetrySettings) -> Option<OtlpConfig> {
        Self::resolve_with(t, |k| std::env::var(k).ok())
    }

    /// [`OtlpConfig::resolve`] with `env` for the environment.
    fn resolve_with(
        t: &TelemetrySettings,
        env: impl Fn(&str) -> Option<String>,
    ) -> Option<OtlpConfig> {
        let set = |k: &str| env(k).filter(|v| !v.is_empty());
        let endpoint = if OTLP_ENDPOINT_VARS.iter().any(|k| set(k).is_some()) {
            None
        } else {
            Some(t.otlp_endpoint.clone()?)
        };
        let protocol = match set("OTEL_EXPORTER_OTLP_PROTOCOL").as_deref() {
            Some("grpc") => OtlpProtocol::Grpc,
            Some("http/protobuf") => OtlpProtocol::Http,
            _ => t.otlp_protocol.unwrap_or(OtlpProtocol::Http),
        };
        Some(OtlpConfig { endpoint, protocol })
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
    /// What the debug server renders, when the debug port is on.
    routes: Option<Routes>,
    tasks: Vec<JoinHandle<()>>,
    #[cfg(feature = "otel")]
    otel: Option<super::otel::OtelProviders>,
}

/// Installs the global subscriber (the fmt layer filtered by `RUST_LOG`
/// or `cfg.log_default`, plus the span sinks that are on) and, when a
/// metric sink is on, the metrics recorder; turns telemetry collection on
/// when any sink is. Call inside a runtime context.
pub fn init<W>(mut cfg: TelemetryConfig, writer: W) -> Telemetry
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    #[cfg(feature = "otel")]
    let otel = otel_providers(&mut cfg);
    #[cfg(not(feature = "otel"))]
    warn_without_otel(&mut cfg);
    let env =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cfg.log_default));
    let fmt = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_filter(env);
    let state = cfg.debug_addr.map(|_| {
        (
            Arc::new(TimingState::default()),
            Arc::new(FlameState::default()),
        )
    });
    let timing: Option<SpanLayer> = state
        .as_ref()
        .map(|(t, _)| TimingLayer::new(t.clone()).boxed());
    let flame: Option<SpanLayer> = state
        .as_ref()
        .map(|(_, f)| FlameLayer::new(f.clone()).boxed());
    #[cfg(feature = "otel")]
    let otel_layer: Option<SpanLayer> = otel.as_ref().map(|p| p.layer().boxed());
    #[cfg(not(feature = "otel"))]
    let otel_layer: Option<SpanLayer> = None;
    let targets = sink_targets(cfg.trace_files);
    let sinks: Vec<SpanLayer> = [timing, flame, otel_layer]
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
        #[cfg(feature = "otel")]
        (Some(RecorderKind::Otel), _) => otel
            .as_ref()
            .is_some_and(|p| metrics::set_global_recorder(p.recorder()).is_ok()),
        #[cfg(feature = "otel")]
        (Some(RecorderKind::Fanout), Some(r)) => {
            let mut fanout = metrics_util::layers::FanoutBuilder::default().add_recorder(r);
            if let Some(p) = &otel {
                fanout = fanout.add_recorder(p.recorder());
            }
            metrics::set_global_recorder(fanout.build()).is_ok()
        }
        // Without the feature `cfg.otlp` is always cleared: never reached.
        _ => false,
    };
    if installed {
        publish::describe();
    }
    super::set_exporting(cfg.otlp.is_some());
    super::set_enabled(cfg.summary || cfg.debug_addr.is_some() || cfg.otlp.is_some());
    let prometheus = handle.filter(|_| installed);
    let routes = state.map(|(timing, flame)| Routes {
        prometheus: prometheus.clone(),
        timing,
        flame,
    });
    Telemetry {
        cfg,
        prometheus,
        recording: installed,
        routes,
        tasks: Vec::new(),
        #[cfg(feature = "otel")]
        otel,
    }
}

/// The OTLP providers for `cfg.otlp`. Clears `cfg.otlp`, after one
/// warning, when they cannot be built.
#[cfg(feature = "otel")]
fn otel_providers(cfg: &mut TelemetryConfig) -> Option<super::otel::OtelProviders> {
    let otlp = cfg.otlp.as_ref()?;
    // The endpoint id is not known yet: the identity loads later.
    match super::otel::providers(otlp, cfg.side, None) {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("irsync: OpenTelemetry export off: {e:#}");
            cfg.otlp = None;
            None
        }
    }
}

/// Without the feature: one warning, and `cfg.otlp` cleared, when OTLP export was asked for.
#[cfg(not(feature = "otel"))]
fn warn_without_otel(cfg: &mut TelemetryConfig) {
    if cfg.otlp.take().is_some() {
        eprintln!("irsync: built without OpenTelemetry support; ignoring [telemetry]");
    }
}

impl Telemetry {
    /// The Prometheus recorder's handle, when the debug port is on.
    pub fn prometheus(&self) -> Option<&PrometheusHandle> {
        self.prometheus.as_ref()
    }

    /// Starts the background work the sinks need: publishing the meters
    /// and the debug server. Returns the debug server's bound address, or
    /// the bind error; `None` when the debug port is off.
    pub async fn start(&mut self) -> io::Result<Option<SocketAddr>> {
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
        let (Some(addr), Some(routes)) = (self.cfg.debug_addr, self.routes.clone()) else {
            return Ok(None);
        };
        let (bound, task) = http::serve(addr, routes, http::Limits::default()).await?;
        self.tasks.push(task);
        Ok(Some(bound))
    }

    /// Publishes the meters now, so the debug port shows the final values.
    pub fn flush(&self) {
        if self.recording {
            publish::once(self.cfg.side);
        }
    }

    /// Stops the background work, publishes the meters a last time and
    /// flushes OTLP export; takes at most two seconds in all.
    pub async fn shutdown(self) {
        #[cfg(feature = "otel")]
        let started = std::time::Instant::now();
        let flush = async move {
            for t in &self.tasks {
                t.abort();
            }
            if self.recording {
                publish::once(self.cfg.side);
            }
            #[cfg(feature = "otel")]
            if let Some(p) = self.otel {
                p.shutdown(SHUTDOWN_BUDGET.saturating_sub(started.elapsed()))
                    .await;
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
            endpoint: Some("http://c:4318".parse().unwrap()),
            protocol: OtlpProtocol::Http,
        });
        assert_eq!(recorder_kind(&cfg), Some(RecorderKind::Fanout));
        cfg.debug_addr = None;
        assert_eq!(recorder_kind(&cfg), Some(RecorderKind::Otel));
    }

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        |k| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn otlp_from_env_or_settings() {
        let none = TelemetrySettings::default();
        assert_eq!(OtlpConfig::resolve_with(&none, env(&[])), None);
        let set = TelemetrySettings {
            otlp_endpoint: Some("http://c:4317".parse().unwrap()),
            otlp_protocol: Some(OtlpProtocol::Grpc),
        };
        assert_eq!(
            OtlpConfig::resolve_with(&set, env(&[])),
            Some(OtlpConfig {
                endpoint: Some("http://c:4317".parse().unwrap()),
                protocol: OtlpProtocol::Grpc,
            })
        );
        // The environment wins: its endpoints are the SDK's to read.
        let vars = [
            (
                "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
                "http://e:4318/v1/traces",
            ),
            ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf"),
        ];
        let from_env = Some(OtlpConfig {
            endpoint: None,
            protocol: OtlpProtocol::Http,
        });
        assert_eq!(OtlpConfig::resolve_with(&set, env(&vars)), from_env);
        assert_eq!(OtlpConfig::resolve_with(&none, env(&vars)), from_env);
        // An empty variable is unset.
        let empty = [("OTEL_EXPORTER_OTLP_ENDPOINT", "")];
        assert_eq!(OtlpConfig::resolve_with(&none, env(&empty)), None);
    }
}
