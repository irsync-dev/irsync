//! Sub-chunk delta for `DELTA` files: a chunk the receiver lacks but whose
//! neighbourhood it holds in the old file is offered with block
//! signatures, and the sender answers with copy/literal operations instead
//! of the chunk's bytes. Verification stays chunk-level, so a basis that
//! no longer matches only costs a raw re-request.

mod common;

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::{archive_opts, Flow, Harness};

const KIB: usize = 1024;
const MIB: usize = 1024 * 1024;

async fn within30<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(30), f)
        .await
        .expect("test timed out after 30 s")
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

/// Overwrites `bytes` at `offset` in place (same size, mtime moves).
fn patch(p: &Path, offset: u64, bytes: &[u8]) {
    fs::OpenOptions::new()
        .write(true)
        .open(p)
        .unwrap()
        .write_all_at(bytes, offset)
        .unwrap();
}

fn count(events: &[String], prefix: &str) -> usize {
    events.iter().filter(|e| e.starts_with(prefix)).count()
}

/// Syncs `x.bin` with `content` once in direction `flow`, so the second
/// run is a `DELTA` against a warm index.
async fn seeded(flow: Flow, content: &[u8]) -> Harness {
    let h = Harness::new().await;
    fs::write(h.src(flow).join("x.bin"), content).unwrap();
    let out = h.run(&archive_opts(), flow).await.unwrap();
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    h
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_changed_byte_in_a_large_file_costs_a_block_not_a_chunk() {
    within30(async {
        let flow = Flow::Pull;
        let h = seeded(flow, &data(4 * MIB, 1)).await;
        let src = h.src(flow).join("x.bin");
        patch(&src, 2 * MIB as u64 + 12_345, &[0x5a]);
        let log: irsync::session::EventLog = Arc::default();
        let mut local = h.local_side();
        local.hooks.events = Some(log.clone());
        let out = h.run_side(&archive_opts(), flow, local).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(
            fs::read(h.dst(flow).join("x.bin")).unwrap(),
            fs::read(&src).unwrap()
        );
        let events = log.lock().unwrap().clone();
        assert_eq!(count(&events, "basis:x.bin"), 1, "{events:?}");
        assert_eq!(count(&events, "ops:x.bin"), 1, "{events:?}");
        assert!(
            out.bytes_transferred < 16 * KIB as u64,
            "{} bytes crossed the wire for a one-byte change",
            out.bytes_transferred
        );
        assert_eq!(out.files_transferred, 1);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scattered_page_writes_cost_blocks_not_chunks() {
    within30(async {
        // Pushed: the sender's count is the bytes it put on the wire.
        let flow = Flow::Push;
        let h = seeded(flow, &data(16 * MIB, 2)).await;
        let src = h.src(flow).join("x.bin");
        for k in 0..8u64 {
            patch(
                &src,
                k * 2 * MIB as u64 + 777_777,
                &data(4 * KIB, 10 + k as u32),
            );
        }
        h.daemon.events.lock().unwrap().clear();
        let out = h.run(&archive_opts(), flow).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(
            fs::read(h.dst(flow).join("x.bin")).unwrap(),
            fs::read(&src).unwrap()
        );
        let events = h.daemon.events.lock().unwrap().clone();
        assert!(count(&events, "ops:x.bin") >= 8, "{events:?}");
        assert!(
            out.bytes_transferred < 8 * 16 * KIB as u64,
            "{} bytes crossed the wire for eight 4 KiB writes",
            out.bytes_transferred
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_single_chunk_file_with_one_edit_is_not_resent_whole() {
    within30(async {
        let flow = Flow::Pull;
        let h = seeded(flow, &data(100 * KIB, 3)).await;
        let src = h.src(flow).join("x.bin");
        patch(&src, 50_000, b"edited");
        let log: irsync::session::EventLog = Arc::default();
        let mut local = h.local_side();
        local.hooks.events = Some(log.clone());
        let out = h.run_side(&archive_opts(), flow, local).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(
            fs::read(h.dst(flow).join("x.bin")).unwrap(),
            fs::read(&src).unwrap()
        );
        let events = log.lock().unwrap().clone();
        assert_eq!(count(&events, "ops:x.bin"), 1, "{events:?}");
        assert!(
            out.bytes_transferred < 4 * KIB as u64,
            "{} bytes crossed the wire for a six-byte edit",
            out.bytes_transferred
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rewritten_file_offers_no_bases() {
    within30(async {
        let flow = Flow::Pull;
        let h = seeded(flow, &data(4 * MIB, 4)).await;
        let src = h.src(flow).join("x.bin");
        fs::write(&src, data(4 * MIB, 5)).unwrap();
        let log: irsync::session::EventLog = Arc::default();
        let mut local = h.local_side();
        local.hooks.events = Some(log.clone());
        let out = h.run_side(&archive_opts(), flow, local).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(
            fs::read(h.dst(flow).join("x.bin")).unwrap(),
            fs::read(&src).unwrap()
        );
        let events = log.lock().unwrap().clone();
        assert_eq!(count(&events, "basis:x.bin"), 0, "{events:?}");
        assert_eq!(count(&events, "ops:x.bin"), 0, "{events:?}");
        assert!(out.bytes_transferred >= 4 * MIB as u64);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chunks_past_the_end_of_the_old_file_get_no_basis() {
    within30(async {
        let flow = Flow::Pull;
        let old = data(4 * MIB, 6);
        let h = seeded(flow, &old).await;
        let src = h.src(flow).join("x.bin");
        let mut new = old.clone();
        new.extend_from_slice(&data(2 * MIB, 7));
        fs::write(&src, &new).unwrap();
        let log: irsync::session::EventLog = Arc::default();
        let mut local = h.local_side();
        local.hooks.events = Some(log.clone());
        let out = h.run_side(&archive_opts(), flow, local).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(h.dst(flow).join("x.bin")).unwrap(), new);
        // Only the old file's last chunk (its end moved) overlaps the old
        // file; the appended chunks start past its end.
        let events = log.lock().unwrap().clone();
        assert!(count(&events, "basis:x.bin") <= 1, "{events:?}");
        assert!(
            out.bytes_transferred < (2 * MIB + 64 * KIB) as u64,
            "{} bytes crossed the wire for a 2 MiB append",
            out.bytes_transferred
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn old_file_modified_in_place_after_the_bases_falls_back_to_raw() {
    within30(async {
        let flow = Flow::Pull;
        let h = seeded(flow, &data(4 * MIB, 8)).await;
        let src = h.src(flow).join("x.bin");
        let dst = h.dst(flow).join("x.bin");
        patch(&src, 2 * MIB as u64 + 100, &[0x11]);
        // Once the bases are out, the old file rots under them: the
        // sender's copies then rebuild wrong bytes, which never verify.
        let rotten = dst.clone();
        let log: irsync::session::EventLog = Arc::default();
        let rerequests = Arc::new(AtomicU64::new(0));
        let mut local = h.local_side();
        local.hooks.events = Some(log.clone());
        local.hooks.rerequests = Some(rerequests.clone());
        local.hooks.after_bases = Some(Arc::new(move |_| {
            let rotten = rotten.clone();
            Box::pin(async move {
                patch(&rotten, 2 * MIB as u64 + 50_000, &data(1024, 9));
            })
        }));
        let out = h.run_side(&archive_opts(), flow, local).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(&dst).unwrap(), fs::read(&src).unwrap());
        assert_eq!(rerequests.load(Ordering::SeqCst), 1);
        let events = log.lock().unwrap().clone();
        assert_eq!(count(&events, "basis:x.bin"), 1, "{events:?}");
        // The rebuilt chunk was rejected; the raw re-send was taken.
        assert_eq!(count(&events, "ops:x.bin"), 0, "{events:?}");
        assert!(!h.dst(flow).join(".irsync.x.bin.part").exists());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_connection_lost_mid_delta_resumes_and_completes() {
    within30(async {
        let flow = Flow::Pull;
        let h = seeded(flow, &data(16 * MIB, 20)).await;
        let src = h.src(flow).join("x.bin");
        for k in 0..8u64 {
            patch(
                &src,
                k * 2 * MIB as u64 + 333_333,
                &data(4 * KIB, 30 + k as u32),
            );
        }
        // The daemon sends: drop its connection a few operation frames in.
        h.daemon.fault.drop_after_bytes(3 * KIB as u64);
        let ropts = irsync::session::reconnect::ReconnectOpts {
            backoff_min: Duration::from_millis(100),
            ..Default::default()
        };
        let (out, reconnects) = h
            .run_reconnect(&archive_opts(), flow, ropts, h.local_side())
            .await;
        let out = out.unwrap();
        assert!(h.daemon.fault.fired());
        assert!(reconnects >= 1);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(
            fs::read(h.dst(flow).join("x.bin")).unwrap(),
            fs::read(&src).unwrap()
        );
        assert!(!h.dst(flow).join(".irsync.x.bin.part").exists());
    })
    .await;
}
