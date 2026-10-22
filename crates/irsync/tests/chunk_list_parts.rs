//! Chunk lists in parts: every chunk list travels as `ChunkListPart`
//! messages, reassembled on the receiver; the rest of the transfer is
//! unchanged, and a connection lost between parts resumes cleanly.

mod common;

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::atomic::Ordering::SeqCst;
use std::time::Duration;

use common::{archive_opts, Flow, Harness};
use irsync::session::reconnect::ReconnectOpts;
use irsync::session::PartsFault;

const MIB: usize = 1024 * 1024;

async fn within60<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(60), f)
        .await
        .expect("test timed out after 60 s")
}

/// Deterministic pseudo-random bytes.
fn data(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2654435761).wrapping_add(1);
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

fn fast() -> ReconnectOpts {
    ReconnectOpts {
        backoff_min: Duration::from_millis(100),
        ..ReconnectOpts::default()
    }
}

fn count(events: &[String], what: &str) -> usize {
    events.iter().filter(|e| e.as_str() == what).count()
}

/// Overwrites `bytes` at `offset` in place.
fn patch(p: &Path, offset: u64, bytes: &[u8]) {
    fs::OpenOptions::new()
        .write(true)
        .open(p)
        .unwrap()
        .write_all_at(bytes, offset)
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chunk_list_is_sent_in_parts_and_reassembled() {
    within60(async {
        let opts = archive_opts();
        let flow = Flow::Push;
        let h = Harness::new().await;
        let content = data(4 * MIB, 31);
        fs::write(h.src(flow).join("big.bin"), &content).unwrap();
        let mut local = h.local_side();
        // Three records per part: a 4 MiB file takes several.
        local.hooks.chunk_list_part_records = Some(3);
        let (out, reconnects) = h.run_reconnect(&opts, flow, fast(), local).await;
        let out = out.unwrap();
        assert_eq!(reconnects, 0);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.files_transferred, 1);
        assert_eq!(fs::read(h.dst(flow).join("big.bin")).unwrap(), content);
        let events = h.daemon.events.lock().unwrap().clone();
        // 4 MiB is at least 4 chunks: several parts, one assembled list.
        assert!(count(&events, "part:big.bin") >= 2, "{events:?}");
        assert_eq!(count(&events, "chunklist:big.bin"), 1, "{events:?}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delta_transfers_work_over_parts() {
    within60(async {
        let opts = archive_opts();
        let flow = Flow::Push;
        let h = Harness::new().await;
        let src = h.src(flow).join("a.bin");
        fs::write(&src, data(4 * MIB, 33)).unwrap();
        let mut local = h.local_side();
        local.hooks.chunk_list_part_records = Some(3);
        let first_run = h.run_side(&opts, flow, local.clone()).await.unwrap();
        assert!(first_run.errors.is_empty(), "{:?}", first_run.errors);

        patch(&src, 3 * MIB as u64, &data(MIB, 34));
        h.daemon.events.lock().unwrap().clear();
        let out = h.run_side(&opts, flow, local).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(
            fs::read(h.dst(flow).join("a.bin")).unwrap(),
            fs::read(&src).unwrap()
        );
        assert!(
            out.bytes_transferred < 2 * MIB as u64,
            "{} bytes crossed the wire",
            out.bytes_transferred
        );
        let events = h.daemon.events.lock().unwrap().clone();
        assert!(count(&events, "part:a.bin") >= 2, "{events:?}");
        assert_eq!(h.daemon.rerequests.load(SeqCst), 0);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_connection_lost_between_parts_resumes_and_completes() {
    within60(async {
        let opts = archive_opts();
        let flow = Flow::Push;
        let h = Harness::new().await;
        let content = data(4 * MIB, 35);
        fs::write(h.src(flow).join("big.bin"), &content).unwrap();
        let fault = PartsFault::after(2);
        let mut local = h.local_side();
        local.hooks.chunk_list_part_records = Some(3);
        local.hooks.drop_after_parts = Some(fault.clone());
        let (out, reconnects) = h.run_reconnect(&opts, flow, fast(), local).await;
        let out = out.unwrap();
        assert!(fault.fired(), "the fault never fired");
        assert_eq!(reconnects, 1);
        assert!(out.resumed, "the second connection did not resume");
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(h.dst(flow).join("big.bin")).unwrap(), content);
        assert!(!h.dst(flow).join(".irsync.big.bin.part").exists());
        let events = h.daemon.events.lock().unwrap().clone();
        // The first connection's half-built list was dropped with it; the
        // resumed one assembled a full sequence.
        assert_eq!(count(&events, "chunklist:big.bin"), 1, "{events:?}");
    })
    .await;
}
