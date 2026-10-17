//! End-to-end sync sessions between an in-process daemon and a client over
//! loopback iroh endpoints, in both directions.

mod common;

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use common::{archive_opts, assert_trees_equal, sync_trees, Flow, Harness};
use irsync::fs::paths::RelPath;
use irsync::session::SideHooks;

const KIB: usize = 1024;
const MIB: usize = 1024 * 1024;

/// Every test must finish well inside this.
async fn within30<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(30), f)
        .await
        .expect("test timed out after 30 s")
}

/// Deterministic pseudo-random bytes.
fn data(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2654435761).wrapping_add(1);
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

fn set_mtime(p: &Path, secs: u64, nanos: u32) {
    let t = SystemTime::UNIX_EPOCH + Duration::new(secs, nanos);
    fs::File::open(p).unwrap().set_modified(t).unwrap();
}

/// A tree with most kinds of entry: nested dirs, empty dir and file, a
/// multi-chunk file, an executable, a symlink and a FIFO.
fn mixed_tree(root: &Path) {
    fs::create_dir_all(root.join("a/b/c")).unwrap();
    fs::create_dir(root.join("empty")).unwrap();
    fs::write(root.join("empty.txt"), b"").unwrap();
    fs::write(root.join("small.txt"), b"hello irsync\n").unwrap();
    fs::write(root.join("a/big.bin"), data(3 * MIB + 12345, 1)).unwrap();
    fs::write(root.join("a/b/mid.bin"), data(300 * KIB, 2)).unwrap();
    fs::write(root.join("a/b/c/run.sh"), b"#!/bin/sh\necho hi\n").unwrap();
    fs::set_permissions(root.join("a/b/c/run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(root.join("small.txt"), fs::Permissions::from_mode(0o640)).unwrap();
    symlink("b/mid.bin", root.join("a/link")).unwrap();
    nix::unistd::mkfifo(
        &root.join("fifo"),
        nix::sys::stat::Mode::from_bits_truncate(0o644),
    )
    .unwrap();
    set_mtime(&root.join("small.txt"), 1_500_000_000, 123_456_789);
}

fn regular_files(root: &Path) -> u64 {
    let mut n = 0;
    for e in fs::read_dir(root).unwrap() {
        let e = e.unwrap();
        let t = e.file_type().unwrap();
        if t.is_dir() {
            n += regular_files(&e.path());
        } else if t.is_file() {
            n += 1;
        }
    }
    n
}

async fn fresh_tree_byte_exact(flow: Flow) {
    let opts = archive_opts();
    let (out, h) = sync_trees(mixed_tree, |_| {}, &opts, flow).await;
    assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
    assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
    assert_eq!(out.files_transferred, regular_files(h.src(flow)));
    assert!(out.bytes_transferred > 3 * MIB as u64);
    assert_eq!(out.exit_code(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_fresh_tree_byte_exact() {
    within30(fresh_tree_byte_exact(Flow::Push)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pull_fresh_tree_byte_exact() {
    within30(fresh_tree_byte_exact(Flow::Pull)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn second_run_transfers_nothing() {
    within30(async {
        let opts = archive_opts();
        for flow in [Flow::Push, Flow::Pull] {
            let (first, h) = sync_trees(mixed_tree, |_| {}, &opts, flow).await;
            assert!(first.errors.is_empty(), "{flow:?}: {:?}", first.errors);
            // Finished files leave no per-file state (chunk lists) behind.
            assert_eq!(first.retained_files, 0, "{flow:?}: retained per-file state");
            let daemon_hashed = h.daemon_files_hashed();

            let second = h.run(&opts, flow).await.unwrap();
            assert!(second.errors.is_empty(), "{flow:?}: {:?}", second.errors);
            assert_eq!(second.bytes_transferred, 0, "{flow:?}");
            assert_eq!(second.files_transferred, 0, "{flow:?}");
            assert_eq!(second.files_hashed, 0, "{flow:?}: client hashed files");
            assert_eq!(
                h.daemon_files_hashed(),
                daemon_hashed,
                "{flow:?}: daemon hashed files"
            );
            assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn modified_file_sends_only_changed_chunks() {
    within30(async {
        let opts = archive_opts();
        let flow = Flow::Push;
        let (first, h) = sync_trees(
            |r| fs::write(r.join("big.bin"), data(4 * MIB, 7)).unwrap(),
            |_| {},
            &opts,
            flow,
        )
        .await;
        assert!(first.errors.is_empty(), "{:?}", first.errors);

        let mut grown = data(4 * MIB, 7);
        grown.extend_from_slice(&data(100 * KIB, 8));
        fs::write(h.src(flow).join("big.bin"), &grown).unwrap();

        let second = h.run(&opts, flow).await.unwrap();
        assert!(second.errors.is_empty(), "{:?}", second.errors);
        assert_eq!(second.files_transferred, 1);
        assert!(
            second.bytes_transferred < 600 * KIB as u64,
            "sent {} bytes for a 100 KiB append",
            second.bytes_transferred
        );
        assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn renamed_file_is_local_copy() {
    within30(async {
        let opts = archive_opts();
        let flow = Flow::Push;
        let content = data(2 * MIB + 77, 9);
        let (first, h) = sync_trees(
            |r| fs::write(r.join("photo.jpg"), &content).unwrap(),
            |_| {},
            &opts,
            flow,
        )
        .await;
        assert!(first.errors.is_empty(), "{:?}", first.errors);

        fs::create_dir(h.src(flow).join("album")).unwrap();
        fs::rename(
            h.src(flow).join("photo.jpg"),
            h.src(flow).join("album/renamed.jpg"),
        )
        .unwrap();

        let second = h.run(&opts, flow).await.unwrap();
        assert!(second.errors.is_empty(), "{:?}", second.errors);
        assert_eq!(second.bytes_transferred, 0);
        assert_eq!(second.files_transferred, 1);
        assert_eq!(
            fs::read(h.dst(flow).join("album/renamed.jpg")).unwrap(),
            content
        );
        // No deletions in this run: the old name stays.
        assert!(h.dst(flow).join("photo.jpg").exists());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sync_non_utf8_filename() {
    within30(async {
        let opts = archive_opts();
        let probe = tempfile::tempdir().unwrap();
        if common::try_create_non_utf8(probe.path(), b"caf\xe9.txt").is_none() {
            return;
        }
        let dir = OsStr::from_bytes(b"d\xff\xfe");
        let file = OsStr::from_bytes(b"caf\xe9.txt");
        for flow in [Flow::Push, Flow::Pull] {
            let (out, h) = sync_trees(
                |r| {
                    fs::create_dir(r.join(dir)).unwrap();
                    fs::write(r.join(dir).join(file), b"latin-1 name").unwrap();
                    fs::write(r.join(file), data(500 * KIB, 3)).unwrap();
                },
                |_| {},
                &opts,
                flow,
            )
            .await;
            assert!(out.errors.is_empty(), "{flow:?}: {:?}", out.errors);
            assert_eq!(out.files_transferred, 2, "{flow:?}");
            assert_eq!(
                fs::read(h.dst(flow).join(dir).join(file)).unwrap(),
                b"latin-1 name"
            );
            assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn symlink_outside_root_not_followed() {
    within30(async {
        let opts = archive_opts();
        for flow in [Flow::Push, Flow::Pull] {
            let (out, h) = sync_trees(
                |r| {
                    fs::create_dir(r.join("sub")).unwrap();
                    symlink("../../etc/passwd", r.join("sub/escape")).unwrap();
                    symlink("/etc", r.join("etc")).unwrap();
                },
                |_| {},
                &opts,
                flow,
            )
            .await;
            assert!(out.errors.is_empty(), "{flow:?}: {:?}", out.errors);
            assert_eq!(out.files_transferred, 0, "{flow:?}");
            assert_eq!(out.bytes_transferred, 0, "{flow:?}");
            let esc = h.dst(flow).join("sub/escape");
            assert!(fs::symlink_metadata(&esc).unwrap().file_type().is_symlink());
            assert_eq!(fs::read_link(&esc).unwrap(), Path::new("../../etc/passwd"));
            let etc = h.dst(flow).join("etc");
            assert!(fs::symlink_metadata(&etc).unwrap().file_type().is_symlink());
            assert_eq!(fs::read_link(&etc).unwrap(), Path::new("/etc"));
            assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_changed_during_transfer_is_resent() {
    within30(async {
        let opts = archive_opts();
        let flow = Flow::Push;
        let h = Harness::new().await;
        let src = h.src(flow).to_path_buf();
        fs::write(src.join("x.bin"), data(4 * MIB, 11)).unwrap();
        fs::write(src.join("other.txt"), b"unchanged").unwrap();
        let new_content = data(3 * MIB + 5, 12);

        let x = RelPath::new(b"x.bin").unwrap();
        let fired = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mut local = h.local_side();
        local.hooks = SideHooks {
            on_first_chunk_sent: Some({
                let (src, new_content, fired) = (src.clone(), new_content.clone(), fired.clone());
                Arc::new(move |p: &RelPath| {
                    let hit = *p == x;
                    let (src, new_content, fired) =
                        (src.clone(), new_content.clone(), fired.clone());
                    Box::pin(async move {
                        if hit {
                            fired.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            fs::write(src.join("x.bin"), &new_content).unwrap();
                        }
                    })
                })
            }),
            ..SideHooks::default()
        };

        let out = h.run_side(&opts, flow, local).await.unwrap();
        assert_eq!(fired.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(h.dst(flow).join("x.bin")).unwrap(), new_content);
        assert_eq!(
            fs::read(h.dst(flow).join("other.txt")).unwrap(),
            b"unchanged"
        );
        // Nothing half-written is left behind under a temporary name.
        assert!(!h.dst(flow).join(".irsync.x.bin.part").exists());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn directory_mtimes_preserved() {
    within30(async {
        let opts = archive_opts();
        for flow in [Flow::Push, Flow::Pull] {
            let (out, h) = sync_trees(
                |r| {
                    fs::create_dir_all(r.join("a/b/c")).unwrap();
                    fs::create_dir(r.join("z")).unwrap();
                    fs::write(r.join("a/b/c/f.txt"), b"f").unwrap();
                    fs::write(r.join("a/g.txt"), data(200 * KIB, 4)).unwrap();
                    fs::write(r.join("z/h.txt"), b"h").unwrap();
                    // Deepest first, so setting a child does not disturb its parent.
                    for (i, d) in ["a/b/c", "a/b", "a", "z"].iter().enumerate() {
                        set_mtime(&r.join(d), 1_200_000_000 + i as u64 * 1000, 500 + i as u32);
                    }
                    set_mtime(r, 1_300_000_000, 42);
                },
                |_| {},
                &opts,
                flow,
            )
            .await;
            assert!(out.errors.is_empty(), "{flow:?}: {:?}", out.errors);
            for d in ["", "a", "a/b", "a/b/c", "z"] {
                let (s, t) = (
                    fs::metadata(h.src(flow).join(d)).unwrap(),
                    fs::metadata(h.dst(flow).join(d)).unwrap(),
                );
                assert_eq!(
                    (s.mtime(), s.mtime_nsec()),
                    (t.mtime(), t.mtime_nsec()),
                    "{flow:?}: mtime of {d:?}"
                );
            }
            assert_trees_equal(h.src(flow), h.dst(flow), &opts.preserve);
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exit_code_23_on_partial_failure() {
    if nix::unistd::geteuid().is_root() {
        eprintln!("skipping: root can read a mode-000 file");
        return;
    }
    within30(async {
        let opts = archive_opts();
        let flow = Flow::Push;
        let h = Harness::new().await;
        let src = h.src(flow);
        fs::write(src.join("ok.txt"), b"fine").unwrap();
        fs::write(src.join("secret.txt"), b"unreadable").unwrap();
        fs::set_permissions(src.join("secret.txt"), fs::Permissions::from_mode(0o000)).unwrap();

        let out = h.run(&opts, flow).await.unwrap();
        fs::set_permissions(src.join("secret.txt"), fs::Permissions::from_mode(0o600)).unwrap();

        assert_eq!(out.exit_code(), 23);
        assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
        assert_eq!(out.errors[0].0, RelPath::new(b"secret.txt").unwrap());
        assert_eq!(out.files_transferred, 1);
        assert_eq!(fs::read(h.dst(flow).join("ok.txt")).unwrap(), b"fine");
        assert!(!h.dst(flow).join("secret.txt").exists());
    })
    .await;
}

fn is_root() -> bool {
    nix::unistd::geteuid().is_root()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_with_unwritable_dest_dir_exits_23() {
    if is_root() {
        eprintln!("skipping: root can write into a read-only directory");
        return;
    }
    within30(async {
        let opts = archive_opts();
        let flow = Flow::Push;
        let h = Harness::new().await;
        let (src, dst) = (h.src(flow).to_path_buf(), h.dst(flow).to_path_buf());
        fs::create_dir_all(src.join("ro/sub")).unwrap();
        fs::write(src.join("ro/sub/f.txt"), b"under a dir that cannot be made").unwrap();
        fs::write(src.join("ro/g.txt"), b"cannot be placed").unwrap();
        fs::write(src.join("ok.txt"), b"fine").unwrap();
        fs::create_dir(dst.join("ro")).unwrap();
        fs::set_permissions(dst.join("ro"), fs::Permissions::from_mode(0o555)).unwrap();

        let out = h.run(&opts, flow).await.unwrap();
        fs::set_permissions(dst.join("ro"), fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(out.exit_code(), 23, "{:?}", out.errors);
        let failed: Vec<&[u8]> = out.errors.iter().map(|(p, _)| p.as_bytes()).collect();
        // The mkdir failure and its child are only known to the daemon: they
        // reach the client through SessionDone.
        assert!(failed.contains(&&b"ro/sub"[..]), "{:?}", out.errors);
        assert!(failed.contains(&&b"ro/sub/f.txt"[..]), "{:?}", out.errors);
        assert!(failed.contains(&&b"ro/g.txt"[..]), "{:?}", out.errors);
        assert_eq!(fs::read(dst.join("ok.txt")).unwrap(), b"fine");
        assert!(!dst.join("ro/g.txt").exists());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unreadable_source_subdir_exits_23() {
    if is_root() {
        eprintln!("skipping: root can read a mode-000 directory");
        return;
    }
    within30(async {
        let opts = archive_opts();
        for flow in [Flow::Push, Flow::Pull] {
            let h = Harness::new().await;
            let src = h.src(flow).to_path_buf();
            fs::create_dir(src.join("locked")).unwrap();
            fs::write(src.join("locked/hidden.txt"), b"x").unwrap();
            fs::write(src.join("ok.txt"), b"fine").unwrap();
            fs::set_permissions(src.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();

            let out = h.run(&opts, flow).await;
            fs::set_permissions(src.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
            let out = out.unwrap();

            assert!(
                !out.errors.is_empty(),
                "{flow:?}: incomplete listing not reported"
            );
            assert_eq!(out.exit_code(), 23, "{flow:?}");
            assert_eq!(fs::read(h.dst(flow).join("ok.txt")).unwrap(), b"fine");
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_index_rows_are_module_relative() {
    within30(async {
        let opts = archive_opts();
        let h = Harness::new().await;
        fs::write(h.local.path().join("f.bin"), data(300 * KIB, 5)).unwrap();
        let mut hello = Harness::hello(Flow::Push);
        hello.path = b"sub".to_vec();
        let out = h.run_hello(&opts, hello, h.local_side()).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(
            fs::read(h.module.path().join("sub/f.bin")).unwrap(),
            data(300 * KIB, 5)
        );

        let ix = h.daemon_index();
        let r = |s: &[u8]| RelPath::new(s).unwrap();
        let row = ix.get_entry(&r(b"sub/f.bin")).unwrap();
        assert!(
            row.is_some_and(|e| e.digest.is_some()),
            "no module-relative row"
        );
        assert!(ix.get_entry(&r(b"f.bin")).unwrap().is_none());

        // Pulling the same sub-path reuses those rows: nothing is re-hashed.
        let before = h.daemon_files_hashed();
        let mut hello = Harness::hello(Flow::Pull);
        hello.path = b"sub".to_vec();
        let pulled = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let index = irsync::index::Index::open(state.path(), pulled.path())
            .unwrap()
            .into_handle();
        let side = irsync::session::LocalSide::new(
            pulled.path().to_path_buf(),
            true,
            index,
            Arc::new(irsync::filter::FilterSet::new(vec![])),
        );
        let out = h.run_hello(&opts, hello, side).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(
            h.daemon_files_hashed(),
            before,
            "daemon re-hashed a known file"
        );
        assert_eq!(
            fs::read(pulled.path().join("f.bin")).unwrap(),
            data(300 * KIB, 5)
        );
    })
    .await;
}

/// Events are really emitted during a session: every placed file, its bytes,
/// itemize codes on a pull, and the connection state.
async fn events_during_sync(flow: Flow) {
    use irsync::session::reconnect::{ConnState, ReconnectOpts};
    use irsync::session::Event;
    let opts = irsync::cli::SyncOptions::from_args(
        &irsync::cli::Args::try_parse_from(["irsync", "-a", "-i", "src/", "host:m/"]).unwrap(),
    );
    let h = Harness::new().await;
    mixed_tree(h.src(flow));
    let local = h.local_side();
    let mut rx = local.hooks.bus.subscribe();
    let (out, _) = h
        .run_reconnect(&opts, flow, ReconnectOpts::default(), local)
        .await;
    let out = out.expect("sync session");
    let mut events = Vec::new();
    while let Ok(e) = rx.try_recv() {
        events.push(e);
    }
    let done = events
        .iter()
        .filter(|e| matches!(e, Event::FileDone(..)))
        .count() as u64;
    assert!(done >= 1, "no FileDone in {events:?}");
    assert_eq!(done, out.files_transferred);
    let bytes: u64 = events
        .iter()
        .map(|e| if let Event::Bytes(n) = e { *n } else { 0 })
        .sum();
    assert_eq!(bytes, out.bytes_transferred);
    let last_conn = events.iter().rev().find_map(|e| match e {
        Event::Conn(c) => Some(*c),
        _ => None,
    });
    assert_eq!(last_conn, Some(ConnState::Direct), "{events:?}");
    let new_file = match flow {
        Flow::Pull => ">f+++++++++",
        Flow::Push => "<f+++++++++",
    };
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::Itemized(p, c) if p.as_bytes() == b"small.txt" && c == new_file
        )),
        "no itemize for small.txt in {events:?}"
    );
    assert_eq!(
        out.listed.reg,
        regular_files(h.src(flow)),
        "{:?}",
        out.listed
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_emits_events() {
    within30(events_during_sync(Flow::Push)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pull_emits_events() {
    within30(events_during_sync(Flow::Pull)).await;
}
