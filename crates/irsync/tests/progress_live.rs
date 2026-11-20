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
            literal: 4,
            matched: 0
        }
    );
    assert_eq!(l.rows[&rp("f")].started, t);
}

#[test]
fn matched_before_start_is_kept() {
    use irsync::cli::progress::render::{render, Geometry};
    let (mut l, t) = (live(), Instant::now());
    l.apply(
        &Event::Total {
            party: Party::Local,
            files: 1,
            bytes: 400,
        },
        t,
    );
    l.apply(&Event::FileMatched(rp("d"), 300), t);
    // Matched but no stream yet: counted, not shown.
    assert_eq!(l.in_flight(), 0);
    assert_eq!(l.transfer_fraction(), (300, 400));
    let lines = render(&l, &Geometry::fixed(120, 40), t);
    assert!(!lines.iter().any(|x| x.contains(" d ")), "{lines:?}");
    l.apply(&Event::FileStart(rp("d"), 400), t);
    assert_eq!(l.in_flight(), 1);
    let lines = render(&l, &Geometry::fixed(120, 40), t);
    assert!(lines.iter().any(|x| x.contains(" d ")), "{lines:?}");
    l.apply(&Event::FileBytes(rp("d"), 50), t);
    assert_eq!(l.rows[&rp("d")].position(), 350);
    assert_eq!(l.in_flight(), 1);
    // A later, smaller match (more of the file requested) replaces it.
    l.apply(&Event::FileMatched(rp("d"), 200), t);
    assert_eq!(l.rows[&rp("d")].position(), 250);
}

#[test]
fn sent_rows_leave_the_block_but_keep_their_bytes() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(
        &Event::Total {
            party: Party::Local,
            files: 1,
            bytes: 100,
        },
        t,
    );
    l.apply(&Event::FileStart(rp("a"), 100), t);
    l.apply(&Event::FileBytes(rp("a"), 60), t);
    assert_eq!(l.in_flight(), 1);
    // Written in full: it leaves at once, ahead of its `FileSent`.
    l.apply(&Event::FileBytes(rp("a"), 40), t);
    assert_eq!(l.in_flight(), 0);
    l.apply(&Event::FileSent(rp("a")), t);
    assert_eq!(l.in_flight(), 0);
    assert_eq!(l.transfer_fraction().0, 100);
    assert_eq!(l.transfer_percent(), Some(100));
    // A re-request opens a stream again: the row is back.
    l.apply(&Event::FileStart(rp("a"), 100), t);
    assert_eq!(l.in_flight(), 1);
    l.apply(&Event::FileSent(rp("a")), t);
    l.apply(&Event::FileDone(rp("a"), 100), t);
    assert_eq!(l.transfer_fraction().0, 100);
    assert_eq!(l.transfer_percent(), Some(100));
    assert_eq!(l.xfr, 1);
    // Sent for a file without a row: ignored.
    l.apply(&Event::FileSent(rp("zz")), t);
    assert!(l.rows.is_empty());
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

use irsync::cli::progress::render::{bar, goodput_text, render, truncate_middle, Geometry};

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
fn block_has_rule_phases_and_rows_large_files_first() {
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

#[test]
fn phase_bars_are_dropped_when_they_do_not_fit() {
    let (mut l, t) = mid_sync();
    l.apply(
        &Event::Delete {
            done: 5,
            total: Some(10),
            party: Party::Local,
        },
        t,
    );
    let lines = render(&l, &Geometry::fixed(44, 30), t);
    let transfer = lines.iter().find(|l| l.starts_with("transfer")).unwrap();
    let delete = lines.iter().find(|l| l.starts_with("delete")).unwrap();
    assert!(
        transfer.contains('%') && !transfer.contains('░'),
        "{transfer}"
    );
    assert!(delete.contains("50%") && !delete.contains('░'), "{delete}");
    assert!(!transfer.contains('…') && !delete.contains('…'));
}

#[test]
fn connection_state_survives_at_80_columns() {
    use irsync::session::reconnect::ConnState;
    let (mut l, t) = mid_sync();
    l.apply(&Event::Conn(ConnState::Reconnecting { attempt: 2 }), t);
    let lines = render(&l, &Geometry::fixed(80, 30), t);
    assert!(lines[2].contains("[reconnecting #2]"), "{}", lines[2]);
    assert!(lines[2].chars().count() <= 80);
}

#[test]
fn skips_re_reported_after_a_reconnect_are_not_counted_twice() {
    use irsync::session::reconnect::ConnState;
    let (mut l, t) = (live(), Instant::now());
    l.apply(
        &Event::Total {
            party: Party::Local,
            files: 3,
            bytes: 1000,
        },
        t,
    );
    l.apply(&Event::FileDone(rp("a"), 200), t);
    l.apply(&Event::Skipped(rp("b"), 100), t);
    assert_eq!(l.transfer_fraction(), (300, 1000));
    l.apply(&Event::Conn(ConnState::Reconnecting { attempt: 1 }), t);
    assert_eq!(l.transfer_fraction(), (300, 1000), "never backwards");
    // The new connection re-reports the unchanged file (not the placed one).
    l.apply(&Event::Skipped(rp("b"), 100), t);
    assert_eq!(l.transfer_fraction(), (300, 1000));
    // A genuinely new skip.
    l.apply(&Event::Skipped(rp("c"), 200), t);
    assert_eq!(l.transfer_fraction(), (500, 1000));
    // A second reconnect keeps the larger total.
    l.apply(&Event::Conn(ConnState::Reconnecting { attempt: 2 }), t);
    l.apply(&Event::Skipped(rp("b"), 100), t);
    assert_eq!(l.transfer_fraction(), (500, 1000));
}

#[test]
fn a_reconnect_shows_the_listing_walking_again() {
    use irsync::session::reconnect::ConnState;
    let (mut l, t) = (live(), Instant::now());
    l.apply(
        &Event::Total {
            party: Party::Local,
            files: 5,
            bytes: 50,
        },
        t,
    );
    l.apply(
        &Event::Total {
            party: Party::Peer,
            files: 4,
            bytes: 40,
        },
        t,
    );
    assert!(l.local.done && l.peer.done);
    l.apply(&Event::Conn(ConnState::Reconnecting { attempt: 1 }), t);
    // A resumed connection with a complete stored plan lists nothing: the
    // walks stay done until one of them actually restarts.
    assert!(l.local.done && l.peer.done);
    l.apply(&Event::Conn(ConnState::Direct), t);
    l.apply(
        &Event::Scanned {
            party: Party::Local,
            entries: 2,
            bytes: 20,
            at: Some(rp("d")),
        },
        t,
    );
    assert!(!l.local.done, "the local walk restarted");
    assert!(l.peer.done, "the peer's has not (yet)");
    assert_eq!((l.local.entries, l.local.bytes), (5, 50), "counts kept");
    assert_eq!((l.peer.entries, l.peer.bytes), (4, 40));
    let list = render(&l, &Geometry::fixed(80, 24), t)[1].clone();
    assert!(list.contains("5 entries  walking d/"), "{list}");
    l.apply(
        &Event::Total {
            party: Party::Local,
            files: 5,
            bytes: 50,
        },
        t,
    );
    assert!(l.local.done);
    // A later scan of a finished walk (no reconnect since) changes nothing.
    l.apply(
        &Event::Scanned {
            party: Party::Local,
            entries: 5,
            bytes: 50,
            at: Some(rp("d")),
        },
        t,
    );
    assert!(l.local.done);
}

#[test]
fn rows_awaiting_their_ack_or_their_stream_do_not_expire() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(
        &Event::Total {
            party: Party::Local,
            files: 3,
            bytes: 30,
        },
        t,
    );
    // Sent, ack outstanding.
    l.apply(&Event::FileStart(rp("s"), 10), t);
    l.apply(&Event::FileBytes(rp("s"), 10), t);
    l.apply(&Event::FileSent(rp("s")), t);
    // Pending: matched before its stream opened.
    l.apply(&Event::FileMatched(rp("p"), 4), t);
    // Moving, opened, unsent: still expires.
    l.apply(&Event::FileStart(rp("m"), 10), t);
    l.apply(&Event::FileBytes(rp("m"), 3), t);
    l.tick(t + ROW_EXPIRY + Duration::from_secs(1));
    assert!(l.rows.contains_key(&rp("s")), "sent row kept");
    assert!(l.rows.contains_key(&rp("p")), "pending row kept");
    assert!(!l.rows.contains_key(&rp("m")), "idle moving row dropped");
    assert_eq!(l.transfer_fraction(), (14, 30));
}

#[test]
fn wide_characters_are_measured_in_cells() {
    let t = Instant::now();
    let mut l = live();
    l.apply(&Event::FileStart(rp("照片/2024/视频_0007.mov"), 1 << 30), t);
    l.apply(
        &Event::FileStart(rp(&("照片/".repeat(40) + "视频_0007.mov")), 10),
        t,
    );
    let g = Geometry::fixed(60, 20);
    let lines = render(&l, &g, t);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("照片/") && l.contains(".mov")),
        "{lines:?}"
    );
    for line in &lines {
        assert!(console::measure_text_width(line) <= 60, "{line:?}");
    }
    let cut = truncate_middle("日本語日本語", 7, &g);
    assert!(console::measure_text_width(&cut) <= 7, "{cut:?}");
    assert!(cut.contains('…'), "{cut:?}");
    assert_eq!(truncate_middle("日本語日本語", 12, &g), "日本語日本語");
}

#[test]
fn failed_events_are_counted_and_the_last_three_kept_in_order() {
    let mut l = live();
    let now = Instant::now();
    for i in 1..=4 {
        l.apply(
            &Event::Failed(rp(&format!("f{i}")), format!("boom {i}")),
            now,
        );
    }
    assert_eq!(l.failed, 4);
    let recent: Vec<(String, String)> = l
        .recent_errors
        .iter()
        .map(|(p, m)| {
            (
                String::from_utf8_lossy(p.as_bytes()).into_owned(),
                m.clone(),
            )
        })
        .collect();
    assert_eq!(
        recent,
        vec![
            ("f2".to_string(), "boom 2".to_string()),
            ("f3".to_string(), "boom 3".to_string()),
            ("f4".to_string(), "boom 4".to_string()),
        ]
    );
}

#[test]
fn matched_bytes_follow_each_files_latest_estimate() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(&Event::FileMatched(rp("b"), 300), t); // before FileStart
    l.apply(&Event::FileStart(rp("b"), 400), t);
    l.apply(&Event::FileMatched(rp("b"), 350), t);
    assert_eq!(l.matched, 350);
    // The sender's estimate shrinks as more chunks are requested.
    l.apply(&Event::FileMatched(rp("b"), 100), t);
    assert_eq!(l.matched, 100);
    // Hashed again (a retry): the next estimate replaces, not adds.
    l.apply(&Event::FileHashing(rp("b"), 400), t);
    l.apply(&Event::FileMatched(rp("b"), 100), t);
    assert_eq!(l.matched, 100);
    l.apply(&Event::FileDone(rp("b"), 400), t);
    l.apply(&Event::FileMatched(rp("c"), 50), t);
    assert_eq!(l.matched, 150, "a finished file keeps its share");
}

#[test]
fn synced_rate_is_never_below_the_sent_rate() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(&Event::FileMatched(rp("f"), 1000), t);
    l.tick(t);
    l.apply(&Event::FileMatched(rp("f"), 200), t);
    l.apply(&Event::FileBytes(rp("f"), 300), t);
    l.tick(t + Duration::from_secs(1));
    assert_eq!(l.current_rates(), Some((300.0, 300.0)));
}

#[test]
fn current_rates_cover_the_last_five_seconds() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(&Event::FileStart(rp("f"), 1 << 30), t);
    l.tick(t);
    assert_eq!(l.current_rates(), None);
    l.apply(&Event::FileBytes(rp("f"), 500), t);
    l.tick(t + Duration::from_millis(500));
    assert_eq!(l.current_rates(), None, "under 1 s of samples");
    l.apply(&Event::FileMatched(rp("f"), 1500), t);
    l.tick(t + Duration::from_secs(1));
    assert_eq!(l.current_rates(), Some((500.0, 2000.0)));
    // Bytes stop; ticks go on: the burst leaves the window.
    for s in 2..=7 {
        l.tick(t + Duration::from_secs(s));
    }
    assert_eq!(l.current_rates(), Some((0.0, 0.0)));
}

#[test]
fn a_late_tick_uses_the_real_time_span() {
    let (mut l, t) = (live(), Instant::now());
    l.tick(t);
    l.apply(&Event::FileBytes(rp("f"), 1000), t);
    l.tick(t + Duration::from_secs(10));
    assert_eq!(l.current_rates(), Some((100.0, 100.0)));
}

#[test]
fn goodput_is_hidden_without_wire_bytes() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(&Event::FileMatched(rp("f"), 1000), t);
    assert_eq!(l.goodput(), None);
    l.apply(&Event::FileBytes(rp("f"), 250), t);
    assert_eq!(l.goodput(), Some(5.0));
}

#[test]
fn a_new_session_clears_matched_and_samples() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(&Event::FileMatched(rp("f"), 1000), t);
    l.tick(t);
    l.apply(&Event::SessionStart { index: 1, of: 2 }, t);
    assert_eq!(l.matched, 0);
    assert!(l.samples.is_empty());
}

#[test]
fn goodput_is_a_percent_up_to_two_and_a_ratio_above() {
    let mut g = Geometry::fixed(80, 20);
    assert_eq!(goodput_text(1.0, &g), "goodput 100%");
    assert_eq!(goodput_text(2.0, &g), "goodput 200%");
    assert_eq!(goodput_text(2.1, &g), "goodput 2.1×");
    assert_eq!(goodput_text(999.9, &g), "goodput 999.9×");
    assert_eq!(goodput_text(5000.0, &g), "goodput >999×");
    g.unicode = false;
    assert_eq!(goodput_text(9.2, &g), "goodput 9.2x");
}

#[test]
fn transfer_line_shows_labelled_rates_and_goodput() {
    let (mut l, t) = mid_sync();
    l.tick(t);
    l.apply(
        &Event::FileMatched(rp("2024/trip/clip_0007.mov"), 5_480_000_000),
        t,
    );
    l.apply(
        &Event::FileBytes(rp("2024/trip/clip_0007.mov"), 1_000_000),
        t,
    );
    l.tick(t + Duration::from_secs(1));
    let lines = render(&l, &Geometry::fixed(200, 40), t + Duration::from_secs(1));
    let line = &lines[2];
    assert!(line.contains("976.56KB/s sent"), "{line}");
    assert!(line.contains("5.10GB/s synced"), "{line}");
    assert!(line.contains("goodput 9.0×"), "{line}");
    let order = [
        "B/s sent",
        "B/s synced",
        "goodput",
        "0:00:01",
        "in flight",
        "xfr#",
        "[connecting]",
    ];
    let at: Vec<usize> = order.iter().map(|s| line.find(s).unwrap()).collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{line}");
}

#[test]
fn narrow_transfer_lines_drop_xfr_before_goodput_before_synced() {
    let (mut l, t) = mid_sync();
    l.tick(t);
    l.apply(
        &Event::FileMatched(rp("2024/trip/clip_0007.mov"), 1_000_000_000),
        t,
    );
    l.tick(t + Duration::from_secs(1));
    let now = t + Duration::from_secs(1);
    let full = render(&l, &Geometry::fixed(200, 40), now)[2].clone();
    let w = full.chars().count();
    let at = |width| render(&l, &Geometry::fixed(width, 40), now)[2].clone();
    assert!(full.contains("xfr#"), "{full}");
    assert!(!at(w - 1).contains("xfr#") && at(w - 1).contains("goodput"));
    let no_xfr = at(w - 1).chars().count();
    assert!(!at(no_xfr - 1).contains("goodput") && at(no_xfr - 1).contains("synced"));
    for width in 20..=200 {
        for line in render(&l, &Geometry::fixed(width, 40), now) {
            assert!(line.chars().count() <= width, "{width}: {line:?}");
        }
    }
}

#[test]
fn done_line_shows_averages_and_goodput() {
    let t = Instant::now();
    let mut l = Live::new(Party::Local, DeleteMode::Off, t);
    l.apply(&Event::FileMatched(rp("f"), 3_000_000), t);
    l.apply(&Event::FileBytes(rp("f"), 1_000_000), t);
    l.transfer_done = true;
    l.xfr = 1;
    let lines = render(&l, &Geometry::fixed(200, 30), t + Duration::from_secs(2));
    assert!(
        lines[2].contains("avg 488.28KB/s sent  1.91MB/s synced  goodput 4.0×  0:00:02"),
        "{}",
        lines[2]
    );
}

#[test]
fn reconnecting_and_done_transfer_lines_fit_every_width() {
    use irsync::session::reconnect::ConnState;
    let (mut l, t) = mid_sync();
    l.apply(
        &Event::FileMatched(rp("2024/trip/clip_0007.mov"), 1_000_000_000),
        t,
    );
    l.apply(&Event::Conn(ConnState::Reconnecting { attempt: 12 }), t);
    let now = t + Duration::from_secs(3);
    let mut done = l.clone();
    done.transfer_done = true;
    for width in 20..=200 {
        for live in [&l, &done] {
            for line in render(live, &Geometry::fixed(width, 40), now) {
                assert!(line.chars().count() <= width, "{width}: {line:?}");
            }
        }
    }
}

#[test]
fn a_resumed_plan_starts_the_percent_at_what_is_settled() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(
        &Event::Total {
            party: Party::Local,
            files: 4,
            bytes: 1000,
        },
        t,
    );
    l.apply(&Event::Settled(600), t);
    assert_eq!(l.transfer_fraction(), (600, 1000));
    l.apply(&Event::FileStart(rp("c"), 400), t);
    l.apply(&Event::FileBytes(rp("c"), 100), t);
    assert_eq!(l.transfer_fraction(), (700, 1000));
    l.apply(&Event::FileDone(rp("c"), 400), t);
    assert_eq!(l.transfer_fraction(), (1000, 1000));
}

/// A reconnect within one run: the block has counted the skipped and the
/// finished files itself, and the resumed plan's settled bytes take them in.
#[test]
fn settled_bytes_are_not_added_to_what_the_block_counted_itself() {
    use irsync::session::reconnect::ConnState;
    let (mut l, t) = (live(), Instant::now());
    l.apply(
        &Event::Total {
            party: Party::Local,
            files: 4,
            bytes: 1000,
        },
        t,
    );
    l.apply(&Event::PeerSkipped(100), t);
    l.apply(&Event::Skipped(rp("a"), 100), t);
    l.apply(&Event::FileStart(rp("b"), 200), t);
    l.apply(&Event::FileBytes(rp("b"), 200), t);
    l.apply(&Event::FileDone(rp("b"), 200), t);
    // In flight when the connection drops, with a matched estimate.
    l.apply(&Event::FileStart(rp("c"), 600), t);
    l.apply(&Event::FileMatched(rp("c"), 50), t);
    l.apply(&Event::FileBytes(rp("c"), 300), t);
    assert_eq!(l.transfer_fraction(), (750, 1000));
    assert_eq!(l.matched, 50);

    l.apply(&Event::Conn(ConnState::Reconnecting { attempt: 1 }), t);
    // Both skips and the finished file; `c` is requested again.
    l.apply(&Event::Settled(400), t);
    assert_eq!(l.transfer_fraction(), (400, 1000));
    assert_eq!(l.in_flight(), 0);
    assert_eq!(l.matched, 0, "the dropped row's estimate");
    assert_eq!(l.xfr, 1, "files finished in this run stay counted");

    // What the receiver kept of `c` comes back as matched.
    l.apply(&Event::FileStart(rp("c"), 600), t);
    l.apply(&Event::FileMatched(rp("c"), 300), t);
    l.apply(&Event::FileBytes(rp("c"), 300), t);
    l.apply(&Event::FileDone(rp("c"), 600), t);
    assert_eq!(l.transfer_fraction(), (1000, 1000));
}

/// A push writes a small file to its stream at once, then waits for the
/// receiver's acknowledgement with no event for the row. It is not drawn
/// (so never as `stalled`) and still counts toward the percent.
#[test]
fn a_fully_written_row_waits_out_of_sight_and_is_never_stalled() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(
        &Event::Total {
            party: Party::Local,
            files: 2,
            bytes: 2000,
        },
        t,
    );
    l.apply(&Event::FileStart(rp("a"), 1000), t);
    l.apply(&Event::FileBytes(rp("a"), 400), t);
    l.apply(&Event::FileStart(rp("b"), 1000), t);
    l.apply(&Event::FileBytes(rp("b"), 1000), t);
    assert_eq!(l.in_flight(), 1, "only the file still being written");

    let later = t + STALL_AFTER + Duration::from_secs(1);
    l.tick(later);
    let frame = render(&l, &Geometry::fixed(100, 40), later);
    assert!(
        !frame.iter().any(|line| line.contains("100%")),
        "{frame:#?}"
    );
    assert_eq!(
        frame.iter().filter(|line| line.contains("stalled")).count(),
        1,
        "the half-written file, with no data for 3 s: {frame:#?}"
    );
    // Kept past the expiry of a shown row, for the percent.
    l.tick(t + ROW_EXPIRY + Duration::from_secs(1));
    assert_eq!(l.transfer_fraction(), (1000, 2000));
    l.apply(&Event::FileDone(rp("b"), 1000), t);
    assert_eq!(l.transfer_fraction(), (1000, 2000));
}

/// A matched estimate can say the whole file is in place before its data
/// is asked for; when it shrinks the row is drawn again.
#[test]
fn a_row_comes_back_when_its_matched_estimate_shrinks() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(&Event::FileStart(rp("a"), 1000), t);
    l.apply(&Event::FileMatched(rp("a"), 1000), t);
    assert_eq!(l.in_flight(), 0);
    l.apply(&Event::FileMatched(rp("a"), 600), t);
    assert_eq!(l.in_flight(), 1);
}

#[test]
fn a_file_the_receiver_waits_for_has_no_row_until_its_data_comes() {
    let mut l = Live::new(Party::Peer, DeleteMode::Off, Instant::now());
    let t = Instant::now();
    l.apply(
        &Event::Total {
            party: Party::Peer,
            files: 2,
            bytes: 2000,
        },
        t,
    );
    // Opened for writing, both waiting for the sender's streams.
    for p in ["a", "b"] {
        l.apply(&Event::FileOpened(rp(p), 1000), t);
        l.apply(&Event::FileMatched(rp(p), 200), t);
    }
    assert_eq!(l.in_flight(), 0);
    // What is matched counts already.
    assert_eq!(l.transfer_fraction(), (400, 2000));
    // However long the wait, nothing is drawn, stalled or dropped.
    let late = t + ROW_EXPIRY * 2;
    l.tick(late);
    assert_eq!(l.in_flight(), 0);
    assert_eq!(l.rows.len(), 2);

    // The first data makes the row, with its size known.
    l.apply(&Event::FileBytes(rp("a"), 300), late);
    assert_eq!(l.in_flight(), 1);
    let row = &l.rows[&rp("a")];
    assert!(row.shown());
    assert_eq!((row.size, row.position()), (1000, 500));
    assert_eq!(row.stalled_for(late), None);
    assert_eq!(row.stalled_for(late + STALL_AFTER), Some(STALL_AFTER));
    assert_eq!(l.transfer_fraction(), (700, 2000));
    // The rate is taken from the first byte, not from the wait before it.
    l.tick(late + Duration::from_secs(1));
    assert_eq!(l.rows[&rp("a")].rate, 300.0);
}

#[test]
fn a_file_opened_again_waits_again() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(&Event::FileOpened(rp("a"), 1000), t);
    l.apply(&Event::FileBytes(rp("a"), 300), t);
    assert_eq!(l.in_flight(), 1);
    // A new connection plans the file again: it waits for its stream.
    l.apply(&Event::FileOpened(rp("a"), 1000), t);
    assert_eq!(l.in_flight(), 0);
    assert_eq!(l.rows[&rp("a")].position(), 300);
    l.apply(&Event::FileBytes(rp("a"), 700), t);
    // All of it is here: waiting to be placed, not drawn.
    assert_eq!(l.in_flight(), 0);
    l.apply(&Event::FileDone(rp("a"), 1000), t);
    assert!(l.rows.is_empty());
}

/// The paths of the rows drawn, top to bottom.
fn row_names(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|l| l.starts_with("  [") || l.starts_with("  hashing"))
        .map(|l| {
            l.split_whitespace()
                .find(|w| w.starts_with('f') || w.starts_with("big"))
                .unwrap()
                .to_string()
        })
        .collect()
}

/// Rows `(name, size, bytes per second)` one second into their transfer;
/// a rate of 0 is a row whose stream is open and has moved nothing.
fn rows_at_rates(rows: &[(&str, u64, u64)]) -> (Live, Instant) {
    let (mut l, t) = (live(), Instant::now());
    for (name, size, _) in rows {
        l.apply(&Event::FileStart(rp(name), *size), t);
    }
    let t = t + Duration::from_secs(1);
    for (name, _, rate) in rows.iter().filter(|r| r.2 > 0) {
        l.apply(&Event::FileBytes(rp(name), *rate), t);
    }
    l.tick(t);
    (l, t)
}

#[test]
fn moving_rows_come_first_fastest_first() {
    let (l, t) = rows_at_rates(&[
        ("f1", 1 << 20, 0),
        ("f2", 1 << 20, 10_000),
        ("f3", 1 << 20, 0),
        ("f4", 1 << 20, 300_000),
        ("f5", 1 << 20, 40_000),
    ]);
    let lines = render(&l, &Geometry::fixed(120, 40), t);
    assert_eq!(row_names(&lines), ["f4", "f5", "f2", "f1", "f3"]);
}

#[test]
fn rows_of_about_the_same_speed_keep_their_order() {
    // Within a factor of two the order is by name, so two rows whose
    // rates cross each second do not swap places.
    let (l, t) = rows_at_rates(&[
        ("f1", 1 << 20, 70_000),
        ("f2", 1 << 20, 120_000),
        ("f3", 1 << 20, 66_000),
        ("f4", 1 << 20, 600_000),
    ]);
    let lines = render(&l, &Geometry::fixed(120, 40), t);
    assert_eq!(row_names(&lines), ["f4", "f1", "f2", "f3"]);
}

#[test]
fn moving_files_take_the_places_and_stalled_ones_are_counted() {
    let mut rows: Vec<(String, u64, u64)> = Vec::new();
    for i in 0..10 {
        // The names that sort first move nothing.
        rows.push((format!("f{i:02}"), 1 << 20, 0));
    }
    for i in 10..20 {
        rows.push((format!("f{i:02}"), 1 << 20, 50_000));
    }
    let rows: Vec<(&str, u64, u64)> = rows.iter().map(|r| (r.0.as_str(), r.1, r.2)).collect();
    let (mut l, t) = rows_at_rates(&rows);
    // Height 14: three rows fit.
    let g = Geometry::fixed(120, 14);
    let lines = render(&l, &g, t);
    assert_eq!(row_names(&lines), ["f10", "f11", "f12"]);
    assert!(lines.iter().any(|l| l == "  … and 17 more"), "{lines:?}");

    // A second later the idle ones count as stalled; the others still move.
    let t = t + Duration::from_secs(1);
    for (name, _, rate) in rows.iter().filter(|r| r.2 > 0) {
        l.apply(&Event::FileBytes(rp(name), *rate), t);
    }
    l.tick(t);
    let lines = render(&l, &g, t);
    assert_eq!(row_names(&lines), ["f10", "f11", "f12"]);
    assert!(
        lines.iter().any(|l| l == "  … and 17 more (10 stalled)"),
        "{lines:?}"
    );
}

#[test]
fn stalled_rows_are_drawn_when_there_is_room() {
    let (mut l, t) = rows_at_rates(&[("f1", 1 << 20, 0), ("f2", 1 << 20, 50_000)]);
    let t = t + Duration::from_secs(1);
    l.apply(&Event::FileBytes(rp("f2"), 50_000), t);
    l.tick(t);
    let lines = render(&l, &Geometry::fixed(120, 40), t);
    assert_eq!(row_names(&lines), ["f2", "f1"]);
    assert!(lines.last().unwrap().contains("stalled 2s"), "{lines:?}");
    assert!(lines.iter().all(|l| !l.contains("more")), "{lines:?}");
}

#[test]
fn a_large_file_keeps_the_top_place_until_it_stalls() {
    let mut rows = vec![("big.mov", 1u64 << 30, 20_000u64)];
    let names: Vec<String> = (0..20).map(|i| format!("f{i:02}")).collect();
    for n in &names {
        rows.push((n.as_str(), 1 << 20, 500_000));
    }
    let (mut l, t) = rows_at_rates(&rows);
    let g = Geometry::fixed(120, 14);
    // Slower than every other row, and still on top.
    assert_eq!(row_names(&render(&l, &g, t)), ["big.mov", "f00", "f01"]);

    // The small files go on; the large one gets nothing for two seconds.
    let mut now = t;
    for _ in 0..2 {
        now += Duration::from_secs(1);
        for n in &names {
            l.apply(&Event::FileBytes(rp(n), 100_000), now);
        }
        l.tick(now);
    }
    let lines = render(&l, &g, now);
    assert_eq!(row_names(&lines), ["f00", "f01", "f02"]);
    assert!(
        lines.iter().any(|l| l == "  … and 18 more (1 stalled)"),
        "{lines:?}"
    );
}

#[test]
fn files_of_one_size_are_not_pinned() {
    // No file stands out, so the order is by speed alone.
    let mut rows: Vec<(String, u64, u64)> = (0..30)
        .map(|i| (format!("f{i:02}"), 1 << 20, 10_000))
        .collect();
    rows[29].2 = 900_000;
    let rows: Vec<(&str, u64, u64)> = rows.iter().map(|r| (r.0.as_str(), r.1, r.2)).collect();
    let (l, t) = rows_at_rates(&rows);
    let lines = render(&l, &Geometry::fixed(120, 14), t);
    assert_eq!(row_names(&lines), ["f29", "f00", "f01"]);
}

#[test]
fn hashing_rows_rank_between_moving_and_idle_ones() {
    let (mut l, t) = rows_at_rates(&[("f1", 1 << 20, 0), ("f3", 1 << 20, 1_000)]);
    l.apply(&Event::FileHashing(rp("f2"), 1 << 20), t);
    let lines = render(&l, &Geometry::fixed(120, 40), t);
    assert_eq!(row_names(&lines), ["f3", "f2", "f1"]);
}
