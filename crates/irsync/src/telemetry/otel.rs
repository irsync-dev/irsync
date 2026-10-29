//! OTLP export: the tracer and meter providers, and the trace context that
//! travels from the client to the daemon.

use super::init::OtlpConfig;
use super::otel_bridge::OtelRecorder;
use super::Side;
use crate::config::settings::OtlpProtocol;
use anyhow::Context as _;
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::{MetricExporter, SpanExporter, WithExportConfig, WithTonicConfig};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::resource::{EnvResourceDetector, TelemetryResourceDetector};
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider, Tracer};
use opentelemetry_sdk::Resource;
use std::collections::HashMap;
use std::time::Duration;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::registry::LookupSpan;

/// How often metrics are exported unless `OTEL_METRIC_EXPORT_INTERVAL` says otherwise.
const METRIC_INTERVAL: Duration = Duration::from_secs(10);
const SCOPE: &str = "irsync";

/// The `tracing` layer that turns spans into OTel spans.
pub type OtelLayer<S> = tracing_opentelemetry::OpenTelemetryLayer<S, Tracer>;

/// The tracer and meter providers OTLP export runs on.
pub struct OtelProviders {
    tracer: SdkTracerProvider,
    meter: SdkMeterProvider,
}

/// Builds the providers for `cfg`: a batch span processor and a periodic
/// metric reader, each with its own OTLP exporter. The other `OTEL_*`
/// variables are the SDK's. Call inside a runtime context (tonic needs one).
pub fn providers(
    cfg: &OtlpConfig,
    side: Side,
    endpoint_id: Option<String>,
) -> anyhow::Result<OtelProviders> {
    // The OTLP HTTP client's reqwest is built without a crypto provider
    // (iroh's choice); install one unless something else has.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let resource = resource(side, endpoint_id);
    let (spans, metrics) = exporters(cfg)?;
    let mut tracer = SdkTracerProvider::builder()
        .with_batch_exporter(spans)
        .with_resource(resource.clone());
    if let Some(sampler) = sampler(side) {
        tracer = tracer.with_sampler(sampler);
    }
    let tracer = tracer.build();
    let mut reader = PeriodicReader::builder(metrics);
    if !env_set("OTEL_METRIC_EXPORT_INTERVAL") {
        reader = reader.with_interval(METRIC_INTERVAL);
    }
    let meter = SdkMeterProvider::builder()
        .with_reader(reader.build())
        .with_resource(resource)
        .build();
    Ok(OtelProviders { tracer, meter })
}

/// Whether the environment variable `k` is set and not empty.
fn env_set(k: &str) -> bool {
    std::env::var_os(k).is_some_and(|v| !v.is_empty())
}

/// The daemon samples every session, whatever the client's trace context
/// says, unless `OTEL_TRACES_SAMPLER` picks a sampler; the client keeps the
/// SDK's (parent-based, always on).
fn sampler(side: Side) -> Option<Sampler> {
    (side == Side::Daemon && !env_set("OTEL_TRACES_SAMPLER")).then_some(Sampler::AlwaysOn)
}

/// `service.name` (`OTEL_SERVICE_NAME` wins), `service.version`, the
/// endpoint id when known, and `OTEL_RESOURCE_ATTRIBUTES`.
fn resource(side: Side, endpoint_id: Option<String>) -> Resource {
    let default_name = match side {
        Side::Client => "irsync",
        Side::Daemon => "irsync-daemon",
    };
    let mut b = Resource::builder_empty()
        .with_service_name(default_name)
        .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
        .with_detectors(&[
            Box::new(TelemetryResourceDetector),
            Box::new(EnvResourceDetector::new()),
        ]);
    if let Some(id) = endpoint_id {
        b = b.with_attribute(KeyValue::new("irsync.endpoint_id", id));
    }
    match std::env::var("OTEL_SERVICE_NAME") {
        Ok(name) if !name.is_empty() => b.with_service_name(name),
        _ => b,
    }
    .build()
}

/// The span and metric exporters for `cfg`'s protocol. `[telemetry]`'s
/// endpoint is a base URL, as `OTEL_EXPORTER_OTLP_ENDPOINT` is; without
/// it the SDK takes the endpoints from the environment.
fn exporters(cfg: &OtlpConfig) -> anyhow::Result<(SpanExporter, MetricExporter)> {
    let base = cfg
        .endpoint
        .as_ref()
        .map(|u| u.as_str().trim_end_matches('/').to_owned());
    match cfg.protocol {
        OtlpProtocol::Http => {
            let at = |path: &str| base.as_ref().map(|b| format!("{b}{path}"));
            let mut spans = SpanExporter::builder()
                .with_http()
                .with_protocol(opentelemetry_otlp::Protocol::HttpBinary);
            if let Some(e) = at("/v1/traces") {
                spans = spans.with_endpoint(e);
            }
            let mut metrics = MetricExporter::builder()
                .with_http()
                .with_protocol(opentelemetry_otlp::Protocol::HttpBinary);
            if let Some(e) = at("/v1/metrics") {
                metrics = metrics.with_endpoint(e);
            }
            Ok((
                spans.build().context("span exporter")?,
                metrics.build().context("metric exporter")?,
            ))
        }
        OtlpProtocol::Grpc => {
            let https = grpc_is_https(base.as_deref());
            let mut spans = SpanExporter::builder().with_tonic();
            let mut metrics = MetricExporter::builder().with_tonic();
            if let Some(b) = &base {
                spans = spans.with_endpoint(b);
                metrics = metrics.with_endpoint(b);
            }
            if https {
                // tonic trusts no roots unless told to.
                let tls = opentelemetry_otlp::tonic_types::transport::ClientTlsConfig::new()
                    .with_native_roots();
                spans = spans.with_tls_config(tls.clone());
                metrics = metrics.with_tls_config(tls);
            }
            Ok((
                spans.build().context("span exporter")?,
                metrics.build().context("metric exporter")?,
            ))
        }
    }
}

/// Whether the gRPC endpoint in use (`base`, else the environment's) is `https`.
fn grpc_is_https(base: Option<&str>) -> bool {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let any_https = |e: Option<String>| e.is_some_and(|e| e.starts_with("https:"));
    match base {
        Some(b) => b.starts_with("https:"),
        None => {
            let general = env("OTEL_EXPORTER_OTLP_ENDPOINT");
            any_https(env("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT").or(general.clone()))
                || any_https(env("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT").or(general))
        }
    }
}

impl OtelProviders {
    /// The layer exporting spans through the tracer provider.
    pub fn layer<S>(&self) -> OtelLayer<S>
    where
        S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    {
        tracing_opentelemetry::layer().with_tracer(self.tracer.tracer(SCOPE))
    }

    /// A metrics recorder forwarding to the meter provider.
    pub fn recorder(&self) -> OtelRecorder {
        OtelRecorder::new(self.meter.meter(SCOPE))
    }

    /// Flushes and shuts down both providers on threads of their own;
    /// returns after `deadline` at the latest, leaving them behind. Warns
    /// once when export failed or did not finish in time.
    pub async fn shutdown(self, deadline: Duration) {
        let OtelProviders { tracer, meter } = self;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        on_thread("spans", tx.clone(), move || tracer.shutdown());
        on_thread("metrics", tx, move || meter.shutdown());
        let waited = tokio::time::timeout(deadline, async {
            let mut errors = Vec::new();
            while let Some(r) = rx.recv().await {
                errors.extend(r.err());
            }
            errors
        })
        .await;
        match waited {
            Ok(errors) if errors.is_empty() => {}
            Ok(errors) => eprintln!("irsync: OpenTelemetry export: {}", errors.join("; ")),
            Err(_) => eprintln!(
                "irsync: OpenTelemetry export did not finish within {:.1} s",
                deadline.as_secs_f32()
            ),
        }
    }
}

/// Runs `f` on a detached thread and sends its error, if any, labelled `what`.
fn on_thread(
    what: &'static str,
    tx: tokio::sync::mpsc::UnboundedSender<Result<(), String>>,
    f: impl FnOnce() -> opentelemetry_sdk::error::OTelSdkResult + Send + 'static,
) {
    std::thread::spawn(move || {
        let _ = tx.send(f().map_err(|e| format!("{what}: {e}")));
    });
}

/// `span`'s trace context as `(traceparent, tracestate)`; empty strings
/// when the span is not exported.
pub fn inject(span: &tracing::Span) -> (String, String) {
    let mut carrier = HashMap::new();
    TraceContextPropagator::new().inject_context(&span.context(), &mut carrier);
    (
        carrier.remove("traceparent").unwrap_or_default(),
        carrier.remove("tracestate").unwrap_or_default(),
    )
}

/// The remote parent context in `traceparent` and `tracestate`; an empty
/// context when they hold none, or `traceparent` is not well formed.
pub fn extract(traceparent: &str, tracestate: &str) -> opentelemetry::Context {
    if !well_formed(traceparent) {
        return opentelemetry::Context::new();
    }
    let carrier: HashMap<String, String> =
        [("traceparent", traceparent), ("tracestate", tracestate)]
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
    TraceContextPropagator::new().extract(&carrier)
}

/// Whether `traceparent` has the shape of a W3C version-00 header: four
/// fields of 2, 32, 16 and 2 lowercase hex digits. The SDK's parser takes
/// shorter ids too.
fn well_formed(traceparent: &str) -> bool {
    let lens: Vec<usize> = traceparent.split('-').map(str::len).collect();
    lens == [2, 32, 16, 2]
        && traceparent
            .bytes()
            .all(|b| b == b'-' || b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Makes the remote context in `traceparent` and `tracestate` the parent
/// of `span`; does nothing when they hold none or the span is not exported.
pub fn set_parent(span: &tracing::Span, traceparent: &str, tracestate: &str) {
    if traceparent.is_empty() {
        return;
    }
    let _ = span.set_parent(extract(traceparent, tracestate));
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::trace::TraceContextExt;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn trace_context_round_trips() {
        let provider = SdkTracerProvider::builder().build();
        let layer = tracing_opentelemetry::layer().with_tracer(provider.tracer("t"));
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("session");
            let (traceparent, tracestate) = inject(&span);
            assert!(traceparent.starts_with("00-"), "{traceparent}");
            assert_eq!(tracestate, "");
            let cx = extract(&traceparent, &tracestate);
            assert_eq!(
                cx.span().span_context().trace_id(),
                span.context().span().span_context().trace_id()
            );

            let child = tracing::info_span!("session");
            set_parent(&child, &traceparent, &tracestate);
            assert_eq!(inject(&child).0[..35], traceparent[..35]);
        });
        // No exporting layer: nothing to carry.
        assert_eq!(
            inject(&tracing::info_span!("session")),
            (String::new(), String::new())
        );
        assert!(!extract("", "").span().span_context().is_valid());
    }

    #[test]
    fn resource_names_the_side_and_version() {
        if std::env::var_os("OTEL_SERVICE_NAME").is_some() {
            return;
        }
        let r = resource(Side::Daemon, Some("abc".into()));
        let get = |k: &'static str| r.get(&opentelemetry::Key::new(k)).map(|v| v.to_string());
        assert_eq!(get("service.name").as_deref(), Some("irsync-daemon"));
        assert_eq!(
            get("service.version").as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(get("irsync.endpoint_id").as_deref(), Some("abc"));
        let r = resource(Side::Client, None);
        assert_eq!(
            r.get(&opentelemetry::Key::new("service.name"))
                .map(|v| v.to_string()),
            Some("irsync".into())
        );
    }

    #[test]
    fn daemon_samples_what_the_client_did_not() {
        if env_set("OTEL_TRACES_SAMPLER") {
            return;
        }
        assert!(sampler(Side::Client).is_none());
        let provider = SdkTracerProvider::builder()
            .with_sampler(sampler(Side::Daemon).unwrap())
            .build();
        // The client's context, its sampled flag off.
        let parent = extract(
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00",
            "",
        );
        assert!(!parent.span().span_context().is_sampled());
        use opentelemetry::trace::{Span as _, Tracer as _};
        let span = provider.tracer("t").start_with_context("session", &parent);
        assert!(span.span_context().is_sampled());
    }

    #[test]
    fn malformed_context_is_ignored() {
        let exporter = opentelemetry_sdk::trace::InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let layer = tracing_opentelemetry::layer().with_tracer(provider.tracer("t"));
        let subscriber = tracing_subscriber::registry().with(layer);
        let valid = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let huge = format!("k={}", "v".repeat(1024 * 1024));
        tracing::subscriber::with_default(subscriber, || {
            for bad in [
                "garbage",
                "00-4bf92f3577b34da6a3ce929d0e0e47-00f067aa0ba902b7-01",
                "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
            ] {
                let span = tracing::info_span!("session", bad);
                set_parent(&span, bad, "");
            }
            let span = tracing::info_span!("session", bad = "tracestate");
            set_parent(&span, valid, &huge);
            let (traceparent, tracestate) = inject(&span);
            assert_eq!(traceparent[..35], valid[..35]);
            assert!(tracestate.len() < 1024, "{}", tracestate.len());
        });
        let spans = exporter.get_finished_spans().unwrap();
        assert_eq!(spans.len(), 4);
        for s in &spans[..3] {
            assert_eq!(
                s.parent_span_id,
                opentelemetry::trace::SpanId::INVALID,
                "{s:?}"
            );
            assert!(!s.parent_span_is_remote);
        }
        assert!(spans[3].parent_span_is_remote);
        assert!(spans[3].span_context.trace_state().header().len() < 1024);
    }
}
