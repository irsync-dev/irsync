//! Source files that change while the sender works on them: a change during
//! the hash pass, or after it (caught when the receiver asks before placing
//! a file), is retried once, and given up with exit 23 if it happens again.

mod common;

use std::fs;
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use common::{archive_opts, Flow, Harness};
use irsync::fs::paths::RelPath;
use irsync::hasher::HashHook;
use irsync::session::reconnect::ReconnectOpts;
use irsync::session::{FirstChunkHook, FrameHook, PartsFault, SideHooks, StreamKind};
use irsync_proto::v1::control::Msg;

const KIB: usize = 1024;
const MIB: usize = 1024 * 1024;
const SIZE: usize = 8 * MIB;
const TEAR: usize = 256 * KIB;

/// Every test must finish well inside this.
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

/// Source and destination both hold the same `x.bin`; the destination's
/// mtime is far in the past, so the planner picks a delta. Returns the
/// original content.
fn seed(h: &Harness, flow: Flow) -> Vec<u8> {
    let content = data(SIZE, 7);
    fs::write(h.src(flow).join("x.bin"), &content).unwrap();
    let dst = h.dst(flow).join("x.bin");
    fs::write(&dst, &content).unwrap();
    fs::File::options()
        .write(true)
        .open(&dst)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000))
        .unwrap();
    content
}

/// Writes other data over the first and the last 256 KiB of `path`, then
/// sets its mtime to 1e9 + `n` seconds (`n` differs per tear).
fn tear(path: &Path, seed: u32, n: u32) {
    let f = fs::File::options().write(true).open(path).unwrap();
    f.write_all_at(&data(TEAR, seed), 0).unwrap();
    f.write_all_at(&data(TEAR, seed + 1), (SIZE - TEAR) as u64)
        .unwrap();
    // Coarse timestamps (kernels before 6.13) may not show the write itself.
    f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000 + u64::from(n)))
        .unwrap();
}

/// A hook that tears `x.bin` once it has read 4 MiB of it, on the first
/// pass only or on every pass. `passes` counts the passes over `x.bin`.
fn tearing_hook(src: PathBuf, every_pass: bool, passes: Arc<AtomicU32>) -> HashHook {
    let torn = Arc::new(AtomicBool::new(false));
    Arc::new(move |p: &RelPath, bytes: u64| {
        if p.as_bytes() != b"x.bin" {
            return;
        }
        if bytes == 0 {
            passes.fetch_add(1, Ordering::SeqCst);
            torn.store(false, Ordering::SeqCst);
            return;
        }
        let pass = passes.load(Ordering::SeqCst);
        if bytes >= (4 * MIB) as u64
            && (every_pass || pass == 1)
            && !torn.swap(true, Ordering::SeqCst)
        {
            tear(&src.join("x.bin"), 100 + 2 * pass, pass);
        }
    })
}

/// Runs a sync of `flow` with the tearing hook on the sending side.
async fn run_torn(
    h: &Harness,
    flow: Flow,
    every_pass: bool,
    passes: Arc<AtomicU32>,
    no_checks: bool,
) -> irsync::session::Outcome {
    let hook = tearing_hook(h.src(flow).to_path_buf(), every_pass, passes);
    let mut local = h.local_side();
    match flow {
        Flow::Push => {
            local.hooks = SideHooks {
                during_hash: Some(hook),
                no_change_checks: no_checks,
                ..SideHooks::default()
            }
        }
        Flow::Pull => {
            let mut d = h.daemon.hooks.lock().unwrap();
            d.during_hash = Some(hook);
            d.no_change_checks = no_checks;
        }
    }
    h.run_side(&archive_opts(), flow, local).await.unwrap()
}

async fn torn_hash_is_retried_for(flow: Flow) {
    let h = Harness::new().await;
    seed(&h, flow);
    let passes = Arc::new(AtomicU32::new(0));
    let out = run_torn(&h, flow, false, passes.clone(), false).await;
    assert_eq!(out.exit_code(), 0, "{:?}", out.errors);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert!(
        fs::read(h.dst(flow).join("x.bin")).unwrap()
            == fs::read(h.src(flow).join("x.bin")).unwrap(),
        "the destination differs from the source"
    );
    assert_eq!(passes.load(Ordering::SeqCst), 2);
}

async fn torn_hash_twice_fails_for(flow: Flow) {
    let h = Harness::new().await;
    let before = seed(&h, flow);
    let passes = Arc::new(AtomicU32::new(0));
    let out = run_torn(&h, flow, true, passes.clone(), false).await;
    assert_eq!(out.exit_code(), 23, "{:?}", out.errors);
    assert_eq!(passes.load(Ordering::SeqCst), 2);
    assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
    let (path, msg) = &out.errors[0];
    assert_eq!(path.as_bytes(), b"x.bin");
    assert!(
        msg.contains("changed while it was hashed (after retry)"),
        "{msg}"
    );
    assert!(
        fs::read(h.dst(flow).join("x.bin")).unwrap() == before,
        "the destination changed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn torn_hash_is_retried() {
    within30(torn_hash_is_retried_for(Flow::Push)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn torn_hash_twice_fails_with_23() {
    within30(torn_hash_twice_fails_for(Flow::Push)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn torn_hash_is_retried_on_pull() {
    within30(torn_hash_is_retried_for(Flow::Pull)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn torn_hash_twice_fails_with_23_on_pull() {
    within30(torn_hash_twice_fails_for(Flow::Pull)).await;
}

/// With the change checks off, the torn hash of the original bug lands:
/// exit 0, and the destination holds neither the old nor the new content.
async fn torn_hash_lands_torn_for(flow: Flow) {
    let h = Harness::new().await;
    let before = seed(&h, flow);
    let passes = Arc::new(AtomicU32::new(0));
    let out = run_torn(&h, flow, false, passes.clone(), true).await;
    assert_eq!(out.exit_code(), 0, "{:?}", out.errors);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(passes.load(Ordering::SeqCst), 1, "no retry expected");
    let landed = fs::read(h.dst(flow).join("x.bin")).unwrap();
    assert!(landed != before, "the destination holds the old content");
    assert!(
        landed != fs::read(h.src(flow).join("x.bin")).unwrap(),
        "the destination holds the new content"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn torn_hash_lands_torn_with_checks_off() {
    within30(torn_hash_lands_torn_for(Flow::Push)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn torn_hash_lands_torn_with_checks_off_on_pull() {
    within30(torn_hash_lands_torn_for(Flow::Pull)).await;
}

/// Source and destination hold `x.bin`, the destination's with other data
/// from 1 MiB to 7 MiB and an mtime far in the past: the planner picks a
/// delta, the sender sends the middle (in one request: with most of the
/// file to send, no bases are offered), and the destination copies the
/// rest, the first 256 KiB among it, from its own old copy. Returns the
/// destination's content.
fn seed_delta(h: &Harness, flow: Flow) -> Vec<u8> {
    let content = data(SIZE, 7);
    fs::write(h.src(flow).join("x.bin"), &content).unwrap();
    let mut old = content;
    old[MIB..7 * MIB].copy_from_slice(&data(6 * MIB, 8));
    let dst = h.dst(flow).join("x.bin");
    fs::write(&dst, &old).unwrap();
    fs::File::options()
        .write(true)
        .open(&dst)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000))
        .unwrap();
    old
}

/// Writes other data over the first 256 KiB of `path` only, then sets its
/// mtime to 1e9 + `n` seconds.
fn rewrite_head(path: &Path, seed: u32, n: u32) {
    let f = fs::File::options().write(true).open(path).unwrap();
    f.write_all_at(&data(TEAR, seed), 0).unwrap();
    f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000 + u64::from(n)))
        .unwrap();
}

/// After the first chunk of `x.bin` went out, rewrites its first 256 KiB:
/// a range the destination copies locally, which the sender never reads
/// again.
fn head_after_first_chunk(src: PathBuf, fired: Arc<AtomicU32>) -> FirstChunkHook {
    Arc::new(move |p: &RelPath| {
        let hit = p.as_bytes() == b"x.bin";
        let (src, fired) = (src.clone(), fired.clone());
        Box::pin(async move {
            if hit {
                let n = fired.fetch_add(1, Ordering::SeqCst) + 1;
                rewrite_head(&src.join("x.bin"), 200 + n, n);
            }
        })
    })
}

/// A receiver's frame hook: before every `MayPlace` goes out, rewrites the
/// first 256 KiB of the source's `x.bin` (the only file), as a writer would
/// right after the sender hashed it.
fn head_before_may_place(src: PathBuf, asked: Arc<AtomicU32>) -> FrameHook {
    Arc::new(move |kind, m| {
        if kind == StreamKind::Control && matches!(m, Msg::MayPlace(_)) {
            let n = asked.fetch_add(1, Ordering::SeqCst) + 1;
            rewrite_head(&src.join("x.bin"), 300 + n, 10 + n);
        }
        vec![m]
    })
}

/// The local side for `flow` with `head_before_may_place` on the receiver.
fn changing_before_may_place(
    h: &Harness,
    flow: Flow,
    asked: Arc<AtomicU32>,
) -> irsync::session::LocalSide {
    let hook = head_before_may_place(h.src(flow).to_path_buf(), asked);
    let mut local = h.local_side();
    match flow {
        Flow::Push => h.daemon.hooks.lock().unwrap().tamper_frames = Some(hook),
        Flow::Pull => local.hooks.tamper_frames = Some(hook),
    }
    local
}

async fn change_after_hash_is_retried_for(flow: Flow) {
    let h = Harness::new().await;
    seed_delta(&h, flow);
    let fired = Arc::new(AtomicU32::new(0));
    let hook = head_after_first_chunk(h.src(flow).to_path_buf(), fired.clone());
    let mut local = h.local_side();
    match flow {
        Flow::Push => local.hooks.on_first_chunk_sent = Some(hook),
        Flow::Pull => h.daemon.hooks.lock().unwrap().on_first_chunk_sent = Some(hook),
    }
    let out = h.run_side(&archive_opts(), flow, local).await.unwrap();
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "the hook did not fire once"
    );
    assert_eq!(out.exit_code(), 0, "{:?}", out.errors);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert!(
        fs::read(h.dst(flow).join("x.bin")).unwrap()
            == fs::read(h.src(flow).join("x.bin")).unwrap(),
        "the destination differs from the source"
    );
    assert!(!h.dst(flow).join(".irsync.x.bin.part").exists());
}

async fn change_after_hash_twice_fails_for(flow: Flow) {
    let h = Harness::new().await;
    let before = seed_delta(&h, flow);
    let asked = Arc::new(AtomicU32::new(0));
    let local = changing_before_may_place(&h, flow, asked.clone());
    let out = h.run_side(&archive_opts(), flow, local).await.unwrap();
    assert_eq!(out.exit_code(), 23, "{:?}", out.errors);
    assert_eq!(asked.load(Ordering::SeqCst), 2);
    assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
    let (path, msg) = &out.errors[0];
    assert_eq!(path.as_bytes(), b"x.bin");
    assert!(
        msg.contains("changed after it was hashed (after retry)"),
        "{msg}"
    );
    assert!(
        fs::read(h.dst(flow).join("x.bin")).unwrap() == before,
        "the destination changed"
    );
    assert!(!h.dst(flow).join(".irsync.x.bin.part").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn change_after_hash_is_retried() {
    within30(change_after_hash_is_retried_for(Flow::Push)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn change_after_hash_twice_fails_with_23() {
    within30(change_after_hash_twice_fails_for(Flow::Push)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn change_after_hash_is_retried_on_pull() {
    within30(change_after_hash_is_retried_for(Flow::Pull)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn change_after_hash_twice_fails_with_23_on_pull() {
    within30(change_after_hash_twice_fails_for(Flow::Pull)).await;
}

/// Asking before placing costs a round trip per file; with one file in
/// flight at a time nothing hides it, and the run must still finish.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ask_before_placing_with_one_file_in_flight() {
    within30(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        for i in 0..200u32 {
            fs::write(
                h.src(flow).join(format!("f{i:03}")),
                data(1000 + i as usize, i),
            )
            .unwrap();
        }
        h.daemon.hooks.lock().unwrap().max_inflight = Some(1);
        let opts = archive_opts();
        let out = h.run(&opts, flow).await.unwrap();
        assert_eq!(out.exit_code(), 0, "{:?}", out.errors);
        assert_eq!(out.files_transferred, 200);
        common::assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
    })
    .await;
}

/// The connection drops while a verified file waits for its verdict: the
/// resumed session places it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drop_while_waiting_for_verdict_resumes() {
    within30(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        let content = data(4 * MIB, 41);
        fs::write(h.src(flow).join("x.bin"), &content).unwrap();
        let fault = PartsFault::after(1);
        let mut local = h.local_side();
        local.hooks.drop_before_verdict = Some(fault.clone());
        let ropts = ReconnectOpts {
            backoff_min: Duration::from_millis(100),
            ..ReconnectOpts::default()
        };
        let (out, reconnects) = h.run_reconnect(&archive_opts(), flow, ropts, local).await;
        let out = out.unwrap();
        assert!(fault.fired(), "the fault never fired");
        assert_eq!(reconnects, 1);
        assert!(out.resumed, "the second connection did not resume");
        assert_eq!(out.exit_code(), 0, "{:?}", out.errors);
        assert_eq!(fs::read(h.dst(flow).join("x.bin")).unwrap(), content);
        assert!(!h.dst(flow).join(".irsync.x.bin.part").exists());
    })
    .await;
}

/// `--inplace` writes into the destination itself: a source that changes
/// after every hash is still retried once and given up, never reported
/// placed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inplace_change_after_hash_retries_then_fails() {
    within30(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        seed_delta(&h, flow);
        let asked = Arc::new(AtomicU32::new(0));
        let local = changing_before_may_place(&h, flow, asked.clone());
        let args =
            irsync::cli::Args::try_parse_from(["irsync", "-a", "--inplace", "src/", "host:m/"])
                .unwrap();
        let opts = irsync::cli::SyncOptions::from_args(&args);
        let out = h.run_side(&opts, flow, local).await.unwrap();
        assert_eq!(out.exit_code(), 23, "{:?}", out.errors);
        assert_eq!(asked.load(Ordering::SeqCst), 2);
        assert_eq!(out.files_transferred, 0);
        assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
        let (path, msg) = &out.errors[0];
        assert_eq!(path.as_bytes(), b"x.bin");
        assert!(
            msg.contains("changed after it was hashed (after retry)"),
            "{msg}"
        );
    })
    .await;
}

/// Plain `--append` (the tail goes to a `.part`, its prefix trusted): a
/// source that changes after every hash is retried once and given up, never
/// reported placed, and the destination keeps its old content.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn append_change_after_hash_retries_then_fails() {
    within30(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        let old = data(4 * MIB, 51);
        let dst = h.dst(flow).join("x.bin");
        fs::write(&dst, &old).unwrap();
        let mut new = old.clone();
        new.extend_from_slice(&data(MIB, 52));
        fs::write(h.src(flow).join("x.bin"), &new).unwrap();
        let asked = Arc::new(AtomicU32::new(0));
        let local = changing_before_may_place(&h, flow, asked.clone());
        let args =
            irsync::cli::Args::try_parse_from(["irsync", "-a", "--append", "src/", "host:m/"])
                .unwrap();
        let opts = irsync::cli::SyncOptions::from_args(&args);
        let out = h.run_side(&opts, flow, local).await.unwrap();
        assert_eq!(out.exit_code(), 23, "{:?}", out.errors);
        assert_eq!(asked.load(Ordering::SeqCst), 2);
        assert_eq!(out.files_transferred, 0);
        assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
        let (path, msg) = &out.errors[0];
        assert_eq!(path.as_bytes(), b"x.bin");
        assert!(
            msg.contains("changed after it was hashed (after retry)"),
            "{msg}"
        );
        assert!(fs::read(&dst).unwrap() == old, "the destination changed");
        assert!(!h.dst(flow).join(".irsync.x.bin.part").exists());
    })
    .await;
}

/// The file changes after the walk but before it is hashed. The retry
/// hashes it as it is now, and it is placed with that size and mtime, not
/// the walk's; a second run then has nothing to do.
#[tokio::test]
async fn retry_places_the_hashed_mtime() {
    within30(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        seed(&h, flow);
        let src = h.src(flow).join("x.bin");
        let new_mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1_500_000_000);
        let first = Arc::new(AtomicBool::new(false));
        let hook: HashHook = {
            let src = src.clone();
            Arc::new(move |p: &RelPath, bytes: u64| {
                if p.as_bytes() == b"x.bin" && bytes == 0 && !first.swap(true, Ordering::SeqCst) {
                    let f = fs::File::options().append(true).open(&src).unwrap();
                    (&f).write_all(&data(KIB, 77)).unwrap();
                    f.set_modified(new_mtime).unwrap();
                }
            })
        };
        let mut local = h.local_side();
        local.hooks = SideHooks {
            during_hash: Some(hook),
            ..SideHooks::default()
        };
        let out = h.run_side(&archive_opts(), flow, local).await.unwrap();
        assert_eq!(out.exit_code(), 0, "{:?}", out.errors);
        let dst = h.dst(flow).join("x.bin");
        let (s, d) = (fs::metadata(&src).unwrap(), fs::metadata(&dst).unwrap());
        assert_eq!(d.len(), SIZE as u64 + KIB as u64);
        assert_eq!(d.len(), s.len());
        assert_eq!(d.modified().unwrap(), s.modified().unwrap());
        assert_eq!(d.modified().unwrap(), new_mtime);
        assert!(fs::read(&dst).unwrap() == fs::read(&src).unwrap());
        let again = h.run(&archive_opts(), flow).await.unwrap();
        assert_eq!(again.exit_code(), 0, "{:?}", again.errors);
        assert_eq!(again.files_transferred, 0);
    })
    .await;
}

/// `--paranoid` hashes a sent file again before it is placed. The stat
/// compare is skipped (`skip_place_stat`), so only the second hash can see
/// the change: the receiver rewrites the source's first 256 KiB (a range
/// it copies from its own old copy) before every `MayPlace`. The tree is
/// under the system temporary folder, so no clones are made.
async fn paranoid_second_hash_for(flow: Flow) {
    let h = Harness::new().await;
    let before = seed_delta(&h, flow);
    let asked = Arc::new(AtomicU32::new(0));
    let mut local = changing_before_may_place(&h, flow, asked.clone());
    match flow {
        Flow::Push => local.hooks.skip_place_stat = true,
        Flow::Pull => h.daemon.hooks.lock().unwrap().skip_place_stat = true,
    }
    let args = irsync::cli::Args::try_parse_from(["irsync", "-a", "--paranoid", "src/", "host:m/"])
        .unwrap();
    let opts = irsync::cli::SyncOptions::from_args(&args);
    let out = h.run_side(&opts, flow, local).await.unwrap();
    assert_eq!(out.exit_code(), 23, "{:?}", out.errors);
    assert_eq!(asked.load(Ordering::SeqCst), 2);
    assert_eq!(out.files_transferred, 0);
    assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
    let (path, msg) = &out.errors[0];
    assert_eq!(path.as_bytes(), b"x.bin");
    assert!(
        msg.contains("changed after it was sent (after retry)"),
        "{msg}"
    );
    assert!(
        fs::read(h.dst(flow).join("x.bin")).unwrap() == before,
        "the destination changed"
    );
    assert!(!h.dst(flow).join(".irsync.x.bin.part").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paranoid_second_hash_catches_a_change_the_stat_misses() {
    within30(paranoid_second_hash_for(Flow::Push)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paranoid_reaches_the_daemon_on_pull() {
    within30(paranoid_second_hash_for(Flow::Pull)).await;
}

/// `--append --paranoid`: the sent digest of an `APPEND` tail has no
/// whole-file hash, so the second hash must compare its chunks. The
/// receiver rewrites bytes inside the source's tail (same size) before
/// every `MayPlace`, with the stat compare skipped.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paranoid_append_catches_a_same_size_tail_rewrite() {
    within30(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        let old = data(4 * MIB, 61);
        let dst = h.dst(flow).join("x.bin");
        fs::write(&dst, &old).unwrap();
        let mut new = old.clone();
        new.extend_from_slice(&data(MIB, 62));
        let src = h.src(flow).join("x.bin");
        fs::write(&src, &new).unwrap();
        let asked = Arc::new(AtomicU32::new(0));
        let hook: FrameHook = {
            let (src, asked) = (src.clone(), asked.clone());
            Arc::new(move |kind, m| {
                if kind == StreamKind::Control && matches!(m, Msg::MayPlace(_)) {
                    let n = asked.fetch_add(1, Ordering::SeqCst) + 1;
                    let f = fs::File::options().write(true).open(&src).unwrap();
                    f.write_all_at(&data(64 * KIB, 400 + n), (4 * MIB + 128 * KIB) as u64)
                        .unwrap();
                }
                vec![m]
            })
        };
        h.daemon.hooks.lock().unwrap().tamper_frames = Some(hook);
        let mut local = h.local_side();
        local.hooks.skip_place_stat = true;
        let args = irsync::cli::Args::try_parse_from([
            "irsync",
            "-a",
            "--append",
            "--paranoid",
            "src/",
            "host:m/",
        ])
        .unwrap();
        let opts = irsync::cli::SyncOptions::from_args(&args);
        let out = h.run_side(&opts, flow, local).await.unwrap();
        assert_eq!(out.exit_code(), 23, "{:?}", out.errors);
        assert_eq!(asked.load(Ordering::SeqCst), 2);
        assert_eq!(out.files_transferred, 0);
        assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
        let (path, msg) = &out.errors[0];
        assert_eq!(path.as_bytes(), b"x.bin");
        assert!(
            msg.contains("changed after it was sent (after retry)"),
            "{msg}"
        );
        assert!(fs::read(&dst).unwrap() == old, "the destination changed");
    })
    .await;
}

/// `--paranoid` on a tree nothing writes to: the second hash agrees with
/// the first for new files (small and multi-chunk), a delta (not with
/// `--append`) and a file the destination holds the start of, and
/// everything is placed.
async fn paranoid_unchanged_tree_for(flow: Flow, extra: &str) {
    let h = Harness::new().await;
    for i in 0..5u32 {
        fs::write(
            h.src(flow).join(format!("s{i}")),
            data(1000 + i as usize, i),
        )
        .unwrap();
    }
    fs::write(h.src(flow).join("big.bin"), data(SIZE, 70)).unwrap();
    // `--append` skips a destination that is not shorter: no delta then.
    if extra != "--append" {
        seed_delta(&h, flow);
    }
    let old = data(2 * MIB, 71);
    fs::write(h.dst(flow).join("tail.bin"), &old).unwrap();
    let mut new = old;
    new.extend_from_slice(&data(MIB, 72));
    fs::write(h.src(flow).join("tail.bin"), &new).unwrap();
    let mut argv = vec!["irsync", "-a", "--paranoid"];
    if !extra.is_empty() {
        argv.push(extra);
    }
    argv.extend(["src/", "host:m/"]);
    let opts =
        irsync::cli::SyncOptions::from_args(&irsync::cli::Args::try_parse_from(argv).unwrap());
    let out = h.run(&opts, flow).await.unwrap();
    assert_eq!(out.exit_code(), 0, "{:?}", out.errors);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    common::assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paranoid_unchanged_tree_places_everything() {
    within30(paranoid_unchanged_tree_for(Flow::Push, "")).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paranoid_unchanged_tree_places_everything_on_pull() {
    within30(paranoid_unchanged_tree_for(Flow::Pull, "")).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paranoid_unchanged_tree_with_append_places_everything() {
    within30(paranoid_unchanged_tree_for(Flow::Push, "--append")).await;
}

/// What happens to the source's `y.txt` once the first chunk of `x.bin`
/// has gone out.
#[derive(Clone, Copy)]
enum Late {
    Rewrite,
    Delete,
}

const Y_OLD: &[u8] = b"y as it was at the walk\n";
const Y_NEW: &[u8] = b"y as it is at the end of the run, longer than before\n";

/// `x.bin` is new (sent in several chunks) and `y.txt` is identical on both
/// sides (skipped at planning). After the first chunk of `x.bin` went out,
/// `late` happens to the source's `y.txt`, with a distinct mtime.
fn late_hook(src: PathBuf, late: Late, fired: Arc<AtomicU32>) -> FirstChunkHook {
    Arc::new(move |p: &RelPath| {
        let hit = p.as_bytes() == b"x.bin";
        let (y, fired) = (src.join("y.txt"), fired.clone());
        Box::pin(async move {
            if hit && fired.fetch_add(1, Ordering::SeqCst) == 0 {
                match late {
                    Late::Rewrite => {
                        fs::write(&y, Y_NEW).unwrap();
                        fs::File::options()
                            .write(true)
                            .open(&y)
                            .unwrap()
                            .set_modified(
                                SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_050),
                            )
                            .unwrap();
                    }
                    Late::Delete => fs::remove_file(&y).unwrap(),
                }
            }
        })
    })
}

/// Seeds `x.bin` (source only) and `y.txt` (both sides, same mtime) and
/// puts `late_hook` on the sending side. Returns the local side and the
/// hook's counter.
fn seed_late(h: &Harness, flow: Flow, late: Late) -> (irsync::session::LocalSide, Arc<AtomicU32>) {
    fs::write(h.src(flow).join("x.bin"), data(SIZE, 81)).unwrap();
    for dir in [h.src(flow), h.dst(flow)] {
        let y = dir.join("y.txt");
        fs::write(&y, Y_OLD).unwrap();
        fs::File::options()
            .write(true)
            .open(&y)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_005))
            .unwrap();
    }
    let fired = Arc::new(AtomicU32::new(0));
    let hook = late_hook(h.src(flow).to_path_buf(), late, fired.clone());
    let mut local = h.local_side();
    match flow {
        Flow::Push => local.hooks.on_first_chunk_sent = Some(hook),
        Flow::Pull => h.daemon.hooks.lock().unwrap().on_first_chunk_sent = Some(hook),
    }
    (local, fired)
}

fn opts_with(extra: &[&'static str]) -> irsync::cli::SyncOptions {
    let mut argv = vec!["irsync", "-a"];
    argv.extend_from_slice(extra);
    argv.extend(["src/", "host:m/"]);
    irsync::cli::SyncOptions::from_args(&irsync::cli::Args::try_parse_from(argv).unwrap())
}

async fn recheck_resends_for(flow: Flow) {
    let h = Harness::new().await;
    let (local, fired) = seed_late(&h, flow, Late::Rewrite);
    let out = h
        .run_side(&opts_with(&["--paranoid"]), flow, local)
        .await
        .unwrap();
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "the hook did not fire once"
    );
    assert_eq!(out.exit_code(), 0, "{:?}", out.errors);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert!(out.vanished.is_empty(), "{:?}", out.vanished);
    assert_eq!(fs::read(h.dst(flow).join("y.txt")).unwrap(), Y_NEW);
    common::assert_trees_equal(h.src(flow), h.dst(flow), &archive_opts().preserve);
}

/// `--paranoid` stats every listed file again at the end: `y.txt`, which
/// was up to date at the walk and changed during the run, is sent then.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paranoid_recheck_resends_a_file_changed_after_the_walk() {
    within30(recheck_resends_for(Flow::Push)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paranoid_recheck_resends_a_file_changed_after_the_walk_on_pull() {
    within30(recheck_resends_for(Flow::Pull)).await;
}

/// Without `--paranoid` there is no final re-check: the destination keeps
/// `y.txt` as it was at the walk, a state the source really held.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_recheck_without_paranoid() {
    within30(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        let (local, fired) = seed_late(&h, flow, Late::Rewrite);
        let out = h.run_side(&opts_with(&[]), flow, local).await.unwrap();
        assert_eq!(
            fired.load(Ordering::SeqCst),
            1,
            "the hook did not fire once"
        );
        assert_eq!(out.exit_code(), 0, "{:?}", out.errors);
        assert!(out.vanished.is_empty(), "{:?}", out.vanished);
        assert_eq!(fs::read(h.dst(flow).join("y.txt")).unwrap(), Y_OLD);
    })
    .await;
}

async fn recheck_vanished_for(flow: Flow) {
    let h = Harness::new().await;
    let (local, fired) = seed_late(&h, flow, Late::Delete);
    let out = h
        .run_side(&opts_with(&["--paranoid"]), flow, local)
        .await
        .unwrap();
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "the hook did not fire once"
    );
    assert_eq!(out.exit_code(), 24, "{:?} {:?}", out.errors, out.vanished);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(out.vanished, vec![RelPath::new(b"y.txt").unwrap()]);
    assert_eq!(fs::read(h.dst(flow).join("y.txt")).unwrap(), Y_OLD);
    assert_eq!(
        fs::read(h.dst(flow).join("x.bin")).unwrap(),
        fs::read(h.src(flow).join("x.bin")).unwrap()
    );
}

/// A listed file gone by the end of a `--paranoid` run: exit 24, and the
/// destination keeps its copy.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paranoid_recheck_vanished_file_exits_24() {
    within30(recheck_vanished_for(Flow::Push)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paranoid_recheck_vanished_file_exits_24_on_pull() {
    within30(recheck_vanished_for(Flow::Pull)).await;
}

/// `--paranoid` with the change checks off makes no clones and runs no
/// final re-check: a file deleted after the walk gives exit 0, not 24.
async fn paranoid_checks_off_for(flow: Flow) {
    let h = Harness::new().await;
    let (mut local, fired) = seed_late(&h, flow, Late::Delete);
    let base = tempfile::tempdir().unwrap();
    match flow {
        Flow::Push => {
            local.hooks.no_change_checks = true;
            local.hooks.clones_within = Some(base.path().to_path_buf());
        }
        Flow::Pull => {
            let mut d = h.daemon.hooks.lock().unwrap();
            d.no_change_checks = true;
            d.clones_within = Some(base.path().to_path_buf());
        }
    }
    let out = h
        .run_side(&opts_with(&["--paranoid"]), flow, local)
        .await
        .unwrap();
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "the hook did not fire once"
    );
    assert_eq!(out.exit_code(), 0, "{:?} {:?}", out.errors, out.vanished);
    assert!(out.vanished.is_empty(), "{:?}", out.vanished);
    let clones: Vec<_> = fs::read_dir(base.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(clones.is_empty(), "clone folders made: {clones:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paranoid_checks_off_skips_clone_and_recheck() {
    within30(paranoid_checks_off_for(Flow::Push)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paranoid_checks_off_skips_clone_and_recheck_on_pull() {
    within30(paranoid_checks_off_for(Flow::Pull)).await;
}

/// A vanished file and a file given up: 23 outranks 24.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vanished_and_failed_exits_23() {
    within30(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        let (local, fired) = seed_late(&h, flow, Late::Delete);
        // Before every `MayPlace` of `x.bin`, its head changes: given up.
        let asked = Arc::new(AtomicU32::new(0));
        let src = h.src(flow).join("x.bin");
        let hook: FrameHook = {
            let asked = asked.clone();
            Arc::new(move |kind, m| {
                if kind == StreamKind::Control && matches!(m, Msg::MayPlace(_)) {
                    let n = asked.fetch_add(1, Ordering::SeqCst) + 1;
                    rewrite_head(&src, 500 + n, 20 + n);
                }
                vec![m]
            })
        };
        h.daemon.hooks.lock().unwrap().tamper_frames = Some(hook);
        let out = h
            .run_side(&opts_with(&["--paranoid"]), flow, local)
            .await
            .unwrap();
        assert_eq!(
            fired.load(Ordering::SeqCst),
            1,
            "the hook did not fire once"
        );
        assert_eq!(out.exit_code(), 23, "{:?} {:?}", out.errors, out.vanished);
        assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
        assert_eq!(out.errors[0].0.as_bytes(), b"x.bin");
        assert_eq!(out.vanished, vec![RelPath::new(b"y.txt").unwrap()]);
    })
    .await;
}

/// A file sent again by the final re-check has had its retry: the
/// receiver rewrites the source's `y.txt` before every `MayPlace` (the
/// first, for `x.bin`, makes the re-check send `y.txt`; the second, for
/// `y.txt` itself, changes it again), and it is given up with exit 23.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paranoid_recheck_second_change_exits_23() {
    within30(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        let (local, _) = seed_late(&h, flow, Late::Rewrite);
        let asked = Arc::new(AtomicU32::new(0));
        let y = h.src(flow).join("y.txt");
        let hook: FrameHook = {
            let asked = asked.clone();
            Arc::new(move |kind, m| {
                if kind == StreamKind::Control && matches!(m, Msg::MayPlace(_)) {
                    let n = asked.fetch_add(1, Ordering::SeqCst) + 1;
                    fs::write(&y, format!("y, rewrite {n} before a MayPlace\n")).unwrap();
                    fs::File::options()
                        .write(true)
                        .open(&y)
                        .unwrap()
                        .set_modified(
                            SystemTime::UNIX_EPOCH
                                + Duration::from_secs(1_000_000_100 + u64::from(n)),
                        )
                        .unwrap();
                }
                vec![m]
            })
        };
        h.daemon.hooks.lock().unwrap().tamper_frames = Some(hook);
        // Only the frame hook changes `y.txt`.
        let mut local = local;
        local.hooks.on_first_chunk_sent = None;
        let out = h
            .run_side(&opts_with(&["--paranoid"]), flow, local)
            .await
            .unwrap();
        assert_eq!(out.exit_code(), 23, "{:?}", out.errors);
        assert_eq!(asked.load(Ordering::SeqCst), 2);
        assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
        let (path, msg) = &out.errors[0];
        assert_eq!(path.as_bytes(), b"y.txt");
        assert!(
            msg.contains("changed after it was hashed (after retry)"),
            "{msg}"
        );
        assert_eq!(fs::read(h.dst(flow).join("y.txt")).unwrap(), Y_OLD);
        assert!(out.vanished.is_empty(), "{:?}", out.vanished);
    })
    .await;
}

/// A `--paranoid` session that resumes from the receiver's stored plan
/// (the sender lists nothing) still settles: the connection drops while
/// a file waits for its verdict, and the next one places it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paranoid_resumed_session_settles() {
    within30(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        let content = data(4 * MIB, 91);
        fs::write(h.src(flow).join("x.bin"), &content).unwrap();
        let fault = PartsFault::after(1);
        let mut local = h.local_side();
        local.hooks.drop_before_verdict = Some(fault.clone());
        let ropts = ReconnectOpts {
            backoff_min: Duration::from_millis(100),
            ..ReconnectOpts::default()
        };
        let (out, reconnects) = h
            .run_reconnect(&opts_with(&["--paranoid"]), flow, ropts, local)
            .await;
        let out = out.unwrap();
        assert!(fault.fired(), "the fault never fired");
        assert_eq!(reconnects, 1);
        assert!(out.resumed, "the second connection did not resume");
        assert_eq!(out.exit_code(), 0, "{:?}", out.errors);
        assert_eq!(fs::read(h.dst(flow).join("x.bin")).unwrap(), content);
    })
    .await;
}

/// Counts the control frames of the kinds `count` names that a side sends.
fn frame_counter(count: fn(&Msg) -> bool, n: Arc<AtomicU32>) -> FrameHook {
    Arc::new(move |kind, m| {
        if kind == StreamKind::Control && count(&m) {
            n.fetch_add(1, Ordering::SeqCst);
        }
        vec![m]
    })
}

/// `-H --paranoid`: `m.txt` and `n.txt` are one file at both ends, up to
/// date, and kept at planning. During the run the source's file is
/// rewritten in place (both names): the re-check sends `m.txt` again, as a
/// new inode at the destination, and `n.txt` must be linked to it again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paranoid_recheck_relinks_hard_links() {
    use std::os::unix::fs::MetadataExt;
    within30(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        fs::write(h.src(flow).join("x.bin"), data(SIZE, 83)).unwrap();
        for dir in [h.src(flow), h.dst(flow)] {
            let m = dir.join("m.txt");
            fs::write(&m, Y_OLD).unwrap();
            fs::File::options()
                .write(true)
                .open(&m)
                .unwrap()
                .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_005))
                .unwrap();
            fs::hard_link(&m, dir.join("n.txt")).unwrap();
        }
        let fired = Arc::new(AtomicU32::new(0));
        let hook: FirstChunkHook = {
            let (m, fired) = (h.src(flow).join("m.txt"), fired.clone());
            Arc::new(move |p: &RelPath| {
                let hit = p.as_bytes() == b"x.bin";
                let (m, fired) = (m.clone(), fired.clone());
                Box::pin(async move {
                    if hit && fired.fetch_add(1, Ordering::SeqCst) == 0 {
                        // In place: both names see it.
                        fs::write(&m, Y_NEW).unwrap();
                        fs::File::options()
                            .write(true)
                            .open(&m)
                            .unwrap()
                            .set_modified(
                                SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_050),
                            )
                            .unwrap();
                    }
                })
            })
        };
        let mut local = h.local_side();
        local.hooks.on_first_chunk_sent = Some(hook);
        let out = h
            .run_side(&opts_with(&["-H", "--paranoid"]), flow, local)
            .await
            .unwrap();
        assert_eq!(
            fired.load(Ordering::SeqCst),
            1,
            "the hook did not fire once"
        );
        assert_eq!(out.exit_code(), 0, "{:?}", out.errors);
        let (m, n) = (h.dst(flow).join("m.txt"), h.dst(flow).join("n.txt"));
        assert_eq!(fs::read(&m).unwrap(), Y_NEW);
        assert_eq!(fs::read(&n).unwrap(), Y_NEW);
        assert_eq!(
            fs::metadata(&m).unwrap().ino(),
            fs::metadata(&n).unwrap().ino(),
            "the hard link is lost"
        );
    })
    .await;
}

/// `--paranoid` on a tree nothing writes to: the re-check finds nothing
/// to send again (no `Replan`) and settles once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paranoid_unchanged_tree_sends_no_replan() {
    within30(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        for i in 0..5u32 {
            fs::write(h.src(flow).join(format!("s{i}")), data(1000, i)).unwrap();
        }
        // Up to date already: listed, never planned, re-checked all the same.
        fs::write(h.src(flow).join("same"), Y_OLD).unwrap();
        fs::write(h.dst(flow).join("same"), Y_OLD).unwrap();
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_005);
        for dir in [h.src(flow), h.dst(flow)] {
            fs::File::options()
                .write(true)
                .open(dir.join("same"))
                .unwrap()
                .set_modified(t)
                .unwrap();
        }
        let (replans, settled) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)));
        let mut local = h.local_side();
        let (r, s) = (replans.clone(), settled.clone());
        local.hooks.tamper_frames = Some(Arc::new(move |kind, m| {
            if kind == StreamKind::Control {
                match m {
                    Msg::Replan(_) => r.fetch_add(1, Ordering::SeqCst),
                    Msg::Settled(_) => s.fetch_add(1, Ordering::SeqCst),
                    _ => 0,
                };
            }
            vec![m]
        }));
        let opts = opts_with(&["--paranoid"]);
        let out = h.run_side(&opts, flow, local).await.unwrap();
        assert_eq!(out.exit_code(), 0, "{:?}", out.errors);
        assert_eq!(replans.load(Ordering::SeqCst), 0);
        assert_eq!(settled.load(Ordering::SeqCst), 1);
        common::assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
    })
    .await;
}

/// `--paranoid --dry-run` changes nothing, so there is no final re-check:
/// the receiver never asks for one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paranoid_dry_run_sends_no_settle() {
    within30(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        fs::write(h.src(flow).join("a"), data(1000, 1)).unwrap();
        let settles = Arc::new(AtomicU32::new(0));
        h.daemon.hooks.lock().unwrap().tamper_frames = Some(frame_counter(
            |m| matches!(m, Msg::Settle(_)),
            settles.clone(),
        ));
        let out = h
            .run_side(
                &opts_with(&["--paranoid", "--dry-run"]),
                flow,
                h.local_side(),
            )
            .await
            .unwrap();
        assert_eq!(out.exit_code(), 0, "{:?}", out.errors);
        assert_eq!(settles.load(Ordering::SeqCst), 0);
        assert!(!h.dst(flow).join("a").exists());
    })
    .await;
}

/// `--paranoid --delete`: the deletions after the transfers wait for the
/// files the re-check sends again. `z.old` (destination only) still exists
/// when the re-sent `y.txt` asks to be placed, and is gone at the end.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paranoid_delete_runs_after_the_replanned_files() {
    within30(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        let (local, fired) = seed_late(&h, flow, Late::Rewrite);
        let z = h.dst(flow).join("z.old");
        fs::write(&z, b"extraneous").unwrap();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let hook: FrameHook = {
            let (z, seen) = (z.clone(), seen.clone());
            Arc::new(move |kind, m| {
                if kind == StreamKind::Control && matches!(m, Msg::MayPlace(_)) {
                    seen.lock().unwrap().push(z.exists());
                }
                vec![m]
            })
        };
        h.daemon.hooks.lock().unwrap().tamper_frames = Some(hook);
        let out = h
            .run_side(&opts_with(&["--paranoid", "--delete"]), flow, local)
            .await
            .unwrap();
        assert_eq!(
            fired.load(Ordering::SeqCst),
            1,
            "the hook did not fire once"
        );
        assert_eq!(out.exit_code(), 0, "{:?}", out.errors);
        // `x.bin`, then `y.txt` sent again.
        assert_eq!(*seen.lock().unwrap(), vec![true, true]);
        assert!(!z.exists(), "z.old was not deleted");
        assert_eq!(fs::read(h.dst(flow).join("y.txt")).unwrap(), Y_NEW);
    })
    .await;
}

/// How a hook changes `x.bin` between the walk and the hash pass.
#[derive(Clone, Copy)]
enum ModeChange {
    /// A new file with mode 600 renamed over it (a save-by-rename).
    RenameOver,
    /// `chmod 600`, content untouched.
    Chmod,
}

/// The source's mode changes after the walk and before the hash pass (on
/// the first pass only). The file is placed with the mode it had when it
/// was hashed, not the walk's: the source never held its new content (or
/// its current state) with the old mode.
async fn hashed_mode_is_placed_for(flow: Flow, change: ModeChange) {
    use std::os::unix::fs::PermissionsExt;
    let h = Harness::new().await;
    seed(&h, flow);
    let src = h.src(flow).join("x.bin");
    fs::set_permissions(&src, fs::Permissions::from_mode(0o644)).unwrap();
    let first = Arc::new(AtomicBool::new(false));
    let hook: HashHook = {
        let (src, first) = (src.clone(), first.clone());
        Arc::new(move |p: &RelPath, bytes: u64| {
            if p.as_bytes() != b"x.bin" || bytes != 0 || first.swap(true, Ordering::SeqCst) {
                return;
            }
            match change {
                ModeChange::RenameOver => {
                    let tmp = src.with_file_name("x.bin.new");
                    fs::write(&tmp, data(SIZE, 8)).unwrap();
                    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600)).unwrap();
                    fs::rename(&tmp, &src).unwrap();
                }
                ModeChange::Chmod => {
                    fs::set_permissions(&src, fs::Permissions::from_mode(0o600)).unwrap();
                }
            }
        })
    };
    let mut local = h.local_side();
    match flow {
        Flow::Push => {
            local.hooks = SideHooks {
                during_hash: Some(hook),
                ..SideHooks::default()
            }
        }
        Flow::Pull => h.daemon.hooks.lock().unwrap().during_hash = Some(hook),
    }
    let out = h.run_side(&archive_opts(), flow, local).await.unwrap();
    assert!(first.load(Ordering::SeqCst), "the hook never fired");
    assert_eq!(out.exit_code(), 0, "{:?}", out.errors);
    let dst = h.dst(flow).join("x.bin");
    assert!(fs::read(&dst).unwrap() == fs::read(&src).unwrap());
    let mode = fs::metadata(&dst).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode, 0o600, "placed with mode {mode:o}");
    let again = h.run(&archive_opts(), flow).await.unwrap();
    assert_eq!(again.exit_code(), 0, "{:?}", again.errors);
    assert_eq!(again.files_transferred, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rename_over_before_hash_places_the_new_mode() {
    within30(hashed_mode_is_placed_for(
        Flow::Push,
        ModeChange::RenameOver,
    ))
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rename_over_before_hash_places_the_new_mode_on_pull() {
    within30(hashed_mode_is_placed_for(
        Flow::Pull,
        ModeChange::RenameOver,
    ))
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chmod_before_hash_places_the_new_mode() {
    within30(hashed_mode_is_placed_for(Flow::Push, ModeChange::Chmod)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chmod_before_hash_places_the_new_mode_on_pull() {
    within30(hashed_mode_is_placed_for(Flow::Pull, ModeChange::Chmod)).await;
}

/// A group of ours other than the effective one, if any: an owner may move
/// a file to it without root.
fn other_group() -> Option<u32> {
    let egid = nix::unistd::getegid().as_raw();
    irsync::fs::meta::supplementary_groups()
        .into_iter()
        .find(|&g| g != egid)
}

/// The source's group changes after the walk and before the hash pass:
/// the file is placed with the group it had when it was hashed.
async fn hashed_group_is_placed_for(flow: Flow) {
    use std::os::unix::fs::MetadataExt;
    let Some(g2) = other_group() else {
        eprintln!("skipped: this user is in only one group");
        return;
    };
    let h = Harness::new().await;
    seed(&h, flow);
    let src = h.src(flow).join("x.bin");
    let first = Arc::new(AtomicBool::new(false));
    let hook: HashHook = {
        let (src, first) = (src.clone(), first.clone());
        Arc::new(move |p: &RelPath, bytes: u64| {
            if p.as_bytes() == b"x.bin" && bytes == 0 && !first.swap(true, Ordering::SeqCst) {
                std::os::unix::fs::chown(&src, None, Some(g2)).unwrap();
            }
        })
    };
    let mut local = h.local_side();
    match flow {
        Flow::Push => {
            local.hooks = SideHooks {
                during_hash: Some(hook),
                ..SideHooks::default()
            }
        }
        Flow::Pull => h.daemon.hooks.lock().unwrap().during_hash = Some(hook),
    }
    let out = h.run_side(&archive_opts(), flow, local).await.unwrap();
    assert!(first.load(Ordering::SeqCst), "the hook never fired");
    assert_eq!(out.exit_code(), 0, "{:?}", out.errors);
    let gid = fs::metadata(h.dst(flow).join("x.bin")).unwrap().gid();
    assert_eq!(gid, g2, "placed with the walk's group");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chgrp_before_hash_places_the_new_group() {
    within30(hashed_group_is_placed_for(Flow::Push)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chgrp_before_hash_places_the_new_group_on_pull() {
    within30(hashed_group_is_placed_for(Flow::Pull)).await;
}

/// What happens to `b.bin`, a hard link of `a.bin` when listed, before
/// `a.bin` is hashed (then `a.bin` is rewritten in place, which `b.bin`
/// no longer sees).
#[derive(Clone, Copy, Debug)]
enum LinkChange {
    /// Replaced by a file of its own.
    Replaced,
    /// Renamed away: gone.
    Gone,
}

/// `-H`: `b.bin` never held `a.bin`'s new content, so it must not be
/// linked to the copy of it. Replaced, it is sent as the file it now is,
/// in the same run; gone, it fails (exit 23). Either way the next run
/// leaves an exact copy.
async fn changed_link_is_not_linked_for(flow: Flow, change: LinkChange) {
    use std::os::unix::fs::MetadataExt;
    let h = Harness::new().await;
    let (a, b) = (h.src(flow).join("a.bin"), h.src(flow).join("b.bin"));
    fs::write(&a, data(SIZE, 21)).unwrap();
    fs::hard_link(&a, &b).unwrap();
    let first = Arc::new(AtomicBool::new(false));
    let hook: HashHook = {
        let (a, b, first) = (a.clone(), b.clone(), first.clone());
        Arc::new(move |p: &RelPath, bytes: u64| {
            if p.as_bytes() != b"a.bin" || bytes != 0 || first.swap(true, Ordering::SeqCst) {
                return;
            }
            match change {
                LinkChange::Replaced => {
                    let tmp = b.with_file_name("b.bin.new");
                    fs::write(&tmp, data(1000, 22)).unwrap();
                    fs::rename(&tmp, &b).unwrap();
                }
                LinkChange::Gone => fs::rename(&b, b.with_file_name("c.bin")).unwrap(),
            }
            // In place: only `a.bin` names this file now.
            fs::File::options()
                .write(true)
                .open(&a)
                .unwrap()
                .write_all_at(&data(TEAR, 23), 0)
                .unwrap();
        })
    };
    let mut local = h.local_side();
    match flow {
        Flow::Push => local.hooks.during_hash = Some(hook),
        Flow::Pull => h.daemon.hooks.lock().unwrap().during_hash = Some(hook),
    }
    let opts = opts_with(&["-H"]);
    let out = h.run_side(&opts, flow, local).await.unwrap();
    assert!(first.load(Ordering::SeqCst), "the hook never fired");
    let (da, db) = (h.dst(flow).join("a.bin"), h.dst(flow).join("b.bin"));
    assert!(fs::read(&da).unwrap() == fs::read(&a).unwrap());
    match change {
        LinkChange::Replaced => {
            assert_eq!(out.exit_code(), 0, "{:?}", out.errors);
            assert_eq!(fs::read(&db).unwrap(), data(1000, 22));
            assert_ne!(
                fs::metadata(&da).unwrap().ino(),
                fs::metadata(&db).unwrap().ino()
            );
        }
        LinkChange::Gone => {
            assert_eq!(out.exit_code(), 23, "{:?}", out.errors);
            assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
            let (p, m) = &out.errors[0];
            assert_eq!(p.as_bytes(), b"b.bin");
            assert!(m.contains("changed during the sync"), "{m}");
            assert!(!db.exists(), "b.bin was placed");
        }
    }
    let again = h.run(&opts, flow).await.unwrap();
    assert_eq!(again.exit_code(), 0, "{:?}", again.errors);
    assert!(fs::read(&da).unwrap() == fs::read(&a).unwrap());
    match change {
        LinkChange::Replaced => assert_eq!(fs::read(&db).unwrap(), data(1000, 22)),
        LinkChange::Gone => {
            assert!(!db.exists());
            assert!(fs::read(h.dst(flow).join("c.bin")).unwrap() == fs::read(&a).unwrap());
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replaced_link_is_sent_as_its_own_file() {
    within30(changed_link_is_not_linked_for(
        Flow::Push,
        LinkChange::Replaced,
    ))
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replaced_link_is_sent_as_its_own_file_on_pull() {
    within30(changed_link_is_not_linked_for(
        Flow::Pull,
        LinkChange::Replaced,
    ))
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn link_renamed_away_is_not_linked() {
    within30(changed_link_is_not_linked_for(Flow::Push, LinkChange::Gone)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn link_renamed_away_is_not_linked_on_pull() {
    within30(changed_link_is_not_linked_for(Flow::Pull, LinkChange::Gone)).await;
}

/// `-H` on a tree nothing writes to: every link is checked once (one
/// `CheckLink` each) and made.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unchanged_links_are_checked_and_made() {
    use std::os::unix::fs::MetadataExt;
    within30(async {
        let flow = Flow::Pull;
        let h = Harness::new().await;
        for i in 0..3u32 {
            let f = h.src(flow).join(format!("f{i}"));
            fs::write(&f, data(1000, i)).unwrap();
            fs::hard_link(&f, h.src(flow).join(format!("g{i}"))).unwrap();
            fs::hard_link(&f, h.src(flow).join(format!("h{i}"))).unwrap();
        }
        let checks = Arc::new(AtomicU32::new(0));
        let mut local = h.local_side();
        local.hooks.tamper_frames = Some(frame_counter(
            |m| matches!(m, Msg::CheckLink(_)),
            checks.clone(),
        ));
        let out = h.run_side(&opts_with(&["-H"]), flow, local).await.unwrap();
        assert_eq!(out.exit_code(), 0, "{:?}", out.errors);
        assert_eq!(checks.load(Ordering::SeqCst), 6);
        for i in 0..3u32 {
            let ino = |n: &str| {
                fs::metadata(h.dst(flow).join(format!("{n}{i}")))
                    .unwrap()
                    .ino()
            };
            assert_eq!(ino("f"), ino("g"));
            assert_eq!(ino("f"), ino("h"));
        }
    })
    .await;
}
