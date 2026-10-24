use irsync::cli::progress::state::{Live, RowState, ROW_EXPIRY, STALL_AFTER};
use irsync::cli::DeleteMode;
use irsync::fs::paths::RelPath;
use irsync::session::{Event, Party};
use std::time::{Duration, Instant};

fn rp(s: &str) -> RelPath {
    RelPath::new(s.as_bytes()).unwrap()
}
fn live() -> Live {
    Live::new(Party::Local, DeleteMode::Off, Instant::now())
}

#[test]
fn percent_counts_skipped_matched_and_done_files() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(
        &Event::Total {
            party: Party::Local,
            files: 3,
            bytes: 1000,
        },
        t,
    );
    l.apply(&Event::Skipped(rp("a"), 500), t);
    l.apply(&Event::FileStart(rp("b"), 400), t);
    l.apply(&Event::FileMatched(rp("b"), 300), t);
    l.apply(&Event::FileBytes(rp("b"), 50), t);
    assert_eq!(l.transfer_fraction(), (850, 1000));
    assert_eq!(l.transfer_percent(), Some(85));
    l.apply(&Event::FileDone(rp("b"), 400), t);
    assert_eq!(l.transfer_fraction(), (900, 1000));
    assert_eq!(l.xfr, 1);
    assert_eq!(l.in_flight(), 0);
}

#[test]
fn percent_is_none_before_a_total_and_capped_after() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(&Event::FileStart(rp("b"), 100), t);
    l.apply(&Event::FileBytes(rp("b"), 100), t);
    assert_eq!(l.transfer_percent(), None);
    l.apply(
        &Event::Total {
            party: Party::Local,
            files: 1,
            bytes: 50,
        },
        t,
    );
    assert_eq!(l.transfer_percent(), Some(100));
}

#[test]
fn pull_uses_the_peer_total_as_denominator() {
    let mut l = Live::new(Party::Peer, DeleteMode::Off, Instant::now());
    let t = Instant::now();
    l.apply(
        &Event::Total {
            party: Party::Local,
            files: 9,
            bytes: 9,
        },
        t,
    );
    l.apply(
        &Event::Total {
            party: Party::Peer,
            files: 1,
            bytes: 200,
        },
        t,
    );
    l.apply(&Event::PeerSkipped(0), t);
    l.apply(&Event::Skipped(rp("x"), 100), t);
    assert_eq!(l.transfer_fraction(), (100, 200));
}

#[test]
fn restarted_file_refreshes_its_row() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(&Event::FileStart(rp("f"), 10), t);
    l.apply(&Event::FileBytes(rp("f"), 4), t);
    l.apply(&Event::FileStart(rp("f"), 10), t + Duration::from_secs(1));
    assert_eq!(l.in_flight(), 1);
    assert_eq!(
        l.rows[&rp("f")].state,
        RowState::Moving {
            literal: 0,
            matched: 0
        }
    );
    assert_eq!(l.rows[&rp("f")].started, t);
}

#[test]
fn hashing_row_clears_on_hashed_and_turns_into_moving_on_start() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(&Event::FileHashing(rp("h"), 5), t);
    assert_eq!(l.rows[&rp("h")].state, RowState::Hashing);
    l.apply(&Event::FileHashed(rp("h")), t);
    assert_eq!(l.in_flight(), 0);
    l.apply(&Event::FileHashing(rp("g"), 5), t);
    l.apply(&Event::FileStart(rp("g"), 5), t);
    assert!(matches!(l.rows[&rp("g")].state, RowState::Moving { .. }));
}

#[test]
fn stalled_and_expired_rows() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(&Event::FileStart(rp("s"), 10), t);
    l.apply(&Event::FileBytes(rp("s"), 1), t);
    assert_eq!(
        l.rows[&rp("s")].stalled_for(t + Duration::from_secs(1)),
        None
    );
    assert_eq!(
        l.rows[&rp("s")].stalled_for(t + STALL_AFTER),
        Some(STALL_AFTER)
    );
    l.tick(t + ROW_EXPIRY);
    assert_eq!(l.in_flight(), 0);
}

#[test]
fn walks_never_go_backwards_and_delete_finishes_at_total() {
    let (mut l, t) = (
        Live::new(Party::Local, DeleteMode::After, Instant::now()),
        Instant::now(),
    );
    l.apply(
        &Event::Scanned {
            party: Party::Local,
            entries: 2000,
            bytes: 10,
            at: Some(rp("a")),
        },
        t,
    );
    l.apply(
        &Event::Scanned {
            party: Party::Local,
            entries: 1000,
            bytes: 5,
            at: None,
        },
        t,
    );
    assert_eq!(
        (l.local.entries, l.local.bytes, l.local.at.clone()),
        (2000, 10, Some(rp("a")))
    );
    let d = l.delete.as_ref().unwrap();
    assert!(!d.started && d.mode == DeleteMode::After);
    l.apply(
        &Event::Delete {
            party: Party::Peer,
            done: 3,
            total: Some(3),
        },
        t,
    );
    l.tick(t);
    assert!(l.delete.as_ref().unwrap().finished);
}

#[test]
fn second_session_resets_counters_but_keeps_connection() {
    use irsync::session::reconnect::ConnState;
    let (mut l, t) = (live(), Instant::now());
    l.apply(&Event::Conn(ConnState::Relay), t);
    l.apply(&Event::FileDone(rp("a"), 7), t);
    l.apply(&Event::SessionStart { index: 1, of: 2 }, t);
    assert_eq!((l.xfr, l.done_bytes, l.session), (0, 0, (1, 2)));
    assert_eq!(l.conn, ConnState::Relay);
}

use irsync::cli::progress::render::{bar, render, truncate_middle, Geometry};

fn mid_sync() -> (Live, Instant) {
    let t = Instant::now();
    let mut l = Live::new(Party::Local, DeleteMode::After, t);
    l.apply(
        &Event::Scanned {
            party: Party::Local,
            entries: 18204,
            bytes: 7_000_000_000,
            at: Some(rp("2023")),
        },
        t,
    );
    l.apply(
        &Event::Scanned {
            party: Party::Peer,
            entries: 9120,
            bytes: 0,
            at: Some(rp("2022")),
        },
        t,
    );
    l.apply(
        &Event::FileStart(rp("2024/trip/clip_0007.mov"), 1_900_000_000),
        t,
    );
    l.apply(
        &Event::FileBytes(rp("2024/trip/clip_0007.mov"), 684_000_000),
        t,
    );
    l.apply(
        &Event::FileStart(rp("2024/trip/IMG_0419.heic"), 4_200_000),
        t,
    );
    l.apply(
        &Event::FileHashing(rp("2024/trip/clip_0009.mov"), 2_400_000_000),
        t,
    );
    (l, t)
}

#[test]
fn block_has_rule_phases_and_rows_largest_first() {
    let (l, t) = mid_sync();
    let lines = render(&l, &Geometry::fixed(120, 40), t);
    assert!(lines[0].chars().all(|c| c == '─'), "{lines:?}");
    assert!(
        lines[1].starts_with("list      18,204 entries  walking 2023/"),
        "{}",
        lines[1]
    );
    assert!(
        lines[1].contains("peer 9,120 entries  walking 2022/"),
        "{}",
        lines[1]
    );
    assert!(lines[2].starts_with("transfer  ["), "{}", lines[2]);
    assert!(
        lines[2].contains("3 in flight") && lines[2].contains("[connecting]"),
        "{}",
        lines[2]
    );
    assert!(
        lines[3].contains("clip_0009.mov") && lines[3].contains("hashing"),
        "{}",
        lines[3]
    );
    assert!(
        lines[4].contains("clip_0007.mov") && lines[4].contains(" 36%"),
        "{}",
        lines[4]
    );
    assert!(lines[5].contains("IMG_0419.heic"), "{}", lines[5]);
    assert!(
        lines
            .last()
            .unwrap()
            .starts_with("delete    waiting (--delete-after)"),
        "{lines:?}"
    );
    for l in &lines {
        assert!(l.chars().count() <= 120, "{l:?}");
    }
}

#[test]
fn done_phases_collapse() {
    let t = Instant::now();
    let mut l = Live::new(Party::Local, DeleteMode::Off, t);
    l.apply(
        &Event::Total {
            party: Party::Local,
            files: 4010,
            bytes: 7_210_000_000,
        },
        t,
    );
    l.apply(
        &Event::Total {
            party: Party::Peer,
            files: 0,
            bytes: 0,
        },
        t,
    );
    l.transfer_done = true;
    l.xfr = 4010;
    let lines = render(&l, &Geometry::fixed(100, 30), t + Duration::from_secs(176));
    assert!(
        lines[1].starts_with("list      done  4,010 entries"),
        "{}",
        lines[1]
    );
    assert!(
        lines[2].starts_with("transfer  done  ")
            && lines[2].contains("4,010 files")
            && lines[2].contains("0:02:56"),
        "{}",
        lines[2]
    );
    assert_eq!(lines.len(), 3);
}

#[test]
fn rows_are_capped_to_half_the_height() {
    let t = Instant::now();
    let mut l = live();
    for i in 0..40 {
        l.apply(&Event::FileStart(rp(&format!("f{i:02}")), 100 - i), t);
    }
    let lines = render(&l, &Geometry::fixed(100, 24), t);
    assert!(lines.len() <= 12, "{}", lines.len());
    assert!(lines.last().unwrap().contains("… and"), "{lines:?}");
    let lines = render(&l, &Geometry::fixed(100, 6), t);
    assert!(
        lines.iter().filter(|l| l.starts_with("  [")).count() >= 3,
        "{lines:?}"
    );
}

#[test]
fn long_paths_are_truncated_in_the_middle_never_wrapped() {
    let t = Instant::now();
    let mut l = live();
    let long = "a/".repeat(80) + "end.bin";
    l.apply(&Event::FileStart(rp(&long), 10), t);
    let g = Geometry::fixed(60, 20);
    let lines = render(&l, &g, t);
    for line in &lines {
        assert!(line.chars().count() <= 60, "{line:?}");
    }
    assert!(
        lines
            .iter()
            .any(|l| l.contains('…') && l.contains("end.bin")),
        "{lines:?}"
    );
    assert_eq!(truncate_middle("abcdefghij", 7, &g), "abc…hij");
}

#[test]
fn narrow_terminals_drop_columns() {
    let (l, t) = mid_sync();
    let lines = render(&l, &Geometry::fixed(59, 30), t);
    let row = lines.iter().find(|l| l.contains("clip_0007")).unwrap();
    assert!(row.contains('[') && !row.contains("B/s"), "{row}");
    let lines = render(&l, &Geometry::fixed(39, 30), t);
    let row = lines.iter().find(|l| l.contains("clip_0007")).unwrap();
    assert!(!row.contains('[') && row.contains("36%"), "{row}");
    for line in &lines {
        assert!(line.chars().count() <= 39, "{line:?}");
    }
}

#[test]
fn ascii_glyphs_without_utf8_locale() {
    let mut g = Geometry::fixed(80, 20);
    assert_eq!(bar(1, 2, 4, &g), "[██░░]");
    g.unicode = false;
    assert_eq!(bar(1, 2, 4, &g), "[##--]");
    let (l, t) = mid_sync();
    let lines = render(&l, &g, t);
    assert!(lines.iter().all(|l| l.is_ascii()), "{lines:?}");
}

#[test]
fn stalled_and_reconnecting_are_shown() {
    use irsync::session::reconnect::ConnState;
    let (mut l, t) = mid_sync();
    l.apply(&Event::Conn(ConnState::Reconnecting { attempt: 3 }), t);
    let later = t + Duration::from_secs(8);
    let lines = render(&l, &Geometry::fixed(120, 40), later);
    assert!(
        lines[2].contains("0B/s") && lines[2].contains("[reconnecting #3]"),
        "{}",
        lines[2]
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("clip_0007") && l.contains("stalled 8s")),
        "{lines:?}"
    );
}
