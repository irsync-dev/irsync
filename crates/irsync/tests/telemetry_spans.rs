//! The span tree of a session: one `session` span per side with the phases
//! and per-file spans under it, over loopback endpoints with an in-process
//! daemon.

mod common;

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use common::{archive_opts, Flow, Harness};
use irsync::session::reconnect::ReconnectOpts;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Level, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

const MIB: usize = 1024 * 1024;

#[derive(Clone, Debug)]
struct Rec {
    id: u64,
    name: &'static str,
    level: Level,
    parent: Option<u64>,
    fields: HashMap<String, String>,
}

#[derive(Default)]
struct Fields(HashMap<String, String>);

impl Visit for Fields {
    fn record_str(&mut self, f: &Field, v: &str) {
        self.0.insert(f.name().into(), v.into());
    }
    fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
        self.0.insert(f.name().into(), format!("{v:?}"));
    }
}

static RECS: Mutex<Vec<Rec>> = Mutex::new(Vec::new());

/// Records every irsync span created, with its parent (explicit or contextual).
struct Capture;

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Capture {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        // Dependencies (quinn has `send`, `recv` and `frame` spans) are not ours.
        if !attrs.metadata().target().starts_with("irsync") {
            return;
        }
        let parent = if attrs.is_root() {
            None
        } else if let Some(p) = attrs.parent() {
            Some(p.into_u64())
        } else {
            ctx.current_span().id().map(Id::into_u64)
        };
        let mut f = Fields::default();
        attrs.record(&mut f);
        RECS.lock().unwrap().push(Rec {
            id: id.into_u64(),
            name: attrs.metadata().name(),
            level: *attrs.metadata().level(),
            parent,
            fields: f.0,
        });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, _ctx: Context<'_, S>) {
        let mut f = Fields::default();
        values.record(&mut f);
        let mut recs = RECS.lock().unwrap();
        if let Some(r) = recs.iter_mut().find(|r| r.id == id.into_u64()) {
            r.fields.extend(f.0);
        }
    }
}

/// Installs the capture layer for the whole binary (spans made on any
/// runtime or blocking thread are seen) and serializes the tests, which
/// share it; the guard returns the spans of this test only.
async fn capture() -> tokio::sync::MutexGuard<'static, ()> {
    static INIT: OnceLock<()> = OnceLock::new();
    static SERIAL: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    INIT.get_or_init(|| {
        tracing::subscriber::set_global_default(tracing_subscriber::registry().with(Capture))
            .expect("no other global subscriber in this test binary");
    });
    let guard = SERIAL.get_or_init(Default::default).lock().await;
    RECS.lock().unwrap().clear();
    guard
}

fn recs() -> Vec<Rec> {
    RECS.lock().unwrap().clone()
}

fn named<'a>(all: &'a [Rec], name: &str) -> Vec<&'a Rec> {
    all.iter().filter(|r| r.name == name).collect()
}

fn ancestors<'a>(all: &'a [Rec], r: &Rec) -> Vec<&'a Rec> {
    let mut out = Vec::new();
    let mut at = r.parent;
    while let Some(id) = at {
        let Some(p) = all.iter().find(|p| p.id == id) else {
            break;
        };
        out.push(p);
        at = p.parent;
    }
    out
}

fn side<'a>(all: &'a [Rec], name: &str, side: &str) -> Vec<&'a Rec> {
    all.iter()
        .filter(|r| r.name == name && r.fields.get("side").is_some_and(|s| s == side))
        .collect()
}

fn five_files(root: &Path) {
    for i in 0..5u8 {
        fs::write(root.join(format!("f{i}.bin")), vec![i; 40_000]).unwrap();
    }
}

fn fast() -> ReconnectOpts {
    ReconnectOpts {
        backoff_min: Duration::from_millis(100),
        ..ReconnectOpts::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_span_tree() {
    let _serial = capture().await;
    let opts = archive_opts();
    let h = Harness::new().await;
    five_files(h.src(Flow::Push));
    let (out, _) = h
        .run_reconnect(&opts, Flow::Push, fast(), h.local_side())
        .await;
    out.unwrap();

    let all = recs();
    let client = side(&all, "session", "client");
    let daemon = side(&all, "session", "daemon");
    assert_eq!(client.len(), 1, "{all:#?}");
    assert_eq!(daemon.len(), 1, "{all:#?}");
    assert_eq!(named(&all, "session").len(), 2);
    assert_eq!(client[0].parent, None);
    assert_eq!(daemon[0].parent, None);
    assert!(
        client[0].fields["session_id"].len() == 32,
        "{:?}",
        client[0]
    );
    // Not exporting: the endpoint id stays off the span (and the log lines).
    for s in [client[0], daemon[0]] {
        assert!(!s.fields.contains_key("endpoint_id"), "{s:?}");
    }
    for phase in ["handshake", "list", "plan", "transfer"] {
        let spans = named(&all, phase);
        assert!(!spans.is_empty(), "no {phase} span");
        for s in spans {
            assert!(
                ancestors(&all, s).iter().any(|a| a.name == "session"),
                "{phase} outside a session: {s:?}"
            );
        }
    }
    // Each side opened its own handshake, list and transfer under its session.
    for session in [client[0], daemon[0]] {
        for phase in ["handshake", "list", "transfer"] {
            assert!(
                named(&all, phase)
                    .iter()
                    .any(|s| ancestors(&all, s).iter().any(|a| a.id == session.id)),
                "{phase} missing under {:?}",
                session.fields["side"]
            );
        }
    }
    assert!(named(&all, "chunk").is_empty());
    assert!(named(&all, "frame").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_spans_are_debug() {
    let _serial = capture().await;
    let opts = archive_opts();
    for flow in [Flow::Push, Flow::Pull] {
        RECS.lock().unwrap().clear();
        let h = Harness::new().await;
        five_files(h.src(flow));
        let (out, _) = h.run_reconnect(&opts, flow, fast(), h.local_side()).await;
        out.unwrap();

        let all = recs();
        let files = named(&all, "file");
        // Five per side.
        assert_eq!(files.len(), 10, "{flow:?}: {files:#?}");
        for f in &files {
            assert_eq!(f.level, Level::DEBUG, "{f:?}");
            assert_eq!(f.fields["kind"], "new");
            assert_eq!(
                f.parent
                    .map(|p| all.iter().find(|r| r.id == p).unwrap().name),
                Some("transfer")
            );
        }
        for child in ["hash", "send", "recv", "place"] {
            let spans = named(&all, child);
            assert!(!spans.is_empty(), "{flow:?}: no {child} span");
            for s in spans {
                assert_eq!(s.level, Level::DEBUG);
                let parent = all.iter().find(|r| Some(r.id) == s.parent).unwrap();
                assert_eq!(parent.name, "file", "{s:?}");
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconnect_stays_in_one_session_span() {
    let _serial = capture().await;
    let opts = archive_opts();
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
        .run_reconnect(&opts, Flow::Push, fast(), h.local_side())
        .await;
    out.unwrap();
    assert!(h.daemon.fault.fired() && reconnects >= 1);

    let all = recs();
    let client = side(&all, "session", "client");
    assert_eq!(client.len(), 1, "{client:#?}");
    let connects: Vec<_> = named(&all, "connect")
        .into_iter()
        .filter(|c| c.parent == Some(client[0].id))
        .collect();
    assert!(connects.len() >= 2, "{connects:#?}");
    assert!(!named(&all, "reconnect").is_empty());
    assert!(client[0].fields["reconnects"].parse::<u64>().unwrap() >= 1);
    // The daemon saw one session per connection, each its own root.
    let daemon = side(&all, "session", "daemon");
    assert!(daemon.len() >= 2);
    assert!(daemon.iter().all(|d| d.parent.is_none()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_connection_still_records_its_network_figures() {
    let _serial = capture().await;
    // The figures come from the connection sampler, which runs only with
    // telemetry on.
    irsync::telemetry::set_enabled(true);
    let opts = archive_opts();
    let h = Harness::new().await;
    let root = h.src(Flow::Push);
    for i in 0..20u32 {
        let bytes: Vec<u8> = (0..2 * MIB as u32)
            .map(|j| (j.wrapping_mul(2654435761).wrapping_add(i * 7919) >> 13) as u8)
            .collect();
        fs::write(root.join(format!("f{i:02}.bin")), bytes).unwrap();
    }
    h.daemon.fault.drop_after_bytes(10 * MIB as u64);
    let (out, _) = h
        .run_reconnect(&opts, Flow::Push, fast(), h.local_side())
        .await;
    out.unwrap();
    assert!(h.daemon.fault.fired());
    // The daemon's session on the dropped connection failed; its span
    // still carries what that connection measured.
    let all = recs();
    let daemon = side(&all, "session", "daemon");
    assert!(daemon.len() >= 2, "{daemon:#?}");
    for d in &daemon {
        for f in ["rtt_max_ms", "cwnd", "lost_packets"] {
            assert!(d.fields.contains_key(f), "{f} missing: {d:#?}");
        }
    }
}
