//! The listing on its own stream: listing frames never travel on the
//! control stream, a listing cut short never looks complete, and a session
//! resumes whether or not the receiver lists again.

mod common;

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
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
