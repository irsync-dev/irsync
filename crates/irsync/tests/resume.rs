//! Reconnect and resume: a session outlives a dropped connection and a
//! restarted client, over loopback iroh endpoints with an in-process daemon.

mod common;

use std::fs;
use std::path::Path;
use std::time::Duration;

use common::{archive_opts, assert_trees_equal, Flow, Harness};
use irsync::session::engine::SessionRefused;
use irsync::session::reconnect::ReconnectOpts;
use irsync_proto::v1::error::Code;

const MIB: usize = 1024 * 1024;
const FILES: usize = 20;
const FILE_SIZE: usize = 2 * MIB;
const TOTAL: u64 = (FILES * FILE_SIZE) as u64;

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

/// 20 files of 2 MiB each, two of them in a subdirectory.
fn twenty_files(root: &Path) {
    fs::create_dir(root.join("sub")).unwrap();
    for i in 0..FILES {
        let name = if i < 2 {
            format!("sub/f{i:02}.bin")
        } else {
            format!("f{i:02}.bin")
        };
        fs::write(root.join(name), data(FILE_SIZE, i as u32 + 1)).unwrap();
    }
}

/// No reconnects: the first failure ends the run.
fn no_reconnect() -> ReconnectOpts {
    ReconnectOpts {
        max: Duration::ZERO,
        ..fast()
    }
}

/// Default reconnect options with a 100 ms first backoff.
fn fast() -> ReconnectOpts {
    ReconnectOpts {
        backoff_min: Duration::from_millis(100),
        ..ReconnectOpts::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connection_drop_mid_transfer_completes_byte_exact() {
    within60(async {
        let opts = archive_opts();
        for flow in [Flow::Push, Flow::Pull] {
            let h = Harness::new().await;
            twenty_files(h.src(flow));
            h.daemon.fault.drop_after_bytes(10 * MIB as u64);

            let (out, reconnects) = h.run_reconnect(&opts, flow, fast(), h.local_side()).await;
            let out = out.unwrap_or_else(|e| panic!("{flow:?}: {e:#}"));
            assert!(h.daemon.fault.fired(), "{flow:?}: the fault never fired");
            assert_eq!(reconnects, 1, "{flow:?}");
            assert!(
                out.resumed,
                "{flow:?}: the second connection did not resume"
            );
            assert!(out.errors.is_empty(), "{flow:?}: {:?}", out.errors);
            assert!(
                out.bytes_transferred < TOTAL,
                "{flow:?}: resumed connection re-sent everything ({} bytes)",
                out.bytes_transferred
            );
            assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stalled_connection_is_dropped_after_timeout() {
    within60(async {
        for flow in [Flow::Push, Flow::Pull] {
            let h = Harness::new().await;
            fs::write(h.src(flow).join("f"), data(100 * 1024, 3)).unwrap();
            // The daemon acknowledges Hello, then goes silent.
            h.daemon.fault.stall_after_ack_once();
            let mut opts = archive_opts();
            opts.timeout = Some(Duration::from_secs(2));
            let start = std::time::Instant::now();
            let (out, reconnects) = h.run_reconnect(&opts, flow, fast(), h.local_side()).await;
            let elapsed = start.elapsed();
            let out = out.unwrap_or_else(|e| panic!("{flow:?}: {e:#}"));
            assert!(out.errors.is_empty(), "{flow:?}: {:?}", out.errors);
            // Redialled (the stalled session was never stored, so the
            // daemon answers BAD_SESSION and a fresh one follows at once).
            assert!(reconnects >= 1, "{flow:?}");
            assert!(
                elapsed < Duration::from_secs(6),
                "{flow:?}: took {elapsed:?}"
            );
            assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resumed_pull_keeps_inflight_cap() {
    within60(async {
        let opts = archive_opts();
        let flow = Flow::Pull;
        let h = Harness::new().await;
        twenty_files(h.src(flow));
        h.daemon.fault.drop_after_bytes(10 * MIB as u64);
        let (first, _) = h
            .run_reconnect(&opts, flow, no_reconnect(), h.local_side())
            .await;
        assert!(first.is_err());
        // The restarted client resumes the stored plan with a cap of 2.
        let peak = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut local = h.local_side();
        local.hooks.max_inflight = Some(2);
        local.hooks.inflight_peak = Some(peak.clone());
        let (second, _) = h.run_reconnect(&opts, flow, fast(), local).await;
        let second = second.unwrap_or_else(|e| panic!("{e:#}"));
        assert!(second.resumed);
        assert!(second.errors.is_empty(), "{:?}", second.errors);
        let peak = peak.load(std::sync::atomic::Ordering::SeqCst);
        assert!((1..=2).contains(&peak), "peak in flight {peak}");
        assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_restart_resumes_same_session() {
    within60(async {
        let opts = archive_opts();
        for flow in [Flow::Push, Flow::Pull] {
            let h = Harness::new().await;
            twenty_files(h.src(flow));
            h.daemon.fault.drop_after_bytes(10 * MIB as u64);

            // First "process": the connection drops and it gives up.
            let (first, reconnects) = h
                .run_reconnect(&opts, flow, no_reconnect(), h.local_side())
                .await;
            assert!(first.is_err(), "{flow:?}: first run should fail");
            assert_eq!(reconnects, 0, "{flow:?}");
            let ids = h.daemon.session_ids();
            assert_eq!(ids.len(), 1, "{flow:?}");

            // Second "process": a fresh LocalSide over the same dirs and state.
            let (second, reconnects) = h.run_reconnect(&opts, flow, fast(), h.local_side()).await;
            let second = second.unwrap_or_else(|e| panic!("{flow:?}: {e:#}"));
            assert_eq!(reconnects, 0, "{flow:?}");
            assert!(second.resumed, "{flow:?}: HelloAck.resumed was false");
            assert_eq!(second.session_id, ids[0], "{flow:?}: not the same session");
            assert!(second.errors.is_empty(), "{flow:?}: {:?}", second.errors);
            assert!(
                second.bytes_transferred < TOTAL,
                "{flow:?}: run 2 sent {} of {TOTAL} bytes",
                second.bytes_transferred
            );
            assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);

            // A finished session is gone: the next run starts fresh.
            let (third, _) = h.run_reconnect(&opts, flow, fast(), h.local_side()).await;
            let third = third.unwrap();
            assert!(!third.resumed, "{flow:?}: finished session resumed again");
            assert_eq!(third.bytes_transferred, 0, "{flow:?}");
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_resume_flag_starts_fresh() {
    within60(async {
        let mut opts = archive_opts();
        for flow in [Flow::Push, Flow::Pull] {
            let h = Harness::new().await;
            twenty_files(h.src(flow));
            h.daemon.fault.drop_after_bytes(30 * MIB as u64);

            let (first, _) = h
                .run_reconnect(&opts, flow, no_reconnect(), h.local_side())
                .await;
            assert!(first.is_err(), "{flow:?}: first run should fail");

            opts.no_resume = true;
            let (second, _) = h.run_reconnect(&opts, flow, fast(), h.local_side()).await;
            opts.no_resume = false;
            let second = second.unwrap_or_else(|e| panic!("{flow:?}: {e:#}"));
            assert!(!second.resumed, "{flow:?}: --no-resume resumed");
            assert_ne!(second.session_id, h.daemon.session_ids()[0], "{flow:?}");
            assert!(second.errors.is_empty(), "{flow:?}: {:?}", second.errors);
            // Placed files are up to date and partial files are adopted.
            assert!(
                second.bytes_transferred < TOTAL / 2,
                "{flow:?}: fresh session sent {} of {TOTAL} bytes",
                second.bytes_transferred
            );
            assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreign_client_cannot_resume_session() {
    within60(async {
        let opts = archive_opts();
        let flow = Flow::Push;
        let (h, guest) = Harness::with_guest().await;
        twenty_files(h.src(flow));
        h.daemon.fault.drop_after_bytes(10 * MIB as u64);
        let (first, _) = h
            .run_reconnect(&opts, flow, no_reconnect(), h.local_side())
            .await;
        assert!(first.is_err());
        let id = h.daemon.session_ids()[0];

        // Another authorised key presents the dormant session's id.
        let guest_dir = tempfile::tempdir().unwrap();
        let guest_state = tempfile::tempdir().unwrap();
        fs::write(guest_dir.path().join("evil.txt"), b"not yours").unwrap();
        let side = Harness::side_for(guest_dir.path(), guest_state.path());
        let conn = guest
            .connect(h.daemon.addr.clone(), irsync_proto::ALPN)
            .await
            .unwrap();
        let mut hello = Harness::hello(flow);
        hello.session_id = id.to_vec();
        let err = irsync::session::engine::run_client(conn.clone(), hello, side, &opts)
            .await
            .expect_err("foreign client resumed a session");
        conn.close(0u32.into(), b"done");
        let refused = err
            .chain()
            .find_map(|e| e.downcast_ref::<SessionRefused>())
            .unwrap_or_else(|| panic!("not a refusal: {err:#}"));
        assert_eq!(refused.code, Code::BadSession);
        assert!(!h.module.path().join("evil.txt").exists());

        // The owner can still resume it.
        let (again, _) = h.run_reconnect(&opts, flow, fast(), h.local_side()).await;
        let again = again.unwrap();
        assert!(again.resumed);
        assert_eq!(again.session_id, id);
        assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn incomplete_plan_resumes_with_fresh_listing() {
    within60(async {
        let opts = archive_opts();
        // Pull: the client is the receiver, so its stored plan decides.
        let flow = Flow::Pull;
        let h = Harness::new().await;
        twenty_files(h.src(flow));
        h.daemon.fault.drop_after_bytes(30 * MIB as u64);
        let (first, _) = h
            .run_reconnect(&opts, flow, no_reconnect(), h.local_side())
            .await;
        assert!(first.is_err());
        let id = h.daemon.session_ids()[0];

        // As if the connection had dropped before planning finished.
        let mut ix = irsync::index::Index::open(h.client_state.path(), h.local.path()).unwrap();
        let row = ix.load_session_by_id(&id).unwrap().expect("stored session");
        let mut rec = irsync::session::SessionRecord::from_row(&row).unwrap();
        assert!(rec.plan.plan_complete);
        assert!(
            !rec.progress.done_file_ids.is_empty(),
            "nothing placed in run 1"
        );
        rec.plan.plan_complete = false;
        ix.save_session(&rec.to_row()).unwrap();
        drop(ix);

        let (out, _) = h.run_reconnect(&opts, flow, fast(), h.local_side()).await;
        let out = out.unwrap_or_else(|e| panic!("{e:#}"));
        assert!(out.resumed);
        assert_eq!(out.session_id, id);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(out.bytes_transferred < TOTAL, "{}", out.bytes_transferred);
        // Files placed before the drop count as up to date, not as transferred.
        assert!(out.files_skipped >= rec.progress.done_file_ids.len() as u64);
        assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_session_falls_back_to_fresh() {
    within60(async {
        let opts = archive_opts();
        let flow = Flow::Pull;
        let h = Harness::new().await;
        twenty_files(h.src(flow));
        h.daemon.fault.drop_after_bytes(10 * MIB as u64);
        let (first, _) = h
            .run_reconnect(&opts, flow, no_reconnect(), h.local_side())
            .await;
        assert!(first.is_err());
        // The daemon forgot the session (e.g. dormant-session GC).
        let gone = h
            .daemon_index()
            .delete_sessions_older_than(Duration::ZERO)
            .unwrap();
        assert_eq!(gone.len(), 1);

        let (out, reconnects) = h.run_reconnect(&opts, flow, fast(), h.local_side()).await;
        let out = out.unwrap_or_else(|e| panic!("{e:#}"));
        assert_eq!(reconnects, 0, "BAD_SESSION is not a reconnect");
        assert!(!out.resumed);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_that_drops_after_ack_respects_max() {
    within60(async {
        let opts = archive_opts();
        let flow = Flow::Push;
        let h = Harness::new().await;
        twenty_files(h.src(flow));
        h.daemon.fault.drop_after_ack();
        let ropts = ReconnectOpts {
            max: Duration::from_secs(3),
            ..fast()
        };
        let started = std::time::Instant::now();
        let (out, reconnects) = h.run_reconnect(&opts, flow, ropts, h.local_side()).await;
        let took = started.elapsed();
        let err = out.expect_err("a daemon that never lets data through cannot succeed");
        assert!(
            took < Duration::from_secs(10),
            "gave up after {took:?}: {err:#}"
        );
        assert!(
            took >= Duration::from_secs(3),
            "gave up early ({took:?}): {err:#}"
        );
        assert!(reconnects >= 2, "only {reconnects} reconnects");
        let hellos = h.daemon.hellos.load(std::sync::atomic::Ordering::SeqCst);
        assert!(hellos >= 3, "only {hellos} connections reached the daemon");
    })
    .await;
}

/// Pushes one multi-chunk file with the client's data streams cut short by `hook`.
async fn push_with_truncation(
    hook: std::sync::Arc<irsync::send::TruncateHook>,
) -> (anyhow::Result<irsync::session::Outcome>, Harness) {
    let opts = archive_opts();
    let flow = Flow::Push;
    let h = Harness::new().await;
    fs::write(h.src(flow).join("big.bin"), data(4 * MIB, 77)).unwrap();
    fs::write(h.src(flow).join("small.txt"), b"fine").unwrap();
    let mut local = h.local_side();
    local.hooks.truncate_streams = Some(hook);
    let out = h.run_side(&opts, flow, local).await;
    (out, h)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn short_stream_is_re_requested_and_completes() {
    within60(async {
        let hook = irsync::send::TruncateHook::once(2);
        let (out, h) = push_with_truncation(hook.clone()).await;
        let out = out.unwrap();
        assert_eq!(hook.fired(), 1, "the stream was never cut");
        assert_eq!(
            h.daemon
                .rerequests
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.files_transferred, 2);
        assert_trees_equal(
            h.src(Flow::Push),
            h.dst(Flow::Push),
            &archive_opts().preserve,
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn four_short_streams_give_write_error() {
    within60(async {
        let hook = irsync::send::TruncateHook::always(2);
        let (out, h) = push_with_truncation(hook.clone()).await;
        let out = out.unwrap();
        // The first stream and three re-requests, all cut short.
        assert_eq!(hook.fired(), 4);
        assert_eq!(
            h.daemon
                .rerequests
                .load(std::sync::atomic::Ordering::SeqCst),
            3
        );
        assert_eq!(out.exit_code(), 23);
        assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
        let (path, msg) = &out.errors[0];
        assert_eq!(path.as_bytes(), b"big.bin");
        assert!(msg.contains("incomplete after retries"), "{msg}");
        assert_eq!(out.files_transferred, 1);
        assert!(!h.dst(Flow::Push).join("big.bin").exists());
        assert_eq!(
            fs::read(h.dst(Flow::Push).join("small.txt")).unwrap(),
            b"fine"
        );
    })
    .await;
}
