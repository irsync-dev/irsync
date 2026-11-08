//! `--fsync` and the export's `fsync` setting: received files are flushed
//! only when asked (rsync's default is no flush), on whichever side
//! receives.

mod common;

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use common::{Flow, Harness};

async fn within30<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(30), f)
        .await
        .expect("test timed out after 30 s")
}

fn opts(extra: &[&str]) -> irsync::cli::SyncOptions {
    let mut argv = vec!["irsync", "-a"];
    argv.extend_from_slice(extra);
    argv.extend(["src/", "host:m/"]);
    let args = irsync::cli::Args::try_parse_from(argv).unwrap();
    irsync::cli::SyncOptions::from_args(&args)
}

fn seed(dir: &std::path::Path) {
    fs::create_dir_all(dir.join("d")).unwrap();
    fs::write(dir.join("a.txt"), b"alpha").unwrap();
    fs::write(dir.join("d/b.txt"), b"beta").unwrap();
}

fn fsynced(events: &[String]) -> Vec<String> {
    let mut v: Vec<String> = events
        .iter()
        .filter(|e| e.starts_with("fsync:"))
        .cloned()
        .collect();
    v.sort();
    v
}

const BOTH: [&str; 2] = ["fsync:a.txt", "fsync:d/b.txt"];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_push_flushes_on_the_daemon_only_with_fsync() {
    within30(async {
        for (extra, want) in [(&[][..], &[][..]), (&["--fsync"][..], &BOTH[..])] {
            let h = Harness::new().await;
            seed(h.src(Flow::Push));
            let out = h.run(&opts(extra), Flow::Push).await.unwrap();
            assert!(out.errors.is_empty(), "{:?}", out.errors);
            assert_eq!(
                fs::read(h.dst(Flow::Push).join("d/b.txt")).unwrap(),
                b"beta"
            );
            let events = h.daemon.events.lock().unwrap().clone();
            assert_eq!(fsynced(&events), want, "{extra:?}: {events:?}");
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pull_flushes_on_the_client_only_with_fsync() {
    within30(async {
        for (extra, want) in [(&[][..], &[][..]), (&["--fsync"][..], &BOTH[..])] {
            let h = Harness::new().await;
            seed(h.src(Flow::Pull));
            let log: irsync::session::EventLog = Arc::default();
            let mut local = h.local_side();
            local.hooks.events = Some(log.clone());
            let out = h.run_side(&opts(extra), Flow::Pull, local).await.unwrap();
            assert!(out.errors.is_empty(), "{:?}", out.errors);
            assert_eq!(fs::read(h.dst(Flow::Pull).join("a.txt")).unwrap(), b"alpha");
            let events = log.lock().unwrap().clone();
            assert_eq!(fsynced(&events), want, "{extra:?}: {events:?}");
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_export_with_fsync_flushes_every_push_into_it() {
    within30(async {
        let h = Harness::with_export(|e| e.fsync = true).await;
        seed(h.src(Flow::Push));
        let out = h.run(&opts(&[]), Flow::Push).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let events = h.daemon.events.lock().unwrap().clone();
        assert_eq!(fsynced(&events), BOTH, "{events:?}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_export_with_fsync_does_not_make_a_pulling_client_flush() {
    within30(async {
        let h = Harness::with_export(|e| e.fsync = true).await;
        seed(h.src(Flow::Pull));
        let log: irsync::session::EventLog = Arc::default();
        let mut local = h.local_side();
        local.hooks.events = Some(log.clone());
        let out = h.run_side(&opts(&[]), Flow::Pull, local).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let events = log.lock().unwrap().clone();
        assert_eq!(fsynced(&events), Vec::<String>::new(), "{events:?}");
    })
    .await;
}
