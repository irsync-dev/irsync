//! Events and itemize codes during real sessions, both directions.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::time::Duration;

use common::{Flow, Harness};
use irsync::cli::{Args, SyncOptions};
use irsync::fs::paths::RelPath;
use irsync::session::reconnect::ReconnectOpts;
use irsync::session::{Event, Outcome, Party};

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

/// Runs one session and returns what the client's bus carried. The bus is
/// drained while the session runs, as the reporter does: per-file events
/// for many files outgrow its capacity.
async fn run(h: &Harness, o: &SyncOptions, flow: Flow) -> (Outcome, Vec<Event>) {
    run_with(h, o, flow, ReconnectOpts::default()).await
}

/// [`run`] with these reconnect options.
async fn run_with(
    h: &Harness,
    o: &SyncOptions,
    flow: Flow,
    ropts: ReconnectOpts,
) -> (Outcome, Vec<Event>) {
    use tokio::sync::broadcast::error::{RecvError, TryRecvError};
    let local = h.local_side();
    let mut rx = local.hooks.bus.subscribe();
    let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
    let collector = tokio::spawn(async move {
        let mut events = Vec::new();
        'collect: loop {
            tokio::select! {
                biased;
                r = rx.recv() => match r {
                    Ok(e) => events.push(e),
                    Err(RecvError::Lagged(n)) => panic!("the test lost {n} events"),
                    Err(RecvError::Closed) => break 'collect,
                },
                _ = &mut stopped => loop {
                    match rx.try_recv() {
                        Ok(e) => events.push(e),
                        Err(TryRecvError::Lagged(n)) => panic!("the test lost {n} events"),
                        Err(_) => break 'collect,
                    }
                },
            }
        }
        events
    });
    let (out, _) = h.run_reconnect(o, flow, ropts, local).await;
    let _ = stop.send(());
    let events = collector.await.expect("event collector");
    (out.expect("sync session"), events)
}

/// Itemize lines as (path, code with the direction normalised to `>`).
fn items(events: &[Event]) -> BTreeSet<(String, String)> {
    // The root directory's item depends on the destination temp dir's mtime.
    events
        .iter()
        .filter_map(|e| match e {
            Event::Itemized(p, c) if !p.as_bytes().is_empty() => {
                let c = c.replacen('<', ">", 1);
                Some((String::from_utf8_lossy(p.as_bytes()).into_owned(), c))
            }
            _ => None,
        })
        .collect()
}

fn tree(root: &std::path::Path) {
    fs::create_dir_all(root.join("d/e")).unwrap();
    fs::write(root.join("top.txt"), b"top").unwrap();
    fs::write(root.join("d/e/deep.bin"), vec![7u8; 5000]).unwrap();
    symlink("top.txt", root.join("d/link")).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_itemize_matches_pull() {
    within30(async {
        let o = opts(&["-i"]);
        let (hp, hl) = (Harness::new().await, Harness::new().await);
        tree(hp.src(Flow::Push));
        tree(hl.src(Flow::Pull));
        let (_, push) = run(&hp, &o, Flow::Push).await;
        let (_, pull) = run(&hl, &o, Flow::Pull).await;
        let (push_items, pull_items) = (items(&push), items(&pull));
        assert!(
            push_items.contains(&("top.txt".into(), ">f+++++++++".into())),
            "{push_items:?}"
        );
        assert!(pull.iter().any(
            |e| matches!(e, Event::Itemized(p, c) if p.as_bytes() == b"top.txt" && c == ">f+++++++++")
        ));
        assert!(push.iter().any(
            |e| matches!(e, Event::Itemized(p, c) if p.as_bytes() == b"top.txt" && c == "<f+++++++++")
        ));
        assert_eq!(push_items, pull_items);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dry_run_push_itemizes() {
    within30(async {
        let o = opts(&["-i", "-n"]);
        let h = Harness::new().await;
        tree(h.src(Flow::Push));
        let (_, events) = run(&h, &o, Flow::Push).await;
        assert!(
            events.iter().any(|e| matches!(e, Event::Itemized(p, c)
                if p.as_bytes() == b"top.txt" && c == "<f+++++++++")),
            "{events:?}"
        );
        // -v -n lists the files that would be transferred.
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::FileDone(p, _) if p.as_bytes() == b"d/e/deep.bin")));
        assert_eq!(fs::read_dir(h.dst(Flow::Push)).unwrap().count(), 0);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dry_run_pull_lists_files_for_verbose() {
    within30(async {
        let h = Harness::new().await;
        tree(h.src(Flow::Pull));
        let (_, events) = run(&h, &opts(&["-n"]), Flow::Pull).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::FileDone(p, 0) if p.as_bytes() == b"top.txt")));
        assert_eq!(fs::read_dir(h.dst(Flow::Pull)).unwrap().count(), 0);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hardlink_emits_events_and_counts_created() {
    within30(async {
        let h = Harness::new().await;
        let src = h.src(Flow::Pull);
        fs::write(src.join("a"), b"shared").unwrap();
        fs::hard_link(src.join("a"), src.join("b")).unwrap();
        let (out, events) = run(&h, &opts(&["-H"]), Flow::Pull).await;
        assert!(
            events.iter().any(|e| matches!(e, Event::Hardlinked(..))),
            "{events:?}"
        );
        // The file and its link were both created.
        assert_eq!(out.created, 2, "{out:?}");
        assert_eq!(out.files_transferred, 1);
    })
    .await;
}

async fn deletion_events(flow: Flow, dry: bool) {
    let h = Harness::new().await;
    fs::write(h.src(flow).join("keep"), b"k").unwrap();
    fs::write(h.dst(flow).join("keep"), b"k").unwrap();
    fs::write(h.dst(flow).join("extra"), b"x").unwrap();
    let o = if dry {
        opts(&["--delete", "-n"])
    } else {
        opts(&["--delete"])
    };
    let (_, events) = run(&h, &o, flow).await;
    let has = |f: fn(&Event) -> bool| events.iter().any(f);
    if dry {
        assert!(
            has(|e| matches!(e, Event::WouldDelete(p) if p.as_bytes() == b"extra")),
            "{flow:?} {events:?}"
        );
        assert!(!has(|e| matches!(e, Event::Deleted(_))));
        assert!(h.dst(flow).join("extra").exists());
    } else {
        assert!(
            has(|e| matches!(e, Event::Deleted(p) if p.as_bytes() == b"extra")),
            "{flow:?} {events:?}"
        );
        assert!(!h.dst(flow).join("extra").exists());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deleted_and_would_delete_events() {
    for flow in [Flow::Pull, Flow::Push] {
        for dry in [false, true] {
            within30(deletion_events(flow, dry)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scanned_event_emitted_on_push() {
    within30(async {
        let h = Harness::new().await;
        for i in 0..1100 {
            fs::write(h.src(Flow::Push).join(format!("f{i}")), b"x").unwrap();
        }
        let (_, events) = run(&h, &opts(&[]), Flow::Push).await;
        assert!(
            events.iter().any(|e| matches!(e, Event::Scanned { party: Party::Local, entries, .. } if *entries >= 1000)),
            "no local Scanned >= 1000: {events:?}"
        );
        let total = events.iter().find_map(|e| match e {
            Event::Total { party: Party::Local, files, bytes } => Some((*files, *bytes)),
            _ => None,
        });
        assert_eq!(total, Some((1100, 1100)));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pull_reports_total_from_peer_listing() {
    within30(async {
        let h = Harness::new().await;
        tree(h.src(Flow::Pull));
        let (_, events) = run(&h, &opts(&[]), Flow::Pull).await;
        let total = events.iter().find_map(|e| match e {
            Event::Total {
                party: Party::Peer,
                files,
                bytes,
            } => Some((*files, *bytes)),
            _ => None,
        });
        assert_eq!(total, Some((2, 3 + 5000)));
    })
    .await;
}

fn big(p: &RelPath) -> bool {
    p.as_bytes() == b"big.bin"
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn per_file_events_in_both_directions() {
    for flow in [Flow::Push, Flow::Pull] {
        within30(async {
            let h = Harness::new().await;
            fs::write(h.src(flow).join("big.bin"), vec![3u8; 3_000_000]).unwrap();
            let (out, events) = run(&h, &opts(&[]), flow).await;
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, Event::FileStart(p, 3_000_000) if big(p))),
                "{flow:?}: no FileStart with size: {events:?}"
            );
            let bytes: u64 = events
                .iter()
                .filter_map(|e| match e {
                    Event::FileBytes(p, n) if big(p) => Some(*n),
                    _ => None,
                })
                .sum();
            assert_eq!(bytes, out.bytes_transferred, "{flow:?}");
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, Event::FileMatched(p, 0) if big(p))),
                "{flow:?}: a new file matches nothing: {events:?}"
            );
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, Event::FileDone(p, 3_000_000) if big(p))),
                "{flow:?}"
            );
        })
        .await;
    }
}

/// `n` bytes that do not repeat (so chunks and blocks are all distinct).
fn noise(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn after_a_reconnect_files_placed_earlier_are_not_reported_skipped() {
    use irsync::session::reconnect::ConnState;
    within30(async {
        let h = Harness::new().await;
        let src = h.src(Flow::Pull);
        fs::write(src.join("same.txt"), b"unchanged").unwrap();
        run(&h, &opts(&[]), Flow::Pull).await;
        for i in 0..10 {
            fs::write(src.join(format!("f{i:02}.bin")), noise(2 << 20, i + 1)).unwrap();
        }
        h.daemon.sender_parallel(4);
        h.daemon.fault.drop_after_bytes(9 << 20);
        let fast = ReconnectOpts {
            backoff_min: Duration::from_millis(100),
            ..ReconnectOpts::default()
        };
        // With `--delete` the stored plan is not reused: the second
        // connection lists and plans afresh, past the files the first placed.
        let (out, events) = run_with(&h, &opts(&["--delete"]), Flow::Pull, fast).await;
        assert!(h.daemon.fault.fired(), "the fault never fired");
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let cut = events
            .iter()
            .position(|e| matches!(e, Event::Conn(ConnState::Reconnecting { .. })))
            .expect("a reconnect");
        let placed: BTreeSet<RelPath> = events[..cut]
            .iter()
            .filter_map(|e| match e {
                Event::FileDone(p, _) => Some(p.clone()),
                _ => None,
            })
            .collect();
        assert!(!placed.is_empty(), "nothing placed before the drop");
        let skipped_after: BTreeSet<RelPath> = events[cut..]
            .iter()
            .filter_map(|e| match e {
                Event::Skipped(p, _) => Some(p.clone()),
                _ => None,
            })
            .collect();
        assert!(
            placed.is_disjoint(&skipped_after),
            "placed on the first connection {placed:?}, skipped on the second {skipped_after:?}"
        );
        assert!(
            skipped_after.contains(&RelPath::new(b"same.txt").unwrap()),
            "the unchanged file is still reported: {skipped_after:?} {:?}",
            events
                .iter()
                .filter(|e| !matches!(e, Event::FileBytes(..) | Event::Bytes(_)))
                .collect::<Vec<_>>()
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_delta_never_reports_the_whole_file_matched() {
    within30(async {
        // Two small edits: every changed chunk gets a basis, so the
        // receiver's first request (chunks without one) is empty and the
        // rest follow after the signatures.
        let h = Harness::new().await;
        let old = noise(12_000_000, 7);
        let mut new = old.clone();
        new[3_000_000..3_000_100].fill(0);
        new[9_000_000..9_000_100].fill(0);
        fs::write(h.dst(Flow::Push).join("big.bin"), &old).unwrap();
        fs::write(h.src(Flow::Push).join("big.bin"), &new).unwrap();
        let (out, events) = run(&h, &opts(&[]), Flow::Push).await;
        assert!(out.bytes_transferred < 4_000_000, "a delta: {out:?}");
        let matched: Vec<u64> = events
            .iter()
            .filter_map(|e| match e {
                Event::FileMatched(p, n) if big(p) => Some(*n),
                _ => None,
            })
            .collect();
        assert!(!matched.is_empty(), "{events:?}");
        assert!(
            matched.iter().all(|&n| n < 12_000_000),
            "matched never covers the chunks still to send: {matched:?}"
        );
        assert!(matched.iter().any(|&n| n > 0), "{matched:?}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hashing_events_when_a_file_is_read() {
    within30(async {
        // Push with -c: the sender reads the source file for its digest.
        let h = Harness::new().await;
        fs::write(h.src(Flow::Push).join("big.bin"), vec![3u8; 3_000_000]).unwrap();
        let (_, events) = run(&h, &opts(&["-c"]), Flow::Push).await;
        let start = events
            .iter()
            .position(|e| matches!(e, Event::FileHashing(p, 3_000_000) if big(p)));
        let end = events
            .iter()
            .position(|e| matches!(e, Event::FileHashed(p) if big(p)));
        let open = events
            .iter()
            .position(|e| matches!(e, Event::FileStart(p, _) if big(p)));
        assert!(start.is_some(), "no FileHashing: {events:?}");
        assert!(
            start < end && end <= open,
            "hashing, then hashed, then the stream: {events:?}"
        );
        // Pull over a differing old file: the receiver reads it for the delta basis.
        let h = Harness::new().await;
        fs::write(h.src(Flow::Pull).join("big.bin"), vec![3u8; 3_000_000]).unwrap();
        fs::write(h.dst(Flow::Pull).join("big.bin"), vec![4u8; 2_000_000]).unwrap();
        let (_, events) = run(&h, &opts(&[]), Flow::Pull).await;
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::FileHashing(p, 2_000_000) if big(p))),
            "{events:?}"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn receiver_opens_a_part_only_when_its_data_arrives() {
    within30(async {
        let h = Harness::new().await;
        // Distinct contents: nothing can be filled from a local copy, so
        // every file waits for the sender's stream.
        for i in 0..50 {
            fs::write(h.src(Flow::Push).join(format!("f{i}")), format!("file {i}")).unwrap();
        }
        let (out, _) = run(&h, &opts(&["--parallel", "2"]), Flow::Push).await;
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        // The daemon receives. Every file is planned at once (`chunklist:`),
        // but its `.part` must not be opened (`open:`) until its data is on
        // the way: after the request for it (`need:`), on the first write.
        // How many `.part` files are open at one moment is a matter of how
        // fast the receiver places them behind the sender's streams, which
        // differs between platforms, so it is not asserted.
        let log = h.daemon.events.lock().unwrap().clone();
        let position = |what: &str| log.iter().position(|e| e == what);
        let mut placed = 0;
        for i in 0..50 {
            let (need, open) = (
                position(&format!("need:f{i}")),
                position(&format!("open:f{i}")),
            );
            assert!(need.is_some() && open.is_some(), "f{i}: {log:?}");
            assert!(
                need < open,
                "f{i}: .part opened before its data was requested: {log:?}"
            );
            placed += usize::from(position(&format!("placed:f{i}")).is_some());
        }
        assert_eq!(placed, 50, "{log:?}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sender_file_start_means_a_stream_opened_not_a_plan() {
    within30(async {
        let h = Harness::new().await;
        for i in 0..50 {
            fs::write(h.src(Flow::Push).join(format!("f{i}")), b"x").unwrap();
        }
        let (_, events) = run(&h, &opts(&["--parallel", "2"]), Flow::Push).await;
        // A file is in flight from its FileStart until its stream has closed
        // (FileSent), or until the ack when that is handled first: in-flight
        // is bounded by the streams, not by the plan.
        let mut open = std::collections::BTreeSet::new();
        let mut max_open = 0;
        for e in &events {
            match e {
                Event::FileStart(p, _) => {
                    open.insert(p.clone());
                    max_open = max_open.max(open.len());
                }
                Event::FileSent(p) | Event::FileDone(p, _) => {
                    open.remove(p);
                }
                _ => {}
            }
        }
        // `--parallel 2` streams; FileSent trails the stream closing by a
        // scheduling step, so allow some slack. (Closing on the ack alone
        // measured the daemon's finalize latency instead, and reached 14
        // under load.) Before this change every planned file started at
        // once and this peaked at 50.
        assert!(max_open <= 10, "in flight peaked at {max_open}: {events:?}");
        // `FileSent` sits between a file's `FileStart` and its `FileDone`.
        // (Not every file gets one: when the ack is handled before the
        // send's own completion, the file is already done.)
        for (i, e) in events.iter().enumerate() {
            let Event::FileSent(p) = e else { continue };
            assert!(
                events[..i]
                    .iter()
                    .any(|e| matches!(e, Event::FileStart(q, _) if q == p)),
                "FileSent({p:?}) before its FileStart: {events:?}"
            );
            assert!(
                events[i..]
                    .iter()
                    .any(|e| matches!(e, Event::FileDone(q, _) if q == p)),
                "FileSent({p:?}) without a FileDone after it: {events:?}"
            );
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn second_run_reports_skipped_bytes_locally_on_pull() {
    within30(async {
        let h = Harness::new().await;
        tree(h.src(Flow::Pull));
        run(&h, &opts(&[]), Flow::Pull).await;
        let (_, events) = run(&h, &opts(&[]), Flow::Pull).await;
        let skipped: u64 = events
            .iter()
            .filter_map(|e| match e {
                Event::Skipped(_, n) => Some(*n),
                _ => None,
            })
            .sum();
        assert_eq!(skipped, 3 + 5000, "{events:?}");
        assert!(
            events.iter().any(|e| matches!(
                e,
                Event::Total {
                    party: Party::Local,
                    ..
                }
            )),
            "local listing now reports: {events:?}"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_listing_is_counted_on_push_and_local_walk_says_where_it_is() {
    within30(async {
        let h = Harness::new().await;
        fs::create_dir_all(h.src(Flow::Push).join("deep/dir")).unwrap();
        for i in 0..1100 {
            fs::write(h.src(Flow::Push).join(format!("deep/dir/f{i}")), b"x").unwrap();
        }
        fs::write(h.dst(Flow::Push).join("extra"), b"x").unwrap();
        let (_, events) = run(&h, &opts(&[]), Flow::Push).await;
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Total { party: Party::Peer, files: 1, .. })),
            "{events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(e, Event::Scanned { party: Party::Local, at: Some(p), .. } if p.as_bytes() == b"deep/dir")),
            "{events:?}"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_learns_peer_skipped_bytes_from_the_heartbeat() {
    within30(async {
        let h = Harness::new().await;
        tree(h.src(Flow::Push));
        run(&h, &opts(&[]), Flow::Push).await;
        let (_, events) = run(&h, &opts(&[]), Flow::Push).await;
        let max = events
            .iter()
            .filter_map(|e| match e {
                Event::PeerSkipped(n) => Some(*n),
                _ => None,
            })
            .max();
        assert_eq!(max, Some(3 + 5000), "{events:?}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_sees_the_peer_walk_position() {
    within30(async {
        let h = Harness::new().await;
        fs::create_dir_all(h.dst(Flow::Push).join("old/stuff")).unwrap();
        for i in 0..3000 {
            fs::write(h.dst(Flow::Push).join(format!("old/stuff/f{i}")), b"x").unwrap();
        }
        let (_, events) = run(&h, &opts(&[]), Flow::Push).await;
        // The walk sends a heartbeat before its first entry (naming the
        // root) and again at its 1000th entry (naming `old/stuff`); either
        // one is enough here.
        assert!(
            events.iter().any(|e| matches!(
                e,
                Event::Scanned {
                    party: Party::Peer,
                    at: Some(_),
                    ..
                }
            )),
            "{events:?}"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_sees_the_peer_root_before_any_entry() {
    within30(async {
        let h = Harness::new().await;
        fs::create_dir_all(h.dst(Flow::Push)).unwrap();
        for i in 0..3 {
            fs::write(h.dst(Flow::Push).join(format!("f{i}")), b"x").unwrap();
        }
        let (_, events) = run(&h, &opts(&[]), Flow::Push).await;
        assert!(
            events.iter().any(|e| matches!(
                e,
                Event::Scanned { party: Party::Peer, at: Some(p), .. } if p.as_bytes().is_empty()
            )),
            "{events:?}"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_progress_events_both_directions() {
    for flow in [Flow::Pull, Flow::Push] {
        within30(async {
            let h = Harness::new().await;
            fs::write(h.src(flow).join("keep"), b"k").unwrap();
            for i in 0..5 {
                fs::write(h.dst(flow).join(format!("extra{i}")), b"x").unwrap();
            }
            let (_, events) = run(&h, &opts(&["--delete"]), flow).await;
            let party = if flow == Flow::Pull {
                Party::Local
            } else {
                Party::Peer
            };
            let last = events.iter().rev().find_map(|e| match e {
                Event::Delete {
                    party: p,
                    done,
                    total,
                } if *p == party => Some((*done, *total)),
                _ => None,
            });
            assert_eq!(last, Some((5, Some(5))), "{flow:?}: {events:?}");
        })
        .await;
    }
}

fn failed(events: &[Event], path: &str) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Failed(p, msg) if p.as_bytes() == path.as_bytes() => Some(msg.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_the_sender_cannot_read_is_reported_as_failed_on_the_bus() {
    use std::os::unix::fs::PermissionsExt;
    if nix::unistd::geteuid().is_root() {
        eprintln!("skipping: running as root, mode 0o000 does not stop reads");
        return;
    }
    within30(async {
        let h = Harness::new().await;
        let src = h.src(Flow::Push);
        fs::write(src.join("ok.txt"), b"fine").unwrap();
        fs::write(src.join("locked.bin"), vec![1u8; 3000]).unwrap();
        fs::set_permissions(src.join("locked.bin"), fs::Permissions::from_mode(0o000)).unwrap();
        let (out, events) = run(&h, &opts(&[]), Flow::Push).await;
        let msgs = failed(&events, "locked.bin");
        assert_eq!(msgs.len(), 1, "one Failed event: {events:?}");
        assert!(msgs[0].contains("ermission denied"), "{msgs:?}");
        assert!(
            out.errors
                .iter()
                .any(|(p, m)| p.as_bytes() == b"locked.bin" && m == &msgs[0]),
            "the outcome records the same error: {:?}",
            out.errors
        );
        assert!(failed(&events, "ok.txt").is_empty());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_the_receiver_cannot_write_is_reported_as_failed_on_the_bus() {
    use std::os::unix::fs::PermissionsExt;
    if nix::unistd::geteuid().is_root() {
        eprintln!("skipping: running as root, a read-only directory does not stop writes");
        return;
    }
    within30(async {
        let h = Harness::new().await;
        let src = h.src(Flow::Pull);
        fs::create_dir(src.join("ro")).unwrap();
        fs::write(src.join("ro/x.bin"), vec![2u8; 3000]).unwrap();
        let dst = h.dst(Flow::Pull);
        fs::create_dir(dst.join("ro")).unwrap();
        fs::set_permissions(dst.join("ro"), fs::Permissions::from_mode(0o555)).unwrap();
        let (out, events) = run(&h, &opts(&[]), Flow::Pull).await;
        fs::set_permissions(dst.join("ro"), fs::Permissions::from_mode(0o755)).unwrap();
        let msgs = failed(&events, "ro/x.bin");
        assert_eq!(msgs.len(), 1, "one Failed event: {events:?}");
        assert!(msgs[0].contains("ermission denied"), "{msgs:?}");
        assert!(
            out.errors.iter().any(|(p, _)| p.as_bytes() == b"ro/x.bin"),
            "the outcome records the error: {:?}",
            out.errors
        );
    })
    .await;
}
