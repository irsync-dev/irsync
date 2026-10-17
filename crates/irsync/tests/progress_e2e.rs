//! Events and itemize codes during real sessions, both directions.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::time::Duration;

use common::{Flow, Harness};
use irsync::cli::{Args, SyncOptions};
use irsync::session::reconnect::ReconnectOpts;
use irsync::session::{Event, Outcome};

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

/// Runs one session and returns what the client's bus carried.
async fn run(h: &Harness, o: &SyncOptions, flow: Flow) -> (Outcome, Vec<Event>) {
    let local = h.local_side();
    let mut rx = local.hooks.bus.subscribe();
    let (out, _) = h
        .run_reconnect(o, flow, ReconnectOpts::default(), local)
        .await;
    let mut events = Vec::new();
    while let Ok(e) = rx.try_recv() {
        events.push(e);
    }
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
        assert!(events.contains(&Event::Scanned(1000)), "no Scanned(1000)");
        let total = events.iter().find_map(|e| match e {
            Event::Total { files, bytes } => Some((*files, *bytes)),
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
            Event::Total { files, bytes } => Some((*files, *bytes)),
            _ => None,
        });
        assert_eq!(total, Some((2, 3 + 5000)));
    })
    .await;
}
