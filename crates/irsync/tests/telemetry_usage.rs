//! Each session's `Outcome` carries a `Usage`: what the session spent its time and bytes on.

mod common;

use std::fs;
use std::path::Path;
use std::time::Duration;

use common::{archive_opts, assert_trees_equal, sync_trees, Flow};
use irsync::telemetry::meter::{Activity, Counter};

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
    assert!(u.count(Counter::FsyncNanos) > 0);
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
