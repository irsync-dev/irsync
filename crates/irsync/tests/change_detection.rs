//! Source files that change while the sender works on them: a change during
//! the hash pass is caught, retried once, and given up with exit 23 if it
//! happens again.

mod common;

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use common::{archive_opts, Flow, Harness};
use irsync::fs::paths::RelPath;
use irsync::hasher::HashHook;
use irsync::session::SideHooks;

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

/// Writes other data over the first and the last 256 KiB of `path`.
fn tear(path: &Path, seed: u32) {
    let f = fs::File::options().write(true).open(path).unwrap();
    f.write_all_at(&data(TEAR, seed), 0).unwrap();
    f.write_all_at(&data(TEAR, seed + 1), (SIZE - TEAR) as u64)
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
            tear(&src.join("x.bin"), 100 + 2 * pass);
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
    let out = run_torn(&h, flow, true, passes).await;
    assert_eq!(out.exit_code(), 23, "{:?}", out.errors);
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
