//! Include/exclude rules reach the daemon (`Hello.filters`): a sending
//! daemon walks with the client's rules, in the client's order, with the
//! `*-from` files expanded on the client.

mod common;

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use common::{assert_trees_equal, Flow, Harness};
use irsync::cli::{Args, SyncOptions};
use irsync::filter::FilterSet;

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

/// Runs one session with the CLI's filters compiled on the client side.
async fn run(h: &Harness, o: &SyncOptions, flow: Flow) -> irsync::session::Outcome {
    let mut local = Harness::side_for(h.local.path(), h.client_state.path());
    local.filters = Arc::new(FilterSet::from_rules(&o.filters).unwrap());
    h.run_side(o, flow, local)
        .await
        .unwrap_or_else(|e| panic!("{flow:?}: {e:#}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exclude_applies_on_the_sending_side_in_both_directions() {
    within30(async {
        let o = opts(&["--exclude", "*.o", "--exclude", "build/"]);
        for flow in [Flow::Push, Flow::Pull] {
            let h = Harness::new().await;
            let (src, dst) = (h.src(flow), h.dst(flow));
            fs::write(src.join("keep.txt"), b"k").unwrap();
            fs::write(src.join("junk.o"), b"j").unwrap();
            fs::create_dir_all(src.join("build/x")).unwrap();
            fs::write(src.join("build/x/out"), b"b").unwrap();
            fs::create_dir(src.join("sub")).unwrap();
            fs::write(src.join("sub/also.o"), b"a").unwrap();
            fs::write(src.join("sub/text"), b"t").unwrap();
            let out = run(&h, &o, flow).await;
            assert!(out.errors.is_empty(), "{flow:?}: {:?}", out.errors);
            assert!(dst.join("keep.txt").exists(), "{flow:?}");
            assert!(dst.join("sub/text").exists(), "{flow:?}");
            for gone in ["junk.o", "build", "sub/also.o"] {
                assert!(
                    dst.join(gone).symlink_metadata().is_err(),
                    "{flow:?}: {gone} crossed despite the filter"
                );
            }
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn include_before_exclude_keeps_its_order_over_the_wire() {
    within30(async {
        // rsync: the first matching rule wins, so `--include keep.o` before
        // `--exclude *.o` keeps exactly that one object file.
        let o = opts(&["--include", "keep.o", "--exclude", "*.o"]);
        for flow in [Flow::Push, Flow::Pull] {
            let h = Harness::new().await;
            let (src, dst) = (h.src(flow), h.dst(flow));
            fs::write(src.join("keep.o"), b"k").unwrap();
            fs::write(src.join("drop.o"), b"d").unwrap();
            let out = run(&h, &o, flow).await;
            assert!(out.errors.is_empty(), "{flow:?}: {:?}", out.errors);
            assert!(dst.join("keep.o").exists(), "{flow:?}");
            assert!(!dst.join("drop.o").exists(), "{flow:?}");
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exclude_from_is_read_on_the_client_not_the_daemon() {
    within30(async {
        let rules = tempfile::NamedTempFile::new().unwrap();
        fs::write(rules.path(), "# objects\n*.o\n\ntmp/\n").unwrap();
        let path = rules.path().to_str().unwrap().to_string();
        let o = opts(&["--exclude-from", &path]);
        for flow in [Flow::Push, Flow::Pull] {
            let h = Harness::new().await;
            let (src, dst) = (h.src(flow), h.dst(flow));
            fs::write(src.join("a.txt"), b"a").unwrap();
            fs::write(src.join("b.o"), b"b").unwrap();
            fs::create_dir(src.join("tmp")).unwrap();
            fs::write(src.join("tmp/c"), b"c").unwrap();
            let out = run(&h, &o, flow).await;
            assert!(out.errors.is_empty(), "{flow:?}: {:?}", out.errors);
            assert!(dst.join("a.txt").exists(), "{flow:?}");
            assert!(!dst.join("b.o").exists(), "{flow:?}");
            assert!(dst.join("tmp").symlink_metadata().is_err(), "{flow:?}");
        }
        // Without rules the same trees sync in full.
        let h = Harness::new().await;
        fs::write(h.src(Flow::Push).join("b.o"), b"b").unwrap();
        let o = opts(&[]);
        run(&h, &o, Flow::Push).await;
        assert_trees_equal(h.src(Flow::Push), h.dst(Flow::Push), &o.preserve);
    })
    .await;
}
