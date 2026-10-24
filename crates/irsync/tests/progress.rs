//! The reporter renders synthetic session events to a buffer; `plan::itemize`
//! produces rsync's 11-character codes.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use irsync::cli::progress::{escape_path, human, Reporter};
use irsync::cli::Preserve;
use irsync::fs::paths::RelPath;
use irsync::plan::{itemize, Decision, Side};
use irsync::scan::ListStats;
use irsync::session::reconnect::ConnState;
use irsync::session::{Event, Outcome, Party};
use irsync_proto::v1::{file_entry::Kind, FileEntry};

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Buf {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

fn arch() -> Preserve {
    Preserve {
        perms: true,
        times: true,
        owner: true,
        group: true,
        ..Preserve::default()
    }
}

fn rp(s: &str) -> RelPath {
    RelPath::new(s.as_bytes()).unwrap()
}

fn reporter(buf: &Buf, tty: bool, v: u8, stats: bool, itemize: bool) -> Reporter {
    Reporter::new(Box::new(buf.clone()), tty, v, stats, itemize, false)
}

async fn feed(r: Reporter, events: Vec<Event>) -> Reporter {
    let (tx, rx) = tokio::sync::broadcast::channel(64);
    for e in events {
        tx.send(e).unwrap();
    }
    drop(tx);
    r.run(rx, None).await
}

fn entry(kind: Kind, size: u64, mtime: i64, mode: u32) -> FileEntry {
    FileEntry {
        path: b"x".to_vec(),
        kind: kind as i32,
        size,
        mtime_sec: mtime,
        mode,
        ..Default::default()
    }
}

fn side(kind: Kind, size: u64, mtime: i64) -> Side {
    Side {
        entry: entry(kind, size, mtime, 0o644),
        digest: None,
    }
}

#[tokio::test]
async fn verbose_lists_each_file() {
    let buf = Buf::default();
    let r = reporter(&buf, false, 1, false, false);
    feed(
        r,
        vec![
            Event::FileStart(rp("a/one"), 10),
            Event::Bytes(10),
            Event::FileDone(rp("a/one"), 10),
            Event::FileDone(rp("two"), 0),
            Event::Deleted(rp("gone")),
        ],
    )
    .await;
    assert_eq!(buf.text(), "a/one\ntwo\ndeleting gone\n");
}

#[tokio::test]
async fn quiet_default_prints_no_file_lines_and_no_progress_off_tty() {
    let buf = Buf::default();
    let r = reporter(&buf, false, 0, false, false);
    feed(r, vec![Event::Bytes(5), Event::FileDone(rp("a"), 5)]).await;
    assert_eq!(buf.text(), "");
}

#[tokio::test]
async fn non_utf8_paths_are_escaped_not_fatal() {
    let buf = Buf::default();
    let r = reporter(&buf, false, 1, false, false);
    let p = RelPath::new(b"d/\xff\xfe\nx").unwrap();
    feed(r, vec![Event::FileDone(p, 1)]).await;
    assert_eq!(buf.text(), "d/\\377\\376\\012x\n");
    assert_eq!(escape_path(b"a\\b"), "a\\134b");
    assert_eq!(escape_path("héllo".as_bytes()), "héllo");
}

#[tokio::test]
async fn itemize_new_file_code_is_gt_f_plus() {
    let buf = Buf::default();
    let r = reporter(&buf, false, 0, false, true);
    let code = itemize(
        &Decision::Whole,
        Some(&side(Kind::Regular, 3, 1)),
        None,
        '>',
        &arch(),
    );
    assert_eq!(code, ">f+++++++++");
    feed(
        r,
        vec![
            Event::Itemized(rp("new.txt"), code),
            Event::FileDone(rp("new.txt"), 3),
        ],
    )
    .await;
    assert_eq!(buf.text(), ">f+++++++++ new.txt\n");
}

#[tokio::test]
async fn itemize_mtime_size_change_code() {
    let src = side(Kind::Regular, 20, 200);
    let dst = side(Kind::Regular, 10, 100);
    let code = itemize(&Decision::Delta, Some(&src), Some(&dst), '>', &arch());
    assert_eq!(code, ">f.st......");
    let buf = Buf::default();
    let r = reporter(&buf, false, 0, false, true);
    feed(r, vec![Event::Itemized(rp("f"), code)]).await;
    assert_eq!(buf.text(), ">f.st...... f\n");
}

#[test]
fn itemize_codes_for_other_kinds() {
    let d = side(Kind::Directory, 0, 1);
    assert_eq!(
        itemize(&Decision::Mkdir, Some(&d), None, '>', &arch()),
        "cd+++++++++"
    );
    let l = side(Kind::Symlink, 0, 1);
    assert_eq!(
        itemize(&Decision::Symlink, Some(&l), None, '>', &arch()),
        "cL+++++++++"
    );
    let h = side(Kind::Hardlink, 0, 1);
    let hl = Decision::Hardlink { target: rp("t") };
    assert_eq!(itemize(&hl, Some(&h), None, '>', &arch()), "hf+++++++++");
    assert_eq!(
        itemize(&Decision::Delete, None, None, '>', &arch()),
        "*deleting  "
    );
    // Push: `<`; checksum-only difference: `c`; permissions: `p`.
    let a = side(Kind::Regular, 5, 1);
    let mut b = side(Kind::Regular, 5, 1);
    assert_eq!(
        itemize(&Decision::Delta, Some(&a), Some(&b), '<', &arch()),
        "<fc........"
    );
    b.entry.mode = 0o600;
    assert_eq!(
        itemize(&Decision::MetaOnly, Some(&a), Some(&b), '>', &arch()),
        ".f...p....."
    );
}

#[tokio::test]
async fn stats_block_has_rsync_headings() {
    let buf = Buf::default();
    let mut r = reporter(&buf, false, 0, true, false);
    let out = Outcome {
        files_transferred: 2,
        bytes_transferred: 1500,
        transferred_size: 4000,
        created: 3,
        bytes_sent: 1234567,
        bytes_received: 89,
        listed: ListStats {
            reg: 5,
            dir: 2,
            link: 1,
            special: 0,
            total_size: 10_000,
        },
        ..Outcome::default()
    };
    r.finish(&out, Duration::from_secs(2));
    let t = buf.text();
    for h in [
        "Number of files: 8 (reg: 5, dir: 2, link: 1, special: 0)",
        "Number of created files: 3",
        "Number of deleted files: 0",
        "Number of regular files transferred: 2",
        "Total file size: 10,000 bytes",
        "Total transferred file size: 4,000 bytes",
        "Literal data: 1,500 bytes",
        "Matched data: 2,500 bytes",
        "File list size: 0",
        "Total bytes sent: 1,234,567",
        "Total bytes received: 89",
        "sent 1,234,567 bytes  received 89 bytes",
        "total size is 10,000  speedup is ",
    ] {
        assert!(t.contains(h), "missing {h:?} in:\n{t}");
    }
}

#[tokio::test]
async fn lagging_reporter_survives() {
    let (tx, rx) = tokio::sync::broadcast::channel(2);
    for i in 0..50u64 {
        let _ = tx.send(Event::Bytes(i));
    }
    drop(tx);
    let buf = Buf::default();
    reporter(&buf, false, 1, false, false).run(rx, None).await;
}

#[test]
fn itemize_letters_only_for_preserved_attributes() {
    let mut src = side(Kind::Regular, 5, 200);
    let mut dst = side(Kind::Regular, 5, 100);
    src.entry.uid = 1;
    dst.entry.uid = 2;
    // uid differs but -o is off: no `o`; mtime differs and -t is on: `t`.
    let p = Preserve {
        times: true,
        ..Preserve::default()
    };
    assert_eq!(
        itemize(&Decision::MetaOnly, Some(&src), Some(&dst), '>', &p),
        ".f..t......"
    );
    let none = Preserve::default();
    assert_eq!(
        itemize(&Decision::MetaOnly, Some(&src), Some(&dst), '>', &none),
        ".f........."
    );
    let o = Preserve {
        owner: true,
        ..Preserve::default()
    };
    assert_eq!(
        itemize(&Decision::MetaOnly, Some(&src), Some(&dst), '>', &o),
        ".f....o...."
    );
}

#[test]
fn deleting_code_is_eleven_chars_and_lines_match() {
    assert_eq!(irsync::plan::DELETING.len(), 11);
    let buf = Buf::default();
    let mut r = reporter(&buf, false, 0, false, true);
    r.handle(Event::Itemized(rp("x"), irsync::plan::DELETING.into()));
    r.handle(Event::WouldDelete(rp("y")));
    assert_eq!(buf.text(), "*deleting   x\n*deleting   y\n");
}

#[test]
fn root_item_prints_dot_slash() {
    let buf = Buf::default();
    let mut r = reporter(&buf, false, 0, false, true);
    r.handle(Event::Itemized(RelPath::root(), ".d..t......".into()));
    assert_eq!(buf.text(), ".d..t...... ./\n");
}

#[tokio::test]
async fn reporter_task_terminates_when_bus_dropped() {
    let bus = irsync::session::EventBus::default();
    let buf = Buf::default();
    let task = tokio::spawn(reporter(&buf, true, 1, false, false).run(bus.subscribe(), None));
    let clone = bus.clone();
    clone.emit(Event::Bytes(1));
    drop(clone);
    drop(bus);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("reporter did not stop")
        .unwrap();
}

#[test]
fn frame_shows_connection_state_and_percent() {
    let buf = Buf::default();
    let mut r = reporter(&buf, true, 0, false, false);
    r.handle(Event::Conn(ConnState::Direct));
    r.handle(Event::Total {
        party: Party::Local,
        files: 10,
        bytes: 1000,
    });
    r.handle(Event::FileDone(rp("a"), 500));
    let f = r.frame(std::time::Instant::now());
    assert!(
        f[2].contains(" 50%") && f[2].contains("[direct]") && f[2].contains("xfr#1"),
        "{f:?}"
    );
    r.handle(Event::Conn(ConnState::Reconnecting { attempt: 1 }));
    let f = r.frame(std::time::Instant::now());
    assert!(f[2].contains("[reconnecting #1]"), "{f:?}");
    assert_eq!(human(456 * 1024 + 800), "456.78K");
    assert_eq!(human(12), "12");
}

#[tokio::test]
async fn tty_reporter_draws_a_block_and_clears_it_before_the_trailer() {
    let buf = Buf::default();
    let mut r = feed(
        reporter(&buf, true, 1, false, false),
        vec![
            Event::FileStart(rp("a"), 10),
            Event::FileBytes(rp("a"), 10),
            Event::FileDone(rp("a"), 10),
        ],
    )
    .await;
    r.finish(&Outcome::default(), Duration::from_secs(1));
    let t = buf.text();
    assert!(t.contains("transfer"), "block drawn: {t:?}");
    assert!(t.contains("\x1b["), "cursor movement used: {t:?}");
    let trailer = t.rfind("sent ").expect("trailer");
    assert!(
        !t[trailer..].contains("transfer"),
        "block gone before the trailer: {t:?}"
    );
    assert!(t.contains("a\n"), "-v line printed above: {t:?}");
}

#[tokio::test]
async fn plain_reporter_output_is_unchanged_with_new_events() {
    let buf = Buf::default();
    feed(
        reporter(&buf, false, 1, false, false),
        vec![
            Event::FileHashing(rp("a"), 10),
            Event::FileHashed(rp("a")),
            Event::FileStart(rp("a"), 10),
            Event::FileMatched(rp("a"), 5),
            Event::FileBytes(rp("a"), 5),
            Event::Skipped(rp("s"), 1),
            Event::PeerSkipped(3),
            Event::Scanned {
                party: Party::Peer,
                entries: 1,
                bytes: 1,
                at: None,
            },
            Event::Delete {
                party: Party::Local,
                done: 0,
                total: Some(1),
            },
            Event::SessionStart { index: 0, of: 1 },
            Event::FileDone(rp("a"), 10),
        ],
    )
    .await;
    assert_eq!(buf.text(), "a\n");
}
