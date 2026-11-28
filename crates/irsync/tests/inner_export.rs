//! On Windows two spellings can name one directory (another case, an 8.3
//! short name). A request through an outer export must not reach an
//! inner export's directory that way and bypass the inner export's rules.
#![cfg(windows)]

mod common;

use std::fs;
use std::time::Duration;

use common::{subst_of, Flow, Harness, Subst};
use irsync::cli::Args;
use irsync::options::SyncOptions;

const DIR: &str = "readonly-inner";

async fn within30<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(30), f)
        .await
        .expect("test timed out after 30 s")
}

fn opts() -> SyncOptions {
    SyncOptions::from_args(&Args::try_parse_from(["irsync", "-a", "src/", "host:m/"]).unwrap())
}

/// An outer read-write export `m` with a read-only export `ro` inside it.
async fn harness() -> Harness {
    let h = Harness::with_inner_export("ro", DIR, |e| e.read_only = true).await;
    fs::write(h.local.path().join("f"), b"pushed").unwrap();
    h
}

/// A push to `m/<dir>/x`; the error text.
async fn push_error(h: &Harness, dir: &str) -> String {
    let mut hello = Harness::hello(Flow::Push);
    hello.req.path = format!("m/{dir}/x").into_bytes();
    let res = within30(h.run_hello(&opts(), hello, h.local_side())).await;
    format!(
        "{:#}",
        res.expect_err(&format!("the push into {dir} went ahead"))
    )
}

fn written(h: &Harness) -> bool {
    h.export.path().join(DIR).join("x").exists()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_exact_spelling_meets_the_inner_exports_rules() {
    let h = harness().await;
    let e = push_error(&h, DIR).await;
    assert!(e.contains("read-only"), "{e}");
    assert!(!written(&h));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_case_is_refused_by_identity() {
    let h = harness().await;
    let e = push_error(&h, &DIR.to_uppercase()).await;
    assert!(
        e.contains("is inside export ro; ask for it by that export"),
        "{e}"
    );
    assert!(!written(&h));
}

/// A check for an inner export that cannot finish (here a test hook makes
/// it fail) refuses the request; it never lets one through.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_check_refuses() {
    let h = harness().await;
    h.daemon.hooks.lock().unwrap().fail_inner_export_check = true;
    // (The exact spelling is refused before, as the read-only export.)
    for dir in [DIR.to_uppercase(), "elsewhere".to_string()] {
        let e = push_error(&h, &dir).await;
        assert!(e.contains("checking the path failed"), "{dir}: {e}");
        assert!(!e.contains("ro;"), "{dir}: {e}");
    }
    assert!(!written(&h));
    assert!(!h.export.path().join("elsewhere").exists());
}

/// An inner export whose directory does not exist yet: the exact spelling
/// meets its rules as today; another case is refused, and nothing is made.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_missing_inner_export_is_compared_by_name() {
    let h = harness().await;
    fs::remove_dir(h.export.path().join(DIR)).unwrap();
    let e = push_error(&h, DIR).await;
    assert!(e.contains("read-only"), "{e}");
    for other in [DIR.to_uppercase(), "Readonly-Inner".to_string()] {
        let e = push_error(&h, &other).await;
        assert!(
            e.contains("is inside export ro; ask for it by that export"),
            "{other}: {e}"
        );
    }
    let left: Vec<_> = fs::read_dir(h.export.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(left.is_empty(), "made: {left:?}");
}

/// An inner export's root that exists but is a file, or a link the
/// serving open refuses (here a junction): neither a walk nor a session
/// passes through it, so it is skipped and requests through `m` go
/// ahead, in any spelling of it too. (A real error still refuses: see
/// `an_error_inside_the_check_refuses`.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_inner_export_root_that_is_a_file_or_a_link_does_not_disturb_requests() {
    for link in [false, true] {
        let h = harness().await;
        let root = h.export.path().join(DIR);
        fs::remove_dir(&root).unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        if link {
            irsync::platform::testutil::junction(elsewhere.path(), &root).unwrap();
        } else {
            fs::write(&root, b"a file").unwrap();
        }
        let out = push(&h, "elsewhere")
            .await
            .unwrap_or_else(|e| panic!("link {link}: {e:#}"));
        assert!(out.errors.is_empty(), "link {link}: {:?}", out.errors);
        assert_eq!(
            fs::read(h.export.path().join("elsewhere").join("f")).unwrap(),
            b"pushed"
        );
        // Through it in another spelling: the session itself refuses to
        // pass a file or a link, and nothing lands behind it.
        let _ = push(&h, &format!("{}/x", DIR.to_uppercase())).await;
        assert!(
            listed(elsewhere.path()).is_empty(),
            "link {link}: written through"
        );
        if link {
            fs::remove_dir(&root).unwrap();
        } else {
            assert_eq!(fs::read(&root).unwrap(), b"a file");
        }
    }
}

/// An I/O error inside the check (a test hook) refuses the request.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_error_inside_the_check_refuses() {
    let h = harness().await;
    h.daemon.hooks.lock().unwrap().inner_export_check_io_error = true;
    let e = push_error(&h, "elsewhere").await;
    assert!(
        e.contains("checking the path failed: test hook: an error inside the check"),
        "{e}"
    );
    assert!(!h.export.path().join("elsewhere").exists());
}

/// The 8.3 short name of `dir`, if its volume makes them.
fn short_name(dir: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("cmd")
        .args(["/c", "dir", "/x", "/ad"])
        .arg(dir.parent()?)
        .output()
        .ok()?;
    let long = dir.file_name()?.to_str()?.to_string();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.trim_end().ends_with(&long))
        .find_map(|l| {
            // date time <DIR> SHORT LONG: a short name sits before the long
            // one (which may hold spaces).
            let before = l.trim_end().strip_suffix(long.as_str())?;
            let s = before.split_whitespace().last()?;
            s.contains('~').then(|| s.to_string())
        })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_short_name_is_refused_by_identity() {
    let h = harness().await;
    let Some(short) = short_name(&h.export.path().join(DIR)) else {
        eprintln!("skipping: this volume makes no 8.3 short names");
        return;
    };
    let e = push_error(&h, &short).await;
    assert!(
        e.contains("is inside export ro; ask for it by that export"),
        "{short}: {e}"
    );
    assert!(!written(&h));
}

/// `path` in the verbatim form, so a test sees the name exactly as written
/// (Win32 drops a trailing dot or space of the last name otherwise).
fn verbatim(path: &std::path::Path) -> std::path::PathBuf {
    let shown = path.display().to_string();
    assert!(shown.len() > 3 && &shown[1..3] == r":\", "{shown}");
    std::path::PathBuf::from(format!(r"\\?\{shown}"))
}

/// A push to `m/<dir>` followed by a dot or a space (`readonly-inner.`): a
/// name of its own on NTFS, which Win32 would open as `readonly-inner`.
/// The session works in a directory with exactly that name inside the
/// outer export (made by the push, or there already), and nothing reaches
/// the read-only inner export: with `--delete`, what the inner export
/// holds is not seen as the destination's (so it is neither deleted nor
/// kept as the destination's), and what the directory of that name holds
/// is.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_trailing_dot_or_space_is_a_name_of_its_own() {
    let delete = SyncOptions::from_args(
        &Args::try_parse_from(["irsync", "-a", "--delete", "src/", "host:m/"]).unwrap(),
    );
    for tail in [".", " "] {
        for there in [false, true] {
            let h = harness().await;
            let name = format!("{DIR}{tail}");
            let inner = h.export.path().join(DIR);
            fs::write(inner.join("keep"), b"inner").unwrap();
            let own = verbatim(h.export.path()).join(&name);
            if there {
                fs::create_dir(&own).unwrap();
                fs::write(own.join("old"), b"old").unwrap();
            }
            let mut hello = Harness::hello(Flow::Push);
            hello.req.path = format!("m/{name}").into_bytes();
            let out = within30(h.run_hello(&delete, hello, h.local_side()))
                .await
                .unwrap_or_else(|e| panic!("{name:?}: {e:#}"));
            assert!(out.errors.is_empty(), "{name:?} {there}: {:?}", out.errors);
            let left: Vec<_> = fs::read_dir(&inner)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert_eq!(
                left,
                ["keep"],
                "{name:?} {there}: the read-only export changed"
            );
            assert_eq!(fs::read(inner.join("keep")).unwrap(), b"inner");
            let mut got: Vec<_> = fs::read_dir(&own)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            got.sort();
            assert_eq!(got, ["f"], "{name:?} {there}");
            assert_eq!(fs::read(own.join("f")).unwrap(), b"pushed");
        }
    }
}

/// A push to `m/<path>`; its result.
async fn push(h: &Harness, path: &str) -> anyhow::Result<irsync::session::Outcome> {
    let mut hello = Harness::hello(Flow::Push);
    hello.req.path = format!("m/{path}").into_bytes();
    within30(h.run_hello(&opts(), hello, h.local_side())).await
}

fn refused_as_inside_ro(res: anyhow::Result<irsync::session::Outcome>, what: &str) {
    let e = format!(
        "{:#}",
        res.expect_err(&format!("the push to {what} went ahead"))
    );
    assert!(
        e.contains("is inside export ro; ask for it by that export"),
        "{what}: {e}"
    );
}

/// The names in `dir`.
fn listed(dir: &std::path::Path) -> Vec<std::ffi::OsString> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect()
}

/// `m` at `<tmp>\.tmpAbC` and the read-only `ro` at `<tmp>\.TMPABC\ro`
/// (the shared part written in another case), `ro` not made yet: a push
/// through `m` in any spelling of `ro` is refused, and nothing is made.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_inner_root_configured_in_another_case_is_compared_from_the_shared_directory() {
    let h = Harness::with_other_export(
        "ro",
        |m| {
            let upper = m.file_name().unwrap().to_string_lossy().to_uppercase();
            assert_ne!(upper, m.file_name().unwrap().to_string_lossy(), "{m:?}");
            m.parent().unwrap().join(upper).join("ro")
        },
        |e| e.read_only = true,
    )
    .await;
    fs::write(h.local.path().join("f"), b"pushed").unwrap();
    for dir in ["RO/x", "ro/x", "Ro"] {
        refused_as_inside_ro(push(&h, dir).await, dir);
    }
    assert!(
        listed(h.export.path()).is_empty(),
        "made: {:?}",
        listed(h.export.path())
    );
}

/// The read-only `ro` at `<m>\Photos 2024\ro`, where `Photos 2024` exists
/// and `ro` not yet: the existing parent is judged by identity, so its
/// 8.3 short name is the same directory, and only `ro` by its name. The
/// exact spelling still meets the inner export's rules.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_missing_inner_root_below_a_parent_reached_by_its_short_name_is_refused() {
    const PARENT: &str = "Photos 2024";
    let h = Harness::with_other_export(
        "ro",
        |m| {
            fs::create_dir(m.join(PARENT)).unwrap();
            m.join(PARENT).join("ro")
        },
        |e| e.read_only = true,
    )
    .await;
    fs::write(h.local.path().join("f"), b"pushed").unwrap();
    let parent = h.export.path().join(PARENT);
    let e = push_error_at(&h, &format!("{PARENT}/ro/x")).await;
    assert!(e.contains("read-only"), "{e}");
    let Some(short) = short_name(&parent) else {
        eprintln!("skipping: this volume makes no 8.3 short names");
        return;
    };
    for dir in [
        format!("{short}/RO/x"),
        format!("{short}/ro"),
        format!("{}/Ro", PARENT.to_uppercase()),
    ] {
        refused_as_inside_ro(push(&h, &dir).await, &dir);
    }
    assert!(listed(&parent).is_empty(), "made: {:?}", listed(&parent));
}

/// A push to `m/<path>` that must fail; the error text.
async fn push_error_at(h: &Harness, path: &str) -> String {
    format!(
        "{:#}",
        push(h, path)
            .await
            .expect_err(&format!("the push to {path} went ahead"))
    )
}

/// An inner root that existed when an earlier request was checked and has
/// been removed since: nothing about it is kept, so a push in another
/// spelling is refused by its name, and nothing is made.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_inner_root_removed_after_a_request_is_compared_by_name() {
    let h = harness().await;
    let out = push(&h, "elsewhere").await.expect("the push to elsewhere");
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    fs::remove_dir_all(h.export.path().join("elsewhere")).unwrap();
    fs::remove_dir(h.export.path().join(DIR)).unwrap();
    let e = push_error(&h, &DIR.to_uppercase()).await;
    assert!(
        e.contains("is inside export ro; ask for it by that export"),
        "{e}"
    );
    assert!(
        listed(h.export.path()).is_empty(),
        "made: {:?}",
        listed(h.export.path())
    );
}

/// The read-only `ro` at `<m>\a\b\ro`, where only `a` exists: a push to
/// `m/A/B`, an ancestor of `ro`'s root, goes ahead; one into `ro` in
/// another spelling, through what that push made, does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_push_to_an_ancestor_of_a_missing_inner_root_is_allowed() {
    let h = Harness::with_other_export(
        "ro",
        |m| {
            fs::create_dir(m.join("a")).unwrap();
            m.join("a").join("b").join("ro")
        },
        |e| e.read_only = true,
    )
    .await;
    fs::write(h.local.path().join("f"), b"pushed").unwrap();
    let out = push(&h, "A/B").await.expect("the push to A/B");
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    let made = h.export.path().join("a");
    assert_eq!(listed(&made), ["B"]);
    assert_eq!(fs::read(made.join("B").join("f")).unwrap(), b"pushed");
    for dir in ["a/b/RO/x", "A/B/ro"] {
        refused_as_inside_ro(push(&h, dir).await, dir);
    }
    assert_eq!(listed(&made.join("B")), ["f"]);
}

/// A granted export on a drive that went away after the daemon started
/// (as a drive whose medium was taken out) is skipped without being
/// opened: requests to `m` go ahead. The drive is a `subst` of a temporary
/// directory, removed once the daemon runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_export_on_a_drive_that_went_away_does_not_disturb_other_requests() {
    let backing = tempfile::tempdir().unwrap();
    fs::create_dir(backing.path().join("e")).unwrap();
    let Some(drive) = subst_of(backing.path()) else {
        eprintln!("skipping: no free drive letter takes a subst");
        return;
    };
    let letter = drive.0;
    let h = Harness::with_other_export(
        "away",
        move |_| std::path::PathBuf::from(format!(r"{letter}:\e")),
        |_| {},
    )
    .await;
    drop(drive);
    assert!(
        fs::metadata(format!(r"{letter}:\")).is_err(),
        "{letter}: is still there"
    );
    fs::write(h.local.path().join("f"), b"pushed").unwrap();
    for dir in ["x", "E/y"] {
        let out = push(&h, dir)
            .await
            .unwrap_or_else(|e| panic!("{dir}: {e:#}"));
        assert!(out.errors.is_empty(), "{dir}: {:?}", out.errors);
    }
    assert_eq!(
        fs::read(h.export.path().join("x").join("f")).unwrap(),
        b"pushed"
    );
}

/// A granted export whose configured parent is a regular file cannot
/// exist: it is skipped, and requests to `m` go ahead.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_export_below_a_file_does_not_disturb_other_requests() {
    let elsewhere = tempfile::tempdir().unwrap();
    let file = elsewhere.path().join("file");
    // A directory while the daemon starts (which opens every export's
    // index and, as on Unix, refuses a root below a file), a file by the
    // time the requests come.
    fs::create_dir(&file).unwrap();
    let below = file.join("e");
    let h = Harness::with_other_export("below-file", move |_| below, |_| {}).await;
    fs::remove_dir(&file).unwrap();
    fs::write(&file, b"a file").unwrap();
    fs::write(h.local.path().join("f"), b"pushed").unwrap();
    let out = push(&h, "x").await.expect("the push to x");
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(
        fs::read(h.export.path().join("x").join("f")).unwrap(),
        b"pushed"
    );
}

/// The read-only `ro` configured through a junction that leads to `m`'s
/// own directory (`<j>\readonly-inner`, `<j>` a junction to `m`): the
/// junction is followed to the real directory, so `ro` is still caught in
/// another spelling, whether its root exists or not yet.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_inner_export_configured_through_a_junction_is_still_caught() {
    for exists in [true, false] {
        let links = tempfile::tempdir().unwrap();
        let j = links.path().join("j");
        let j2 = j.clone();
        let h = Harness::with_other_export(
            "ro",
            move |m| {
                irsync::platform::testutil::junction(m, &j2).unwrap();
                if exists {
                    fs::create_dir(m.join(DIR)).unwrap();
                }
                j2.join(DIR)
            },
            |e| e.read_only = true,
        )
        .await;
        fs::write(h.local.path().join("f"), b"pushed").unwrap();
        for dir in [DIR.to_uppercase(), "Readonly-Inner".to_string()] {
            refused_as_inside_ro(
                push(&h, &format!("{dir}/x")).await,
                &format!("{exists} {dir}"),
            );
        }
        let left = listed(h.export.path());
        if exists {
            assert_eq!(left, [DIR], "made: {left:?}");
            assert!(listed(&h.export.path().join(DIR)).is_empty());
        } else {
            assert!(left.is_empty(), "made: {left:?}");
        }
        // The junction goes before the directory it leads to.
        fs::remove_dir(&j).unwrap();
        drop(h);
    }
}

/// The read-only `ro` configured through a `subst` drive letter that maps
/// to `m`'s root (`X:\readonly-inner`): another start than `m`'s, but the
/// same directory, so its identity still catches a push through `m` in
/// another spelling.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_inner_export_configured_through_a_subst_drive_is_still_caught() {
    let made = std::sync::Arc::new(std::sync::Mutex::new(None::<Subst>));
    let slot = made.clone();
    let h = Harness::with_other_export(
        "ro",
        move |m| {
            fs::create_dir(m.join(DIR)).unwrap();
            match subst_of(m) {
                Some(d) => {
                    let p = std::path::PathBuf::from(format!(r"{}:\{DIR}", d.0));
                    *slot.lock().unwrap() = Some(d);
                    p
                }
                // Not reached by a request; the test skips below.
                None => m.join("unused"),
            }
        },
        |e| e.read_only = true,
    )
    .await;
    let Some(letter) = made.lock().unwrap().as_ref().map(|d| d.0) else {
        eprintln!("skipping: no free drive letter takes a subst");
        return;
    };
    assert!(fs::metadata(format!(r"{letter}:\{DIR}")).unwrap().is_dir());
    fs::write(h.local.path().join("f"), b"pushed").unwrap();
    for dir in [DIR.to_uppercase(), "Readonly-Inner".to_string()] {
        refused_as_inside_ro(push(&h, &format!("{dir}/x")).await, &dir);
    }
    assert!(listed(&h.export.path().join(DIR)).is_empty());
    // The drive goes when `made` is dropped, also when a check above fails.
}

/// The volume GUID path (`\\?\Volume{…}\`) of the volume `drive` (`C:\`),
/// from `mountvol`.
fn volume_guid_path(drive: &str) -> Option<String> {
    let out = std::process::Command::new("mountvol")
        .args([drive, "/L"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && text.starts_with(r"\\?\Volume{")).then_some(text)
}

/// The read-only `ro` configured by the volume GUID path of `m`'s own
/// volume (`\\?\Volume{…}\…\readonly-inner`): a local name, not a share,
/// so its root is opened and compared by identity, and a push through `m`
/// in another spelling is refused naming `ro`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_inner_export_configured_by_its_volume_guid_path_is_caught() {
    let tmp = std::env::temp_dir();
    let shown = tmp.display().to_string();
    let Some(drive) = shown.get(..3).filter(|d| d.ends_with(r":\")) else {
        eprintln!("skipping: the temp directory is not on a drive letter");
        return;
    };
    let Some(guid) = volume_guid_path(drive) else {
        eprintln!("skipping: mountvol gives no volume GUID path for {drive}");
        return;
    };
    let h = Harness::with_other_export(
        "ro",
        move |m| {
            fs::create_dir(m.join(DIR)).unwrap();
            let rest = m.join(DIR).display().to_string()[3..].to_string();
            std::path::PathBuf::from(format!("{guid}{rest}"))
        },
        |e| e.read_only = true,
    )
    .await;
    fs::write(h.local.path().join("f"), b"pushed").unwrap();
    for dir in [DIR.to_uppercase(), "Readonly-Inner".to_string()] {
        refused_as_inside_ro(push(&h, &format!("{dir}/x")).await, &dir);
    }
    assert!(listed(&h.export.path().join(DIR)).is_empty());
}
