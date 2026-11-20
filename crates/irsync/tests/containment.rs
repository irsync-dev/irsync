//! A hostile sender must not make the receiver write, link or chmod outside
//! its root: listings naming irsync's own temp files and hard links whose
//! targets were not received as regular files are refused. And a sender
//! reads only files below its root, whatever happens to the folders on the
//! way while it works.

mod common;

use std::fs;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::{archive_opts, assert_trees_equal, Flow, Harness};
use irsync::fs::paths::RelPath;
use irsync::session::reconnect::ReconnectOpts;
use irsync_proto::v1::{file_entry::Kind, FileEntry};

async fn within30<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(30), f)
        .await
        .expect("test timed out after 30 s")
}

fn entry(path: &[u8], kind: Kind, size: u64, link_target: &[u8]) -> FileEntry {
    let mode = match kind {
        Kind::Symlink => 0o120777,
        Kind::Directory => 0o040755,
        _ => 0o100644,
    };
    FileEntry {
        path: path.to_vec(),
        kind: kind as i32,
        size,
        mtime_sec: 1_600_000_000,
        mode,
        nlink: 2,
        link_target: link_target.to_vec(),
        ..FileEntry::default()
    }
}

/// Pushes the local tree with `inject` adding entries after the entry at `after`.
async fn push_with(
    h: &Harness,
    after: &'static [u8],
    extra: Vec<FileEntry>,
) -> irsync::session::Outcome {
    let mut local = h.local_side();
    local.hooks.inject_listing = Some(Arc::new(move |e: &FileEntry| {
        if e.path == after {
            extra.clone()
        } else {
            Vec::new()
        }
    }));
    within30(h.run_side(&archive_opts(), Flow::Push, local))
        .await
        .expect("session")
}

fn has_error(out: &irsync::session::Outcome, path: &[u8], needle: &str) -> bool {
    out.errors
        .iter()
        .any(|(p, m)| p.as_bytes() == path && m.contains(needle))
}

#[tokio::test]
async fn peer_listing_temp_name_is_rejected() {
    let h = Harness::new().await;
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("victim");
    fs::write(&victim, b"keep me").unwrap();
    fs::write(h.local.path().join("x"), b"real content").unwrap();
    // A symlink named like the receiver's `.part` for `x`, listed before
    // `x`: if accepted, the writer for `x` would write through it.
    let target = victim.as_os_str().as_encoded_bytes().to_vec();
    let out = push_with(
        &h,
        b"",
        vec![
            entry(b".irsync.x.part", Kind::Symlink, 0, &target),
            entry(b".irsync.y.tmp", Kind::Regular, 3, b""),
        ],
    )
    .await;
    assert!(
        has_error(&out, b".irsync.x.part", "reserved"),
        "{:?}",
        out.errors
    );
    assert!(has_error(&out, b".irsync.y.tmp", "reserved"));
    assert!(fs::symlink_metadata(h.export.path().join(".irsync.x.part")).is_err());
    assert!(fs::symlink_metadata(h.export.path().join(".irsync.y.tmp")).is_err());
    assert_eq!(
        fs::read(h.export.path().join("x")).unwrap(),
        b"real content"
    );
    assert_eq!(fs::read(&victim).unwrap(), b"keep me");
    assert_eq!(out.exit_code(), 23);
}

#[tokio::test]
async fn hardlink_target_outside_root_rejected() {
    let h = Harness::new().await;
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret"), b"private key").unwrap();
    // A symlink inside the export leading out of it.
    symlink(outside.path(), h.export.path().join("esc")).unwrap();
    fs::write(h.local.path().join("a"), b"a").unwrap();
    let out = push_with(
        &h,
        b"a",
        vec![entry(b"b", Kind::Hardlink, 11, b"esc/secret")],
    )
    .await;
    assert!(has_error(&out, b"b", "hard link"), "{:?}", out.errors);
    assert!(fs::symlink_metadata(h.export.path().join("b")).is_err());
    assert_eq!(
        fs::metadata(outside.path().join("secret")).unwrap().nlink(),
        1
    );
}

#[tokio::test]
async fn hardlink_to_failed_target_rejected() {
    let h = Harness::new().await;
    // Stale content at the target path on the receiver.
    fs::create_dir(h.export.path().join("zz")).unwrap();
    fs::write(h.export.path().join("zz/t"), b"stale").unwrap();
    fs::write(h.local.path().join("a"), b"a").unwrap();
    // `zz/t` is listed without its parent directory (a planner error), then
    // a link to it.
    let out = push_with(
        &h,
        b"a",
        vec![
            entry(b"zz/t", Kind::Regular, 5, b""),
            entry(b"zzz", Kind::Hardlink, 5, b"zz/t"),
        ],
    )
    .await;
    assert!(has_error(&out, b"zz/t", "parent"), "{:?}", out.errors);
    assert!(has_error(&out, b"zzz", "hard link"), "{:?}", out.errors);
    assert!(fs::symlink_metadata(h.export.path().join("zzz")).is_err());
    assert_eq!(fs::read(h.export.path().join("zz/t")).unwrap(), b"stale");
}

fn small_tree(dir: &std::path::Path) {
    fs::create_dir_all(dir.join("a/b")).unwrap();
    fs::write(dir.join("top"), b"top").unwrap();
    fs::write(dir.join("a/one"), b"one").unwrap();
    fs::write(dir.join("a/b/two"), b"two").unwrap();
    symlink("one", dir.join("a/ln")).unwrap();
}

#[tokio::test]
async fn a_root_given_through_a_symlink_works_both_ways() {
    // The export path and the local folder are both symlinks to the real
    // folders: only what is below a root is guarded, not the way to it.
    let real = tempfile::tempdir().unwrap();
    let links = tempfile::tempdir().unwrap();
    let (export, src, dst) = (
        real.path().join("export"),
        real.path().join("src"),
        real.path().join("dst"),
    );
    for d in [&export, &src, &dst] {
        fs::create_dir(d).unwrap();
    }
    for (name, to) in [("export", &export), ("src", &src), ("dst", &dst)] {
        symlink(to, links.path().join(name)).unwrap();
    }
    small_tree(&src);
    let export_link = links.path().join("export");
    let h = Harness::with_export(|e| e.path = export_link).await;
    let opts = archive_opts();

    let push = Harness::side_for(&links.path().join("src"), h.client_state.path());
    let out = within30(h.run_side(&opts, Flow::Push, push))
        .await
        .expect("push");
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_trees_equal(&src, &export, &opts.preserve);

    let pull = Harness::side_for(&links.path().join("dst"), h.client_state.path());
    let out = within30(h.run_side(&opts, Flow::Pull, pull))
        .await
        .expect("pull");
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_trees_equal(&export, &dst, &opts.preserve);
}

#[tokio::test]
async fn a_wide_tree_finishes_under_a_low_handle_limit() {
    // Far more folders than the folder cache holds, with a small window of
    // files in flight: folders are opened again as needed, never all held.
    let h = Harness::new().await;
    for i in 0..300 {
        let d = h.local.path().join(format!("d{i:03}"));
        fs::create_dir(&d).unwrap();
        for j in 0..3 {
            fs::write(d.join(format!("f{j}")), format!("{i}/{j}")).unwrap();
        }
    }
    {
        let mut hooks = h.daemon.hooks.lock().unwrap();
        hooks.max_inflight = Some(8);
        hooks.dir_cache = Some(4);
    }
    let side = || {
        let mut local = h.local_side();
        local.hooks.max_inflight = Some(8);
        local.hooks.dir_cache = Some(4);
        local
    };
    let opts = archive_opts();
    let no_emfile = |out: &irsync::session::Outcome| {
        assert!(
            !out.errors
                .iter()
                .any(|(_, m)| m.contains("Too many open files")),
            "{:?}",
            out.errors
        );
        assert!(out.errors.is_empty(), "{:?}", out.errors);
    };
    let out = within30(h.run_side(&opts, Flow::Push, side()))
        .await
        .expect("push");
    no_emfile(&out);
    assert_trees_equal(h.local.path(), h.export.path(), &opts.preserve);

    // And back into an empty folder.
    fs::remove_dir_all(h.local.path()).unwrap();
    fs::create_dir(h.local.path()).unwrap();
    let out = within30(h.run_side(&opts, Flow::Pull, side()))
        .await
        .expect("pull");
    no_emfile(&out);
    assert_trees_equal(h.export.path(), h.local.path(), &opts.preserve);
}

/// The file outside the export a swapped folder would lead to.
const SECRET: &[u8] = b"private key";
/// `d/secret` inside the export: the same size as [`SECRET`].
const INSIDE: &[u8] = b"public data";

/// Makes `outside/secret` and the export's `d/secret`, with other
/// content of the same size and the same mtime, so only the content tells
/// them apart. Returns the outside folder.
fn plant_secret(export: &Path) -> tempfile::TempDir {
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret"), SECRET).unwrap();
    fs::create_dir(export.join("d")).unwrap();
    fs::write(export.join("d/secret"), INSIDE).unwrap();
    let mtime = fs::metadata(outside.path().join("secret"))
        .unwrap()
        .modified()
        .unwrap();
    fs::File::options()
        .write(true)
        .open(export.join("d/secret"))
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    outside
}

/// Moves the export's `d` aside and puts a symlink to `outside` in its
/// place.
fn swap_d(export: &Path, outside: &Path) {
    fs::rename(export.join("d"), export.join("d.real")).unwrap();
    symlink(outside, export.join("d")).unwrap();
}

/// A hook that swaps `d` (once) when the event `what` is recorded for
/// `at`; `fired` says it did.
fn swap_on(
    what: &'static str,
    at: &'static [u8],
    export: &Path,
    outside: &Path,
    fired: &Arc<AtomicBool>,
) -> irsync::session::EventHook {
    let (export, outside, fired) = (export.to_path_buf(), outside.to_path_buf(), fired.clone());
    Arc::new(move |event: &str, path: &RelPath| {
        if event == what && path.as_bytes() == at && !fired.swap(true, Ordering::SeqCst) {
            swap_d(&export, &outside);
        }
    })
}

/// Nothing from outside arrived: the client's `d/secret` is missing or
/// holds the inside content, and the file failed.
fn assert_nothing_from_outside(local: &Path, out: &irsync::session::Outcome) {
    if let Ok(got) = fs::read(local.join("d/secret")) {
        assert_eq!(got, INSIDE, "content from outside the export arrived");
    }
    assert!(
        out.errors.iter().any(|(p, _)| p.as_bytes() == b"d/secret"),
        "{:?}",
        out.errors
    );
    assert_eq!(out.exit_code(), 23);
}

/// Pulls with `hook` on the daemon and the given folder cache.
async fn pull_with_hook(
    h: &Harness,
    hook: irsync::session::EventHook,
    dir_cache: Option<usize>,
) -> irsync::session::Outcome {
    {
        let mut hooks = h.daemon.hooks.lock().unwrap();
        hooks.on_event = Some(hook);
        hooks.dir_cache = dir_cache;
    }
    within30(h.run_side(&archive_opts(), Flow::Pull, h.local_side()))
        .await
        .expect("pull")
}

#[tokio::test]
async fn pull_swap_before_hashing_sends_nothing_from_outside() {
    let h = Harness::new().await;
    let outside = plant_secret(h.export.path());
    let fired = Arc::new(AtomicBool::new(false));
    let hook = swap_on(
        "hash-open",
        b"d/secret",
        h.export.path(),
        outside.path(),
        &fired,
    );
    let out = pull_with_hook(&h, hook, None).await;
    assert!(fired.load(Ordering::SeqCst), "the hook never fired");
    assert_nothing_from_outside(h.local.path(), &out);
}

#[tokio::test]
async fn pull_swap_before_sending_sends_nothing_from_outside() {
    let h = Harness::new().await;
    let outside = plant_secret(h.export.path());
    let fired = Arc::new(AtomicBool::new(false));
    let hook = swap_on(
        "send-open",
        b"d/secret",
        h.export.path(),
        outside.path(),
        &fired,
    );
    // No folder cache: the hashing would otherwise leave `d` open, and the
    // send would read the real folder through that handle (see the next
    // test). Without it, the send walks from the root again.
    let out = pull_with_hook(&h, hook, Some(0)).await;
    assert!(fired.load(Ordering::SeqCst), "the hook never fired");
    assert_nothing_from_outside(h.local.path(), &out);
}

#[tokio::test]
async fn pull_swap_before_sending_reads_the_folder_already_open() {
    let h = Harness::new().await;
    let outside = plant_secret(h.export.path());
    let fired = Arc::new(AtomicBool::new(false));
    let hook = swap_on(
        "send-open",
        b"d/secret",
        h.export.path(),
        outside.path(),
        &fired,
    );
    let out = pull_with_hook(&h, hook, None).await;
    assert!(fired.load(Ordering::SeqCst), "the hook never fired");
    // The folder was opened to hash the file: the send goes on through
    // that handle, which still is the real folder, now `d.real`.
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(fs::read(h.local.path().join("d/secret")).unwrap(), INSIDE);
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
async fn resumed_pull_refuses_a_symlinked_parent() {
    let h = Harness::new().await;
    let outside = plant_secret(h.export.path());
    // The first run drops the connection as soon as `d/secret` is hashed,
    // so the client keeps a plan that names it.
    let fault = h.daemon.fault.clone();
    h.daemon.hooks.lock().unwrap().on_event = Some(Arc::new(move |event: &str, path: &RelPath| {
        if event == "hash-open" && path.as_bytes() == b"d/secret" {
            fault.drop_after_bytes(0);
        }
    }));
    let opts = archive_opts();
    let (first, _) =
        within30(h.run_reconnect(&opts, Flow::Pull, no_reconnect(), h.local_side())).await;
    assert!(first.is_err(), "the first run should fail");
    h.daemon.hooks.lock().unwrap().on_event = None;
    swap_d(h.export.path(), outside.path());
    let (second, _) = within30(h.run_reconnect(&opts, Flow::Pull, fast(), h.local_side())).await;
    let second = second.unwrap_or_else(|e| panic!("{e:#}"));
    assert!(second.resumed, "the second run did not resume");
    assert_nothing_from_outside(h.local.path(), &second);
}

#[tokio::test]
async fn each_session_opens_the_local_folder_anew() {
    // Sessions over one client side (as reconnect attempts are) do not
    // keep the folder of an earlier one: removed and made again, the
    // folder at the path is the one read.
    let h = Harness::new().await;
    fs::write(h.local.path().join("a"), b"first").unwrap();
    let side = h.local_side();
    let opts = archive_opts();
    let out = within30(h.run_side(&opts, Flow::Push, side.clone()))
        .await
        .expect("first push");
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    fs::remove_dir_all(h.local.path()).unwrap();
    fs::create_dir(h.local.path()).unwrap();
    fs::write(h.local.path().join("b"), b"second").unwrap();
    let out = within30(h.run_side(&opts, Flow::Push, side))
        .await
        .expect("second push");
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(fs::read(h.export.path().join("b")).unwrap(), b"second");
}

/// An outside folder holding `keep` (`keep me`, mode 0o600), where a
/// swapped folder would lead.
fn canary() -> tempfile::TempDir {
    let outside = tempfile::tempdir().unwrap();
    let keep = outside.path().join("keep");
    fs::write(&keep, b"keep me").unwrap();
    fs::set_permissions(&keep, fs::Permissions::from_mode(0o600)).unwrap();
    outside
}

/// The canary is as it was: only `keep`, with its content and mode.
fn assert_canary(outside: &Path) {
    let names: Vec<_> = fs::read_dir(outside)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["keep"], "something appeared outside the export");
    let keep = outside.join("keep");
    assert_eq!(fs::read(&keep).unwrap(), b"keep me");
    assert_eq!(fs::metadata(&keep).unwrap().mode() & 0o7777, 0o600);
}

/// Pushes with `hook` on the daemon.
async fn push_with_hook(
    h: &Harness,
    hook: Option<irsync::session::EventHook>,
    opts: &irsync::cli::SyncOptions,
) -> irsync::session::Outcome {
    h.daemon.hooks.lock().unwrap().on_event = hook;
    within30(h.run_side(opts, Flow::Push, h.local_side()))
        .await
        .expect("push")
}

/// `n` bytes that do not repeat, so chunk boundaries fall by content.
fn noise(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// Some bytes to push, `n` long.
fn bytes(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| (i as u8).wrapping_mul(31) ^ seed).collect()
}

/// The pushed `d/x` either failed or went into the real folder, now
/// `d.real`.
fn x_failed_or_inside(h: &Harness, out: &irsync::session::Outcome, want: &[u8]) {
    let inside = fs::read(h.export.path().join("d.real/x")).ok();
    assert!(
        inside.as_deref() == Some(want) || out.errors.iter().any(|(p, _)| p.as_bytes() == b"d/x"),
        "{:?}",
        out.errors
    );
}

#[tokio::test]
async fn push_swap_before_the_first_write_stays_inside() {
    let h = Harness::new().await;
    let outside = canary();
    let data = bytes(200_000, 1);
    fs::create_dir(h.local.path().join("d")).unwrap();
    fs::write(h.local.path().join("d/x"), &data).unwrap();
    let fired = Arc::new(AtomicBool::new(false));
    let hook = swap_on("part-open", b"d/x", h.export.path(), outside.path(), &fired);
    let out = push_with_hook(&h, Some(hook), &archive_opts()).await;
    assert!(fired.load(Ordering::SeqCst), "the hook never fired");
    assert_canary(outside.path());
    x_failed_or_inside(&h, &out, &data);
}

#[tokio::test]
async fn push_swap_before_placing_stays_inside() {
    let h = Harness::new().await;
    let outside = canary();
    let data = bytes(200_000, 2);
    fs::create_dir(h.local.path().join("d")).unwrap();
    fs::write(h.local.path().join("d/x"), &data).unwrap();
    let fired = Arc::new(AtomicBool::new(false));
    let hook = swap_on("place", b"d/x", h.export.path(), outside.path(), &fired);
    let out = push_with_hook(&h, Some(hook), &archive_opts()).await;
    assert!(fired.load(Ordering::SeqCst), "the hook never fired");
    assert_canary(outside.path());
    x_failed_or_inside(&h, &out, &data);
}

#[tokio::test]
async fn push_inplace_swap_stays_inside() {
    let h = Harness::new().await;
    let outside = canary();
    let (old, mut new) = (bytes(300_000, 3), bytes(300_000, 3));
    new[150_000..150_100].fill(7);
    for (root, data) in [(h.export.path(), &old), (h.local.path(), &new)] {
        fs::create_dir(root.join("d")).unwrap();
        fs::write(root.join("d/x"), data).unwrap();
    }
    fs::File::options()
        .write(true)
        .open(h.export.path().join("d/x"))
        .unwrap()
        .set_modified(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000))
        .unwrap();
    let fired = Arc::new(AtomicBool::new(false));
    let hook = swap_on("inplace", b"d/x", h.export.path(), outside.path(), &fired);
    let mut opts = archive_opts();
    opts.inplace = true;
    let out = push_with_hook(&h, Some(hook), &opts).await;
    assert!(fired.load(Ordering::SeqCst), "the hook never fired");
    assert_canary(outside.path());
    x_failed_or_inside(&h, &out, &new);
}

fn with_partial_dir(dir: &str) -> irsync::cli::SyncOptions {
    let mut opts = archive_opts();
    opts.partial_dir = Some(dir.into());
    opts
}

#[tokio::test]
async fn partial_dir_that_is_a_symlink_is_refused() {
    let h = Harness::new().await;
    let outside = canary();
    symlink(outside.path(), h.export.path().join(".p")).unwrap();
    for name in ["a", "b"] {
        fs::write(h.local.path().join(name), bytes(100_000, 4)).unwrap();
    }
    let out = push_with_hook(&h, None, &with_partial_dir(".p")).await;
    for name in [b"a", b"b"] {
        assert!(
            has_error(&out, name, "symlink or not a directory"),
            "{:?}",
            out.errors
        );
    }
    assert_canary(outside.path());
}

#[tokio::test]
async fn a_symlink_below_the_partial_dir_is_refused() {
    let h = Harness::new().await;
    let outside = canary();
    fs::create_dir(h.export.path().join(".p")).unwrap();
    symlink(outside.path(), h.export.path().join(".p/sub")).unwrap();
    fs::create_dir(h.local.path().join("sub")).unwrap();
    fs::write(h.local.path().join("sub/x"), bytes(100_000, 5)).unwrap();
    let out = push_with_hook(&h, None, &with_partial_dir(".p")).await;
    assert!(
        has_error(&out, b"sub/x", "symlink or not a directory"),
        "{:?}",
        out.errors
    );
    assert!(!h.export.path().join("sub/x").exists());
    assert_canary(outside.path());
}

#[tokio::test]
async fn a_folder_made_again_mid_session_still_takes_files() {
    // The writer keeps its folders open: `d` removed and made again
    // between two files must not leave the second writing into the
    // removed folder.
    let h = Harness::new().await;
    fs::create_dir(h.local.path().join("d")).unwrap();
    fs::write(h.local.path().join("d/a"), bytes(100_000, 6)).unwrap();
    fs::write(h.local.path().join("d/b"), bytes(100_000, 7)).unwrap();
    let export = h.export.path().to_path_buf();
    let fired = Arc::new(AtomicBool::new(false));
    let f = fired.clone();
    let hook: irsync::session::EventHook = Arc::new(move |event: &str, path: &RelPath| {
        if event == "part-open" && path.as_bytes() == b"d/b" && !f.swap(true, Ordering::SeqCst) {
            fs::remove_file(export.join("d/a")).unwrap();
            fs::remove_dir(export.join("d")).unwrap();
            fs::create_dir(export.join("d")).unwrap();
        }
    });
    h.daemon.hooks.lock().unwrap().max_inflight = Some(1);
    let out = push_with_hook(&h, Some(hook), &archive_opts()).await;
    assert!(fired.load(Ordering::SeqCst), "the hook never fired");
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(
        fs::read(h.export.path().join("d/b")).unwrap(),
        bytes(100_000, 7)
    );
}

#[tokio::test]
async fn a_folder_made_again_mid_session_takes_files_in_a_new_subfolder() {
    // `d` is open from the first file; made again without `e`, the
    // second file's folder `d/e` is made in the new `d`.
    let h = Harness::new().await;
    fs::create_dir_all(h.local.path().join("d/e")).unwrap();
    fs::write(h.local.path().join("d/a"), bytes(100_000, 10)).unwrap();
    fs::write(h.local.path().join("d/e/b"), bytes(100_000, 11)).unwrap();
    let export = h.export.path().to_path_buf();
    let fired = Arc::new(AtomicBool::new(false));
    let f = fired.clone();
    let hook: irsync::session::EventHook = Arc::new(move |event: &str, path: &RelPath| {
        if event == "part-open" && path.as_bytes() == b"d/e/b" && !f.swap(true, Ordering::SeqCst) {
            fs::remove_dir_all(export.join("d")).unwrap();
            fs::create_dir(export.join("d")).unwrap();
        }
    });
    h.daemon.hooks.lock().unwrap().max_inflight = Some(1);
    let out = push_with_hook(&h, Some(hook), &archive_opts()).await;
    assert!(fired.load(Ordering::SeqCst), "the hook never fired");
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(
        fs::read(h.export.path().join("d/e/b")).unwrap(),
        bytes(100_000, 11)
    );
}

#[tokio::test]
async fn a_folder_made_again_before_placing_still_takes_files() {
    // With the parts kept elsewhere, the destination folder is opened at
    // the rename: `d` removed and made again after the first file was
    // placed must still take the second.
    let h = Harness::new().await;
    fs::create_dir(h.local.path().join("d")).unwrap();
    fs::write(h.local.path().join("d/a"), bytes(100_000, 8)).unwrap();
    fs::write(h.local.path().join("d/b"), bytes(100_000, 9)).unwrap();
    let export = h.export.path().to_path_buf();
    let fired = Arc::new(AtomicBool::new(false));
    let f = fired.clone();
    let hook: irsync::session::EventHook = Arc::new(move |event: &str, path: &RelPath| {
        if event == "place" && path.as_bytes() == b"d/b" && !f.swap(true, Ordering::SeqCst) {
            fs::remove_file(export.join("d/a")).unwrap();
            fs::remove_dir(export.join("d")).unwrap();
            fs::create_dir(export.join("d")).unwrap();
        }
    });
    h.daemon.hooks.lock().unwrap().max_inflight = Some(1);
    let out = push_with_hook(&h, Some(hook), &with_partial_dir(".p")).await;
    assert!(fired.load(Ordering::SeqCst), "the hook never fired");
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(
        fs::read(h.export.path().join("d/b")).unwrap(),
        bytes(100_000, 9)
    );
}

/// Pushes a local `d` holding what `make` puts there, swapping the
/// export's `d` for a symlink to the canary folder just before `d/<name>`
/// is made on the daemon.
async fn push_node_with_swap(name: &'static [u8], make: impl FnOnce(&Path)) {
    let h = Harness::new().await;
    let outside = canary();
    fs::create_dir(h.local.path().join("d")).unwrap();
    make(&h.local.path().join("d"));
    let fired = Arc::new(AtomicBool::new(false));
    let at: &'static [u8] = Box::leak([b"d/", name].concat().into_boxed_slice());
    let hook = swap_on("node", at, h.export.path(), outside.path(), &fired);
    let _ = push_with_hook(&h, Some(hook), &archive_opts()).await;
    assert!(fired.load(Ordering::SeqCst), "the hook never fired");
    assert_canary(outside.path());
}

#[tokio::test]
async fn symlink_placed_while_the_parent_is_swapped_stays_inside() {
    push_node_with_swap(b"l", |d| symlink("target", d.join("l")).unwrap()).await;
}

#[tokio::test]
async fn fifo_placed_while_the_parent_is_swapped_stays_inside() {
    push_node_with_swap(b"p", |d| {
        nix::unistd::mkfifo(
            &d.join("p"),
            nix::sys::stat::Mode::from_bits_truncate(0o640),
        )
        .unwrap()
    })
    .await;
}

#[tokio::test]
async fn folder_metadata_never_reaches_a_swapped_folder() {
    // `d` is swapped once its file is placed, before the folders get
    // their metadata: the outside folder keeps its mode and times.
    let h = Harness::new().await;
    let outside = canary();
    let d = h.local.path().join("d");
    fs::create_dir(&d).unwrap();
    fs::write(d.join("x"), bytes(1000, 12)).unwrap();
    fs::set_permissions(&d, fs::Permissions::from_mode(0o750)).unwrap();
    let before = fs::metadata(outside.path()).unwrap();
    assert_ne!(before.mode() & 0o7777, 0o750);
    let fired = Arc::new(AtomicBool::new(false));
    let hook = swap_on("placed", b"d/x", h.export.path(), outside.path(), &fired);
    let _ = push_with_hook(&h, Some(hook), &archive_opts()).await;
    assert!(fired.load(Ordering::SeqCst), "the hook never fired");
    let after = fs::metadata(outside.path()).unwrap();
    assert_eq!(
        after.mode(),
        before.mode(),
        "the outside folder's mode changed"
    );
    assert_eq!(
        (after.mtime(), after.mtime_nsec()),
        (before.mtime(), before.mtime_nsec()),
        "the outside folder's times changed"
    );
    assert_canary(outside.path());
}

#[tokio::test]
async fn local_chunk_copies_never_read_through_a_swapped_folder() {
    // The export holds `d/src`, indexed. A pushed file shares its first
    // chunks, so the daemon plans to copy them from `d/src`. Then `d`
    // is swapped for a folder outside holding a copy of `src`, and the
    // real one is removed: the chunks must come from the sender.
    let h = Harness::new().await;
    let shared = noise(4 << 20, 13);
    fs::create_dir(h.local.path().join("d")).unwrap();
    fs::write(h.local.path().join("d/src"), &shared).unwrap();
    let out = push_with_hook(&h, None, &archive_opts()).await;
    assert!(out.errors.is_empty(), "{:?}", out.errors);

    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("src"), &shared).unwrap();
    fs::remove_dir_all(h.local.path().join("d")).unwrap();
    let mut new = shared.clone();
    new.extend_from_slice(&noise(2 << 20, 14));
    fs::write(h.local.path().join("new"), &new).unwrap();
    let (export, out_dir, fired) = (
        h.export.path().to_path_buf(),
        outside.path().to_path_buf(),
        Arc::new(AtomicBool::new(false)),
    );
    let f = fired.clone();
    let hook: irsync::session::EventHook = Arc::new(move |event: &str, path: &RelPath| {
        if event == "need" && path.as_bytes() == b"new" && !f.swap(true, Ordering::SeqCst) {
            swap_d(&export, &out_dir);
            fs::remove_file(export.join("d.real/src")).unwrap();
        }
    });
    let out = push_with_hook(&h, Some(hook), &archive_opts()).await;
    assert!(fired.load(Ordering::SeqCst), "the hook never fired");
    // The planned copies fail (the real `src` is gone, the swapped `d` is
    // refused), so the chunks are requested again and the file arrives.
    assert!(
        !out.errors.iter().any(|(p, _)| p.as_bytes() == b"new"),
        "{:?}",
        out.errors
    );
    assert_eq!(fs::read(h.export.path().join("new")).unwrap(), new);
    assert!(
        out.bytes_transferred >= new.len() as u64,
        "only {} bytes came from the sender: the rest was read outside",
        out.bytes_transferred
    );
    // Outside is as it was: only `src`, unchanged.
    let names: Vec<_> = fs::read_dir(outside.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["src"], "something appeared outside the export");
    assert_eq!(fs::read(outside.path().join("src")).unwrap(), shared);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resumed_plan_does_not_make_folders_through_a_symlink() {
    // The first push stops while `d/b/f` is written, so the daemon keeps a
    // complete plan. The resumed push makes the planned folders again; `d`
    // is swapped for a symlink to outside just before `d/b` is made.
    let h = Harness::new().await;
    let outside = canary();
    fs::create_dir_all(h.local.path().join("d/b")).unwrap();
    fs::write(h.local.path().join("d/b/f"), bytes(2 << 20, 15)).unwrap();
    let fault = h.daemon.fault.clone();
    h.daemon.hooks.lock().unwrap().on_event = Some(Arc::new(move |event: &str, path: &RelPath| {
        if event == "part-open" && path.as_bytes() == b"d/b/f" {
            fault.drop_after_bytes(0);
        }
    }));
    let opts = archive_opts();
    let (first, _) =
        within30(h.run_reconnect(&opts, Flow::Push, no_reconnect(), h.local_side())).await;
    assert!(first.is_err(), "the first run should fail");
    let fired = Arc::new(AtomicBool::new(false));
    h.daemon.hooks.lock().unwrap().on_event = Some(swap_on(
        "node",
        b"d/b",
        h.export.path(),
        outside.path(),
        &fired,
    ));
    let (second, _) = within30(h.run_reconnect(&opts, Flow::Push, fast(), h.local_side())).await;
    let second = second.unwrap_or_else(|e| panic!("{e:#}"));
    assert!(second.resumed, "the second run did not resume");
    assert!(fired.load(Ordering::SeqCst), "the hook never fired");
    assert_canary(outside.path());
}
