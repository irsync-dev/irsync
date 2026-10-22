//! Remedy 7: a changed file is not rebuilt from scratch. By default the
//! `.part` is a reflink clone of the old file where the filesystem allows,
//! so only changed chunks are written; `--inplace` writes into the
//! destination itself with ascending, forward-only reuse; `--append`
//! transfers only the tail of a file that grew.

mod common;

use std::fs;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::Path;
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

/// A harness whose trees live under `target/`, where the filesystem may
/// clone files; `None` (after a note) when it does not.
async fn reflink_harness() -> Option<Harness> {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
    fs::create_dir_all(base).unwrap();
    let a = fs::File::create(base.join(".probe-a")).unwrap();
    let b = fs::File::create(base.join(".probe-b")).unwrap();
    let ok = irsync::recv::reflink(&a, &b).is_ok();
    let _ = fs::remove_file(base.join(".probe-a"));
    let _ = fs::remove_file(base.join(".probe-b"));
    if !ok {
        eprintln!("skipping: {} does not support reflinks", base.display());
        return None;
    }
    Some(Harness::new_in(base).await)
}

async fn seeded_in(h: &Harness, flow: Flow, content: &[u8]) {
    fs::write(h.src(flow).join("x.bin"), content).unwrap();
    let out = h.run(&archive_opts(), flow).await.unwrap();
    assert!(out.errors.is_empty(), "{:?}", out.errors);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cloned_part_writes_only_the_changed_chunk() {
    within30(async {
        let Some(h) = reflink_harness().await else {
            return;
        };
        let flow = Flow::Pull;
        seeded_in(&h, flow, &data(4 * MIB, 1)).await;
        let src = h.src(flow).join("x.bin");
        let dst = h.dst(flow).join("x.bin");
        patch(&src, 2 * MIB as u64 + 999, &[0x77]);
        let before = fs::metadata(&dst).unwrap().ino();
        let log: irsync::session::EventLog = Arc::default();
        let mut local = h.local_side();
        local.hooks.events = Some(log.clone());
        let out = h.run_side(&archive_opts(), flow, local).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(&dst).unwrap(), fs::read(&src).unwrap());
        let events = log.lock().unwrap().clone();
        assert_eq!(count(&events, "clone:x.bin"), 1, "{events:?}");
        // The unchanged chunks were adopted from the clone: nothing to copy.
        assert_eq!(count(&events, "copied:x.bin"), 0, "{events:?}");
        assert_eq!(count(&events, "chunk:x.bin"), 1, "{events:?}");
        // Still placed by rename: a new inode, no `.part` left.
        assert_ne!(fs::metadata(&dst).unwrap().ino(), before);
        assert!(!h.dst(flow).join(".irsync.x.bin.part").exists());
        assert!(out.bytes_transferred < 16 * KIB as u64);
    })
    .await;
}

fn inplace_opts() -> irsync::cli::SyncOptions {
    let args = irsync::cli::Args::try_parse_from(["irsync", "-a", "--inplace", "src/", "host:m/"])
        .unwrap();
    irsync::cli::SyncOptions::from_args(&args)
}

fn inode(p: &Path) -> u64 {
    fs::metadata(p).unwrap().ino()
}

/// Seeds `x.bin` with `content` in direction `flow` on a plain harness.
async fn seeded(flow: Flow, content: &[u8]) -> Harness {
    let h = Harness::new().await;
    seeded_in(&h, flow, content).await;
    h
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inplace_keeps_the_inode_and_writes_only_the_changed_chunk() {
    within30(async {
        let flow = Flow::Pull;
        let h = seeded(flow, &data(4 * MIB, 11)).await;
        let src = h.src(flow).join("x.bin");
        let dst = h.dst(flow).join("x.bin");
        patch(&src, 2 * MIB as u64 + 999, &[0x77]);
        let before = inode(&dst);
        let log: irsync::session::EventLog = Arc::default();
        let mut local = h.local_side();
        local.hooks.events = Some(log.clone());
        let out = h.run_side(&inplace_opts(), flow, local).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(&dst).unwrap(), fs::read(&src).unwrap());
        assert_eq!(inode(&dst), before);
        let events = log.lock().unwrap().clone();
        assert_eq!(count(&events, "inplace:x.bin"), 1, "{events:?}");
        assert_eq!(count(&events, "chunk:x.bin"), 1, "{events:?}");
        assert_eq!(count(&events, "copied:x.bin"), 0, "{events:?}");
        assert!(!h.dst(flow).join(".irsync.x.bin.part").exists());
        assert!(
            out.bytes_transferred < 16 * KIB as u64,
            "{}",
            out.bytes_transferred
        );
        assert_eq!(out.files_transferred, 1);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inplace_deletion_near_the_start_reuses_chunks_from_above() {
    within30(async {
        let flow = Flow::Pull;
        let old = data(4 * MIB, 12);
        let h = seeded(flow, &old).await;
        let src = h.src(flow).join("x.bin");
        let dst = h.dst(flow).join("x.bin");
        // 100 KiB cut out at 1 MiB: everything after moves down, so its old
        // copy sits above the write cursor and can be taken from the file.
        let mut new = old[..MIB].to_vec();
        new.extend_from_slice(&old[MIB + 100 * KIB..]);
        fs::write(&src, &new).unwrap();
        let before = inode(&dst);
        let log: irsync::session::EventLog = Arc::default();
        let mut local = h.local_side();
        local.hooks.events = Some(log.clone());
        let out = h.run_side(&inplace_opts(), flow, local).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(&dst).unwrap(), new);
        assert_eq!(inode(&dst), before);
        let events = log.lock().unwrap().clone();
        assert_eq!(count(&events, "copied:x.bin"), 1, "{events:?}");
        assert!(
            out.bytes_transferred < MIB as u64,
            "{} bytes crossed for a 100 KiB deletion",
            out.bytes_transferred
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inplace_insertion_near_the_start_takes_shifted_chunks_from_the_network() {
    within30(async {
        let flow = Flow::Pull;
        let old = data(4 * MIB, 13);
        let h = seeded(flow, &old).await;
        let src = h.src(flow).join("x.bin");
        let dst = h.dst(flow).join("x.bin");
        // 100 KiB inserted at 1 MiB: everything after moves up, so its old
        // copy is below the write cursor by the time it is needed, already
        // overwritten. Those chunks must come from the sender, never from
        // the file: the result is byte-exact.
        let mut new = old[..MIB].to_vec();
        new.extend_from_slice(&data(100 * KIB, 14));
        new.extend_from_slice(&old[MIB..]);
        fs::write(&src, &new).unwrap();
        let before = inode(&dst);
        let out = h
            .run_side(&inplace_opts(), flow, h.local_side())
            .await
            .unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(&dst).unwrap(), new);
        assert_eq!(inode(&dst), before);
        assert!(
            out.bytes_transferred > 2 * MIB as u64,
            "{} bytes: shifted chunks were taken from the file being written",
            out.bytes_transferred
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inplace_on_a_push_is_honoured_by_the_daemon() {
    within30(async {
        let flow = Flow::Push;
        let h = seeded(flow, &data(4 * MIB, 15)).await;
        let src = h.src(flow).join("x.bin");
        let dst = h.dst(flow).join("x.bin");
        patch(&src, MIB as u64, &data(4 * KIB, 16));
        let before = inode(&dst);
        h.daemon.events.lock().unwrap().clear();
        let out = h.run(&inplace_opts(), flow).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(&dst).unwrap(), fs::read(&src).unwrap());
        assert_eq!(inode(&dst), before);
        let events = h.daemon.events.lock().unwrap().clone();
        assert_eq!(count(&events, "inplace:x.bin"), 1, "{events:?}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inplace_resumes_after_a_connection_loss() {
    within30(async {
        let flow = Flow::Pull;
        let h = seeded(flow, &data(16 * MIB, 17)).await;
        let src = h.src(flow).join("x.bin");
        let dst = h.dst(flow).join("x.bin");
        for k in 0..8u64 {
            patch(
                &src,
                k * 2 * MIB as u64 + 4321,
                &data(4 * KIB, 20 + k as u32),
            );
        }
        let before = inode(&dst);
        h.daemon.fault.drop_after_bytes(3 * KIB as u64);
        let ropts = irsync::session::reconnect::ReconnectOpts {
            backoff_min: Duration::from_millis(100),
            ..Default::default()
        };
        let (out, reconnects) = h
            .run_reconnect(&inplace_opts(), flow, ropts, h.local_side())
            .await;
        let out = out.unwrap();
        assert!(h.daemon.fault.fired());
        assert!(reconnects >= 1);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(&dst).unwrap(), fs::read(&src).unwrap());
        assert_eq!(inode(&dst), before);
        assert!(!h.dst(flow).join(".irsync.x.bin.part").exists());
    })
    .await;
}

fn append_opts(verify: bool) -> irsync::cli::SyncOptions {
    let flag = if verify {
        "--append-verify"
    } else {
        "--append"
    };
    let args =
        irsync::cli::Args::try_parse_from(["irsync", "-a", flag, "src/", "host:m/"]).unwrap();
    irsync::cli::SyncOptions::from_args(&args)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn append_sends_only_the_tail_and_reads_only_the_tail() {
    within30(async {
        // Pull: the daemon sends; its hasher must not read the file again.
        let flow = Flow::Pull;
        let old = data(4 * MIB, 31);
        let h = seeded(flow, &old).await;
        let src = h.src(flow).join("x.bin");
        let dst = h.dst(flow).join("x.bin");
        let mut new = old.clone();
        new.extend_from_slice(&data(MIB, 32));
        fs::write(&src, &new).unwrap();
        let hashed_before = h.daemon_files_hashed();
        let log: irsync::session::EventLog = Arc::default();
        let mut local = h.local_side();
        local.hooks.events = Some(log.clone());
        let out = h.run_side(&append_opts(false), flow, local).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(&dst).unwrap(), new);
        let events = log.lock().unwrap().clone();
        assert_eq!(count(&events, "append:x.bin"), 1, "{events:?}");
        assert_eq!(
            h.daemon_files_hashed(),
            hashed_before,
            "the sender read the prefix"
        );
        assert!(
            out.bytes_transferred >= MIB as u64
                && out.bytes_transferred < MIB as u64 + 64 * KIB as u64,
            "{} bytes crossed for a 1 MiB append",
            out.bytes_transferred
        );
        assert_eq!(out.files_transferred, 1);
        // The placed file is indexed with its true whole-file hash, so the
        // next run finds it up to date.
        let again = h.run(&archive_opts(), flow).await.unwrap();
        assert_eq!(again.files_transferred, 0);
        assert_eq!(again.files_skipped, 1);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn append_skips_a_changed_file_that_did_not_grow() {
    within30(async {
        let flow = Flow::Pull;
        let old = data(MIB, 33);
        let h = seeded(flow, &old).await;
        let src = h.src(flow).join("x.bin");
        let dst = h.dst(flow).join("x.bin");
        patch(&src, 1000, &data(100, 34));
        let out = h.run(&append_opts(false), flow).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.files_transferred, 0);
        // Content untouched (rsync's rule), attributes applied.
        assert_eq!(fs::read(&dst).unwrap(), old);
        assert_eq!(
            fs::metadata(&dst).unwrap().modified().unwrap(),
            fs::metadata(&src).unwrap().modified().unwrap()
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn append_trusts_the_prefix_and_append_verify_does_not() {
    within30(async {
        let flow = Flow::Pull;
        let old = data(4 * MIB, 35);
        let h = seeded(flow, &old).await;
        let src = h.src(flow).join("x.bin");
        let dst = h.dst(flow).join("x.bin");
        // The head changes and the file grows.
        let mut new = old.clone();
        new[MIB] ^= 0xFF;
        new.extend_from_slice(&data(MIB, 36));
        fs::write(&src, &new).unwrap();
        let out = h.run(&append_opts(false), flow).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let got = fs::read(&dst).unwrap();
        assert_eq!(got.len(), new.len());
        assert_eq!(&got[4 * MIB..], &new[4 * MIB..]);
        assert_eq!(got[MIB], old[MIB], "--append must not look at the prefix");
        // (And now the destination has the source's size and mtime: no later
        // run without -c can tell the heads apart. That is the price.)
        // --append-verify from the old content treats the file as a delta:
        // the changed head is fixed along with the append.
        fs::write(&dst, &old).unwrap();
        let log: irsync::session::EventLog = Arc::default();
        let mut local = h.local_side();
        local.hooks.events = Some(log.clone());
        let out = h.run_side(&append_opts(true), flow, local).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(&dst).unwrap(), new);
        let events = log.lock().unwrap().clone();
        assert_eq!(count(&events, "append:x.bin"), 0, "{events:?}");
        assert!(
            out.bytes_transferred < 2 * MIB as u64,
            "{}",
            out.bytes_transferred
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn append_on_a_push_is_honoured_by_the_daemon() {
    within30(async {
        let flow = Flow::Push;
        let old = data(2 * MIB, 37);
        let h = seeded(flow, &old).await;
        let src = h.src(flow).join("x.bin");
        let dst = h.dst(flow).join("x.bin");
        let mut new = old.clone();
        new.extend_from_slice(&data(512 * KIB, 38));
        fs::write(&src, &new).unwrap();
        h.daemon.events.lock().unwrap().clear();
        let out = h.run(&append_opts(false), flow).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(&dst).unwrap(), new);
        let events = h.daemon.events.lock().unwrap().clone();
        assert_eq!(count(&events, "append:x.bin"), 1, "{events:?}");
        assert!(
            out.bytes_transferred < 600 * KIB as u64,
            "{} bytes crossed for a 512 KiB append",
            out.bytes_transferred
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn append_fails_the_file_when_the_destination_is_not_the_planned_prefix() {
    within30(async {
        let flow = Flow::Pull;
        let old = data(2 * MIB, 39);
        let h = seeded(flow, &old).await;
        let src = h.src(flow).join("x.bin");
        let dst = h.dst(flow).join("x.bin");
        let mut new = old.clone();
        new.extend_from_slice(&data(512 * KIB, 40));
        fs::write(&src, &new).unwrap();
        // The destination changes length between the plan and the chunk
        // list: the sender's tail no longer continues what is there.
        let rotten = dst.clone();
        let mut local = h.local_side();
        local.hooks.before_open = Some(Arc::new(move |_| {
            let rotten = rotten.clone();
            Box::pin(async move {
                let mut bytes = fs::read(&rotten).unwrap();
                bytes.truncate(MIB);
                fs::write(&rotten, bytes).unwrap();
            })
        }));
        let out = h.run_side(&append_opts(false), flow, local).await.unwrap();
        assert_eq!(out.files_transferred, 0);
        assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
        assert!(out.errors[0].1.contains("append"), "{:?}", out.errors);
        assert_eq!(fs::metadata(&dst).unwrap().len(), MIB as u64);
    })
    .await;
}
