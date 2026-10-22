//! `DELTA` and local-reuse transfers: the receiver asks the sender for the
//! chunks it lacks before it copies the ones it has, copies while the
//! network delivers, hashes the old file once and early, and recovers from
//! a local source whose bytes no longer match the index.

mod common;

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

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

fn set_mtime(p: &Path, t: SystemTime) {
    fs::File::open(p).unwrap().set_modified(t).unwrap();
}

/// Position of the first event starting with `prefix`, or a panic naming the log.
fn first(events: &[String], prefix: &str) -> usize {
    events
        .iter()
        .position(|e| e.starts_with(prefix))
        .unwrap_or_else(|| panic!("no {prefix:?} in {events:?}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delta_requests_remote_chunks_before_copying_local_ones() {
    within30(async {
        let opts = archive_opts();
        let flow = Flow::Pull;
        let h = Harness::new().await;
        let src = h.src(flow).join("x.bin");
        fs::write(&src, data(4 * MIB, 1)).unwrap();
        let first_run = h.run(&opts, flow).await.unwrap();
        assert!(first_run.errors.is_empty(), "{:?}", first_run.errors);

        // The tail changes: the head's chunks are local, the tail's are not.
        patch(&src, 3 * MIB as u64, &data(MIB, 2));
        let log: irsync::session::EventLog = Arc::default();
        let mut local = h.local_side();
        local.hooks.events = Some(log.clone());
        // Every local copy takes this long: the network chunks must land
        // while the copies are still running, not after them.
        local.hooks.slow_local_copy = Some(Duration::from_millis(150));
        let out = h.run_side(&opts, flow, local).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(
            fs::read(h.dst(flow).join("x.bin")).unwrap(),
            fs::read(&src).unwrap()
        );
        assert!(
            out.bytes_transferred < 2 * MIB as u64,
            "the unchanged head crossed the wire: {} bytes",
            out.bytes_transferred
        );
        let events = log.lock().unwrap().clone();
        let need = first(&events, "need:x.bin");
        let chunk = first(&events, "chunk:x.bin");
        let copied = first(&events, "copied:x.bin");
        assert!(need < chunk, "{events:?}");
        assert!(
            chunk < copied,
            "network data arrived only after the local copies: {events:?}"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stale_local_chunk_is_re_requested_from_the_sender() {
    within30(async {
        let opts = archive_opts();
        let flow = Flow::Push;
        let h = Harness::new().await;
        let content = data(4 * MIB, 3);
        fs::write(h.src(flow).join("a.bin"), &content).unwrap();
        let first_run = h.run(&opts, flow).await.unwrap();
        assert!(first_run.errors.is_empty(), "{:?}", first_run.errors);

        // The destination's copy of a.bin rots in place, mtime put back, so
        // the planner skips it and its index rows go stale.
        let dst_a = h.dst(flow).join("a.bin");
        let mtime = fs::metadata(&dst_a).unwrap().modified().unwrap();
        patch(&dst_a, 2 * MIB as u64, &data(64 * KIB, 4));
        set_mtime(&dst_a, mtime);
        // A new file with a.bin's content: every chunk is found locally,
        // one of them no longer verifies and must come from the sender.
        fs::write(h.src(flow).join("c.bin"), &content).unwrap();
        h.daemon.events.lock().unwrap().clear();
        let out = h.run(&opts, flow).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let events = h.daemon.events.lock().unwrap().clone();
        assert!(!events.contains(&"plan:a.bin".to_string()), "{events:?}");
        assert_eq!(fs::read(h.dst(flow).join("c.bin")).unwrap(), content);
        assert!(
            out.bytes_transferred >= 64 * KIB as u64 && out.bytes_transferred < 2 * MIB as u64,
            "{} bytes crossed the wire",
            out.bytes_transferred
        );
        // The bad copy is noticed once every local copy has run, and the
        // remaining chunks are asked for in one follow-up request.
        assert_eq!(
            h.daemon
                .rerequests
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert!(!h.dst(flow).join(".irsync.c.bin.part").exists());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delta_hashes_the_old_file_once_starting_at_plan_time() {
    within30(async {
        let opts = archive_opts();
        let flow = Flow::Pull;
        let h = Harness::new().await;
        let src = h.src(flow).join("x.bin");
        fs::write(&src, data(4 * MIB, 5)).unwrap();
        let first_run = h.run(&opts, flow).await.unwrap();
        assert!(first_run.errors.is_empty(), "{:?}", first_run.errors);

        patch(&src, 3 * MIB as u64, &data(MIB, 6));
        // A cold index: the receiver knows nothing about its own x.bin.
        let fresh_state = tempfile::tempdir().unwrap();
        let log: irsync::session::EventLog = Arc::default();
        let mut local = Harness::side_for(h.local.path(), fresh_state.path());
        local.hooks.events = Some(log.clone());
        let out = h.run_side(&opts, flow, local).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(
            fs::read(h.dst(flow).join("x.bin")).unwrap(),
            fs::read(&src).unwrap()
        );
        // Hashed by the planner's early start, then a cache hit when the
        // chunk list is opened: one read, not two.
        assert_eq!(out.files_hashed, 1);
        let events = log.lock().unwrap().clone();
        let prehash = first(&events, "prehash:x.bin");
        let chunklist = first(&events, "chunklist:x.bin");
        assert!(prehash < chunklist, "{events:?}");
        assert!(
            out.bytes_transferred < 2 * MIB as u64,
            "{} bytes crossed the wire",
            out.bytes_transferred
        );
    })
    .await;
}
