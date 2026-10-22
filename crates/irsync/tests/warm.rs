//! Remedy 8: a destination irsync did not create starts contributing
//! chunks. Two ways in: the receiver hashes destination files of the same
//! size as a whole file it is about to fetch (`warm::Cold`), and
//! `irsync index` (`warm::index_root`) hashes a whole tree ahead of time.

mod common;

use std::fs;
use std::path::Path;
use std::time::Duration;

use common::{archive_opts, Flow, Harness};
use irsync::fs::paths::RelPath;
use irsync::index::Index;
use irsync::warm::{index_root, CANDIDATES, MIN_SIZE};

const KIB: usize = 1024;
const MIB: usize = 1024 * 1024;

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

fn rel(s: &str) -> RelPath {
    RelPath::new(s.as_bytes()).unwrap()
}

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}

fn count(events: &[String], prefix: &str) -> usize {
    events.iter().filter(|e| e.starts_with(prefix)).count()
}

// ---------------------------------------------------------------------------
// `index_root`: the explicit warm-up behind `irsync index`.

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn index_root_hashes_every_regular_file_once() {
    let state = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "a.bin", &data(100 * KIB, 1));
    write(root.path(), "sub/b.bin", &data(300 * KIB, 2));
    write(root.path(), "c.txt", b"ten bytes!");
    // Never indexed: irsync's own temporary files and anything that is
    // not a regular file.
    write(root.path(), ".irsync.c.txt.part", &data(5 * KIB, 3));
    std::os::unix::fs::symlink("a.bin", root.path().join("l")).unwrap();
    let ix = Index::open(state.path(), root.path())
        .unwrap()
        .into_handle();

    let r = index_root(ix.clone(), root.path(), 4, false).await.unwrap();
    assert_eq!((r.examined, r.hashed), (3, 3), "{r:?}");
    assert_eq!(r.bytes, (400 * KIB + 10) as u64);
    assert!(r.errors.is_empty() && !r.incomplete, "{r:?}");
    for p in ["a.bin", "sub/b.bin", "c.txt"] {
        let e = ix.lock().unwrap().get_entry(&rel(p)).unwrap();
        assert!(e.is_some_and(|e| e.digest.is_some()), "{p} not indexed");
    }
    assert!(ix
        .lock()
        .unwrap()
        .get_entry(&rel(".irsync.c.txt.part"))
        .unwrap()
        .is_none());

    // Fresh rows pass the quick check: nothing is read again.
    let r = index_root(ix.clone(), root.path(), 4, false).await.unwrap();
    assert_eq!((r.examined, r.hashed, r.bytes), (3, 0, 0), "{r:?}");

    // A changed file is re-hashed, the others are not.
    write(root.path(), "c.txt", b"eleven bytes");
    let r = index_root(ix.clone(), root.path(), 4, false).await.unwrap();
    assert_eq!((r.examined, r.hashed, r.bytes), (3, 1, 12), "{r:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn index_root_dry_run_counts_but_reads_nothing() {
    let state = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "a.bin", &data(100 * KIB, 4));
    write(root.path(), "b.bin", &data(50 * KIB, 5));
    let ix = Index::open(state.path(), root.path())
        .unwrap()
        .into_handle();

    let r = index_root(ix.clone(), root.path(), 4, true).await.unwrap();
    assert_eq!((r.examined, r.hashed), (2, 2), "{r:?}");
    assert_eq!(r.bytes, (150 * KIB) as u64);
    assert!(ix
        .lock()
        .unwrap()
        .get_entry(&rel("a.bin"))
        .unwrap()
        .is_none());

    // Once indexed, a dry run reports nothing left to do.
    index_root(ix.clone(), root.path(), 4, false).await.unwrap();
    let r = index_root(ix, root.path(), 4, true).await.unwrap();
    assert_eq!((r.examined, r.hashed, r.bytes), (2, 0, 0), "{r:?}");
}

// ---------------------------------------------------------------------------
// Candidates at chunk-list time: the receiver hashes destination files of
// the size of a whole file it is about to fetch.

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_renamed_file_on_a_cold_destination_is_copied_locally() {
    within30(async {
        let h = Harness::new().await;
        let flow = Flow::Push;
        let content = data(2 * MIB + 77, 6);
        // The destination holds the content under another name, placed by
        // something other than irsync: its index knows nothing about it.
        // (`attic` sorts before `photos`, so the receiver's walk has listed
        // it before the planner can reach the new name.)
        write(h.dst(flow), "attic/a.bin", &content);
        write(h.src(flow), "photos/a.bin", &content);

        let out = h.run(&archive_opts(), flow).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.bytes_transferred, 0, "the content was on the disk");
        assert_eq!(out.files_transferred, 1);
        assert_eq!(fs::read(h.dst(flow).join("photos/a.bin")).unwrap(), content);
        assert_eq!(h.daemon_files_hashed(), 1, "one candidate, hashed once");
        let events = h.daemon.events.lock().unwrap().clone();
        assert_eq!(count(&events, "warm:attic/a.bin"), 1, "{events:?}");
        assert!(count(&events, "copied:photos/a.bin") >= 1, "{events:?}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_destination_files_of_the_same_size_are_candidates() {
    within30(async {
        let h = Harness::new().await;
        let flow = Flow::Push;
        let size = 300 * KIB + 1;
        write(h.dst(flow), "attic/bigger.bin", &data(size + 1, 7));
        write(h.dst(flow), "attic/smaller.bin", &data(size - 1, 8));
        write(h.src(flow), "photos/new.bin", &data(size, 9));

        let out = h.run(&archive_opts(), flow).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(out.bytes_transferred >= size as u64);
        assert_eq!(
            h.daemon_files_hashed(),
            0,
            "no file of that size: nothing read"
        );
        let events = h.daemon.events.lock().unwrap().clone();
        assert_eq!(count(&events, "warm:"), 0, "{events:?}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_candidate_cap_bounds_the_hashing() {
    within30(async {
        let h = Harness::new().await;
        let flow = Flow::Push;
        let size = 256 * KIB + 5;
        let many = 3 * CANDIDATES;
        for i in 0..many {
            write(
                h.dst(flow),
                &format!("attic/f{i}.bin"),
                &data(size, 100 + i as u32),
            );
        }
        write(h.src(flow), "photos/new.bin", &data(size, 10));

        let out = h.run(&archive_opts(), flow).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(out.bytes_transferred >= size as u64, "the content was new");
        assert_eq!(
            fs::read(h.dst(flow).join("photos/new.bin")).unwrap(),
            data(size, 10)
        );
        assert_eq!(h.daemon_files_hashed() as usize, CANDIDATES);
        let events = h.daemon.events.lock().unwrap().clone();
        assert_eq!(count(&events, "warm:"), CANDIDATES, "{events:?}");

        // The candidates that were hashed stay hashed: a second whole file
        // of that size takes the next ones, never the same ones again.
        write(h.src(flow), "photos/other.bin", &data(size, 11));
        let out = h.run(&archive_opts(), flow).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(h.daemon_files_hashed() as usize, 2 * CANDIDATES);
        let events = h.daemon.events.lock().unwrap().clone();
        assert_eq!(count(&events, "warm:"), 2 * CANDIDATES, "{events:?}");
        let mut warmed: Vec<&String> = events.iter().filter(|e| e.starts_with("warm:")).collect();
        let n = warmed.len();
        warmed.dedup();
        assert_eq!(warmed.len(), n, "a candidate was hashed twice: {events:?}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn small_files_are_never_candidates() {
    within30(async {
        let h = Harness::new().await;
        let flow = Flow::Push;
        let size = MIN_SIZE as usize - 1;
        let content = data(size, 12);
        write(h.dst(flow), "attic/a.bin", &content);
        write(h.src(flow), "photos/a.bin", &content);

        let out = h.run(&archive_opts(), flow).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(
            out.bytes_transferred, size as u64,
            "below the size floor: fetched"
        );
        assert_eq!(h.daemon_files_hashed(), 0);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pull_reuses_a_foreign_local_file() {
    within30(async {
        let h = Harness::new().await;
        let flow = Flow::Pull;
        let content = data(MIB + 3, 13);
        write(h.dst(flow), "attic/a.bin", &content);
        write(h.src(flow), "photos/a.bin", &content);

        let out = h.run(&archive_opts(), flow).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.bytes_transferred, 0);
        assert_eq!(out.files_transferred, 1);
        assert_eq!(out.files_hashed, 1, "the client hashed its one candidate");
        assert_eq!(fs::read(h.dst(flow).join("photos/a.bin")).unwrap(), content);
    })
    .await;
}

// ---------------------------------------------------------------------------
// After a warm-up the session has nothing left to hash.

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn after_index_root_a_rename_costs_no_hashing_in_the_session() {
    within30(async {
        let h = Harness::new().await;
        let flow = Flow::Push;
        let content = data(MIB + 11, 14);
        write(h.dst(flow), "attic/a.bin", &content);
        let r = index_root(h.daemon_index().into_handle(), h.module.path(), 4, false)
            .await
            .unwrap();
        assert_eq!((r.examined, r.hashed), (1, 1), "{r:?}");

        write(h.src(flow), "photos/a.bin", &content);
        let out = h.run(&archive_opts(), flow).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.bytes_transferred, 0);
        assert_eq!(h.daemon_files_hashed(), 0, "the warm-up did the reading");
        assert_eq!(fs::read(h.dst(flow).join("photos/a.bin")).unwrap(), content);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_daemon_with_warm_index_hashes_its_module_at_start() {
    within30(async {
        let content = data(MIB + 17, 15);
        let h = Harness::warm(|module| write(module, "attic/a.bin", &content)).await;
        // The warm-up runs in the background; it has read the file once
        // the index holds its digest.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let e = h.daemon_index().get_entry(&rel("attic/a.bin")).unwrap();
            if e.is_some_and(|e| e.digest.is_some()) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "module never warmed");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let flow = Flow::Push;
        write(h.src(flow), "photos/a.bin", &content);
        let out = h.run(&archive_opts(), flow).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.bytes_transferred, 0);
        assert_eq!(h.daemon_files_hashed(), 0);
    })
    .await;
}
