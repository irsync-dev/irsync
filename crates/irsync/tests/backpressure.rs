//! The listing on its own stream: listing frames never travel on the
//! control stream, a listing cut short never looks complete, and a session
//! resumes whether or not the receiver lists again.

mod common;

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

use common::{archive_opts, assert_trees_equal, with_stream_window, Flow, Harness};
use irsync::cli::DeleteMode;
use irsync::session::reconnect::ReconnectOpts;
use irsync::session::{PartsFault, StreamKind};
use irsync_proto::v1::control::Msg;
use irsync_proto::v1::file_entry::Kind;
use irsync_proto::v1::{ChunkBasis, FileEntry, ListingEntry, PlanItem};

const MIB: usize = 1024 * 1024;

/// How many times shorter the long paths and symlink targets below are
/// than the 3.6 KB the tests were sized for. macOS refuses any path over
/// 1 KB, so there they are a fifth as long.
#[cfg(target_os = "macos")]
const SHORTER: u32 = 5;
#[cfg(not(target_os = "macos"))]
const SHORTER: u32 = 1;

/// A harness whose streams buffer 8 MiB / [`SHORTER`]: with paths that
/// many times shorter, the same number of them fills the buffers between
/// the two sides as the long paths fill 8 MiB, so the tests keep their
/// file counts and timing (and on Linux run with the defaults).
async fn long_path_harness() -> Harness {
    with_stream_window(8 * 1024 * 1024 / SHORTER, Harness::new()).await
}

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

/// For a test that syncs tens of thousands of files twice, which on a
/// loaded macOS machine takes well over 120 s.
async fn within240<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(240), f)
        .await
        .expect("test timed out after 240 s")
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
        let h = long_path_harness().await;
        many_files(h.src(flow), 5000);
        // The transport buffers a stream's first 8 MiB, so the walk only
        // waits once the listing outgrows that: 4000 symlinks with long
        // targets (14 MB of listing) alongside the files. (On macOS the
        // targets and the buffer are a fifth of that: see `SHORTER`.)
        for i in 0..4000 {
            let target = format!("{}/t{i:05}", "x".repeat(3500 / SHORTER as usize));
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

/// Completions and peer frames travel on separate channels, and a
/// completion sent first is not always received first. The receiver's
/// `Planned` for each file reaching its main loop 200 ms late must not let
/// the sender's answer arrive before the file is registered (it would be
/// dropped as "not in flight" and the file would wait forever).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_planned_completion_never_loses_the_answer() {
    within60(async {
        let flow = Flow::Pull;
        let h = Harness::new().await;
        many_files(h.src(flow), 50);
        let mut local = h.local_side();
        local.hooks.delay_planned = Some(Duration::from_millis(200));
        let opts = archive_opts();
        let out = h
            .run_side(&opts, flow, local)
            .await
            .unwrap_or_else(|e| panic!("{e:#}"));
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.files_transferred, 50);
        assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
    })
    .await;
}

/// The writer's budget for paced control frames (`OUT_BUDGET`).
const OUT_BUDGET: usize = 4 * MIB;

/// 20 000 one-byte files in 2 000 directories, all below fifteen nested
/// directories with 240-byte names, so each path is about 3600 bytes long
/// (on macOS three, a fifth as long: see [`SHORTER`] and
/// [`long_path_harness`]). Returns the directory that holds the 2 000.
fn wide_deep_tree(root: &Path) -> std::path::PathBuf {
    deep_tree(root, 2000)
}

/// `dirs` directories of ten one-byte files each, below fifteen nested
/// directories with 240-byte names (see [`wide_deep_tree`]).
fn deep_tree(root: &Path, dirs: u32) -> std::path::PathBuf {
    let mut base = root.to_path_buf();
    for c in (b'a'..b'p').take(15 / SHORTER as usize) {
        base.push(String::from_utf8(vec![c; 240]).unwrap());
    }
    fs::create_dir_all(&base).unwrap();
    for d in 0..dirs {
        let dir = base.join(format!("d{d:04}"));
        fs::create_dir(&dir).unwrap();
        for f in 0..10 {
            fs::write(dir.join(format!("f{f}")), b"x").unwrap();
        }
    }
    base
}

/// Sets the mode of every directory and file below `base`.
fn chmod_all(base: &Path, dirs: u32, files: u32) {
    use std::os::unix::fs::PermissionsExt;
    for d in fs::read_dir(base).unwrap() {
        let dir = d.unwrap().path();
        for f in fs::read_dir(&dir).unwrap() {
            fs::set_permissions(f.unwrap().path(), fs::Permissions::from_mode(files)).unwrap();
        }
        fs::set_permissions(&dir, fs::Permissions::from_mode(dirs)).unwrap();
    }
}

/// A metadata-only change to every one of 20 000 entries, pushed with `-i`:
/// the receiving daemon's planner sends one `Itemized` per entry, none of
/// which takes an in-flight slot. `--bwlimit` paces data streams only, and
/// a metadata-only push has none, so the slow link is the client's main
/// loop taking 0.5 ms per peer event: once the client's queue and the
/// stream's 8 MiB window are full, the daemon's writer stalls. The ~80 MB
/// of `Itemized` frames must then wait in the planner, not in the queue.
/// (Long paths keep the backlog the client still has to read at the end,
/// which the daemon waits for only so long, to a few thousand frames.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn itemize_over_a_slow_link_stays_within_the_out_budget() {
    within240(async {
        let flow = Flow::Push;
        let h = long_path_harness().await;
        let base = wide_deep_tree(h.src(flow));
        h.run(&archive_opts(), flow)
            .await
            .unwrap_or_else(|e| panic!("{e:#}"));
        chmod_all(&base, 0o750, 0o600);
        let peak = Arc::new(AtomicUsize::new(0));
        h.daemon.hooks.lock().unwrap().out_peak = Some(peak.clone());
        let mut local = h.local_side();
        local.hooks.slow_peer_events = Some(Duration::from_micros(500));
        let mut opts = archive_opts();
        opts.itemize = true;
        opts.bwlimit = Some(200 * 1024);
        let out = h
            .run_side(&opts, flow, local)
            .await
            .unwrap_or_else(|e| panic!("{e:#}"));
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
        let peak = peak.load(SeqCst);
        assert!(
            peak <= OUT_BUDGET + 64 * 1024,
            "paced control frames peaked at {peak} bytes"
        );
        assert!(
            peak >= OUT_BUDGET / 2,
            "paced control frames peaked at {peak} bytes: the budget never filled"
        );
    })
    .await;
}

/// The basis budget both sides use in the large-delta tests.
const BASIS_BUDGET: usize = 256 * 1024;

/// A 48 MiB file pushed once, then edited every 64 KiB over its first 40%
/// (one byte per 64 KiB, so every chunk there changes but the old file
/// still holds most of it): ~75 changed chunks, most offered a basis of
/// ~10 KiB, several hundred KiB of bases in all. Pushed again over a slow
/// link with a 256 KiB basis budget on both sides, the sender must never
/// hold more than the budget of bases. (Fewer than half the chunks
/// change: above that the receiver offers no bases at all.)
async fn a_large_delta_within_the_basis_budget(inplace: bool) {
    within120(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        let path = h.src(flow).join("big.bin");
        let mut bytes = data(48 * MIB, 11);
        fs::write(&path, &bytes).unwrap();
        let mut opts = archive_opts();
        opts.inplace = inplace;
        h.run(&opts, flow).await.unwrap_or_else(|e| panic!("{e:#}"));
        for off in (0..bytes.len() * 2 / 5).step_by(64 * 1024) {
            bytes[off + 1000] ^= 0x5a;
        }
        fs::write(&path, &bytes).unwrap();
        let peak = Arc::new(AtomicUsize::new(0));
        h.daemon.hooks.lock().unwrap().basis_budget = Some(BASIS_BUDGET);
        let mut local = h.local_side();
        local.hooks.basis_budget = Some(BASIS_BUDGET);
        local.hooks.basis_peak = Some(peak.clone());
        opts.bwlimit = Some(20 * MIB as u64);
        let out = h
            .run_side(&opts, flow, local)
            .await
            .unwrap_or_else(|e| panic!("{e:#}"));
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
        let peak = peak.load(SeqCst);
        assert!(
            peak <= BASIS_BUDGET,
            "the sender held {peak} bytes of bases"
        );
        assert!(
            peak >= BASIS_BUDGET / 2,
            "the sender held at most {peak} bytes of bases: the budget never filled"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_large_delta_keeps_bases_within_the_budget() {
    a_large_delta_within_the_basis_budget(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_large_inplace_delta_keeps_bases_within_the_budget() {
    a_large_delta_within_the_basis_budget(true).await;
}

/// A 16 MiB file pulled once, then edited once every 64 KiB over its
/// first 40% (about 20 changed chunks, each with a basis) and pulled
/// again, with chunk requests of at most `piece` indices (`None`: the
/// default). Returns the bytes the client received.
async fn a_delta_pulled_in_pieces(inplace: bool, piece: Option<usize>) -> u64 {
    let flow = Flow::Pull;
    let h = Harness::new().await;
    let path = h.src(flow).join("f.bin");
    let mut bytes = data(16 * MIB, 21);
    fs::write(&path, &bytes).unwrap();
    let mut opts = archive_opts();
    opts.inplace = inplace;
    h.run(&opts, flow).await.unwrap_or_else(|e| panic!("{e:#}"));
    for off in (0..bytes.len() * 2 / 5).step_by(64 * 1024) {
        bytes[off + 1000] ^= 0x5a;
    }
    fs::write(&path, &bytes).unwrap();
    let mut local = h.local_side();
    local.hooks.need_piece = piece;
    let out = h
        .run_side(&opts, flow, local)
        .await
        .unwrap_or_else(|e| panic!("{e:#}"));
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
    out.bytes_received
}

/// Chunk requests split into pieces of a few indices (as a file of more
/// than `NEED_PIECE` chunks has them) still sync the file, and lose none
/// of its bases: the delta costs about what it costs in one request.
async fn a_delta_in_pieces_keeps_its_bases(inplace: bool) {
    within120(async {
        let whole = a_delta_pulled_in_pieces(inplace, None).await;
        let pieces = a_delta_pulled_in_pieces(inplace, Some(3)).await;
        assert!(
            pieces <= whole + whole / 10 + 64 * 1024,
            "in pieces {pieces} bytes, in one request {whole}"
        );
        // The bases were used at all: about 20 raw chunks would be 6 MiB.
        assert!(whole < 2 * MIB as u64, "{whole}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delta_in_pieces_keeps_its_bases_with_a_new_file() {
    a_delta_in_pieces_keeps_its_bases(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delta_in_pieces_keeps_its_bases_in_place() {
    a_delta_in_pieces_keeps_its_bases(true).await;
}

/// A receiver (the client, pulling) that sends far more bases than the
/// budget before requesting them: every `ChunkBasis` goes out 10 000
/// times, for successive chunk indices (a basis replaces an earlier one
/// for the same chunk, so plain repeats would not add up).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bases_above_the_budget_fail_the_session() {
    within60(async {
        let flow = Flow::Pull;
        let h = Harness::new().await;
        let path = h.src(flow).join("f.bin");
        let mut bytes = data(8 * MIB, 5);
        fs::write(&path, &bytes).unwrap();
        h.run(&archive_opts(), flow)
            .await
            .unwrap_or_else(|e| panic!("{e:#}"));
        for off in (0..bytes.len() / 4).step_by(64 * 1024) {
            bytes[off + 1000] ^= 0x5a;
        }
        fs::write(&path, &bytes).unwrap();
        h.daemon.hooks.lock().unwrap().basis_budget = Some(64 * 1024);
        let mut local = h.local_side();
        local.hooks.basis_budget = Some(64 * 1024);
        local.hooks.tamper_frames = Some(Arc::new(|kind, msg| match (kind, msg) {
            (StreamKind::Control, Msg::ChunkBasis(b)) => (0..10_000u32)
                .map(|k| {
                    Msg::ChunkBasis(ChunkBasis {
                        chunk_idx: b.chunk_idx.wrapping_add(k),
                        ..b.clone()
                    })
                })
                .collect(),
            (_, m) => vec![m],
        }));
        let err = h
            .run_side(&archive_opts(), flow, local)
            .await
            .expect_err("bases above the budget were accepted");
        let err = format!("{err:#}");
        assert!(err.contains("protocol error"), "{err}");
        assert!(err.contains("chunk bases above the budget"), "{err}");
    })
    .await;
}

/// The room for chunk lists in these tests, in records.
const LIST_ROOM: u64 = 24;

/// 12 files of 3.5 MiB: 10 to 15 chunks each (about 300 KiB a chunk on
/// random data), so one or two lists fill the room, and all of them
/// together (about 150 records) are six times the room.
fn twelve_listed_files(root: &Path) {
    for i in 0..12 {
        let bytes = data(7 * MIB / 2, 100 + i);
        let chunks = irsync_proto::chunk::digest_bytes(&bytes).chunks.len();
        assert!((9..=16).contains(&chunks), "file {i}: {chunks} chunks");
        fs::write(root.join(format!("f{i:02}.bin")), bytes).unwrap();
    }
}

/// A file of 6 MiB with more chunks than half the room of these tests
/// (`LIST_ROOM`) and no more than all of it: 20 for seed 100, 18 for 101.
fn over_half_the_room(seed: u32) -> Vec<u8> {
    let bytes = data(6 * MIB, seed);
    let chunks = irsync_proto::chunk::digest_bytes(&bytes).chunks.len() as u64;
    assert!(
        chunks > LIST_ROOM / 2 && chunks <= LIST_ROOM,
        "{chunks} chunks"
    );
    bytes
}

/// Lists far larger in sum than the room: the sender holds them back until
/// the receiver frees room, and the receiver never holds more than it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chunk_lists_wait_for_room() {
    within120(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        twelve_listed_files(h.src(flow));
        let peak = Arc::new(AtomicU64::new(0));
        {
            let mut d = h.daemon.hooks.lock().unwrap();
            d.list_budget = Some(LIST_ROOM);
            d.list_peak = Some(peak.clone());
        }
        let mut local = h.local_side();
        local.hooks.list_budget = Some(LIST_ROOM);
        local.hooks.chunk_list_part_records = Some(3);
        let opts = archive_opts();
        let out = h
            .run_side(&opts, flow, local)
            .await
            .unwrap_or_else(|e| panic!("{e:#}"));
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.files_transferred, 12);
        assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
        let peak = peak.load(SeqCst);
        assert!(peak <= LIST_ROOM, "the receiver held {peak} records");
        assert!(
            peak >= 9,
            "the receiver held at most {peak} records: no list was ever held"
        );
    })
    .await;
}

/// The same lists, seen from the sender: it digests ahead of the room only
/// as much as the room holds, so what it digested and holds waiting for
/// room stays within it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_sender_digests_ahead_of_the_room_only_a_room() {
    within120(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        twelve_listed_files(h.src(flow));
        h.daemon.hooks.lock().unwrap().list_budget = Some(LIST_ROOM);
        let peak = Arc::new(AtomicU64::new(0));
        let mut local = h.local_side();
        local.hooks.list_budget = Some(LIST_ROOM);
        local.hooks.lists_ahead_peak = Some(peak.clone());
        let opts = archive_opts();
        let out = h
            .run_side(&opts, flow, local)
            .await
            .unwrap_or_else(|e| panic!("{e:#}"));
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.files_transferred, 12);
        assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
        let peak = peak.load(SeqCst);
        assert!(
            peak <= LIST_ROOM,
            "the sender held {peak} records digested ahead of the room"
        );
        assert!(peak >= 9, "the sender never held a list: {peak}");
    })
    .await;
}

/// A sender that announces a list longer than the room it was granted.
/// The receiver must refuse it before it checks the list against the file
/// (which would only fail that file).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chunk_list_beyond_the_room_fails_the_session() {
    within60(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        fs::write(h.src(flow).join("f.bin"), data(4 * MIB, 7)).unwrap();
        h.daemon.hooks.lock().unwrap().list_budget = Some(LIST_ROOM);
        let mut local = h.local_side();
        local.hooks.list_budget = Some(LIST_ROOM);
        local.hooks.chunk_list_part_records = Some(3);
        local.hooks.tamper_frames = Some(Arc::new(|kind, msg| match (kind, msg) {
            (StreamKind::Control, Msg::ChunkListPart(mut p)) => {
                // Every part: they must agree for the list to look sound.
                p.total = LIST_ROOM as u32 + 1;
                vec![Msg::ChunkListPart(p)]
            }
            (_, m) => vec![m],
        }));
        let err = h
            .run_side(&archive_opts(), flow, local)
            .await
            .expect_err("a list beyond the room was accepted");
        let err = format!("{err:#}");
        assert!(err.contains("protocol error"), "{err}");
        assert!(err.contains("chunk list beyond the granted room"), "{err}");
    })
    .await;
}

/// A list for a file id the receiver never asked for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chunk_list_for_an_unplanned_file_fails_the_session() {
    within60(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        fs::write(h.src(flow).join("f.bin"), data(MIB, 8)).unwrap();
        let mut local = h.local_side();
        local.hooks.tamper_frames = Some(Arc::new(|kind, msg| match (kind, msg) {
            (StreamKind::Control, Msg::ChunkListPart(mut p)) => {
                p.file_id += 1000;
                vec![Msg::ChunkListPart(p)]
            }
            (_, m) => vec![m],
        }));
        let err = h
            .run_side(&archive_opts(), flow, local)
            .await
            .expect_err("a list for an unplanned file was accepted");
        let err = format!("{err:#}");
        assert!(err.contains("protocol error"), "{err}");
        assert!(err.contains("never planned"), "{err}");
    })
    .await;
}

/// The connection drops after the first list took most of the room: the
/// next connection starts with the whole room again on both sides (a sender
/// still counting the old list would never send it again).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn list_room_starts_afresh_on_every_connection() {
    within60(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        let bytes = over_half_the_room(100);
        fs::write(h.src(flow).join("x.bin"), &bytes).unwrap();
        h.daemon.hooks.lock().unwrap().list_budget = Some(LIST_ROOM);
        let fault = PartsFault::after(2);
        let mut local = h.local_side();
        local.hooks.list_budget = Some(LIST_ROOM);
        local.hooks.chunk_list_part_records = Some(3);
        local.hooks.drop_after_parts = Some(fault.clone());
        let (out, reconnects) = h.run_reconnect(&archive_opts(), flow, fast(), local).await;
        let out = out.unwrap_or_else(|e| panic!("{e:#}"));
        assert!(fault.fired(), "the fault never fired");
        assert_eq!(reconnects, 1);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(h.dst(flow).join("x.bin")).unwrap(), bytes);
    })
    .await;
}

/// A file that changes while it is sent is listed again. Its old list
/// takes more than half the room and so does the new one: the receiver
/// must drop the old list before the new one can fit, or both wait forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_list_sent_again_first_frees_the_old_one() {
    within60(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        let (old, new) = (over_half_the_room(100), over_half_the_room(101));
        let src = h.src(flow).to_path_buf();
        fs::write(src.join("x.bin"), &old).unwrap();
        h.daemon.hooks.lock().unwrap().list_budget = Some(LIST_ROOM);
        let mut local = h.local_side();
        local.hooks.list_budget = Some(LIST_ROOM);
        let fired = Arc::new(AtomicUsize::new(0));
        local.hooks.on_first_chunk_sent = Some({
            let (src, new, fired) = (src.clone(), new.clone(), fired.clone());
            Arc::new(move |_| {
                let (src, new, fired) = (src.clone(), new.clone(), fired.clone());
                Box::pin(async move {
                    if fired.fetch_add(1, SeqCst) == 0 {
                        fs::write(src.join("x.bin"), &new).unwrap();
                    }
                })
            })
        });
        let opts = archive_opts();
        let out = h
            .run_side(&opts, flow, local)
            .await
            .unwrap_or_else(|e| panic!("{e:#}"));
        assert!(fired.load(SeqCst) >= 1, "the file never changed mid-send");
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        // (Its mtime is the one listed before the change, as in
        // `file_changed_during_transfer_is_resent`.)
        assert_eq!(fs::read(h.dst(flow).join("x.bin")).unwrap(), new);
    })
    .await;
}

/// A receiver that plans far more files than the window of files in flight:
/// the sender refuses to track them. Each plan names a different listed
/// file not planned yet (one plan per listed file is all a receiver may
/// send): 5 000 files already in place, which the receiver's planner skips,
/// and a new one, `z`, listed after them, whose plan the receiver follows
/// with plans for all the others.
///
/// A push with the daemon (the receiver) tampering: the sender's own error
/// is what the client reports, and it is deterministic, as the sender takes
/// the plans in order before anything the receiver sends after them. (In a
/// pull, the receiving client may first fail on a chunk list the sender
/// sent for one of the fresh ids, "never planned", a race.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn more_plans_than_the_window_fail_the_session() {
    within60(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        for i in 0..5000 {
            fs::write(h.src(flow).join(format!("s{i:04}")), b"s").unwrap();
        }
        h.run(&archive_opts(), flow)
            .await
            .unwrap_or_else(|e| panic!("{e:#}"));
        fs::write(h.src(flow).join("z"), data(4096, 1)).unwrap();
        let first = AtomicBool::new(true);
        let local = h.local_side();
        h.daemon.hooks.lock().unwrap().tamper_frames =
            Some(Arc::new(move |kind, msg| match (kind, msg) {
                (StreamKind::Control, Msg::Plan(p)) if first.swap(false, SeqCst) => {
                    assert_eq!(p.path, b"z", "only z is planned");
                    std::iter::once(Msg::Plan(p.clone()))
                        .chain((0..5000u64).map(|k| {
                            Msg::Plan(PlanItem {
                                file_id: p.file_id + 1_000_000 * (k + 1),
                                path: format!("s{k:04}").into_bytes(),
                                ..p.clone()
                            })
                        }))
                        .collect()
                }
                (_, m) => vec![m],
            }));
        let err = h
            .run_side(&archive_opts(), flow, local)
            .await
            .expect_err("more plans than the window were accepted");
        let err = format!("{err:#}");
        assert!(err.contains("protocol error"), "{err}");
        assert!(err.contains("files in flight"), "{err}");
    })
    .await;
}

/// The receiver's plans in a push as the daemon (the receiver) sends them:
/// the first one goes out as `first` makes it (a test adds plans around it).
fn tamper_first_plan(h: &Harness, first: impl Fn(PlanItem) -> Vec<Msg> + Send + Sync + 'static) {
    let once = AtomicBool::new(true);
    h.daemon.hooks.lock().unwrap().tamper_frames =
        Some(Arc::new(move |kind, msg| match (kind, msg) {
            (StreamKind::Control, Msg::Plan(p)) if once.swap(false, SeqCst) => first(p),
            (_, m) => vec![m],
        }));
}

/// A receiver that asks for a path the sender never listed (under a fresh
/// id): the sender refuses it as a protocol error, rather than giving the
/// file up and keeping what that costs for the rest of the session.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plan_for_an_unlisted_path_fails_the_session() {
    within60(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        fs::write(h.src(flow).join("a"), data(4096, 1)).unwrap();
        tamper_first_plan(&h, |p| {
            vec![
                Msg::Plan(PlanItem {
                    file_id: p.file_id + 1_000_000,
                    path: b"never-listed".to_vec(),
                    ..p.clone()
                }),
                Msg::Plan(p),
            ]
        });
        let err = h
            .run_side(&archive_opts(), flow, h.local_side())
            .await
            .expect_err("a plan for an unlisted path was accepted");
        let err = format!("{err:#}");
        assert!(err.contains("protocol error"), "{err}");
        assert!(err.contains("did not list"), "{err}");
    })
    .await;
}

/// A receiver that asks for the same listed path twice, under two ids.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_path_planned_twice_fails_the_session() {
    within60(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        fs::write(h.src(flow).join("a"), data(4096, 1)).unwrap();
        tamper_first_plan(&h, |p| {
            vec![
                Msg::Plan(p.clone()),
                Msg::Plan(PlanItem {
                    file_id: p.file_id + 1_000_000,
                    ..p
                }),
            ]
        });
        let err = h
            .run_side(&archive_opts(), flow, h.local_side())
            .await
            .expect_err("a path planned twice was accepted");
        let err = format!("{err:#}");
        assert!(err.contains("protocol error"), "{err}");
        assert!(err.contains("already planned"), "{err}");
    })
    .await;
}

/// A receiver that grants one record of list room more than it freed: the
/// sender's room would exceed what a connection starts with.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn list_room_beyond_the_budget_fails_the_session() {
    within60(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        for i in 0..3 {
            fs::write(h.src(flow).join(format!("f{i}")), data(MIB, 30 + i)).unwrap();
        }
        h.daemon.hooks.lock().unwrap().tamper_frames =
            Some(Arc::new(|kind, msg| match (kind, msg) {
                (StreamKind::Control, Msg::ListRoom(mut r)) => {
                    r.records += 1;
                    vec![Msg::ListRoom(r)]
                }
                (_, m) => vec![m],
            }));
        let err = h
            .run_side(&archive_opts(), flow, h.local_side())
            .await
            .expect_err("list room beyond the budget was accepted");
        let err = format!("{err:#}");
        assert!(err.contains("protocol error"), "{err}");
        assert!(err.contains("list room"), "{err}");
    })
    .await;
}

/// A sender that starts a file's chunk list over and over (empty first
/// parts, each withdrawing the list so far, then the real list): more
/// first parts than an honest sender's retries allow fail the session.
/// (Empty ones free no room, so this sender never sees a grant it did not
/// expect.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chunk_list_started_too_often_fails_the_session() {
    within60(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        fs::write(h.src(flow).join("f.bin"), data(MIB, 40)).unwrap();
        let mut local = h.local_side();
        local.hooks.tamper_frames = Some(Arc::new(|kind, msg| match (kind, msg) {
            (StreamKind::Control, Msg::ChunkListPart(p)) if p.first == 0 && p.total > 0 => {
                let empty = irsync_proto::v1::ChunkListPart {
                    file_id: p.file_id,
                    ..Default::default()
                };
                let mut v = vec![Msg::ChunkListPart(empty); 4];
                v.push(Msg::ChunkListPart(p));
                v
            }
            (_, m) => vec![m],
        }));
        let err = h
            .run_side(&archive_opts(), flow, local)
            .await
            .expect_err("a chunk list started five times was accepted");
        let err = format!("{err:#}");
        assert!(err.contains("protocol error"), "{err}");
        assert!(err.contains("chunk lists for file id"), "{err}");
    })
    .await;
}

/// A sender that stops reading its control stream (its main loop takes a
/// minute per peer event) while the receiver requests 5 000 files with
/// 3.6 KB paths: once the in-flight window's `PlanItem`s outgrow what the
/// sender's queue and the stream window take, they wait in the receiver's
/// control writer, which must not hold more than its ceiling. (The client
/// runs alone: the daemon's stalled main loop takes minutes to end.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_peer_that_stops_reading_control_fails_the_session() {
    within60(async {
        let flow = Flow::Pull;
        let h = long_path_harness().await;
        deep_tree(h.src(flow), 500);
        h.daemon.hooks.lock().unwrap().slow_peer_events = Some(Duration::from_secs(60));
        let mut local = h.local_side();
        local.hooks.out_ceiling = Some(512 * 1024 / SHORTER as usize);
        let conn = h
            .client
            .connect(h.daemon.addr.clone(), irsync_proto::ALPN)
            .await
            .unwrap();
        let out = irsync::session::engine::run_client(
            conn.clone(),
            Harness::hello(flow),
            local,
            &archive_opts(),
        )
        .await;
        conn.close(0u32.into(), b"done");
        let err = out.expect_err("a peer that stopped reading was waited for");
        let err = format!("{err:#}");
        assert!(err.contains("protocol error"), "{err}");
        assert!(err.contains("not reading its control stream"), "{err}");
    })
    .await;
}

/// A receiver that asks for every chunk 500 times: the sender runs at most
/// one send task per file, merging the repeats, and the transfer succeeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_needs_do_not_multiply_sends() {
    within120(async {
        let flow = Flow::Pull;
        let h = Harness::new().await;
        for i in 0..40 {
            fs::write(
                h.src(flow).join(format!("f{i:02}.bin")),
                data(4096, 200 + i),
            )
            .unwrap();
        }
        let peak = Arc::new(AtomicUsize::new(0));
        h.daemon.hooks.lock().unwrap().send_tasks_peak = Some(peak.clone());
        let mut local = h.local_side();
        local.hooks.max_inflight = Some(8);
        local.hooks.tamper_frames = Some(Arc::new(|kind, msg| match (kind, msg) {
            (StreamKind::Control, Msg::Need(n)) if !n.chunk_idx.is_empty() => {
                vec![Msg::Need(n); 500]
            }
            (_, m) => vec![m],
        }));
        let opts = archive_opts();
        let out = h
            .run_side(&opts, flow, local)
            .await
            .unwrap_or_else(|e| panic!("{e:#}"));
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
        let peak = peak.load(SeqCst);
        assert!(peak >= 1, "no send task was counted");
        assert!(
            peak <= 8,
            "{peak} send tasks ran at once for 8 files in flight"
        );
    })
    .await;
}

/// A listing whose paths go backwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_out_of_order_listing_fails_the_session() {
    within60(async {
        let h = Harness::new().await;
        fs::write(h.local.path().join("b"), b"b").unwrap();
        let mut local = h.local_side();
        local.hooks.inject_listing = Some(Arc::new(|e: &FileEntry| {
            if e.path == b"b" {
                vec![FileEntry {
                    path: b"a".to_vec(),
                    kind: Kind::Regular as i32,
                    size: 1,
                    ..FileEntry::default()
                }]
            } else {
                Vec::new()
            }
        }));
        let err = h
            .run_side(&archive_opts(), Flow::Push, local)
            .await
            .expect_err("an out-of-order listing was accepted");
        let err = format!("{err:#}");
        assert!(err.contains("protocol error"), "{err}");
        assert!(err.contains("listing out of order"), "{err}");
    })
    .await;
}

/// A `ListingEnd` that counts one entry more than the listing carried.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_listing_count_mismatch_fails_the_session() {
    within60(async {
        let h = Harness::new().await;
        fs::write(h.local.path().join("a"), b"a").unwrap();
        let mut local = h.local_side();
        local.hooks.tamper_frames = Some(Arc::new(|kind, msg| match (kind, msg) {
            (StreamKind::Listing, Msg::ListingEnd(mut e)) => {
                e.count += 1;
                vec![Msg::ListingEnd(e)]
            }
            (_, m) => vec![m],
        }));
        let err = h
            .run_side(&archive_opts(), Flow::Push, local)
            .await
            .expect_err("a listing count mismatch was accepted");
        let err = format!("{err:#}");
        assert!(err.contains("protocol error"), "{err}");
        assert!(err.contains("listing count mismatch"), "{err}");
    })
    .await;
}

/// A sender whose listing stream ends without a single frame, to a
/// receiver that lists afresh: the receiver's planner would wait forever
/// for it. (A receiver resuming its stored plan expects exactly that, see
/// `a_resumed_plan_needs_no_receiver_listing`.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_listing_that_ends_without_a_frame_fails_the_session() {
    within60(async {
        let h = Harness::new().await;
        fs::write(h.local.path().join("a"), b"a").unwrap();
        let mut local = h.local_side();
        local.hooks.tamper_frames = Some(Arc::new(|kind, msg| match (kind, msg) {
            (StreamKind::Listing, _) => vec![],
            (_, m) => vec![m],
        }));
        let err = h
            .run_side(&archive_opts(), Flow::Push, local)
            .await
            .expect_err("a listing without a frame was accepted");
        let err = format!("{err:#}");
        assert!(err.contains("protocol error"), "{err}");
        assert!(err.contains("ended without a frame"), "{err}");
    })
    .await;
}

/// An empty source tree still lists its root and a `ListingEnd`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_source_tree_syncs() {
    within60(async {
        for flow in [Flow::Push, Flow::Pull] {
            let h = Harness::new().await;
            let opts = archive_opts();
            let out = h
                .run(&opts, flow)
                .await
                .unwrap_or_else(|e| panic!("{flow:?}: {e:#}"));
            assert!(out.errors.is_empty(), "{flow:?}: {:?}", out.errors);
            assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
        }
    })
    .await;
}

/// A daemon done with a push while the client still has a large backlog
/// of `Itemized` frames to read (a slow client: 4 ms per peer event, about
/// 0.9 MB/s of 3.6 KB frames). The daemon is done once the backlog fits
/// in what the client queues, the stream window and its own budget
/// (~15 MB, ~17 s of reading): it must wait for the client to close, not
/// close the connection after a fixed 10 to 15 s and fail the session.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_daemon_waits_for_a_slow_client_to_finish_reading() {
    within120(async {
        let flow = Flow::Push;
        let h = long_path_harness().await;
        let base = deep_tree(h.src(flow), 500);
        h.run(&archive_opts(), flow)
            .await
            .unwrap_or_else(|e| panic!("{e:#}"));
        chmod_all(&base, 0o750, 0o600);
        // Explicit, so the margin over the ~17 s of reading stays wide
        // whatever the default.
        h.daemon.hooks.lock().unwrap().close_quiet = Some(Duration::from_secs(60));
        let mut local = h.local_side();
        local.hooks.slow_peer_events = Some(Duration::from_millis(4));
        let mut opts = archive_opts();
        opts.itemize = true;
        let (out, reconnects) = h.run_reconnect(&opts, flow, no_reconnect(), local).await;
        let out = out.unwrap_or_else(|e| panic!("{e:#}"));
        assert_eq!(reconnects, 0);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
    })
    .await;
}

/// A client that all but stops reading at the end (one peer event a
/// second, about 2 200 to go, all of which fit in the buffers between the
/// two, so the daemon is done at once): the daemon's wait for it to close
/// ends once nothing has moved for its quiet time (2 s here), and its
/// session ends although the client is still connected.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_daemon_stops_waiting_for_a_client_that_stopped_reading() {
    within120(async {
        let flow = Flow::Push;
        let h = Arc::new(long_path_harness().await);
        let base = deep_tree(h.src(flow), 200);
        h.run(&archive_opts(), flow)
            .await
            .unwrap_or_else(|e| panic!("{e:#}"));
        chmod_all(&base, 0o750, 0o600);
        let before = h.daemon.sessions.load(SeqCst);
        h.daemon.hooks.lock().unwrap().close_quiet = Some(Duration::from_secs(2));
        let mut local = h.local_side();
        local.hooks.slow_peer_events = Some(Duration::from_secs(1));
        let mut opts = archive_opts();
        opts.itemize = true;
        let started = std::time::Instant::now();
        let client = tokio::spawn({
            let h = h.clone();
            async move { h.run_reconnect(&opts, flow, no_reconnect(), local).await }
        });
        while h.daemon.sessions.load(SeqCst) == before {
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "the daemon still waits for the client after {:?}",
                started.elapsed()
            );
            assert!(!client.is_finished(), "the client finished");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        client.abort();
    })
    .await;
}

/// As above, but the daemon's writer still holds far more than the
/// buffers between the two take when its role ends (its `SessionDone`
/// goes out behind 18 MB of `Itemized` frames): it waits for the writer
/// only while the connection moves, then gives up on it and on the client.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_daemon_stops_waiting_for_a_writer_the_client_does_not_read() {
    within120(async {
        let flow = Flow::Push;
        let h = Arc::new(long_path_harness().await);
        let base = deep_tree(h.src(flow), 200);
        h.run(&archive_opts(), flow)
            .await
            .unwrap_or_else(|e| panic!("{e:#}"));
        chmod_all(&base, 0o750, 0o600);
        let before = h.daemon.sessions.load(SeqCst);
        {
            let mut d = h.daemon.hooks.lock().unwrap();
            d.close_quiet = Some(Duration::from_secs(2));
            d.tamper_frames = Some(Arc::new(|kind, msg| match (kind, msg) {
                (StreamKind::Control, m @ Msg::Done(_)) => {
                    let tail = Msg::Itemized(irsync_proto::v1::Itemized {
                        path: vec![b'x'; 3600],
                        code: ".d..p......".into(),
                    });
                    let mut v = vec![tail; 5000];
                    v.push(m);
                    v
                }
                (_, m) => vec![m],
            }));
        }
        let mut local = h.local_side();
        local.hooks.slow_peer_events = Some(Duration::from_secs(1));
        let mut opts = archive_opts();
        opts.itemize = true;
        let started = std::time::Instant::now();
        let client = tokio::spawn({
            let h = h.clone();
            async move { h.run_reconnect(&opts, flow, no_reconnect(), local).await }
        });
        while h.daemon.sessions.load(SeqCst) == before {
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "the daemon still waits for its writer after {:?}",
                started.elapsed()
            );
            assert!(!client.is_finished(), "the client finished");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        client.abort();
    })
    .await;
}

/// The client's side of the above: a pull whose client queues 29 MB of
/// frames behind its `SessionDone`, which the daemon never reads (it stops
/// reading at `SessionDone`, as a peer that does not read would), while it
/// stays connected for 60 s. The client's wait for its writer ends once
/// nothing has moved for its quiet time (2 s here), and the session
/// succeeds: the daemon has what it needs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_client_stops_waiting_for_a_writer_the_daemon_does_not_read() {
    within120(async {
        let flow = Flow::Pull;
        let h = Harness::new().await;
        fs::write(h.src(flow).join("f.bin"), data(MIB, 3)).unwrap();
        {
            let mut d = h.daemon.hooks.lock().unwrap();
            d.close_quiet = Some(Duration::from_secs(60));
            d.stop_reading_after_done = true;
        }
        let mut local = h.local_side();
        local.hooks.close_quiet = Some(Duration::from_secs(2));
        local.hooks.tamper_frames = Some(Arc::new(|kind, msg| match (kind, msg) {
            (StreamKind::Control, m @ Msg::Done(_)) => {
                let tail = Msg::Itemized(irsync_proto::v1::Itemized {
                    path: vec![b'x'; 3600],
                    code: ".d..p......".into(),
                });
                let mut v = vec![m];
                v.extend(std::iter::repeat_n(tail, 8000));
                v
            }
            (_, m) => vec![m],
        }));
        let started = std::time::Instant::now();
        let out = h
            .run_side(&archive_opts(), flow, local)
            .await
            .unwrap_or_else(|e| panic!("{e:#}"));
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the client waited {:?} for its writer",
            started.elapsed()
        );
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_trees_equal(h.src(flow), h.dst(flow), &archive_opts().preserve);
    })
    .await;
}

/// A sender whose lister stops short (five frames, no `ListingEnd`), as
/// one that fails mid-listing, and which then fails for good (the client's
/// first `PlanItem` comes behind a listing frame on the control stream):
/// the client must report the daemon's error, never the transport error of
/// a listing cut short, which would be retried.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sender_whose_listing_ends_short_reports_its_own_error() {
    within60(async {
        let flow = Flow::Pull;
        let h = Harness::new().await;
        twenty_files(h.src(flow));
        h.daemon.hooks.lock().unwrap().cut_listing_after = Some(5);
        let mut local = h.local_side();
        let first = AtomicBool::new(true);
        local.hooks.tamper_frames = Some(Arc::new(move |kind, msg| match (kind, msg) {
            (StreamKind::Control, m @ Msg::Plan(_)) if first.swap(false, SeqCst) => {
                let entry = FileEntry {
                    path: b"zz".to_vec(),
                    kind: Kind::Regular as i32,
                    ..FileEntry::default()
                };
                vec![Msg::Listing(ListingEntry { entry: Some(entry) }), m]
            }
            (_, m) => vec![m],
        }));
        let (out, _) = h
            .run_reconnect(&archive_opts(), flow, no_reconnect(), local)
            .await;
        let err = format!("{:#}", out.expect_err("a broken protocol was accepted"));
        assert!(err.contains("listing frame on the control stream"), "{err}");
    })
    .await;
}

/// A sender that fails before it lists: a resumed daemon sender waits for
/// the receiver's first frame to decide whether to list, and the client's
/// first control frame breaks the protocol. The client (listing afresh,
/// as `--delete` never reuses a stored plan) must report the daemon's
/// error, never that the daemon's listing ended without a frame, whichever
/// stream it reads first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sender_failing_before_its_listing_reports_its_own_error() {
    within120(async {
        let flow = Flow::Pull;
        let h = Harness::new().await;
        twenty_files(h.src(flow));
        h.daemon.sender_parallel(4);
        h.daemon.fault.drop_after_bytes(10 * MIB as u64);
        let (first, _) = h
            .run_reconnect(&archive_opts(), flow, no_reconnect(), h.local_side())
            .await;
        assert!(first.is_err(), "the first run should fail");
        let mut opts = archive_opts();
        opts.delete = DeleteMode::After;
        for i in 0..50 {
            let mut local = h.local_side();
            let first = AtomicBool::new(true);
            local.hooks.tamper_frames = Some(Arc::new(move |kind, msg| match (kind, msg) {
                (StreamKind::Control, m) if first.swap(false, SeqCst) => {
                    let entry = FileEntry {
                        path: b"zz".to_vec(),
                        kind: Kind::Regular as i32,
                        ..FileEntry::default()
                    };
                    vec![Msg::Listing(ListingEntry { entry: Some(entry) }), m]
                }
                (_, m) => vec![m],
            }));
            let (out, _) = h.run_reconnect(&opts, flow, no_reconnect(), local).await;
            let err = format!("{:#}", out.expect_err("a broken protocol was accepted"));
            assert!(
                err.contains("listing frame on the control stream"),
                "run {i}: {err}"
            );
        }
    })
    .await;
}
