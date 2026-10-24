//! Deletions (`--delete*`, `--max-delete`, `--dry-run`, `--ignore-errors`)
//! end to end, in both directions: the receiver removes what the source no
//! longer has, only when asked, and never after an incomplete listing.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{assert_trees_equal, Flow, Harness};
use irsync::cli::{Args, SyncOptions};
use irsync::filter::FilterSet;
use irsync::fs::paths::RelPath;
use irsync::session::{LocalSide, Outcome};

async fn within30<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(30), f)
        .await
        .expect("test timed out after 30 s")
}

/// `irsync -a <extra> src/ host:m/` as the CLI would build it.
fn opts(extra: &[&str]) -> SyncOptions {
    let mut v = vec!["irsync", "-a"];
    v.extend_from_slice(extra);
    v.extend_from_slice(&["src/", "host:m/"]);
    SyncOptions::from_args(&Args::try_parse_from(v).unwrap())
}

fn is_root() -> bool {
    nix::unistd::geteuid().is_root()
}

/// The client's side with the CLI's filters and an event log.
fn side(h: &Harness, o: &SyncOptions) -> (LocalSide, Arc<Mutex<Vec<String>>>) {
    let mut s = Harness::side_for(h.local.path(), h.client_state.path());
    s.filters = Arc::new(FilterSet::from_rules(&o.filters).unwrap());
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    s.hooks.events = Some(log.clone());
    (s, log)
}

/// Runs one session; returns the outcome and the receiver's event log.
async fn run(h: &Harness, o: &SyncOptions, flow: Flow) -> (Outcome, Vec<String>) {
    h.daemon.events.lock().unwrap().clear();
    let (local, log) = side(h, o);
    let out = h
        .run_side(o, flow, local)
        .await
        .unwrap_or_else(|e| panic!("{flow:?}: {e:#}"));
    let events = match flow {
        Flow::Pull => log.lock().unwrap().clone(),
        Flow::Push => h.daemon.events.lock().unwrap().clone(),
    };
    (out, events)
}

fn paths(v: &[RelPath]) -> Vec<String> {
    let mut v: Vec<String> = v
        .iter()
        .map(|p| String::from_utf8_lossy(p.as_bytes()).into_owned())
        .collect();
    v.sort();
    v
}

/// Every path under `root` with its kind and contents (a snapshot to compare).
fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut m = BTreeMap::new();
    fn walk(root: &Path, dir: &Path, m: &mut BTreeMap<String, Vec<u8>>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            let t = e.file_type().unwrap();
            if t.is_dir() {
                m.insert(rel + "/", Vec::new());
                walk(root, &p, m);
            } else if t.is_symlink() {
                let target = fs::read_link(&p).unwrap();
                m.insert(rel, target.to_string_lossy().as_bytes().to_vec());
            } else {
                m.insert(rel, fs::read(&p).unwrap());
            }
        }
    }
    walk(root, root, &mut m);
    m
}

fn first(events: &[String], prefix: &str) -> Option<usize> {
    events.iter().position(|e| e.starts_with(prefix))
}

fn last(events: &[String], prefix: &str) -> Option<usize> {
    events.iter().rposition(|e| e.starts_with(prefix))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_delete_by_default() {
    within30(async {
        let o = opts(&[]);
        for flow in [Flow::Push, Flow::Pull] {
            let h = Harness::new().await;
            fs::write(h.src(flow).join("a.txt"), b"a").unwrap();
            fs::write(h.dst(flow).join("extra.txt"), b"x").unwrap();
            fs::create_dir_all(h.dst(flow).join("extra_dir/deep")).unwrap();
            fs::write(h.dst(flow).join("extra_dir/deep/y"), b"y").unwrap();
            let (out, events) = run(&h, &o, flow).await;
            assert!(out.errors.is_empty(), "{flow:?}: {:?}", out.errors);
            assert!(h.dst(flow).join("extra.txt").exists(), "{flow:?}");
            assert!(h.dst(flow).join("extra_dir/deep/y").exists(), "{flow:?}");
            assert!(out.deleted.is_empty() && out.would_delete.is_empty());
            assert!(first(&events, "delete:").is_none(), "{flow:?}: {events:?}");
            assert_eq!(out.exit_code(), 0);
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_removes_extraneous_after_transfer() {
    within30(async {
        let o = opts(&["--delete"]);
        for flow in [Flow::Push, Flow::Pull] {
            let h = Harness::new().await;
            let outside = tempfile::tempdir().unwrap();
            fs::write(outside.path().join("precious"), b"keep me").unwrap();
            let (src, dst) = (h.src(flow), h.dst(flow));
            fs::create_dir(src.join("sub")).unwrap();
            fs::write(src.join("keep.txt"), b"keep").unwrap();
            fs::write(src.join("sub/new.txt"), b"new").unwrap();
            fs::create_dir_all(dst.join("sub")).unwrap();
            fs::write(dst.join("old.txt"), b"old").unwrap();
            fs::write(dst.join("sub/old2.txt"), b"old2").unwrap();
            fs::create_dir_all(dst.join("gone/deep")).unwrap();
            fs::write(dst.join("gone/deep/x.txt"), b"x").unwrap();
            // A symlink to a directory outside: removed, never followed.
            symlink(outside.path(), dst.join("gone/link")).unwrap();

            let (out, events) = run(&h, &o, flow).await;
            assert!(out.errors.is_empty(), "{flow:?}: {:?}", out.errors);
            assert_trees_equal(src, dst, &o.preserve);
            assert_eq!(
                fs::read(outside.path().join("precious")).unwrap(),
                b"keep me"
            );
            assert_eq!(
                paths(&out.deleted),
                [
                    "gone",
                    "gone/deep",
                    "gone/deep/x.txt",
                    "gone/link",
                    "old.txt",
                    "sub/old2.txt"
                ],
                "{flow:?}"
            );
            // Deepest first.
            let pos = |p: &str| events.iter().position(|e| e == &format!("delete:{p}"));
            assert!(pos("gone/deep/x.txt") < pos("gone/deep"), "{events:?}");
            assert!(pos("gone/deep") < pos("gone"), "{events:?}");
            // After the transfers.
            assert!(
                last(&events, "plan:").unwrap() < first(&events, "delete:").unwrap(),
                "{flow:?}: {events:?}"
            );
            assert_eq!(out.exit_code(), 0);
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_before_frees_space_first() {
    within30(async {
        for mode in ["--delete-before", "--delete-during"] {
            let o = opts(&[mode]);
            for flow in [Flow::Push, Flow::Pull] {
                let h = Harness::new().await;
                fs::write(h.src(flow).join("new.bin"), vec![7u8; 100_000]).unwrap();
                fs::write(h.src(flow).join("z.bin"), vec![8u8; 1000]).unwrap();
                fs::write(h.dst(flow).join("a_old.bin"), vec![1u8; 100_000]).unwrap();
                fs::write(h.dst(flow).join("old.bin"), vec![1u8; 100_000]).unwrap();
                let (out, events) = run(&h, &o, flow).await;
                assert!(out.errors.is_empty(), "{mode} {flow:?}: {:?}", out.errors);
                assert_trees_equal(h.src(flow), h.dst(flow), &o.preserve);
                assert_eq!(paths(&out.deleted), ["a_old.bin", "old.bin"]);
                if mode == "--delete-before" {
                    assert!(
                        last(&events, "delete:").unwrap() < first(&events, "plan:").unwrap(),
                        "{flow:?}: {events:?}"
                    );
                }
            }
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn max_delete_stops_and_exit_25() {
    within30(async {
        let o = opts(&["--delete", "--max-delete=2"]);
        for flow in [Flow::Push, Flow::Pull] {
            let h = Harness::new().await;
            fs::write(h.src(flow).join("keep.txt"), b"k").unwrap();
            for i in 0..5 {
                fs::write(h.dst(flow).join(format!("x{i}")), b"x").unwrap();
            }
            let (out, _) = run(&h, &o, flow).await;
            assert_eq!(out.deleted.len(), 2, "{flow:?}: {:?}", out.deleted);
            let left = (0..5)
                .filter(|i| h.dst(flow).join(format!("x{i}")).exists())
                .count();
            assert_eq!(left, 3, "{flow:?}");
            assert!(out.max_delete_reached, "{flow:?}");
            assert_eq!(out.exit_code(), 25, "{flow:?}");
            assert!(h.dst(flow).join("keep.txt").exists());
        }
        // --max-delete=0 deletes nothing (and still says so).
        let o = opts(&["--delete", "--max-delete=0"]);
        for flow in [Flow::Push, Flow::Pull] {
            let h = Harness::new().await;
            fs::write(h.dst(flow).join("x"), b"x").unwrap();
            let (out, _) = run(&h, &o, flow).await;
            assert!(h.dst(flow).join("x").exists(), "{flow:?}");
            assert!(out.deleted.is_empty());
            assert_eq!(out.exit_code(), 25, "{flow:?}");
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_refused_when_listing_incomplete() {
    if is_root() {
        eprintln!("skipped: permissions do not restrict root");
        return;
    }
    within30(async {
        for flow in [Flow::Push, Flow::Pull] {
            let h = Harness::new().await;
            let (src, dst) = (h.src(flow), h.dst(flow));
            fs::write(src.join("ok.txt"), b"ok").unwrap();
            fs::create_dir(src.join("locked")).unwrap();
            fs::write(src.join("locked/secret.txt"), b"s").unwrap();
            fs::create_dir(dst.join("locked")).unwrap();
            fs::write(dst.join("locked/secret.txt"), b"s").unwrap();
            fs::write(dst.join("extra.txt"), b"x").unwrap();
            fs::set_permissions(src.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();

            let (out, events) = run(&h, &opts(&["--delete"]), flow).await;
            assert!(dst.join("extra.txt").exists(), "{flow:?}");
            assert!(out.deleted.is_empty(), "{flow:?}");
            assert!(first(&events, "delete:").is_none(), "{flow:?}: {events:?}");
            assert!(
                out.errors
                    .iter()
                    .any(|(_, m)| m.contains("deletions skipped")),
                "{flow:?}: {:?}",
                out.errors
            );
            assert_eq!(out.exit_code(), 23);

            // The destination copy of `locked` got mode 000 too; reopen it so
            // only the source listing is incomplete.
            fs::set_permissions(dst.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
            assert!(dst.join("locked/secret.txt").exists(), "{flow:?}");
            let (out, _) = run(&h, &opts(&["--delete", "--ignore-errors"]), flow).await;
            assert!(
                !dst.join("extra.txt").exists(),
                "{flow:?}: {:?}",
                out.errors
            );
            assert!(paths(&out.deleted).contains(&"extra.txt".to_string()));
            fs::set_permissions(src.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
            let _ = fs::set_permissions(dst.join("locked"), fs::Permissions::from_mode(0o755));
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dry_run_lists_without_deleting() {
    within30(async {
        let o = opts(&["--delete", "-n"]);
        for flow in [Flow::Push, Flow::Pull] {
            let h = Harness::new().await;
            let (src, dst) = (h.src(flow), h.dst(flow));
            fs::write(src.join("keep.txt"), b"new contents").unwrap();
            fs::write(src.join("new.txt"), b"new").unwrap();
            fs::create_dir(src.join("newdir")).unwrap();
            fs::write(src.join("newdir/f"), b"f").unwrap();
            fs::write(dst.join("keep.txt"), b"old").unwrap();
            fs::write(dst.join("extra.txt"), b"x").unwrap();
            fs::create_dir(dst.join("extradir")).unwrap();
            fs::write(dst.join("extradir/y"), b"y").unwrap();
            let before = snapshot(dst);

            let (out, events) = run(&h, &o, flow).await;
            assert!(out.errors.is_empty(), "{flow:?}: {:?}", out.errors);
            assert_eq!(snapshot(dst), before, "{flow:?}: the destination changed");
            assert!(out.deleted.is_empty());
            assert_eq!(
                paths(&out.would_delete),
                ["extra.txt", "extradir", "extradir/y"],
                "{flow:?}"
            );
            assert_eq!(out.files_transferred, 0);
            assert_eq!(out.bytes_transferred, 0);
            assert!(first(&events, "plan:").is_none(), "{flow:?}: {events:?}");
            assert_eq!(out.exit_code(), 0);
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dry_run_push_into_missing_path_creates_nothing() {
    within30(async {
        let h = Harness::new().await;
        fs::write(h.local.path().join("a"), b"a").unwrap();
        let mut hello = Harness::hello(Flow::Push);
        hello.path = b"not/there".to_vec();
        let o = opts(&["--delete", "-n"]);
        let (local, _) = side(&h, &o);
        let out = h.run_hello(&o, hello, local).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(!h.export.path().join("not").exists());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_excluded_removes_excluded_dst_files() {
    within30(async {
        let o = opts(&["--delete-excluded", "--exclude", "*.o"]);
        for flow in [Flow::Push, Flow::Pull] {
            let h = Harness::new().await;
            let (src, dst) = (h.src(flow), h.dst(flow));
            fs::write(src.join("keep.txt"), b"k").unwrap();
            fs::create_dir(src.join("sub")).unwrap();
            fs::create_dir(dst.join("sub")).unwrap();
            fs::write(dst.join("junk.o"), b"j").unwrap();
            fs::write(dst.join("sub/more.o"), b"m").unwrap();
            fs::write(dst.join("other.txt"), b"o").unwrap();
            let (out, _) = run(&h, &o, flow).await;
            assert!(out.errors.is_empty(), "{flow:?}: {:?}", out.errors);
            assert_eq!(
                paths(&out.deleted),
                ["junk.o", "other.txt", "sub/more.o"],
                "{flow:?}"
            );
            assert!(dst.join("keep.txt").exists());
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn excluded_dst_files_are_protected_without_delete_excluded() {
    within30(async {
        let o = opts(&["--delete", "--exclude", "*.o", "--exclude", "build/"]);
        // Pull: the client receives and applies its own filters.
        let h = Harness::new().await;
        let dst = h.dst(Flow::Pull);
        fs::write(h.src(Flow::Pull).join("keep.txt"), b"k").unwrap();
        fs::write(dst.join("junk.o"), b"j").unwrap();
        fs::create_dir_all(dst.join("build/x")).unwrap();
        fs::write(dst.join("build/x/out"), b"b").unwrap();
        fs::create_dir(dst.join("gone")).unwrap();
        fs::write(dst.join("gone/a.o"), b"a").unwrap();
        fs::write(dst.join("gone/b.txt"), b"b").unwrap();
        fs::write(dst.join("other.txt"), b"o").unwrap();
        let (out, _) = run(&h, &o, Flow::Pull).await;
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(dst.join("junk.o").exists());
        assert!(dst.join("build/x/out").exists());
        // An extraneous directory holding a protected file stays (without error).
        assert!(dst.join("gone/a.o").exists());
        assert_eq!(paths(&out.deleted), ["gone/b.txt", "other.txt"]);

        // Push: the daemon cannot see the client's filters, so it would treat
        // excluded files as extraneous; the client refuses.
        let h = Harness::new().await;
        fs::write(h.export.path().join("junk.o"), b"j").unwrap();
        let (local, _) = side(&h, &o);
        let conn = h
            .client
            .connect(h.daemon.addr.clone(), irsync_proto::ALPN)
            .await
            .unwrap();
        let res = irsync::session::engine::run_client(
            conn.clone(),
            Harness::hello(Flow::Push),
            local,
            &o,
        )
        .await;
        conn.close(0u32.into(), b"done");
        let err = res.unwrap_err();
        assert!(format!("{err:#}").contains("--delete-excluded"), "{err:#}");
        assert!(h.export.path().join("junk.o").exists());
    })
    .await;
}

#[tokio::test]
async fn delete_pass_reports_progress_per_candidate() {
    use irsync::cli::DeleteMode;
    use irsync::delete::DeletePass;
    use irsync_proto::v1::file_entry::Kind;
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("a"), b"a").unwrap();
    fs::write(root.path().join("b"), b"b").unwrap();
    let mut pass = DeletePass::new(DeleteMode::After, None, false, false, false);
    pass.plan(RelPath::new(b"a").unwrap(), Kind::Regular);
    pass.plan(RelPath::new(b"b").unwrap(), Kind::Regular);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    pass.set_progress(Arc::new(move |d, t| s.lock().unwrap().push((d, t))));
    let rep = pass
        .execute(root.path().to_path_buf(), false)
        .await
        .unwrap();
    assert_eq!(rep.deleted.len(), 2);
    assert_eq!(*seen.lock().unwrap(), vec![(0, 2), (1, 2), (2, 2)]);
}
