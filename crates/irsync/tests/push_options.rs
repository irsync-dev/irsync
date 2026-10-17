//! Options a pushing client sends to the receiving daemon (`-u`, `-c`,
//! `-n -v`) and options a push refuses (`--partial-dir`).

mod common;

use std::fs;
use std::time::{Duration, SystemTime};

use common::{Flow, Harness};
use irsync::cli::{Args, SyncOptions};

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

fn set_mtime(p: &std::path::Path, secs: u64) {
    let t = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
    fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_update_keeps_newer_dest() {
    within30(async {
        let h = Harness::new().await;
        let (src, dst) = (h.local.path().join("f"), h.module.path().join("f"));
        fs::write(&src, b"older source").unwrap();
        set_mtime(&src, 1_600_000_000);
        fs::write(&dst, b"newer destination").unwrap();
        set_mtime(&dst, 1_600_000_100);

        let out = h.run(&opts(&["-u"]), Flow::Push).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(&dst).unwrap(), b"newer destination");

        // Without -u the daemon replaces it: the flag is what kept it.
        let out = h.run(&opts(&[]), Flow::Push).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(&dst).unwrap(), b"older source");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_checksum_detects_same_mtime_change() {
    within30(async {
        let h = Harness::new().await;
        let (src, dst) = (h.local.path().join("f"), h.module.path().join("f"));
        fs::write(&src, b"aaaa").unwrap();
        set_mtime(&src, 1_600_000_000);
        h.run(&opts(&[]), Flow::Push).await.unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"aaaa");

        // Same size and mtime, different content.
        fs::write(&src, b"bbbb").unwrap();
        set_mtime(&src, 1_600_000_000);
        h.run(&opts(&[]), Flow::Push).await.unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"aaaa", "quick check skips it");

        let out = h.run(&opts(&["-c"]), Flow::Push).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(&dst).unwrap(), b"bbbb");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_with_partial_dir_is_refused() {
    within30(async {
        let h = Harness::new().await;
        fs::write(h.local.path().join("f"), b"x").unwrap();
        let conn = h
            .client
            .connect(h.daemon.addr.clone(), irsync_proto::ALPN)
            .await
            .unwrap();
        let err = irsync::session::engine::run_client(
            conn.clone(),
            Harness::hello(Flow::Push),
            h.local_side(),
            &opts(&["--partial-dir", "/tmp/elsewhere"]),
        )
        .await
        .unwrap_err();
        conn.close(0u32.into(), b"done");
        assert!(format!("{err:#}").contains("--partial-dir"), "{err:#}");
        assert!(!h.module.path().join("f").exists());
    })
    .await;
}
