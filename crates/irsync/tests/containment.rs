//! A hostile sender must not make the receiver write, link or chmod outside
//! its root: listings naming irsync's own temp files and hard links whose
//! targets were not received as regular files are refused. And a sender
//! reads only files below its root, whatever happens to the folders on the
//! way while it works.

mod common;

use std::fs;
use std::os::unix::fs::{symlink, MetadataExt};
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

/// Asserts the trees below `a` and `b` are equal; the roots' own
/// metadata is not compared.
fn assert_contents_equal(a: &std::path::Path, b: &std::path::Path) {
    let names = |d: &std::path::Path| {
        let mut v: Vec<_> = fs::read_dir(d)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        v.sort();
        v
    };
    assert_eq!(names(a), names(b));
    for n in names(a) {
        assert_trees_equal(&a.join(&n), &b.join(&n), &archive_opts().preserve);
    }
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
    // The root's own times are still applied by path, so to the symlink
    // that names it; only what is below it is compared.
    assert_contents_equal(&src, &export);

    let pull = Harness::side_for(&links.path().join("dst"), h.client_state.path());
    let out = within30(h.run_side(&opts, Flow::Pull, pull))
        .await
        .expect("pull");
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_contents_equal(&export, &dst);
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
/// `d/secret`; `fired` says it did.
fn swap_on(
    what: &'static str,
    export: &Path,
    outside: &Path,
    fired: &Arc<AtomicBool>,
) -> irsync::session::EventHook {
    let (export, outside, fired) = (export.to_path_buf(), outside.to_path_buf(), fired.clone());
    Arc::new(move |event: &str, path: &RelPath| {
        if event == what && path.as_bytes() == b"d/secret" && !fired.swap(true, Ordering::SeqCst) {
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
    let hook = swap_on("hash-open", h.export.path(), outside.path(), &fired);
    let out = pull_with_hook(&h, hook, None).await;
    assert!(fired.load(Ordering::SeqCst), "the hook never fired");
    assert_nothing_from_outside(h.local.path(), &out);
}

#[tokio::test]
async fn pull_swap_before_sending_sends_nothing_from_outside() {
    let h = Harness::new().await;
    let outside = plant_secret(h.export.path());
    let fired = Arc::new(AtomicBool::new(false));
    let hook = swap_on("send-open", h.export.path(), outside.path(), &fired);
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
    let hook = swap_on("send-open", h.export.path(), outside.path(), &fired);
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
