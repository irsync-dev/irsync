//! Options a pushing client sends to the receiving daemon (`-u`, `-c`,
//! `-n -v`, the preserve flags `-l -p -t -D -H`) and options a push refuses
//! (`--partial-dir`).

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
    opts_with("-a", extra)
}

/// `irsync <base> <extra> src/ host:m/`.
fn opts_with(base: &str, extra: &[&str]) -> SyncOptions {
    let mut v = vec!["irsync", base];
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
        let (src, dst) = (h.local.path().join("f"), h.export.path().join("f"));
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
        let (src, dst) = (h.local.path().join("f"), h.export.path().join("f"));
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
async fn dry_run_verbose_push_lists_files() {
    within30(async {
        let h = Harness::new().await;
        fs::write(h.local.path().join("new.txt"), b"x").unwrap();
        let local = h.local_side();
        let mut rx = local.hooks.bus.subscribe();
        let out = h
            .run_side(&opts(&["-n", "-v"]), Flow::Push, local)
            .await
            .unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let mut listed = Vec::new();
        while let Ok(e) = rx.try_recv() {
            if let irsync::session::Event::FileDone(p, _) = e {
                listed.push(String::from_utf8_lossy(p.as_bytes()).into_owned());
            }
        }
        // What the reporter prints under `-n -v` (FileDone lines).
        assert_eq!(listed, vec!["new.txt".to_string()]);
        assert!(!h.export.path().join("new.txt").exists());
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
        assert!(!h.export.path().join("f").exists());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_without_perms_or_times_leaves_them_to_the_daemon() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    within30(async {
        let h = Harness::new().await;
        let (src, dst) = (h.local.path().join("f"), h.export.path().join("f"));
        fs::write(&src, b"x").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o640)).unwrap();
        set_mtime(&src, 1_600_000_000);
        // Recursion only: the daemon creates the file with its own mode and
        // leaves the mtime at "now".
        let out = h.run(&opts_with("-r", &[]), Flow::Push).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let md = fs::metadata(&dst).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"x");
        assert_ne!(md.mode() & 0o777, 0o640, "-p was not asked for");
        assert_ne!(md.mtime(), 1_600_000_000, "-t was not asked for");
        // With -a the client's flags reach the daemon: mode and mtime copied.
        let out = h.run(&opts(&[]), Flow::Push).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let md = fs::metadata(&dst).unwrap();
        assert_eq!(md.mode() & 0o777, 0o640);
        assert_eq!(md.mtime(), 1_600_000_000);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_without_links_or_specials_skips_them() {
    use std::os::unix::fs::FileTypeExt;
    within30(async {
        let h = Harness::new().await;
        fs::write(h.local.path().join("f"), b"x").unwrap();
        std::os::unix::fs::symlink("f", h.local.path().join("l")).unwrap();
        nix::unistd::mkfifo(
            &h.local.path().join("p"),
            nix::sys::stat::Mode::from_bits_truncate(0o644),
        )
        .unwrap();
        let out = h.run(&opts_with("-rt", &[]), Flow::Push).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(h.export.path().join("f").exists());
        assert!(
            h.export.path().join("l").symlink_metadata().is_err(),
            "a symlink arrived without -l"
        );
        assert!(
            h.export.path().join("p").symlink_metadata().is_err(),
            "a fifo arrived without -D"
        );
        let out = h.run(&opts(&[]), Flow::Push).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(
            fs::read_link(h.export.path().join("l")).unwrap().to_str(),
            Some("f")
        );
        assert!(h
            .export
            .path()
            .join("p")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_fifo());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_hard_links_only_with_h() {
    use std::os::unix::fs::MetadataExt;
    within30(async {
        let h = Harness::new().await;
        fs::write(h.local.path().join("a"), b"same").unwrap();
        fs::hard_link(h.local.path().join("a"), h.local.path().join("b")).unwrap();
        let inode = |n: &str| fs::metadata(h.export.path().join(n)).unwrap().ino();
        let out = h.run(&opts(&[]), Flow::Push).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_ne!(inode("a"), inode("b"), "linked without -H");
        assert_eq!(fs::read(h.export.path().join("b")).unwrap(), b"same");
        let h2 = Harness::new().await;
        fs::write(h2.local.path().join("a"), b"same").unwrap();
        fs::hard_link(h2.local.path().join("a"), h2.local.path().join("b")).unwrap();
        let out = h2.run(&opts(&["-H"]), Flow::Push).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let inode2 = |n: &str| fs::metadata(h2.export.path().join(n)).unwrap().ino();
        assert_eq!(inode2("a"), inode2("b"), "not linked with -H");
    })
    .await;
}
