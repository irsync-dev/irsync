//! irsync on Windows, end to end: each test is a real sync between two
//! directories in one process (a daemon's export and a client's tree),
//! through the helpers in `tests/common`.
#![cfg(windows)]

mod common;

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{archive_opts, Flow, Harness};
use irsync::cli::Args;
use irsync::options::SyncOptions;
use irsync_proto::v1::FileEntry;

async fn within60<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(60), f)
        .await
        .expect("test timed out after 60 s")
}

/// `irsync -a` with `extra` options.
fn opts(extra: &[&str]) -> SyncOptions {
    let mut v = vec!["irsync", "-a"];
    v.extend_from_slice(extra);
    v.extend_from_slice(&["src/", "host:m/"]);
    SyncOptions::from_args(&Args::try_parse_from(v).unwrap())
}

/// The names in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// Whether the outcome holds the error `msg` for `path`, as a push reports
/// what its receiver (the daemon) said.
fn has_error(out: &irsync::session::Outcome, path: &str, msg: &str) -> bool {
    out.errors
        .iter()
        .any(|(p, m)| p.as_bytes() == path.as_bytes() && m == &format!("receiver: {msg}"))
}

/// A new name that differs only by case from one already in the
/// destination fails by itself; the one there is unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_case_only_clash_fails_the_second_entry() {
    let h = Harness::new().await;
    fs::write(h.local.path().join("README"), b"new").unwrap();
    fs::write(h.local.path().join("other"), b"other").unwrap();
    fs::write(h.export.path().join("Readme"), b"old").unwrap();
    let out = within60(h.run(&archive_opts(), Flow::Push))
        .await
        .expect("session");
    assert!(
        has_error(&out, "README", "differs only by case from Readme"),
        "{:?}",
        out.errors
    );
    assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
    assert_eq!(names(h.export.path()), ["Readme", "other"]);
    assert_eq!(fs::read(h.export.path().join("Readme")).unwrap(), b"old");
    assert_eq!(fs::read(h.export.path().join("other")).unwrap(), b"other");
}

/// Two names in one source directory that differ only by case (a
/// case-sensitive source, here a crafted listing): the first arrives, the
/// second fails against it, though it was made in this same session.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_source_names_that_differ_only_by_case_place_the_first() {
    let h = Harness::new().await;
    fs::create_dir(h.local.path().join("d")).unwrap();
    fs::write(h.local.path().join("d").join("README"), b"first").unwrap();
    let mut local = h.local_side();
    local.hooks.inject_listing = Some(Arc::new(|e: &FileEntry| {
        if e.path == b"d/README" {
            // Sorts after `README`; read back through the same file.
            vec![FileEntry {
                path: b"d/Readme".to_vec(),
                ..e.clone()
            }]
        } else {
            Vec::new()
        }
    }));
    let out = within60(h.run_side(&archive_opts(), Flow::Push, local))
        .await
        .expect("session");
    assert!(
        has_error(&out, "d/Readme", "differs only by case from d/README"),
        "{:?}",
        out.errors
    );
    assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
    assert_eq!(names(&h.export.path().join("d")), ["README"]);
    assert_eq!(
        fs::read(h.export.path().join("d").join("README")).unwrap(),
        b"first"
    );
}

/// Writes `files` (path, contents) below `root`, making their directories.
fn put(root: &Path, files: &[(&str, &str)]) {
    for (p, c) in files {
        let p = root.join(p);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, c).unwrap();
    }
}

/// Every file below `root` with its contents, as (path with `/`, text).
fn tree_of(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        for e in fs::read_dir(&dir).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            let rel = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if e.file_type().unwrap().is_dir() {
                out.push((format!("{rel}/"), String::new()));
                stack.push((e.path(), rel));
            } else {
                out.push((rel, fs::read_to_string(e.path()).unwrap()));
            }
        }
    }
    out.sort();
    out
}

fn owned(v: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut v: Vec<_> = v
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    v.sort();
    v
}

/// One push of `src` onto `dst` with `mode`; the outcome and what the
/// destination holds after it.
async fn case_rename(
    mode: &str,
    src: &[(&str, &str)],
    dst: &[(&str, &str)],
) -> (irsync::session::Outcome, Vec<(String, String)>) {
    let h = Harness::new().await;
    put(h.local.path(), src);
    put(h.export.path(), dst);
    let out = within60(h.run(&opts(&[mode]), Flow::Push))
        .await
        .expect("session");
    let left = tree_of(h.export.path());
    (out, left)
}

/// The source renamed `readme` to `README`, and a delete mode is on.
/// Where deletion comes after the new name is refused (`--delete`,
/// `--delete-after`, `--delete-excluded`, and `--delete-during` when the
/// new name sorts first), the refused name protects the old one: the
/// destination keeps `readme` as it was, the run reports the clash, and
/// the next run does the same until the user renames by hand.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rename_by_case_keeps_the_old_name() {
    for mode in [
        "--delete",
        "--delete-after",
        "--delete-excluded",
        "--delete-during",
    ] {
        let (out, left) = case_rename(mode, &[("README", "renamed")], &[("readme", "old")]).await;
        assert!(
            has_error(&out, "README", "differs only by case from readme"),
            "{mode}: {:?}",
            out.errors
        );
        assert_eq!(out.errors.len(), 1, "{mode}: {:?}", out.errors);
        assert_eq!(left, owned(&[("readme", "old")]), "{mode}");
    }
}

/// The same for a directory: `docs/` in the source against `Docs/` in the
/// destination. Nothing under `Docs` is deleted or changed; `docs` fails
/// with the clash and its entries because their directory failed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_directory_renamed_by_case_keeps_the_old_tree() {
    let kept = [("Docs/", ""), ("Docs/a", "old a"), ("Docs/old", "old")];
    for mode in ["--delete", "--delete-after", "--delete-excluded"] {
        let (out, left) = case_rename(
            mode,
            &[("docs/a", "new a"), ("docs/b", "new b")],
            &[("Docs/a", "old a"), ("Docs/old", "old")],
        )
        .await;
        assert!(
            has_error(&out, "docs", "differs only by case from Docs"),
            "{mode}: {:?}",
            out.errors
        );
        for child in ["docs/a", "docs/b"] {
            assert!(
                has_error(&out, child, "not created: its parent directory failed"),
                "{mode}: {:?}",
                out.errors
            );
        }
        assert_eq!(out.errors.len(), 3, "{mode}: {:?}", out.errors);
        assert_eq!(left, owned(&kept), "{mode}");
    }
    // --delete-during with the new name first (`Docs` < `docs`): the old
    // tree is protected before its deletions are due.
    let (out, left) = case_rename(
        "--delete-during",
        &[("Docs/a", "new a"), ("Docs/b", "new b")],
        &[("docs/a", "old a"), ("docs/old", "old")],
    )
    .await;
    assert!(
        has_error(&out, "Docs", "differs only by case from docs"),
        "{:?}",
        out.errors
    );
    assert_eq!(
        left,
        owned(&[("docs/", ""), ("docs/a", "old a"), ("docs/old", "old")])
    );
}

/// Where the old name is deleted before the new one is planned, there is
/// no clash and the rename is done in one run: always with
/// `--delete-before`, and with `--delete-during` when the old directory
/// sorts first and the walk leaves it before it reaches the new one. What
/// the source does not have is deleted, as `--delete` asks.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rename_by_case_done_when_the_old_name_goes_first() {
    let (out, left) = case_rename(
        "--delete-before",
        &[("README", "renamed")],
        &[("readme", "old")],
    )
    .await;
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(left, owned(&[("README", "renamed")]));
    let renamed = owned(&[("docs/", ""), ("docs/a", "new a"), ("docs/b", "new b")]);
    for mode in ["--delete-before", "--delete-during"] {
        let (out, left) = case_rename(
            mode,
            &[("docs/a", "new a"), ("docs/b", "new b")],
            &[("Docs/a", "old a"), ("Docs/old", "old")],
        )
        .await;
        assert!(out.errors.is_empty(), "{mode}: {:?}", out.errors);
        assert_eq!(left, renamed, "{mode}");
    }
}

/// Opens `p` for reading and writing as another program would, sharing
/// what `share` says (1 read, 2 write, 4 delete).
fn hold(p: &Path, share: u32) -> fs::File {
    common::open_sharing(p, share)
}

/// What a session reports for `path`, if anything.
fn error_of(out: &irsync::session::Outcome, path: &str) -> Option<String> {
    out.errors
        .iter()
        .find(|(p, _)| p.as_bytes() == path.as_bytes())
        .map(|(_, m)| m.clone())
}

/// A destination another program holds open: sharing delete, it is
/// replaced; without, its entry fails as in use once the retries (about a
/// second) are spent, and the session goes on; let go within them, it is
/// replaced.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_destination_in_use() {
    let h = Harness::new().await;
    for name in ["shared", "locked", "brief", "other"] {
        fs::write(h.local.path().join(name), format!("new {name}")).unwrap();
    }
    for name in ["shared", "locked", "brief"] {
        fs::write(h.export.path().join(name), b"old").unwrap();
    }
    let shared = hold(&h.export.path().join("shared"), 7);
    let locked = hold(&h.export.path().join("locked"), 3);
    let brief = hold(&h.export.path().join("brief"), 3);
    let letting_go = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        drop(brief);
    });
    let started = std::time::Instant::now();
    let out = within60(h.run(&archive_opts(), Flow::Push))
        .await
        .expect("session");
    letting_go.join().unwrap();
    assert!(
        has_error(&out, "locked", "in use by another program"),
        "{:?}",
        out.errors
    );
    assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "gave up before the retries were spent: {:?}",
        started.elapsed()
    );
    drop((shared, locked));
    let read = |n: &str| fs::read_to_string(h.export.path().join(n)).unwrap();
    assert_eq!(read("shared"), "new shared");
    assert_eq!(read("locked"), "old");
    assert_eq!(read("brief"), "new brief");
    assert_eq!(read("other"), "new other");
}

/// A source file another program has open for writing: sharing read, it is
/// sent; with no sharing, its entry fails as in use and the session goes
/// on.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_source_file_held_by_a_writer() {
    let h = Harness::new().await;
    for name in ["shared", "locked", "other"] {
        fs::write(h.local.path().join(name), format!("new {name}")).unwrap();
    }
    let shared = hold(&h.local.path().join("shared"), 1);
    let locked = hold(&h.local.path().join("locked"), 0);
    let out = within60(h.run(&archive_opts(), Flow::Push))
        .await
        .expect("session");
    drop((shared, locked));
    let msg = error_of(&out, "locked").unwrap_or_default();
    assert!(msg == "in use by another program", "{:?}", out.errors);
    assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
    let read = |n: &str| fs::read_to_string(h.export.path().join(n)).unwrap();
    assert_eq!(read("shared"), "new shared");
    assert_eq!(read("other"), "new other");
    assert!(!h.export.path().join("locked").exists());
}

/// A destination file with the read-only attribute is replaced, and one
/// the source no longer has is deleted (`--delete`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_only_destinations() {
    let h = Harness::new().await;
    fs::write(h.local.path().join("ro"), b"newer").unwrap();
    for name in ["ro", "gone"] {
        let p = h.export.path().join(name);
        fs::write(&p, b"old").unwrap();
        let mut perm = fs::metadata(&p).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&p, perm).unwrap();
    }
    let out = within60(h.run(&opts(&["--delete"]), Flow::Push))
        .await
        .expect("session");
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(names(h.export.path()), ["ro"]);
    assert_eq!(fs::read(h.export.path().join("ro")).unwrap(), b"newer");
}

/// `p` (absolute) in the extended form, so the system takes every name in
/// it as written: no trailing dot or space dropped, `CON` a plain name,
/// no 260-character limit.
fn verbatim(p: &Path) -> std::path::PathBuf {
    let s = p.display().to_string();
    if s.starts_with(r"\\?\") {
        p.to_path_buf()
    } else {
        std::path::PathBuf::from(format!(r"\\?\{s}"))
    }
}

/// A tree of files, nested directories, a hard-link pair, an empty file
/// and a large file of zeros: equal after the first run; the second run
/// transfers nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tree_syncs_and_syncs_again_with_nothing_to_do() {
    use irsync::platform::testutil::file_id;
    let h = Harness::new().await;
    let src = h.local.path();
    fs::create_dir_all(src.join("a").join("b").join("c")).unwrap();
    fs::write(src.join("top"), b"top").unwrap();
    fs::write(src.join("a").join("one"), b"one").unwrap();
    fs::write(src.join("a").join("b").join("c").join("deep"), b"deep").unwrap();
    fs::write(src.join("empty"), b"").unwrap();
    fs::write(src.join("a").join("linked"), b"twice").unwrap();
    fs::hard_link(src.join("a").join("linked"), src.join("also")).unwrap();
    let zeros = fs::File::create(src.join("zeros")).unwrap();
    zeros.set_len(64 << 20).unwrap();
    drop(zeros);
    let o = opts(&["-H"]);
    let first = within60(h.run(&o, Flow::Push))
        .await
        .expect("first session");
    assert!(first.errors.is_empty(), "{:?}", first.errors);
    common::assert_trees_equal(src, h.export.path(), &o.preserve);
    let dst = h.export.path();
    assert_eq!(
        file_id(&dst.join("a").join("linked")).unwrap(),
        file_id(&dst.join("also")).unwrap(),
        "the hard-link pair is one file"
    );
    assert_eq!(fs::metadata(dst.join("zeros")).unwrap().len(), 64 << 20);
    // The second run, itemized (-i): the daemon tells the client every
    // entry it changes in any way, metadata only included.
    let touched = [
        "",
        "top",
        "a",
        "a/b",
        "a/b/c",
        "a/b/c/deep",
        "also",
        "empty",
        "zeros",
    ];
    let mtimes = |root: &Path| -> Vec<std::time::SystemTime> {
        touched
            .iter()
            .map(|p| fs::metadata(root.join(p)).unwrap().modified().unwrap())
            .collect()
    };
    let before = mtimes(dst);
    let o = opts(&["-H", "-i"]);
    let local = h.local_side();
    let mut bus = local.bus.subscribe();
    let second = within60(h.run_side(&o, Flow::Push, local))
        .await
        .expect("second session");
    assert!(second.errors.is_empty(), "{:?}", second.errors);
    assert_eq!(second.files_transferred, 0);
    assert_eq!(second.bytes_transferred, 0);
    assert_eq!(second.created, 0);
    let mut itemized = Vec::new();
    while let Ok(e) = bus.try_recv() {
        if let irsync::session::Event::Itemized(p, code) = e {
            itemized.push((p, code));
        }
    }
    assert!(itemized.is_empty(), "touched: {itemized:?}");
    assert_eq!(mtimes(dst), before, "a time changed");
    common::assert_trees_equal(src, h.export.path(), &o.preserve);
}

/// A path over 260 characters and names Windows takes only by handle or
/// in the extended form (`CON`, a trailing dot or space, a lone surrogate)
/// arrive as they are, both ways.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn long_paths_and_awkward_names_sync() {
    let h = Harness::new().await;
    let src = verbatim(h.local.path());
    let mut deep = src.clone();
    for i in 0..6 {
        deep.push(format!("{i}{}", "d".repeat(50)));
    }
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("far"), b"far").unwrap();
    assert!(deep.join("far").as_os_str().len() > 260 + 4);
    let surrogate = common::name_from_units(&[u16::from(b'x'), 0xD800]);
    for name in [
        std::ffi::OsString::from("CON"),
        "trail.".into(),
        "trail ".into(),
        surrogate.clone(),
    ] {
        fs::write(src.join(&name), name.to_string_lossy().as_bytes()).unwrap();
    }
    let o = archive_opts();
    let out = within60(h.run(&o, Flow::Push)).await.expect("push");
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    let dst = verbatim(h.export.path());
    common::assert_trees_equal(&src, &dst, &o.preserve);
    assert_eq!(fs::read(dst.join(&surrogate)).unwrap(), b"x\xEF\xBF\xBD");
    // And back: a pull into an empty directory.
    for e in fs::read_dir(&src).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            fs::remove_dir_all(&p).unwrap();
        } else {
            fs::remove_file(&p).unwrap();
        }
    }
    let out = within60(h.run(&o, Flow::Pull)).await.expect("pull");
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    common::assert_trees_equal(&dst, &src, &o.preserve);
}

/// A listed name Windows cannot hold (`a:b`, sent by a crafted listing)
/// fails that entry with the rule's message; the other files arrive.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_name_windows_cannot_hold_fails_that_entry_only() {
    let h = Harness::new().await;
    fs::write(h.local.path().join("a"), b"a").unwrap();
    fs::write(h.local.path().join("z"), b"z").unwrap();
    let mut local = h.local_side();
    local.hooks.inject_listing = Some(Arc::new(|e: &FileEntry| {
        if e.path == b"a" {
            vec![FileEntry {
                path: b"a:b".to_vec(),
                ..e.clone()
            }]
        } else {
            Vec::new()
        }
    }));
    let out = within60(h.run_side(&archive_opts(), Flow::Push, local))
        .await
        .expect("session");
    assert!(
        has_error(&out, "a:b", "holds a character Windows does not allow"),
        "{:?}",
        out.errors
    );
    assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
    assert_eq!(names(h.export.path()), ["a", "z"]);
}

/// `--delete` removes a junction and a directory symlink the source does
/// not have, not the directories they lead to or what is in them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_removes_links_not_what_they_point_at() {
    use irsync::platform::testutil::{junction, symlink};
    let h = Harness::new().await;
    let outside = tempfile::tempdir().unwrap();
    for d in ["by_junction", "by_symlink"] {
        fs::create_dir(outside.path().join(d)).unwrap();
        fs::write(outside.path().join(d).join("keep"), b"keep").unwrap();
    }
    fs::write(h.local.path().join("f"), b"f").unwrap();
    junction(
        &outside.path().join("by_junction"),
        &h.export.path().join("j"),
    )
    .unwrap();
    let with_symlink = common::symlinks_available();
    if !with_symlink {
        eprintln!("skipping: the directory symlink half (this process may not make symlinks)");
    }
    if with_symlink {
        symlink(
            &outside.path().join("by_symlink"),
            &h.export.path().join("s"),
        )
        .unwrap();
    }
    let out = within60(h.run(&opts(&["--delete"]), Flow::Push))
        .await
        .expect("session");
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(names(h.export.path()), ["f"]);
    for d in ["by_junction", "by_symlink"] {
        assert_eq!(
            fs::read(outside.path().join(d).join("keep")).unwrap(),
            b"keep",
            "{d}"
        );
    }
}

/// A root given relative, with `/` and with `..` works; at the top of a
/// drive (a `subst` of a temporary directory) `System Volume Information`
/// and `$RECYCLE.BIN` are neither walked, warned about nor deleted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn roots() {
    let h = Harness::new().await;
    let base = tempfile::tempdir().unwrap();
    fs::create_dir(base.path().join("tree")).unwrap();
    fs::write(base.path().join("tree").join("f"), b"f").unwrap();
    fs::write(h.export.path().join("g"), b"g").unwrap();
    // Each test runs in a process of its own (nextest), so the working
    // directory is this test's to set; put back however the test ends.
    struct Cwd(std::path::PathBuf);
    impl Drop for Cwd {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }
    let cwd = Cwd(std::env::current_dir().unwrap());
    std::env::set_current_dir(base.path()).unwrap();
    let slashed = format!("{}/tree", base.path().display()).replace('\\', "/");
    for root in [
        std::path::PathBuf::from("tree"),
        std::path::PathBuf::from(&slashed),
        std::path::PathBuf::from("tree").join("..").join("tree"),
    ] {
        let local = Harness::side_for(&root, h.client_state.path());
        let out = within60(h.run_side(&archive_opts(), Flow::Push, local))
            .await
            .unwrap_or_else(|e| panic!("{}: {e:#}", root.display()));
        assert!(
            out.errors.is_empty(),
            "{}: {:?}",
            root.display(),
            out.errors
        );
        assert_eq!(fs::read(h.export.path().join("f")).unwrap(), b"f");
        let local = Harness::side_for(&root, h.client_state.path());
        let out = within60(h.run_side(&archive_opts(), Flow::Pull, local))
            .await
            .unwrap_or_else(|e| panic!("{}: {e:#}", root.display()));
        assert!(
            out.errors.is_empty(),
            "{}: {:?}",
            root.display(),
            out.errors
        );
        assert_eq!(fs::read(base.path().join("tree").join("g")).unwrap(), b"g");
        fs::remove_file(base.path().join("tree").join("g")).unwrap();
        fs::remove_file(h.export.path().join("f")).unwrap();
    }
    drop(cwd);

    // The top of a drive.
    let backing = tempfile::tempdir().unwrap();
    for d in ["System Volume Information", "$RECYCLE.BIN"] {
        fs::create_dir(backing.path().join(d)).unwrap();
        fs::write(backing.path().join(d).join("inside"), b"system").unwrap();
    }
    fs::write(backing.path().join("top"), b"top").unwrap();
    let Some(drive) = common::subst_of(backing.path()) else {
        eprintln!("skipping: no free drive letter takes a subst");
        return;
    };
    let top = std::path::PathBuf::from(format!(r"{}:\", drive.0));
    fs::remove_file(h.export.path().join("g")).unwrap();
    // As a source: only `top` is sent.
    let local = Harness::side_for(&top, h.client_state.path());
    let out = within60(h.run_side(&archive_opts(), Flow::Push, local))
        .await
        .expect("push from a drive top");
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(names(h.export.path()), ["top"]);
    // As a destination with --delete: they stay, untouched.
    fs::remove_file(backing.path().join("top")).unwrap();
    let local = Harness::side_for(&top, h.client_state.path());
    let out = within60(h.run_side(&opts(&["--delete"]), Flow::Pull, local))
        .await
        .expect("pull to a drive top");
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert!(out.deleted.is_empty(), "{:?}", out.deleted);
    drop(drive);
    assert_eq!(
        names(backing.path()),
        ["$RECYCLE.BIN", "System Volume Information", "top"]
    );
    for d in ["System Volume Information", "$RECYCLE.BIN"] {
        assert_eq!(
            fs::read(backing.path().join(d).join("inside")).unwrap(),
            b"system"
        );
    }
}
