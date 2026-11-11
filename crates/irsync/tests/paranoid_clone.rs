//! `--paranoid` sends a reflink clone of each file where the filesystem
//! can make one: the clone is frozen at one moment the source really held,
//! so a source that changes afterwards still ends in a clean run. Clones
//! live in a hidden per-user folder (`.irsync-clones-<uid>`) that the walk
//! never lists, and are gone once the run ends.
//!
//! Every test bounds where clone folders may go (`clones_within`) to a
//! temporary folder of its own, so no test makes one anywhere else.

mod common;

use std::fs;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use common::{Flow, Harness};
use irsync::fs::paths::RelPath;
use irsync::hasher::HashHook;
use irsync::session::{EventLog, PartsFault};

const MIB: usize = 1024 * 1024;
const SIZE: usize = 8 * MIB;
const TEAR: usize = 256 * 1024;
/// The source's mtime when it is cloned.
const T1: u64 = 1_000_000_005;

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

fn set_mtime(p: &Path, secs: u64) {
    fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
        .unwrap();
}

fn paranoid_opts(extra: &[&str]) -> irsync::cli::SyncOptions {
    let mut argv = vec!["irsync"];
    argv.extend_from_slice(extra);
    argv.extend(["--paranoid", "src/", "host:m/"]);
    irsync::cli::SyncOptions::from_args(&irsync::cli::Args::try_parse_from(argv).unwrap())
}

fn uid() -> u32 {
    nix::unistd::getuid().as_raw()
}

/// The clone folder irsync makes in `base`.
fn clone_dir(base: &Path) -> PathBuf {
    base.join(format!(".irsync-clones-{}", uid()))
}

/// A temporary folder only we can write to: `tempfile` follows the umask,
/// and a group-writable folder is no place for clones (others could swap
/// the clone folder for a symlink).
fn private_tempdir_in(dir: &Path) -> tempfile::TempDir {
    let t = tempfile::tempdir_in(dir).unwrap();
    fs::set_permissions(
        t.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    t
}

/// Whether `dir`'s filesystem clones files.
fn reflinks(dir: &Path) -> bool {
    // Readable: the clone's source must be.
    fs::write(dir.join(".probe-a"), b"probe").unwrap();
    let a = fs::File::open(dir.join(".probe-a")).unwrap();
    let b = fs::File::create(dir.join(".probe-b")).unwrap();
    let ok = irsync::recv::reflink(&a, &b).is_ok();
    let _ = fs::remove_file(dir.join(".probe-a"));
    let _ = fs::remove_file(dir.join(".probe-b"));
    ok
}

/// A folder under `target/` (where the filesystem may clone), holding the
/// harness's trees; `None` (after a note) when it cannot clone.
async fn reflink_harness() -> Option<(tempfile::TempDir, Harness)> {
    let target = Path::new(env!("CARGO_TARGET_TMPDIR"));
    fs::create_dir_all(target).unwrap();
    let base = private_tempdir_in(target);
    if !reflinks(base.path()) {
        eprintln!("skipping: {} does not support reflinks", target.display());
        return None;
    }
    let h = Harness::new_in(base.path()).await;
    Some((base, h))
}

/// Sets the sending side's hooks: clones within `base`, `hook` during
/// every hash pass, `events` as its event log.
fn sending_side(
    h: &Harness,
    flow: Flow,
    base: &Path,
    hook: Option<HashHook>,
    events: &EventLog,
) -> irsync::session::LocalSide {
    let mut local = h.local_side();
    match flow {
        Flow::Push => {
            local.hooks.clones_within = Some(base.to_path_buf());
            local.hooks.during_hash = hook;
            local.hooks.events = Some(events.clone());
        }
        Flow::Pull => {
            let mut d = h.daemon.hooks.lock().unwrap();
            d.clones_within = Some(base.to_path_buf());
            d.during_hash = hook;
        }
    }
    local
}

/// The sending side's event log: the client's on a push, the daemon's on
/// a pull.
fn sender_events(h: &Harness, flow: Flow, local: &EventLog) -> Vec<String> {
    match flow {
        Flow::Push => local.lock().unwrap().clone(),
        Flow::Pull => h.daemon.events.lock().unwrap().clone(),
    }
}

/// What the rewriting hook saw.
#[derive(Default)]
struct Seen {
    /// Passes over `x.bin`.
    passes: AtomicU32,
    /// `x.bin` as it was before the latest rewrite (as its clone holds it).
    before: Mutex<Option<Vec<u8>>>,
    /// A session folder with a clone in it existed during a pass.
    clone_seen: AtomicBool,
}

/// A hook that, on every pass over `x.bin`, once 4 MiB are read, saves the
/// source's content, checks a clone exists in `base`, and rewrites the
/// source's first and last 256 KiB with a new mtime (1e9 + 100 + pass).
fn rewriting_hook(src: PathBuf, base: PathBuf, seen: Arc<Seen>) -> HashHook {
    let torn = Arc::new(AtomicBool::new(false));
    Arc::new(move |p: &RelPath, bytes: u64| {
        if p.as_bytes() != b"x.bin" {
            return;
        }
        if bytes == 0 {
            seen.passes.fetch_add(1, Ordering::SeqCst);
            torn.store(false, Ordering::SeqCst);
            return;
        }
        if bytes < (4 * MIB) as u64 || torn.swap(true, Ordering::SeqCst) {
            return;
        }
        let path = src.join("x.bin");
        *seen.before.lock().unwrap() = Some(fs::read(&path).unwrap());
        if any_clone_in(&base) {
            seen.clone_seen.store(true, Ordering::SeqCst);
        }
        let pass = seen.passes.load(Ordering::SeqCst);
        let f = fs::File::options().write(true).open(&path).unwrap();
        f.write_all_at(&data(TEAR, 100 + 2 * pass), 0).unwrap();
        f.write_all_at(&data(TEAR, 101 + 2 * pass), (SIZE - TEAR) as u64)
            .unwrap();
        drop(f);
        set_mtime(&path, 1_000_000_100 + u64::from(pass));
    })
}

/// Whether a session folder under `base`'s clone folder holds a file
/// other than its lock.
fn any_clone_in(base: &Path) -> bool {
    let Ok(sessions) = fs::read_dir(clone_dir(base)) else {
        return false;
    };
    sessions.flatten().any(|s| {
        fs::read_dir(s.path())
            .map(|d| d.flatten().any(|e| e.file_name() != "lock"))
            .unwrap_or(false)
    })
}

/// No clone folder (nor any session folder in it) is left under `base`.
fn assert_no_clones(base: &Path) {
    let dir = clone_dir(base);
    let left: Vec<_> = fs::read_dir(&dir)
        .map(|d| d.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "left in {}: {left:?}", dir.display());
    assert!(!dir.exists(), "{} is left", dir.display());
}

/// Source and destination both hold an older `x.bin`, so the file goes as
/// a delta; the source has mtime [`T1`].
fn seed(h: &Harness, flow: Flow) {
    let content = data(SIZE, 7);
    let src = h.src(flow).join("x.bin");
    fs::write(&src, &content).unwrap();
    set_mtime(&src, T1);
    let dst = h.dst(flow).join("x.bin");
    let mut old = content;
    old[MIB] ^= 0xFF;
    fs::write(&dst, &old).unwrap();
    set_mtime(&dst, 1_000_000_000);
}

/// A run in which the source of `x.bin` is rewritten while its clone is
/// hashed. Returns what the hook saw, the outcome, and the sending side's
/// events.
async fn run_rewritten(
    flow: Flow,
) -> Option<(
    tempfile::TempDir,
    Harness,
    Arc<Seen>,
    irsync::session::Outcome,
    Vec<String>,
)> {
    let (base, h) = reflink_harness().await?;
    seed(&h, flow);
    let seen = Arc::new(Seen::default());
    let hook = rewriting_hook(
        h.src(flow).to_path_buf(),
        base.path().to_path_buf(),
        seen.clone(),
    );
    let events = EventLog::default();
    let local = sending_side(&h, flow, base.path(), Some(hook), &events);
    let out = h
        .run_side(&paranoid_opts(&["-a"]), flow, local)
        .await
        .unwrap();
    let ev = sender_events(&h, flow, &events);
    Some((base, h, seen, out, ev))
}

async fn sends_a_clone_for(flow: Flow) {
    let Some((base, h, seen, out, events)) = run_rewritten(flow).await else {
        return;
    };
    assert_eq!(out.exit_code(), 0, "{:?}", out.errors);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert!(
        seen.clone_seen.load(Ordering::SeqCst),
        "no clone existed while the file was hashed"
    );
    // Each clone is hashed once; no second hash of the source. The final
    // re-check finds the source changed since the first clone and sends it
    // again, from a second clone, which is accepted though the source
    // changes again.
    assert_eq!(seen.passes.load(Ordering::SeqCst), 2);
    let before = seen.before.lock().unwrap().clone().expect("hook ran");
    assert!(
        fs::read(h.dst(flow).join("x.bin")).unwrap() == before,
        "the destination is not the source as it was last cloned"
    );
    assert!(
        events.iter().any(|e| e == "clone-outdated:x.bin"),
        "{events:?}"
    );
    assert_no_clones(base.path());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paranoid_sends_a_clone_even_if_the_source_changes() {
    within30(sends_a_clone_for(Flow::Push)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paranoid_sends_a_clone_on_pull() {
    within30(sends_a_clone_for(Flow::Pull)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paranoid_places_the_clone_time_stat() {
    within30(async {
        let flow = Flow::Push;
        let Some((_base, h, _seen, out, _)) = run_rewritten(flow).await else {
            return;
        };
        assert_eq!(out.exit_code(), 0, "{:?}", out.errors);
        let src = fs::metadata(h.src(flow).join("x.bin")).unwrap();
        assert_ne!(
            src.mtime() as u64,
            T1,
            "the hook did not rewrite the source"
        );
        // Placed with the stat of the last clone (sent again by the final
        // re-check), taken after the first rewrite and before the second.
        let dst = fs::metadata(h.dst(flow).join("x.bin")).unwrap();
        assert_eq!((dst.mtime() as u64, dst.mtime_nsec()), (1_000_000_101, 0));
        assert_eq!(src.mtime() as u64, 1_000_000_102);
        assert_eq!(dst.len(), SIZE as u64);
    })
    .await;
}

/// A hook that only notes whether a clone existed during a pass.
fn noting_hook(base: PathBuf, seen: Arc<AtomicBool>) -> HashHook {
    Arc::new(move |_: &RelPath, bytes: u64| {
        if bytes > 0 && any_clone_in(&base) {
            seen.store(true, Ordering::SeqCst);
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clones_are_removed_after_the_run() {
    within30(async {
        let flow = Flow::Push;
        let Some((base, h)) = reflink_harness().await else {
            return;
        };
        // Success.
        for i in 0..4u32 {
            fs::write(h.src(flow).join(format!("f{i}")), data(100_000, i)).unwrap();
        }
        let seen = Arc::new(AtomicBool::new(false));
        let hook = noting_hook(base.path().to_path_buf(), seen.clone());
        let events = EventLog::default();
        let local = sending_side(&h, flow, base.path(), Some(hook.clone()), &events);
        let opts = paranoid_opts(&["-a"]);
        let out = h.run_side(&opts, flow, local).await.unwrap();
        assert_eq!(out.exit_code(), 0, "{:?}", out.errors);
        assert!(seen.load(Ordering::SeqCst), "no clone was made");
        common::assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
        assert_no_clones(base.path());

        // A failed file: its destination folder cannot be written (`-r`
        // only, so the folder's mode is left alone).
        fs::create_dir(h.src(flow).join("ro")).unwrap();
        fs::write(h.src(flow).join("ro/g"), data(100_000, 9)).unwrap();
        fs::create_dir(h.dst(flow).join("ro")).unwrap();
        let ro = h.dst(flow).join("ro");
        fs::set_permissions(&ro, std::os::unix::fs::PermissionsExt::from_mode(0o555)).unwrap();
        seen.store(false, Ordering::SeqCst);
        let local = sending_side(&h, flow, base.path(), Some(hook), &events);
        let out = h
            .run_side(&paranoid_opts(&["-r"]), flow, local)
            .await
            .unwrap();
        fs::set_permissions(&ro, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        assert_eq!(out.exit_code(), 23, "{:?}", out.errors);
        assert!(seen.load(Ordering::SeqCst), "no clone was made");
        assert!(!h.dst(flow).join("ro/g").exists());
        assert_no_clones(base.path());
    })
    .await;
}

/// An interrupted run (the connection drops while a file waits for its
/// verdict) leaves no clone either.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clones_are_removed_after_an_interrupted_run() {
    within30(async {
        let flow = Flow::Push;
        let Some((base, h)) = reflink_harness().await else {
            return;
        };
        fs::write(h.src(flow).join("f"), data(100_000, 3)).unwrap();
        let seen = Arc::new(AtomicBool::new(false));
        let hook = noting_hook(base.path().to_path_buf(), seen.clone());
        let events = EventLog::default();
        let mut local = sending_side(&h, flow, base.path(), Some(hook), &events);
        local.hooks.drop_before_verdict = Some(PartsFault::after(1));
        let res = h.run_side(&paranoid_opts(&["-a"]), flow, local).await;
        assert!(res.is_err(), "the run was not interrupted: {res:?}");
        assert!(seen.load(Ordering::SeqCst), "no clone was made");
        assert_no_clones(base.path());
    })
    .await;
}

/// Where the filesystem cannot clone (the system temporary folder, usually
/// tmpfs), `--paranoid` falls back to hashing each file again before it is
/// placed, quietly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paranoid_without_reflink_falls_back() {
    within30(async {
        let flow = Flow::Push;
        let base = private_tempdir_in(&std::env::temp_dir());
        if reflinks(base.path()) {
            eprintln!(
                "skipping: {} supports reflinks",
                std::env::temp_dir().display()
            );
            return;
        }
        let h = Harness::new_in(base.path()).await;
        seed(&h, flow);
        let passes = Arc::new(AtomicU32::new(0));
        let hook: HashHook = {
            let passes = passes.clone();
            Arc::new(move |p: &RelPath, bytes: u64| {
                if p.as_bytes() == b"x.bin" && bytes == 0 {
                    passes.fetch_add(1, Ordering::SeqCst);
                }
            })
        };
        let events = EventLog::default();
        let local = sending_side(&h, flow, base.path(), Some(hook), &events);
        let opts = paranoid_opts(&["-a"]);
        let out = h.run_side(&opts, flow, local).await.unwrap();
        assert_eq!(out.exit_code(), 0, "{:?}", out.errors);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        common::assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
        // The first hash, then the second one before placing.
        assert_eq!(passes.load(Ordering::SeqCst), 2);
        assert_no_clones(base.path());
    })
    .await;
}

/// A clone folder inside the source tree is never listed, and one inside
/// the destination is never deleted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn walk_skips_clone_folders() {
    within30(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        let src = h.src(flow);
        fs::create_dir_all(src.join(".irsync-clones-1000/x")).unwrap();
        fs::write(src.join(".irsync-clones-1000/x/y"), b"clone").unwrap();
        fs::create_dir_all(src.join("d/.irsync-clones-77")).unwrap();
        fs::write(src.join("d/.irsync-clones-77/z"), b"clone").unwrap();
        fs::write(src.join("d/real"), b"real").unwrap();
        let dst = h.dst(flow);
        fs::create_dir_all(dst.join(".irsync-clones-1000")).unwrap();
        fs::write(dst.join(".irsync-clones-1000/keep"), b"keep").unwrap();
        let args =
            irsync::cli::Args::try_parse_from(["irsync", "-a", "--delete", "src/", "host:m/"])
                .unwrap();
        let opts = irsync::cli::SyncOptions::from_args(&args);
        let out = h.run(&opts, flow).await.unwrap();
        assert_eq!(out.exit_code(), 0, "{:?}", out.errors);
        assert_eq!(fs::read(dst.join("d/real")).unwrap(), b"real");
        assert!(!dst.join(".irsync-clones-1000/x").exists());
        assert!(!dst.join("d/.irsync-clones-77").exists());
        assert_eq!(
            fs::read(dst.join(".irsync-clones-1000/keep")).unwrap(),
            b"keep"
        );
        assert!(out.deleted.is_empty(), "{:?}", out.deleted);
    })
    .await;
}
