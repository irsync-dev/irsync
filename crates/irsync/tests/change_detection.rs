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
) -> irsync::session::Outcome {
    let hook = tearing_hook(h.src(flow).to_path_buf(), every_pass, passes);
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
    h.run_side(&archive_opts(), flow, local).await.unwrap()
}

async fn torn_hash_is_retried_for(flow: Flow) {
    let h = Harness::new().await;
    seed(&h, flow);
    let passes = Arc::new(AtomicU32::new(0));
    let out = run_torn(&h, flow, false, passes.clone()).await;
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
    let out = run_torn(&h, flow, true, passes.clone()).await;
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
