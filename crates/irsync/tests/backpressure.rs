//! The listing on its own stream: listing frames never travel on the
//! control stream, a listing cut short never looks complete, and a session
//! resumes whether or not the receiver lists again.

mod common;

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

use common::{archive_opts, assert_trees_equal, Flow, Harness};
use irsync::cli::DeleteMode;
use irsync::session::reconnect::ReconnectOpts;
use irsync::session::{PartsFault, StreamKind};
use irsync_proto::v1::control::Msg;
use irsync_proto::v1::file_entry::Kind;
use irsync_proto::v1::{FileEntry, ListingEntry};

const MIB: usize = 1024 * 1024;

async fn within60<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(60), f)
        .await
        .expect("test timed out after 60 s")
}

async fn within120<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(120), f)
        .await
        .expect("test timed out after 120 s")
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

/// No reconnects: the first failure ends the run.
fn no_reconnect() -> ReconnectOpts {
    ReconnectOpts {
        max: Duration::ZERO,
        ..fast()
    }
}

/// 20 files of 2 MiB each.
fn twenty_files(root: &Path) {
    for i in 0..20 {
        fs::write(root.join(format!("f{i:02}.bin")), data(2 * MIB, i + 1)).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_listing_frame_on_the_control_stream_fails_the_session() {
    within60(async {
        let h = Harness::new().await;
        fs::write(h.local.path().join("a"), b"a").unwrap();
        let mut local = h.local_side();
        let first = AtomicBool::new(true);
        local.hooks.tamper_frames = Some(Arc::new(move |kind, msg| match (kind, msg) {
            (StreamKind::Control, m @ Msg::Heartbeat(_)) if first.swap(false, SeqCst) => {
                let entry = FileEntry {
                    path: b"zz".to_vec(),
                    kind: Kind::Regular as i32,
                    size: 1,
                    ..FileEntry::default()
                };
                vec![m, Msg::Listing(ListingEntry { entry: Some(entry) })]
            }
            (_, m) => vec![m],
        }));
        let err = h
            .run_side(&archive_opts(), Flow::Push, local)
            .await
            .expect_err("a listing frame on the control stream was accepted");
        assert!(format!("{err:#}").contains("protocol error"), "{err:#}");
        assert!(!h.export.path().join("zz").exists());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_listing_cut_short_fails_instead_of_planning_deletions() {
    within60(async {
        for mode in [DeleteMode::Before, DeleteMode::During, DeleteMode::After] {
            let h = Harness::new().await;
            fs::write(h.local.path().join("a"), b"a").unwrap();
            fs::create_dir(h.local.path().join("b")).unwrap();
            fs::write(h.local.path().join("b/c"), b"c").unwrap();
            // Destination-only, after every source path: only the end of
            // the sender's listing says they are extraneous.
            fs::create_dir(h.export.path().join("zz")).unwrap();
            fs::write(h.export.path().join("zz/old"), b"old").unwrap();
            fs::write(h.export.path().join("zzz"), b"old").unwrap();
            let mut opts = archive_opts();
            opts.delete = mode;
            let mut local = h.local_side();
            local.hooks.tamper_frames = Some(Arc::new(|kind, msg| match (kind, msg) {
                (StreamKind::Listing, Msg::ListingEnd(_)) => vec![],
                (_, m) => vec![m],
            }));
            match h.run_side(&opts, Flow::Push, local).await {
                Err(_) => {}
                Ok(out) => assert!(
                    out.errors
                        .iter()
                        .any(|(_, m)| m.contains("listing is incomplete")),
                    "{mode:?}: {:?}",
                    out.errors
                ),
            }
            assert!(h.export.path().join("zz/old").exists(), "{mode:?}");
            assert!(h.export.path().join("zzz").exists(), "{mode:?}");
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_resumed_plan_needs_no_receiver_listing() {
    within60(async {
        let opts = archive_opts();
        // Pull: the client is the receiver, so its stored (complete) plan
        // decides, and it resumes without listing.
        let flow = Flow::Pull;
        let h = Harness::new().await;
        twenty_files(h.src(flow));
        h.daemon.sender_parallel(4);
        h.daemon.fault.drop_after_bytes(10 * MIB as u64);
        let (first, _) = h
            .run_reconnect(&opts, flow, no_reconnect(), h.local_side())
            .await;
        assert!(first.is_err(), "the first run should fail");
        let id = h.daemon.session_ids()[0];

        let (out, _) = h.run_reconnect(&opts, flow, fast(), h.local_side()).await;
        let out = out.unwrap_or_else(|e| panic!("{e:#}"));
        assert!(out.resumed);
        assert_eq!(out.session_id, id);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_connection_lost_mid_listing_resumes() {
    within60(async {
        let opts = archive_opts();
        let flow = Flow::Push;
        let h = Harness::new().await;
        for i in 0..2000 {
            let dir = h.src(flow).join(format!("d{:02}", i / 100));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(format!("f{i:04}")), format!("file {i}")).unwrap();
        }
        let fault = PartsFault::after(500);
        let mut local = h.local_side();
        local.hooks.drop_after_listing = Some(fault.clone());
        let (out, reconnects) = h.run_reconnect(&opts, flow, fast(), local).await;
        let out = out.unwrap_or_else(|e| panic!("{e:#}"));
        assert!(fault.fired(), "the fault never fired");
        assert!(reconnects >= 1, "no reconnect");
        assert!(out.resumed, "the second connection did not resume");
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
    })
    .await;
}

/// The queue between the listing reader and the planner (`LISTING_QUEUE`).
const LISTING_QUEUE: usize = 1024;

/// `n` files of 1-4 KiB spread over 50 directories.
fn many_files(root: &Path, n: u32) {
    for d in 0..50 {
        fs::create_dir(root.join(format!("d{d:02}"))).unwrap();
    }
    for i in 0..n {
        let size = 1024 + (i as usize * 7919) % (3 * 1024 + 1);
        fs::write(
            root.join(format!("d{:02}/f{i:05}", i % 50)),
            data(size, i + 1),
        )
        .unwrap();
    }
}

/// A receiver that plans 1 ms per file against a lister that walks at full
/// speed: the walk must wait on the listing stream's flow control, and the
/// planner's queue must never hold more than its capacity.
async fn slow_planner(flow: Flow) {
    within120(async {
        let h = Harness::new().await;
        many_files(h.src(flow), 5000);
        // The transport buffers a stream's first 8 MiB, so the walk only
        // waits once the listing outgrows that: 4000 symlinks with long
        // targets (14 MB of listing) alongside the files.
        for i in 0..4000 {
            let target = format!("{}/t{i:05}", "x".repeat(3500));
            let link = h.src(flow).join(format!("d{:02}/l{i:05}", i % 50));
            std::os::unix::fs::symlink(target, link).unwrap();
        }
        let peak = Arc::new(AtomicUsize::new(0));
        let waits = Arc::new(AtomicU64::new(0));
        let mut local = h.local_side();
        {
            let mut d = h.daemon.hooks.lock().unwrap();
            match flow {
                Flow::Push => {
                    d.slow_plan = Some(Duration::from_millis(1));
                    d.max_inflight = Some(4);
                    d.listing_peak = Some(peak.clone());
                    local.hooks.listing_waits = Some(waits.clone());
                }
                Flow::Pull => {
                    local.hooks.slow_plan = Some(Duration::from_millis(1));
                    local.hooks.max_inflight = Some(4);
                    local.hooks.listing_peak = Some(peak.clone());
                    d.listing_waits = Some(waits.clone());
                }
            }
        }
        let opts = archive_opts();
        let out = h
            .run_side(&opts, flow, local)
            .await
            .unwrap_or_else(|e| panic!("{e:#}"));
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.files_transferred, 5000);
        assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
        let peak = peak.load(SeqCst);
        assert!(peak <= LISTING_QUEUE, "planner queue peaked at {peak}");
        assert!(
            peak >= LISTING_QUEUE / 2,
            "planner queue peaked at {peak}: the planner never held the listing back"
        );
        assert!(
            waits.load(SeqCst) > 0,
            "the walk never waited on the slow planner"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_planner_slows_the_sender_walk_instead_of_queueing_push() {
    slow_planner(Flow::Push).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_planner_slows_the_sender_walk_instead_of_queueing_pull() {
    slow_planner(Flow::Pull).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_before_with_a_listing_larger_than_the_queues_completes() {
    within120(async {
        let h = Harness::new().await;
        many_files(h.local.path(), 3000);
        // Destination-only files; `--delete-before` waits for its reply
        // only after the whole listing is in.
        fs::create_dir(h.export.path().join("old")).unwrap();
        for i in 0..200 {
            fs::write(h.export.path().join(format!("old/x{i:03}")), b"old").unwrap();
        }
        let mut opts = archive_opts();
        opts.delete = DeleteMode::Before;
        h.daemon.hooks.lock().unwrap().slow_plan = Some(Duration::ZERO);
        let out = h
            .run_side(&opts, Flow::Push, h.local_side())
            .await
            .unwrap_or_else(|e| panic!("{e:#}"));
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(!h.export.path().join("old").exists());
        assert_trees_equal(h.local.path(), h.export.path(), &opts.preserve);
    })
    .await;
}

/// The queue between the stream readers and a role's main loop (`PEER_EVENTS`).
const PEER_EVENTS: usize = 1024;

/// A peer repeating every heartbeat 2000 times neither fails the session
/// nor grows the queue of peer events past its bound.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_peer_flooding_heartbeats_is_throttled_not_fatal() {
    within120(async {
        let h = Harness::new().await;
        many_files(h.local.path(), 200);
        let peak = Arc::new(AtomicUsize::new(0));
        h.daemon.hooks.lock().unwrap().peer_events_peak = Some(peak.clone());
        let mut local = h.local_side();
        local.hooks.tamper_frames = Some(Arc::new(|_, msg| match msg {
            m @ Msg::Heartbeat(_) => vec![m; 2000],
            m => vec![m],
        }));
        let opts = archive_opts();
        let out = h
            .run_side(&opts, Flow::Push, local)
            .await
            .unwrap_or_else(|e| panic!("{e:#}"));
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_trees_equal(h.local.path(), h.export.path(), &opts.preserve);
        let peak = peak.load(SeqCst);
        assert!(peak <= PEER_EVENTS, "peer events peaked at {peak}");
    })
    .await;
}

/// A receiver repeating its first two `FileAck`s 1500 times each (a
/// repeated ack is a no-op) against a sender whose main loop takes 1 ms per
/// peer event: the sender's queue of peer events fills, its control reader
/// waits instead of queueing without limit, and the session still succeeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_peer_flooding_acks_fills_the_queue_and_is_throttled() {
    within120(async {
        let flow = Flow::Pull;
        let h = Harness::new().await;
        many_files(h.src(flow), 200);
        let peak = Arc::new(AtomicUsize::new(0));
        {
            let mut d = h.daemon.hooks.lock().unwrap();
            d.peer_events_peak = Some(peak.clone());
            d.slow_peer_events = Some(Duration::from_millis(1));
        }
        let flooded = Arc::new(AtomicUsize::new(0));
        let mut local = h.local_side();
        let f = flooded.clone();
        local.hooks.tamper_frames = Some(Arc::new(move |_, msg| match msg {
            m @ Msg::Ack(_) if f.fetch_add(1, SeqCst) < 2 => vec![m; 1500],
            m => vec![m],
        }));
        let opts = archive_opts();
        let out = h
            .run_side(&opts, flow, local)
            .await
            .unwrap_or_else(|e| panic!("{e:#}"));
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.files_transferred, 200);
        assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
        assert!(flooded.load(SeqCst) >= 2, "fewer than two acks were sent");
        let peak = peak.load(SeqCst);
        assert!(peak <= PEER_EVENTS, "peer events peaked at {peak}");
        assert!(
            peak >= PEER_EVENTS / 2,
            "peer events peaked at {peak}: the flood never filled the queue"
        );
    })
    .await;
}
