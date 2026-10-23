//! A `DELTA` file whose content turns out to be unchanged (a `touch`, a
//! restore, `-c` against a cold index) is settled in place: metadata only,
//! no `.part`, no rewrite, no second read of the destination.

mod common;

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use common::{archive_opts, Flow, Harness};
use irsync::session::Event;

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

fn mtime(p: &Path) -> SystemTime {
    fs::metadata(p).unwrap().modified().unwrap()
}

fn set_mtime(p: &Path, t: SystemTime) {
    fs::File::open(p).unwrap().set_modified(t).unwrap();
}

fn inode(p: &Path) -> u64 {
    fs::metadata(p).unwrap().ino()
}

fn checksum_opts() -> irsync::cli::SyncOptions {
    let args =
        irsync::cli::Args::try_parse_from(["irsync", "-a", "-c", "src/", "host:m/"]).unwrap();
    irsync::cli::SyncOptions::from_args(&args)
}

/// Itemize codes the bus carried for `name`.
fn itemized(events: &[Event], name: &str) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Itemized(p, c) if p.as_bytes() == name.as_bytes() => Some(c.clone()),
            _ => None,
        })
        .collect()
}

fn has(events: &[String], what: &str) -> bool {
    events.iter().any(|e| e == what)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn touched_source_is_a_metadata_only_update() {
    within30(async {
        let opts = archive_opts();
        let flow = Flow::Pull;
        let h = Harness::new().await;
        let src = h.src(flow).join("x.bin");
        let dst = h.dst(flow).join("x.bin");
        fs::write(&src, data(4 * MIB, 21)).unwrap();
        let first_run = h.run(&opts, flow).await.unwrap();
        assert!(first_run.errors.is_empty(), "{:?}", first_run.errors);
        let ino = inode(&dst);

        // Same bytes, new mtime: the sender's index row is stale, so its
        // listing carries no hash and the planner has to ask for the file.
        set_mtime(&src, mtime(&src) + Duration::from_secs(3600));
        let hashed_before = h.daemon_files_hashed();
        let log: irsync::session::EventLog = Arc::default();
        let mut local = h.local_side();
        local.hooks.events = Some(log.clone());
        let mut rx = local.hooks.bus.subscribe();
        let out = h.run_side(&opts, flow, local).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);

        let events = log.lock().unwrap().clone();
        assert!(has(&events, "identical:x.bin"), "{events:?}");
        for what in ["need:x.bin", "chunk:x.bin", "copied:x.bin"] {
            assert!(!has(&events, what), "{what} in {events:?}");
        }
        assert_eq!(out.files_transferred, 0);
        assert_eq!(out.bytes_transferred, 0);
        // The receiver's index was warm: nothing read here; the sender read
        // its touched file once.
        assert_eq!(out.files_hashed, 0);
        assert_eq!(h.daemon_files_hashed() - hashed_before, 1);
        assert_eq!(mtime(&dst), mtime(&src));
        assert_eq!(inode(&dst), ino, "the destination was rewritten");
        assert!(!h.dst(flow).join(".irsync.x.bin.part").exists());

        let mut bus = Vec::new();
        while let Ok(e) = rx.try_recv() {
            bus.push(e);
        }
        assert_eq!(itemized(&bus, "x.bin"), vec![".f..t......".to_string()]);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn touched_destination_is_a_metadata_only_update() {
    within30(async {
        let opts = archive_opts();
        let flow = Flow::Push;
        let h = Harness::new().await;
        let src = h.src(flow).join("x.bin");
        let dst = h.dst(flow).join("x.bin");
        fs::write(&src, data(4 * MIB, 22)).unwrap();
        let first_run = h.run(&opts, flow).await.unwrap();
        assert!(first_run.errors.is_empty(), "{:?}", first_run.errors);
        let ino = inode(&dst);

        // The destination's mtime moves: its listing carries no hash.
        set_mtime(&dst, mtime(&dst) + Duration::from_secs(3600));
        let hashed_before = h.daemon_files_hashed();
        h.daemon.events.lock().unwrap().clear();
        let out = h.run(&opts, flow).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);

        let events = h.daemon.events.lock().unwrap().clone();
        assert!(has(&events, "identical:x.bin"), "{events:?}");
        assert!(!has(&events, "need:x.bin"), "{events:?}");
        assert_eq!(out.bytes_transferred, 0);
        // The sender's index was warm; the receiver read its touched file once.
        assert_eq!(out.files_hashed, 0);
        assert_eq!(h.daemon_files_hashed() - hashed_before, 1);
        assert_eq!(mtime(&dst), mtime(&src), "mtime not restored");
        assert_eq!(inode(&dst), ino, "the destination was rewritten");
        assert!(!h.dst(flow).join(".irsync.x.bin.part").exists());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn checksum_against_a_cold_index_reads_once_and_changes_nothing() {
    within30(async {
        let flow = Flow::Pull;
        let h = Harness::new().await;
        let src = h.src(flow).join("x.bin");
        let dst = h.dst(flow).join("x.bin");
        fs::write(&src, data(4 * MIB, 23)).unwrap();
        let first_run = h.run(&archive_opts(), flow).await.unwrap();
        assert!(first_run.errors.is_empty(), "{:?}", first_run.errors);
        let (ino, before) = (inode(&dst), mtime(&dst));

        // `-c` with a receiver that has never hashed its copy.
        let hashed_before = h.daemon_files_hashed();
        let fresh_state = tempfile::tempdir().unwrap();
        let log: irsync::session::EventLog = Arc::default();
        let mut local = Harness::side_for(h.local.path(), fresh_state.path());
        local.hooks.events = Some(log.clone());
        let mut rx = local.hooks.bus.subscribe();
        let out = h.run_side(&checksum_opts(), flow, local).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);

        let events = log.lock().unwrap().clone();
        assert!(has(&events, "identical:x.bin"), "{events:?}");
        assert_eq!(out.files_transferred, 0);
        assert_eq!(out.bytes_transferred, 0);
        assert_eq!(out.files_hashed, 1);
        // `-c` reads on both sides, warm index or not.
        assert_eq!(h.daemon_files_hashed() - hashed_before, 1);
        assert_eq!(inode(&dst), ino, "the destination was rewritten");
        assert_eq!(mtime(&dst), before);
        assert!(!h.dst(flow).join(".irsync.x.bin.part").exists());
        // Nothing differed, so nothing is itemized.
        let mut bus = Vec::new();
        while let Ok(e) = rx.try_recv() {
            bus.push(e);
        }
        assert!(itemized(&bus, "x.bin").is_empty(), "{bus:?}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn destination_replaced_after_hashing_falls_through_to_delta() {
    within30(async {
        let opts = archive_opts();
        let flow = Flow::Pull;
        let h = Harness::new().await;
        let src = h.src(flow).join("x.bin");
        let dst = h.dst(flow).join("x.bin");
        fs::write(&src, data(4 * MIB, 24)).unwrap();
        let first_run = h.run(&opts, flow).await.unwrap();
        assert!(first_run.errors.is_empty(), "{:?}", first_run.errors);
        set_mtime(&src, mtime(&src) + Duration::from_secs(3600));

        // The content matched when hashed; the destination is replaced
        // before the receiver checks it is still that file.
        let log: irsync::session::EventLog = Arc::default();
        let mut local = h.local_side();
        local.hooks.events = Some(log.clone());
        local.hooks.before_identical_check = Some({
            let dst = dst.clone();
            Arc::new(move |_: &irsync::fs::paths::RelPath| {
                let dst = dst.clone();
                Box::pin(async move {
                    fs::write(&dst, data(4 * MIB, 25)).unwrap();
                })
            })
        });
        let out = h.run_side(&opts, flow, local).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);

        let events = log.lock().unwrap().clone();
        assert!(!has(&events, "identical:x.bin"), "{events:?}");
        assert!(has(&events, "need:x.bin"), "{events:?}");
        assert_eq!(fs::read(&dst).unwrap(), fs::read(&src).unwrap());
        assert!(!h.dst(flow).join(".irsync.x.bin.part").exists());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn settling_in_place_discards_a_leftover_part_file() {
    within30(async {
        let opts = archive_opts();
        let flow = Flow::Pull;
        let h = Harness::new().await;
        let src = h.src(flow).join("x.bin");
        fs::write(&src, data(4 * MIB, 26)).unwrap();
        let first_run = h.run(&opts, flow).await.unwrap();
        assert!(first_run.errors.is_empty(), "{:?}", first_run.errors);

        // An interrupted earlier attempt left a `.part`; a touch on the
        // source then settles the file in place and the `.part` has no use.
        let part = h.dst(flow).join(".irsync.x.bin.part");
        fs::write(&part, data(MIB, 27)).unwrap();
        set_mtime(&src, mtime(&src) + Duration::from_secs(3600));
        let log: irsync::session::EventLog = Arc::default();
        let mut local = h.local_side();
        local.hooks.events = Some(log.clone());
        let out = h.run_side(&opts, flow, local).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let events = log.lock().unwrap().clone();
        assert!(has(&events, "identical:x.bin"), "{events:?}");
        assert!(!part.exists(), "stale .part kept");
    })
    .await;
}

// ---------------------------------------------------------------------------
// `-c` trusts no index row: every file on both sides is read. A row that is
// fresh by size, mtime, ctime and inode but wrong about the content (bit
// rot, a block-level restore) is what `-c` exists to catch; the tests
// plant such rows directly, since nothing in user space moves the content
// without moving the ctime.

/// Replaces `rel`'s index row under `state` for `root` with the file's
/// current stat and `content`'s digest.
fn plant_row(state: &Path, root: &Path, rel: &str, content: &[u8]) {
    use irsync::fs::meta::lstat_entry_with_ctime;
    let rel = irsync::fs::paths::RelPath::new(rel.as_bytes()).unwrap();
    let (entry, ctime) = lstat_entry_with_ctime(root, &rel).unwrap();
    let digest = irsync_proto::chunk::digest_bytes(content);
    irsync::index::Index::open(state, root)
        .unwrap()
        .put_entry(&rel, &entry, ctime, Some(&digest))
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn checksum_catches_a_destination_whose_row_is_fresh_but_wrong() {
    within30(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        let src = h.src(flow).join("x.bin");
        let dst = h.dst(flow).join("x.bin");
        let (good, rot) = (data(MIB, 31), data(MIB, 32));
        fs::write(&src, &good).unwrap();
        let first_run = h.run(&archive_opts(), flow).await.unwrap();
        assert!(first_run.errors.is_empty(), "{:?}", first_run.errors);

        // The destination's bytes change under an unchanged stat.
        let keep = mtime(&dst);
        fs::write(&dst, &rot).unwrap();
        set_mtime(&dst, keep);
        plant_row(h.daemon._state.path(), h.export.path(), "x.bin", &good);

        // Without `-c` size and mtime decide: the file is skipped.
        let out = h.run(&archive_opts(), flow).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(&dst).unwrap(), rot);
        assert_eq!(out.files_hashed, 0);

        let hashed_before = h.daemon_files_hashed();
        h.daemon.events.lock().unwrap().clear();
        let out = h.run(&checksum_opts(), flow).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let events = h.daemon.events.lock().unwrap().clone();
        assert!(has(&events, "need:x.bin"), "{events:?}");
        assert_eq!(fs::read(&dst).unwrap(), good, "rot not repaired");
        assert!(out.bytes_transferred > 0);
        assert_eq!(out.files_hashed, 1, "the sender read its file");
        assert!(
            h.daemon_files_hashed() - hashed_before >= 1,
            "the receiver read its file"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn checksum_catches_a_source_whose_row_is_fresh_but_wrong() {
    within30(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        let src = h.src(flow).join("x.bin");
        let dst = h.dst(flow).join("x.bin");
        let (old, new) = (data(MIB, 33), data(MIB, 34));
        fs::write(&src, &old).unwrap();
        let first_run = h.run(&archive_opts(), flow).await.unwrap();
        assert!(first_run.errors.is_empty(), "{:?}", first_run.errors);

        let keep = mtime(&src);
        fs::write(&src, &new).unwrap();
        set_mtime(&src, keep);
        plant_row(h.client_state.path(), h.local.path(), "x.bin", &old);

        let out = h.run(&archive_opts(), flow).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(&dst).unwrap(), old, "skipped without -c");

        let hashed_before = h.daemon_files_hashed();
        let out = h.run(&checksum_opts(), flow).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(&dst).unwrap(), new, "change not sent");
        assert_eq!(out.files_hashed, 1);
        assert_eq!(h.daemon_files_hashed() - hashed_before, 1);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn checksum_forces_the_daemon_sender_to_read() {
    within30(async {
        let flow = Flow::Pull;
        let h = Harness::new().await;
        let src = h.src(flow).join("x.bin");
        let dst = h.dst(flow).join("x.bin");
        let (old, new) = (data(MIB, 35), data(MIB, 36));
        fs::write(&src, &old).unwrap();
        let first_run = h.run(&archive_opts(), flow).await.unwrap();
        assert!(first_run.errors.is_empty(), "{:?}", first_run.errors);

        let keep = mtime(&src);
        fs::write(&src, &new).unwrap();
        set_mtime(&src, keep);
        plant_row(h.daemon._state.path(), h.export.path(), "x.bin", &old);

        let hashed_before = h.daemon_files_hashed();
        let out = h.run(&checksum_opts(), flow).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(fs::read(&dst).unwrap(), new, "change not sent");
        assert_eq!(
            h.daemon_files_hashed() - hashed_before,
            1,
            "the daemon read its file"
        );
        assert_eq!(out.files_hashed, 1, "the receiver read its copy");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn checksum_reads_every_file_on_both_sides_and_rewrites_nothing() {
    within30(async {
        let flow = Flow::Push;
        let h = Harness::new().await;
        for (name, seed) in [("a.bin", 37), ("b.bin", 38)] {
            fs::write(h.src(flow).join(name), data(MIB, seed)).unwrap();
        }
        let first_run = h.run(&archive_opts(), flow).await.unwrap();
        assert!(first_run.errors.is_empty(), "{:?}", first_run.errors);
        let inodes: Vec<u64> = ["a.bin", "b.bin"]
            .iter()
            .map(|n| inode(&h.dst(flow).join(n)))
            .collect();

        // Warm on both sides: a plain run reads nothing.
        let hashed_before = h.daemon_files_hashed();
        let out = h.run(&archive_opts(), flow).await.unwrap();
        assert_eq!(
            (out.files_hashed, h.daemon_files_hashed() - hashed_before),
            (0, 0)
        );

        // `-c` reads every file on both sides and settles them in place.
        let hashed_before = h.daemon_files_hashed();
        h.daemon.events.lock().unwrap().clear();
        let out = h.run(&checksum_opts(), flow).await.unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let events = h.daemon.events.lock().unwrap().clone();
        assert!(
            has(&events, "identical:a.bin") && has(&events, "identical:b.bin"),
            "{events:?}"
        );
        assert_eq!(out.files_hashed, 2);
        assert_eq!(h.daemon_files_hashed() - hashed_before, 2);
        assert_eq!(out.bytes_transferred, 0);
        assert_eq!(out.files_transferred, 0);
        for (n, ino) in ["a.bin", "b.bin"].iter().zip(inodes) {
            assert_eq!(inode(&h.dst(flow).join(n)), ino, "{n} rewritten");
        }

        // The forced reads refreshed the rows: the next plain run reads nothing.
        let hashed_before = h.daemon_files_hashed();
        let out = h.run(&archive_opts(), flow).await.unwrap();
        assert_eq!(
            (out.files_hashed, h.daemon_files_hashed() - hashed_before),
            (0, 0)
        );
    })
    .await;
}
