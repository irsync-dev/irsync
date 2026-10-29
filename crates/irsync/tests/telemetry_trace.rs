//! Trace propagation: the daemon's `session` span is a child of the
//! client's, in one in-process trace exported to an in-memory exporter.
#![cfg(feature = "otel")]

mod common;

use std::fs;
use std::sync::OnceLock;
use std::time::Duration;

use common::{archive_opts, Flow, Harness};
use irsync::session::reconnect::ReconnectOpts;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use tracing_subscriber::layer::SubscriberExt;

const MIB: usize = 1024 * 1024;

/// Installs the global subscriber once, with an OTel layer over an
/// in-memory exporter, and turns trace-context handling on; serializes the
/// tests, which share it. The exporter is emptied for each test.
async fn setup() -> (
    tokio::sync::MutexGuard<'static, ()>,
    &'static InMemorySpanExporter,
    &'static SdkTracerProvider,
) {
    static SERIAL: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    static INIT: OnceLock<(InMemorySpanExporter, SdkTracerProvider)> = OnceLock::new();
    let (exporter, provider) = INIT.get_or_init(|| {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let layer = tracing_opentelemetry::layer().with_tracer(provider.tracer("test"));
        tracing::subscriber::set_global_default(tracing_subscriber::registry().with(layer))
            .expect("no other global subscriber in this test binary");
        irsync::telemetry::set_exporting(true);
        (exporter, provider)
    });
    let guard = SERIAL.get_or_init(Default::default).lock().await;
    // The previous test's last spans can still be ending: let them, then clear.
    let mut seen = usize::MAX;
    loop {
        provider.force_flush().unwrap();
        let now = exporter.get_finished_spans().unwrap().len();
        if now == seen {
            break;
        }
        seen = now;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    exporter.reset();
    (guard, exporter, provider)
}

fn sessions(exporter: &InMemorySpanExporter, side: &str) -> Vec<SpanData> {
    exporter
        .get_finished_spans()
        .unwrap()
        .into_iter()
        .filter(|s| {
            s.name == "session"
                && s.attributes
                    .iter()
                    .any(|kv| kv.key.as_str() == "side" && kv.value.as_str() == side)
        })
        .collect()
}

/// Waits until `side` has exported `n` session spans: a span ends once the
/// last task holding a clone of it is gone, which can trail the session.
async fn sessions_exported(
    exporter: &InMemorySpanExporter,
    provider: &SdkTracerProvider,
    side: &str,
    n: usize,
) -> Vec<SpanData> {
    for _ in 0..200 {
        provider.force_flush().unwrap();
        let got = sessions(exporter, side);
        if got.len() >= n {
            return got;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("fewer than {n} {side} session spans exported");
}

fn attr(s: &SpanData, key: &str) -> Option<String> {
    s.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| kv.value.to_string())
}

fn fast() -> ReconnectOpts {
    ReconnectOpts {
        backoff_min: Duration::from_millis(100),
        ..ReconnectOpts::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_session_is_child_of_client_session() {
    let (_serial, exporter, provider) = setup().await;
    let h = Harness::new().await;
    fs::write(h.src(Flow::Push).join("a.txt"), b"hello").unwrap();
    let (out, _) = h
        .run_reconnect(&archive_opts(), Flow::Push, fast(), h.local_side())
        .await;
    out.unwrap();

    let client = sessions_exported(exporter, provider, "client", 1).await;
    let daemon = sessions_exported(exporter, provider, "daemon", 1).await;
    assert_eq!((client.len(), daemon.len()), (1, 1));
    let (client, daemon) = (&client[0], &daemon[0]);
    assert_eq!(
        client.span_context.trace_id(),
        daemon.span_context.trace_id()
    );
    assert_eq!(daemon.parent_span_id, client.span_context.span_id());
    assert!(daemon.parent_span_is_remote);
    for s in [client, daemon] {
        assert!(!attr(s, "endpoint_id").unwrap_or_default().is_empty());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconnect_keeps_trace_id() {
    let (_serial, exporter, provider) = setup().await;
    let h = Harness::new().await;
    let root = h.src(Flow::Push);
    for i in 0..20u32 {
        let bytes: Vec<u8> = (0..2 * MIB as u32)
            .map(|j| (j.wrapping_mul(2654435761).wrapping_add(i * 7919) >> 13) as u8)
            .collect();
        fs::write(root.join(format!("f{i:02}.bin")), bytes).unwrap();
    }
    h.daemon.fault.drop_after_bytes(10 * MIB as u64);
    let (out, reconnects) = h
        .run_reconnect(&archive_opts(), Flow::Push, fast(), h.local_side())
        .await;
    out.unwrap();
    assert!(h.daemon.fault.fired() && reconnects >= 1);

    let client = sessions_exported(exporter, provider, "client", 1).await;
    assert_eq!(client.len(), 1);
    let daemon = sessions_exported(exporter, provider, "daemon", 2).await;
    assert!(daemon.len() >= 2, "{daemon:#?}");
    for d in &daemon {
        assert_eq!(d.span_context.trace_id(), client[0].span_context.trace_id());
        assert_eq!(d.parent_span_id, client[0].span_context.span_id());
    }
}
