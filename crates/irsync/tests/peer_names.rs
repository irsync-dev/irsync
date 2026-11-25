//! Names from a peer that Windows would read as more than one name (`..\x`,
//! `C:\x`, `a:b`) are refused where they enter: a request's partial dir and
//! name refuse the session, a listed entry fails by itself. Nothing is made
//! or read outside the export.
#![cfg(windows)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{archive_opts, Flow, Harness};
use irsync::cli::Args;
use irsync::options::SyncOptions;
use irsync::session::SyncHello;
use irsync_proto::v1::{file_entry::Kind, FileEntry};

async fn within30<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(30), f)
        .await
        .expect("test timed out after 30 s")
}

fn opts(extra: &[&str]) -> SyncOptions {
    let mut v = vec!["irsync", "-a"];
    v.extend_from_slice(extra);
    v.extend_from_slice(&["src/", "host:m/"]);
    SyncOptions::from_args(&Args::try_parse_from(v).unwrap())
}

/// The bytes the hooks below put into the client's `Hello` (a hook is a
/// plain `fn`, so it reads them from here; each test is its own process
/// under nextest, and the tests that set it do so in turn).
static FIELD: Mutex<Vec<u8>> = Mutex::new(Vec::new());

fn set_partial_dir(h: &mut SyncHello) {
    h.req.partial_dir = FIELD.lock().unwrap().clone();
}

fn set_name(h: &mut SyncHello) {
    h.req.name = FIELD.lock().unwrap().clone();
}

/// Names in `dir` that start with `prefix`.
fn named(dir: &Path, prefix: &str) -> Vec<String> {
    match fs::read_dir(dir) {
        Ok(rd) => rd
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(prefix))
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Removes what a failing run made outside, so a red run leaves no litter.
fn clean(paths: &[PathBuf]) {
    for p in paths {
        let _ = fs::remove_dir_all(p);
        let _ = fs::remove_file(p);
    }
}

/// A peer's partial dir that leaves the destination (`..`, a root, a
/// drive) is refused, and nothing is made outside the export.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_partial_dir_from_the_peer_never_leaves_the_destination() {
    let tag = format!("irsync-9b-pd-{}", std::process::id());
    let outside = tempfile::tempdir().unwrap();
    let outside_abs = format!(r"{}\{tag}", outside.path().display());
    let cases = [
        format!(r"..\{tag}"),
        format!(r"..\..\{tag}"),
        format!(r"\{tag}"),
        outside_abs.clone(),
        format!(r"a\..\..\{tag}"),
    ];
    for pd in cases {
        let h = Harness::new().await;
        fs::write(h.local.path().join("f"), vec![7u8; 1 << 20]).unwrap();
        let export = h.export.path().to_path_buf();
        let parent = export.parent().unwrap().to_path_buf();
        let grand = parent.parent().unwrap().to_path_buf();
        let root = PathBuf::from(format!(r"{}\", &export.display().to_string()[..2]));
        let places = [
            parent.join(&tag),
            grand.join(&tag),
            root.join(&tag),
            PathBuf::from(&outside_abs),
        ];
        let before: Vec<_> = [&parent, &grand, &root]
            .iter()
            .map(|d| named(d, &tag))
            .collect();
        *FIELD.lock().unwrap() = pd.as_bytes().to_vec();
        let mut local = h.local_side();
        local.hooks.tamper_hello = Some(set_partial_dir);
        let res = within30(h.run_side(&opts(&["--partial-dir", ".p"]), Flow::Push, local)).await;
        let after: Vec<_> = [&parent, &grand, &root]
            .iter()
            .map(|d| named(d, &tag))
            .collect();
        let made: Vec<_> = places.iter().filter(|p| p.exists()).cloned().collect();
        clean(&made);
        assert!(made.is_empty(), "{pd}: made outside the export: {made:?}");
        assert_eq!(before, after, "{pd}");
        let err = res.expect_err(&format!("{pd}: the daemon took the partial dir"));
        assert!(format!("{err:#}").contains("partial-dir"), "{pd}: {err:#}");
        assert!(!export.join("f").exists(), "{pd}: the push went ahead");
    }
}

/// A dry-run push whose name climbs out of a missing directory (`m/missing`
/// with the name `..\..\x`) is refused: the receiver lists nothing outside.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_name_from_the_peer_never_leads_a_dry_run_outside() {
    let h = Harness::new().await;
    fs::write(h.local.path().join("f"), b"f").unwrap();
    // A sibling of the export, with a file the dry run would offer to delete.
    let tag = format!("irsync-9b-name-{}", std::process::id());
    let outside = h.export.path().parent().unwrap().join(&tag);
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret"), b"s").unwrap();
    *FIELD.lock().unwrap() = format!(r"..\..\{tag}").into_bytes();
    let mut local = h.local_side();
    local.hooks.tamper_hello = Some(set_name);
    let mut hello = Harness::hello(Flow::Push);
    hello.req.path = b"m/missing".to_vec();
    let res = within30(h.run_hello(&opts(&["-n", "--delete"]), hello, local)).await;
    let listed = match &res {
        Ok(out) => out
            .would_delete
            .iter()
            .map(|p| String::from_utf8_lossy(p.as_bytes()).into_owned())
            .collect::<Vec<_>>(),
        Err(_) => Vec::new(),
    };
    let still = outside.join("secret").exists();
    clean(&[outside]);
    assert!(listed.is_empty(), "listed outside the export: {listed:?}");
    assert!(still, "the file outside was removed");
    let err = res.expect_err("the daemon took the name");
    assert!(format!("{err:#}").contains("invalid name"), "{err:#}");
}

fn entry(path: &[u8]) -> FileEntry {
    FileEntry {
        path: path.to_vec(),
        kind: Kind::Regular as i32,
        size: 1,
        mtime_sec: 1_600_000_000,
        mode: 0o100644,
        nlink: 1,
        ..FileEntry::default()
    }
}

/// A listed entry whose name Windows cannot hold fails by itself, with the
/// rule's message; the other files arrive.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_listed_name_windows_cannot_hold_fails_that_entry() {
    let h = Harness::new().await;
    fs::write(h.local.path().join("a"), b"a").unwrap();
    fs::write(h.local.path().join("z"), b"z").unwrap();
    let mut local = h.local_side();
    local.hooks.inject_listing = Some(Arc::new(|e: &FileEntry| {
        if e.path == b"a" {
            vec![entry(b"a:b"), entry(br"a\b")]
        } else {
            Vec::new()
        }
    }));
    let out = within30(h.run_side(&archive_opts(), Flow::Push, local))
        .await
        .expect("session");
    for bad in [&b"a:b"[..], br"a\b"] {
        assert!(
            // The daemon's own message, as the client reports a
            // receiver's errors.
            out.errors.iter().any(|(p, m)| p.as_bytes() == bad
                && m == "receiver: holds a character Windows does not allow"),
            "{:?}: {:?}",
            String::from_utf8_lossy(bad),
            out.errors
        );
    }
    assert_eq!(out.errors.len(), 2, "{:?}", out.errors);
    assert_eq!(fs::read(h.export.path().join("a")).unwrap(), b"a");
    assert_eq!(fs::read(h.export.path().join("z")).unwrap(), b"z");
    let names: Vec<_> = fs::read_dir(h.export.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !names
            .iter()
            .any(|n| n.starts_with("a:") || n.contains('\\')),
        "{names:?}"
    );
}

/// A listed symlink whose target Windows would read as a network or device
/// path fails by itself, and no link is made; a drive-absolute or relative
/// target is made as before.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_link_to_a_network_or_device_path_is_not_created() {
    if !irsync::platform::testutil::symlinks_available() {
        eprintln!("skipping: this process may not make symlinks");
        return;
    }
    let h = Harness::new().await;
    fs::write(h.local.path().join("a"), b"a").unwrap();
    let bad: [(&[u8], &[u8]); 4] = [
        (b"l-unc", br"\\127.0.0.1\C$\x"),
        (b"l-slashes", b"//server/share"),
        (b"l-verbatim", br"\\?\C:\x"),
        (b"l-device", br"\\.\pipe\x"),
    ];
    let good: [(&[u8], &[u8]); 2] = [(b"l-drive", br"C:\x"), (b"l-rel", br"..\y")];
    let link = |name: &[u8], target: &[u8]| FileEntry {
        path: name.to_vec(),
        kind: Kind::Symlink as i32,
        link_target: target.to_vec(),
        mtime_sec: 1_600_000_000,
        mode: 0o120777,
        nlink: 1,
        ..FileEntry::default()
    };
    let mut entries: Vec<FileEntry> = bad.iter().chain(&good).map(|(n, t)| link(n, t)).collect();
    // A listing is in path order.
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let mut local = h.local_side();
    local.hooks.inject_listing = Some(Arc::new(move |e: &FileEntry| {
        if e.path == b"a" {
            entries.clone()
        } else {
            Vec::new()
        }
    }));
    let out = within30(h.run_side(&archive_opts(), Flow::Push, local))
        .await
        .expect("session");
    for (name, _) in bad {
        assert!(
            out.errors.iter().any(|(p, m)| p.as_bytes() == name
                && m == "receiver: a link to a network or device path is not created on Windows"),
            "{:?}: {:?}",
            String::from_utf8_lossy(name),
            out.errors
        );
        let at = h.export.path().join(String::from_utf8_lossy(name).as_ref());
        assert!(at.symlink_metadata().is_err(), "{at:?} was made");
    }
    assert_eq!(out.errors.len(), bad.len(), "{:?}", out.errors);
    for (name, target) in good {
        let at = h.export.path().join(String::from_utf8_lossy(name).as_ref());
        assert_eq!(
            fs::read_link(&at).unwrap().as_os_str(),
            std::ffi::OsStr::new(std::str::from_utf8(target).unwrap()),
            "{at:?}"
        );
    }
    assert_eq!(fs::read(h.export.path().join("a")).unwrap(), b"a");
}

/// The same rule on a pull: a Windows client whose daemon lists a symlink
/// to a network path fails that entry and makes no link; the other files
/// arrive.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pulled_link_to_a_network_path_is_not_created() {
    if !irsync::platform::testutil::symlinks_available() {
        assert!(
            std::env::var_os("CI").is_none(),
            "CI must be able to make symlinks"
        );
        eprintln!("skipping: this process may not make symlinks");
        return;
    }
    let h = Harness::new().await;
    fs::write(h.export.path().join("a"), b"a").unwrap();
    h.daemon.hooks.lock().unwrap().inject_listing = Some(Arc::new(|e: &FileEntry| {
        if e.path == b"a" {
            vec![FileEntry {
                path: b"l-unc".to_vec(),
                kind: Kind::Symlink as i32,
                link_target: br"\\127.0.0.1\x\y".to_vec(),
                mtime_sec: 1_600_000_000,
                mode: 0o120777,
                nlink: 1,
                ..FileEntry::default()
            }]
        } else {
            Vec::new()
        }
    }));
    let out = within30(h.run_side(&archive_opts(), Flow::Pull, h.local_side()))
        .await
        .expect("session");
    let shown: Vec<String> = out
        .errors
        .iter()
        .map(|(p, m)| format!("{}: {m}", String::from_utf8_lossy(p.as_bytes())))
        .collect();
    assert_eq!(
        shown,
        ["l-unc: a link to a network or device path is not created on Windows"]
    );
    let at = h.local.path().join("l-unc");
    assert!(at.symlink_metadata().is_err(), "{at:?} was made");
    assert_eq!(fs::read(h.local.path().join("a")).unwrap(), b"a");
}

/// What a receiver could name in a `PlanItem` or a `CheckLink` to reach
/// past the sender's tree on Windows, with a label: a climb with `..`, a
/// file that exists outside, the same name where nothing exists, both of
/// them in the verbatim form (`\\?\C:\…`, which starts with `\\` as a
/// network path does, and is there on every machine), and a network path
/// (no share there: it must never be opened).
fn paths_out(outside: &Path) -> Vec<(&'static str, Vec<u8>)> {
    fs::write(outside.join("x"), b"outside").unwrap();
    let os = |p: PathBuf| p.to_string_lossy().into_owned().into_bytes();
    let verbatim = |p: PathBuf| format!(r"\\?\{}", p.display());
    let file = verbatim(outside.join("x"));
    assert!(fs::metadata(&file).is_ok(), "{file} does not name the file");
    let mut v = vec![
        ("a climb", br"..\..\x".to_vec()),
        ("a file outside", os(outside.join("x"))),
        ("nothing outside", os(outside.join("none"))),
        ("a verbatim file outside", file.into_bytes()),
        (
            "nothing outside, verbatim",
            verbatim(outside.join("none")).into_bytes(),
        ),
        ("a network path", br"\\127.0.0.1\nosuchshare\x".to_vec()),
    ];
    // The same file through this machine's administrative share, when this
    // user may reach it: a network path that exists.
    let local = outside.join("x").to_string_lossy().into_owned();
    let (drive, rest) = local.split_at(2);
    let share = format!(r"\\127.0.0.1\{}${rest}", drive.trim_end_matches(':'));
    if fs::metadata(&share).is_ok() {
        v.push(("an existing network path", share.into_bytes()));
    } else {
        eprintln!("skipping: the administrative share is not reachable for this user");
    }
    v
}

/// Runs a pull whose receiver (the client) names `path` in its first
/// `PlanItem`, or with `-H` in its `CheckLink`: the daemon's error, and how
/// long the session took.
async fn pull_naming(path: Vec<u8>, check_link: bool) -> (String, Duration) {
    use irsync_proto::v1::control::Msg;
    let h = Harness::new().await;
    fs::write(h.export.path().join("a"), b"linked").unwrap();
    if check_link {
        fs::hard_link(h.export.path().join("a"), h.export.path().join("b")).unwrap();
    }
    let mut local = h.local_side();
    local.hooks.tamper_frames = Some(Arc::new(move |msg| match msg {
        Msg::Plan(mut p) if !check_link => {
            p.path = path.clone();
            vec![Msg::Plan(p)]
        }
        Msg::CheckLink(mut c) if check_link => {
            c.path = path.clone();
            vec![Msg::CheckLink(c)]
        }
        m => vec![m],
    }));
    let opts = if check_link {
        opts(&["-H"])
    } else {
        archive_opts()
    };
    let start = std::time::Instant::now();
    let res = within30(h.run_side(&opts, Flow::Pull, local)).await;
    let took = start.elapsed();
    let err = match res {
        Err(e) => format!("{e:#}"),
        Ok(out) => panic!("the daemon took the path: {:?}", out.errors),
    };
    (err, took)
}

/// A `PlanItem` path Windows would read as more than one name is refused
/// by the name rule where it enters the daemon (a protocol error, before
/// any file is looked at), and the answer is the same whether something
/// exists outside or not.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plan_item_path_from_the_peer_is_refused_by_its_name() {
    let outside = tempfile::tempdir().unwrap();
    let mut answers = Vec::new();
    for (what, path) in paths_out(outside.path()) {
        let (err, took) = pull_naming(path, false).await;
        assert_eq!(
            fs::read(outside.path().join("x")).unwrap(),
            b"outside",
            "{what}: the file outside changed"
        );
        assert_eq!(named(outside.path(), ""), ["x"], "{what}: made outside");
        assert!(
            err.contains("PlanItem for file id")
                && err.contains("has an invalid path: holds a character Windows does not allow"),
            "{what}: {err}"
        );
        assert!(took < Duration::from_secs(5), "{what}: took {took:?}");
        answers.push(err);
    }
    assert!(answers.windows(2).all(|w| w[0] == w[1]), "{answers:#?}");
}

/// The same for a `CheckLink` path (`-H`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_check_link_path_from_the_peer_is_refused_by_its_name() {
    let outside = tempfile::tempdir().unwrap();
    let mut answers = Vec::new();
    for (what, path) in paths_out(outside.path()) {
        let (err, took) = pull_naming(path, true).await;
        assert_eq!(
            fs::read(outside.path().join("x")).unwrap(),
            b"outside",
            "{what}: the file outside changed"
        );
        assert_eq!(named(outside.path(), ""), ["x"], "{what}: made outside");
        assert!(
            err.contains(
                "CheckLink with an invalid path: holds a character Windows does not allow"
            ),
            "{what}: {err}"
        );
        assert!(took < Duration::from_secs(5), "{what}: took {took:?}");
        answers.push(err);
    }
    assert!(answers.windows(2).all(|w| w[0] == w[1]), "{answers:#?}");
}
