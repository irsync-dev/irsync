//! OTLP export: the metrics bridge, and the real binaries against a fake
//! collector and a dead one.
#![cfg(feature = "otel")]

mod common;

use irsync::telemetry::otel_bridge::OtelRecorder;
use opentelemetry::metrics::MeterProvider;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData, ResourceMetrics};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn find<'a>(rm: &'a [ResourceMetrics], name: &str) -> &'a AggregatedMetrics {
    rm.iter()
        .flat_map(|r| r.scope_metrics())
        .flat_map(|s| s.metrics())
        .find(|m| m.name() == name)
        .unwrap_or_else(|| panic!("no metric {name}"))
        .data()
}

#[test]
fn bridge_forwards_counters_and_histograms() {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    let recorder = OtelRecorder::new(provider.meter("test"));
    metrics::with_local_recorder(&recorder, || {
        metrics::counter!("t_total", "side" => "client").absolute(5);
        metrics::counter!("t_total", "side" => "client").absolute(8);
        metrics::histogram!("t_seconds").record(0.5);
    });
    provider.force_flush().unwrap();
    let rm = exporter.get_finished_metrics().unwrap();

    let AggregatedMetrics::U64(MetricData::Sum(sum)) = find(&rm, "t_total") else {
        panic!("t_total is not a u64 sum");
    };
    let points: Vec<_> = sum.data_points().collect();
    assert_eq!(points.len(), 1);
    assert_eq!(points[0].value(), 8);
    assert!(points[0]
        .attributes()
        .any(|kv| kv.key.as_str() == "side" && kv.value.as_str() == "client"));

    let AggregatedMetrics::F64(MetricData::Histogram(h)) = find(&rm, "t_seconds") else {
        panic!("t_seconds is not an f64 histogram");
    };
    let points: Vec<_> = h.data_points().collect();
    assert_eq!(points.len(), 1);
    assert_eq!(points[0].count(), 1);
    assert_eq!(points[0].sum(), 0.5);
}

/// A collector that answers every request `200` and records `METHOD PATH`.
fn fake_collector() -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { break };
            let log = log.clone();
            std::thread::spawn(move || serve(conn, &log));
        }
    });
    (port, seen)
}

/// Answers the requests on one (keep-alive) connection until it closes.
fn serve(conn: TcpStream, log: &Mutex<Vec<String>>) {
    let mut out = conn.try_clone().unwrap();
    let mut r = BufReader::new(conn);
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut len = 0;
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                if k.eq_ignore_ascii_case("content-length") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
        }
        let mut body = vec![0; len];
        if r.read_exact(&mut body).is_err() {
            return;
        }
        let mut parts = line.split_whitespace();
        let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        log.lock().unwrap().push(format!("{method} {path}"));
        let ok =
            "HTTP/1.1 200 OK\r\ncontent-type: application/x-protobuf\r\ncontent-length: 0\r\n\r\n";
        if out.write_all(ok.as_bytes()).is_err() {
            return;
        }
    }
}

/// A daemon exporting `m1` and `m2` (each holding nothing yet) to the client.
struct Setup {
    _dir: tempfile::TempDir,
    _daemon: common::BinaryDaemon,
    tmp: std::path::PathBuf,
    client: std::path::PathBuf,
    id: String,
    address: String,
}

fn run(home: &Path, args: &[&str]) -> Output {
    let o = Command::new(env!("CARGO_BIN_EXE_irsync"))
        .args(args)
        .env("HOME", home)
        .env("XDG_STATE_HOME", home.join("xdg"))
        .env_remove("IRSYNC_PASSPHRASE")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    o
}

fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().to_path_buf();
    let server = tmp.join("server");
    let client = tmp.join("client");
    for d in [
        &server,
        &client,
        &tmp.join("src"),
        &tmp.join("m1"),
        &tmp.join("m2"),
    ] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(tmp.join("src/a.txt"), b"hello").unwrap();
    let daemon = common::spawn_daemon_binary(&server);
    let client_id = String::from_utf8(run(&client, &["id", "show"]).stdout)
        .unwrap()
        .lines()
        .nth(1)
        .unwrap()
        .to_string();
    for m in ["m1", "m2"] {
        let spec = format!("{m}:{}", tmp.join(m).display());
        run(&server, &["export", "add", &spec]);
        run(&server, &["grant", "add", m, &client_id]);
    }
    Setup {
        id: daemon.id.to_string(),
        address: format!("127.0.0.1:{}", daemon.listen.port()),
        _dir: dir,
        _daemon: daemon,
        tmp,
        client,
    }
}

/// Pushes `src/` into export `m` with `env` set; returns the output and how long it took.
fn push(s: &Setup, m: &str, env: &[(&str, &str)]) -> (Output, Duration) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_irsync"));
    c.env("HOME", &s.client)
        .env("XDG_STATE_HOME", s.client.join("xdg"))
        .env_remove("IRSYNC_PASSPHRASE")
        .env_remove("RUST_LOG")
        .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
        .env_remove("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
        .env_remove("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT")
        .env_remove("OTEL_EXPORTER_OTLP_PROTOCOL")
        .envs(env.iter().copied())
        .args(["-a", "--reconnect", "0", "--address", &s.address])
        .arg(format!("{}/", s.tmp.join("src").display()))
        .arg(format!("{}:{m}/", s.id))
        .stdin(Stdio::null());
    let started = Instant::now();
    let o = c.output().unwrap();
    (o, started.elapsed())
}

#[test]
fn exports_spans_to_a_fake_http_collector() {
    let s = setup();
    let (port, seen) = fake_collector();
    let endpoint = format!("http://127.0.0.1:{port}");
    let (o, _) = push(
        &s,
        "m1",
        &[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", &endpoint),
            ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf"),
        ],
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(std::fs::read(s.tmp.join("m1/a.txt")).unwrap(), b"hello");
    let seen = seen.lock().unwrap().clone();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        seen.iter().any(|r| r == "POST /v1/traces"),
        "{seen:?}\n{err}"
    );
    assert!(
        seen.iter().any(|r| r == "POST /v1/metrics"),
        "{seen:?}\n{err}"
    );
}

#[test]
fn dead_collector_does_not_delay_much() {
    let s = setup();
    let (o, normal) = push(&s, "m1", &[]);
    assert_eq!(o.status.code(), Some(0));
    let (o, dead) = push(
        &s,
        "m2",
        &[("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:9")],
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(std::fs::read(s.tmp.join("m2/a.txt")).unwrap(), b"hello");
    assert!(
        dead <= normal + Duration::from_millis(2500),
        "{dead:?} against {normal:?} without export"
    );
}
