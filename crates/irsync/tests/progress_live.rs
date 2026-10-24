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
