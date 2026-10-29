//! `-r` is real: without it (and without `-a`) only the top-level entries
//! are transferred and directories are skipped; `-d` transfers the directories
//! themselves without their contents; `--delete` then never touches a
//! directory. Both directions, since the flags travel in `Hello`.

mod common;

use std::fs;
use std::time::Duration;

use common::{Flow, Harness};
use irsync::cli::{Args, SyncOptions};

async fn within30<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(30), f)
        .await
        .expect("test timed out after 30 s")
}

fn opts(flags: &[&str]) -> SyncOptions {
    let mut v = vec!["irsync"];
    v.extend_from_slice(flags);
    v.extend_from_slice(&["src/", "host:m/"]);
    SyncOptions::from_args(&Args::try_parse_from(v).unwrap())
}

fn seed(src: &std::path::Path) {
    fs::write(src.join("top.txt"), b"t").unwrap();
    std::os::unix::fs::symlink("top.txt", src.join("link")).unwrap();
    fs::create_dir_all(src.join("sub/deeper")).unwrap();
    fs::write(src.join("sub/inner.txt"), b"i").unwrap();
    fs::write(src.join("sub/deeper/x"), b"x").unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_r_only_the_top_level_is_transferred() {
    within30(async {
        for flags in [&["-tl"][..], &["-a", "--no-recursive"][..]] {
            for flow in [Flow::Push, Flow::Pull] {
                let h = Harness::new().await;
                let (src, dst) = (h.src(flow), h.dst(flow));
                seed(src);
                let out = h.run(&opts(flags), flow).await.unwrap();
                assert!(
                    out.errors.is_empty(),
                    "{flags:?} {flow:?}: {:?}",
                    out.errors
                );
                assert_eq!(
                    fs::read(dst.join("top.txt")).unwrap(),
                    b"t",
                    "{flags:?} {flow:?}"
                );
                assert_eq!(
                    fs::read_link(dst.join("link")).unwrap().to_str(),
                    Some("top.txt")
                );
                assert!(
                    dst.join("sub").symlink_metadata().is_err(),
                    "{flags:?} {flow:?}: a directory crossed without -r"
                );
            }
        }
        // With -r (or -a) the whole tree.
        for flow in [Flow::Push, Flow::Pull] {
            let h = Harness::new().await;
            seed(h.src(flow));
            let out = h.run(&opts(&["-rtl"]), flow).await.unwrap();
            assert!(out.errors.is_empty(), "{flow:?}: {:?}", out.errors);
            assert_eq!(fs::read(h.dst(flow).join("sub/deeper/x")).unwrap(), b"x");
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dirs_transfers_directories_without_their_contents() {
    within30(async {
        for flow in [Flow::Push, Flow::Pull] {
            let h = Harness::new().await;
            let (src, dst) = (h.src(flow), h.dst(flow));
            seed(src);
            let out = h.run(&opts(&["-td"]), flow).await.unwrap();
            assert!(out.errors.is_empty(), "{flow:?}: {:?}", out.errors);
            assert!(dst.join("sub").is_dir(), "{flow:?}");
            assert_eq!(
                fs::read_dir(dst.join("sub")).unwrap().count(),
                0,
                "{flow:?}"
            );
            assert_eq!(fs::read(dst.join("top.txt")).unwrap(), b"t");
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_without_r_never_touches_a_directory() {
    within30(async {
        for flags in [&["-t", "--delete"][..], &["-td", "--delete"][..]] {
            for flow in [Flow::Push, Flow::Pull] {
                let h = Harness::new().await;
                let (src, dst) = (h.src(flow), h.dst(flow));
                fs::write(src.join("keep.txt"), b"k").unwrap();
                fs::write(dst.join("old.txt"), b"o").unwrap();
                fs::create_dir_all(dst.join("gone")).unwrap();
                fs::write(dst.join("gone/precious"), b"p").unwrap();
                let out = h.run(&opts(flags), flow).await.unwrap();
                assert!(
                    out.errors.is_empty(),
                    "{flags:?} {flow:?}: {:?}",
                    out.errors
                );
                assert!(!dst.join("old.txt").exists(), "{flags:?} {flow:?}");
                assert_eq!(
                    fs::read(dst.join("gone/precious")).unwrap(),
                    b"p",
                    "{flags:?} {flow:?}: a directory was deleted into"
                );
                assert_eq!(
                    out.deleted.len(),
                    1,
                    "{flags:?} {flow:?}: {:?}",
                    out.deleted
                );
            }
        }
    })
    .await;
}
