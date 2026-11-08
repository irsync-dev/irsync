//! Each session's `Outcome` carries a `Usage`: what the session spent its time and bytes on.

mod common;

use std::fs;
use std::path::Path;
use std::time::Duration;

use common::{archive_opts, assert_trees_equal, sync_trees, Flow, Harness};
use irsync::fs::paths::RelPath;
use irsync::session::SideHooks;
use irsync::telemetry::meter::{Activity, Counter};
use std::sync::Arc;

const FILES: u64 = 20;
const SIZE: usize = 64 * 1024;

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

fn tree(root: &Path) {
    for i in 0..FILES {
        fs::write(root.join(format!("f{i:02}.bin")), data(SIZE, i as u32 + 1)).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_usage_counts_walk_hash_read_send() {
    irsync::telemetry::set_enabled(true);
    let opts = archive_opts();
    let (out, h) = sync_trees(tree, |_| {}, &opts, Flow::Push).await;
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_trees_equal(h.src(Flow::Push), h.dst(Flow::Push), &opts.preserve);
    let u = &out.usage;
    assert!(u.busy(Activity::Walk) > Duration::ZERO);
    assert!(u.busy(Activity::Hash) > Duration::ZERO);
    assert!(u.busy(Activity::Read) > Duration::ZERO);
    assert!(u.busy(Activity::Send) > Duration::ZERO);
    assert_eq!(u.count(Counter::FilesHashed), FILES);
    assert_eq!(u.count(Counter::BytesHashed), FILES * SIZE as u64);
    assert!(u.count(Counter::DiskRead) >= FILES * SIZE as u64);
    assert_eq!(u.count(Counter::BytesLiteral), out.bytes_transferred);
    assert!(u.wall >= u.busy(Activity::Walk));
    assert!(u.wall > Duration::ZERO);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pull_usage_counts_recv_write_place() {
    irsync::telemetry::set_enabled(true);
    let opts = archive_opts();
    let (out, h) = sync_trees(tree, |_| {}, &opts, Flow::Pull).await;
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_trees_equal(h.src(Flow::Pull), h.dst(Flow::Pull), &opts.preserve);
    let u = &out.usage;
    assert!(u.busy(Activity::Recv) > Duration::ZERO);
    assert!(u.busy(Activity::Write) > Duration::ZERO);
    assert!(u.busy(Activity::Place) > Duration::ZERO);
    assert_eq!(u.count(Counter::DiskWrite), FILES * SIZE as u64);
    assert_eq!(u.count(Counter::BytesLiteral), out.bytes_transferred);
    assert_eq!(u.count(Counter::FsyncNanos), 0, "no flush without --fsync");
    let mut opts = opts;
    opts.fsync = true;
    let (out, _h) = sync_trees(tree, |_| {}, &opts, Flow::Pull).await;
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert!(out.usage.count(Counter::FsyncNanos) > 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delta_counts_matched_bytes() {
    irsync::telemetry::set_enabled(true);
    let opts = archive_opts();
    let big = 2 * 1024 * 1024;
    let (first, h) = sync_trees(
        |r| {
            tree(r);
            fs::write(r.join("big.bin"), data(big, 99)).unwrap();
        },
        |_| {},
        &opts,
        Flow::Push,
    )
    .await;
    assert!(first.errors.is_empty(), "{:?}", first.errors);

    let mut changed = data(big, 99);
    changed[big / 2] ^= 0xFF;
    fs::write(h.src(Flow::Push).join("big.bin"), changed).unwrap();
    let second = h.run(&opts, Flow::Push).await.unwrap();
    assert!(second.errors.is_empty(), "{:?}", second.errors);
    let u = &second.usage;
    assert!(u.count(Counter::BytesMatched) > 0);
    assert!(
        u.count(Counter::BytesLiteral) < 65536 * 2,
        "{}",
        u.count(Counter::BytesLiteral)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn renamed_files_count_as_matched_on_the_sender() {
    irsync::telemetry::set_enabled(true);
    let opts = archive_opts();
    let (first, h) = sync_trees(tree, |_| {}, &opts, Flow::Push).await;
    assert!(first.errors.is_empty(), "{:?}", first.errors);
    let src = h.src(Flow::Push);
    for i in 0..FILES {
        fs::rename(
            src.join(format!("f{i:02}.bin")),
            src.join(format!("g{i:02}.bin")),
        )
        .unwrap();
    }
    // The receiver rebuilds each renamed file from its old copy; the sender
    // learns that only from the acknowledgements.
    let second = h.run(&opts, Flow::Push).await.unwrap();
    assert!(second.errors.is_empty(), "{:?}", second.errors);
    let u = &second.usage;
    assert_eq!(u.count(Counter::BytesLiteral), 0);
    assert_eq!(u.count(Counter::BytesMatched), FILES * SIZE as u64);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn matched_and_literal_add_up_when_indexed_copies_are_gone() {
    irsync::telemetry::set_enabled(true);
    let opts = archive_opts();
    let (first, h) = sync_trees(tree, |_| {}, &opts, Flow::Pull).await;
    assert!(first.errors.is_empty(), "{:?}", first.errors);
    // The index still lists the deleted files: their chunks look local,
    // reading them fails, and the receiver asks the sender after all.
    let dst = h.dst(Flow::Pull);
    for i in 0..FILES {
        fs::remove_file(dst.join(format!("f{i:02}.bin"))).unwrap();
    }
    let second = h.run(&opts, Flow::Pull).await.unwrap();
    assert!(second.errors.is_empty(), "{:?}", second.errors);
    assert_trees_equal(h.src(Flow::Pull), dst, &opts.preserve);
    let u = &second.usage;
    let (literal, matched) = (
        u.count(Counter::BytesLiteral),
        u.count(Counter::BytesMatched),
    );
    assert_eq!(
        literal + matched,
        FILES * SIZE as u64,
        "literal {literal} matched {matched}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_pass_is_timed() {
    irsync::telemetry::set_enabled(true);
    let mut opts = archive_opts();
    opts.delete = irsync::cli::DeleteMode::After;
    let (out, _h) = sync_trees(
        tree,
        |r| {
            for i in 0..FILES {
                fs::write(r.join(format!("extra{i}")), b"x").unwrap();
            }
        },
        &opts,
        Flow::Pull,
    )
    .await;
    assert_eq!(out.deleted.len() as u64, FILES);
    assert!(out.usage.busy(Activity::Delete) > Duration::ZERO);
    assert!(out.usage.busy(Activity::Plan) > Duration::ZERO);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_usage_samples_process_and_rtt() {
    irsync::telemetry::set_enabled(true);
    let opts = archive_opts();
    let (out, _h) = sync_trees(tree, |_| {}, &opts, Flow::Push).await;
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    let (p, n) = (&out.usage.process, &out.usage.net);
    assert!(n.rtt_max > Duration::ZERO, "{n:?}");
    assert!(n.rtt_min <= n.rtt_avg && n.rtt_avg <= n.rtt_max, "{n:?}");
    assert!(p.peak_fds > 0 && p.peak_rss > 0, "{p:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_resent_in_the_same_session_is_not_counted_as_matched() {
    irsync::telemetry::set_enabled(true);
    const MIB: usize = 1024 * 1024;
    let opts = archive_opts();
    let flow = Flow::Push;
    let h = Harness::new().await;
    let src = h.src(flow).to_path_buf();
    let old = data(4 * MIB, 21);
    fs::write(src.join("x.bin"), &old).unwrap();
    // Changed after its first chunk went out: the start stays, so the
    // file's second writer adopts what the first one already received.
    let mut new = old[..3 * MIB].to_vec();
    new.extend(data(MIB, 22));
    let x = RelPath::new(b"x.bin").unwrap();
    let mut local = h.local_side();
    local.hooks = SideHooks {
        on_first_chunk_sent: Some({
            let (src, new) = (src.clone(), new.clone());
            Arc::new(move |p: &RelPath| {
                let hit = *p == x;
                let (src, new) = (src.clone(), new.clone());
                Box::pin(async move {
                    if hit {
                        fs::write(src.join("x.bin"), &new).unwrap();
                    }
                })
            })
        }),
        ..SideHooks::default()
    };
    let out = h.run_side(&opts, flow, local).await.unwrap();
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(fs::read(h.dst(flow).join("x.bin")).unwrap(), new);
    // Nothing was at the destination and nothing local to copy from:
    // every byte of the file came from the sender in this session.
    assert_eq!(out.usage.count(Counter::BytesMatched), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn links_and_specials_are_timed_as_place() {
    irsync::telemetry::set_enabled(true);
    let opts = archive_opts();
    let (out, _h) = sync_trees(
        |r| {
            for i in 0..50 {
                std::os::unix::fs::symlink(format!("target{i}"), r.join(format!("l{i:02}")))
                    .unwrap();
                nix::unistd::mkfifo(&r.join(format!("p{i:02}")), nix::sys::stat::Mode::S_IRWXU)
                    .unwrap();
            }
        },
        |_| {},
        &opts,
        Flow::Pull,
    )
    .await;
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    // No regular file: all the placing is making the links and FIFOs.
    assert!(out.usage.busy(Activity::Place) > Duration::ZERO);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bwlimit_pause_is_timed_as_send() {
    irsync::telemetry::set_enabled(true);
    let mut opts = archive_opts();
    // 20 files of 64 KiB at 2 MiB/s: about 0.6 s of pauses.
    opts.bwlimit = Some(2 << 20);
    let (out, _h) = sync_trees(tree, |_| {}, &opts, Flow::Push).await;
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    let send = out.usage.busy(Activity::Send);
    assert!(send >= Duration::from_millis(400), "send {send:?}");
}
