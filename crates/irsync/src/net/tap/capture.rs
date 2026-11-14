//! The capture file: every protocol message as one pcapng packet.
//!
//! `IRSYNC_CAPTURE` names the file. Each record becomes one Wireshark
//! exported-PDU packet for its protobuf decoder, between fake IPv6
//! addresses (one per connection) and ports (one per QUIC stream). One
//! process-wide [`Recorder`] owns the file; a background thread writes it.

use std::ffi::OsString;
use std::fs::{File, OpenOptions, Permissions, TryLockError};
use std::io::{self, BufWriter, Write as _};
use std::net::Ipv6Addr;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use irsync_proto::capture::v1::{Direction, Record, Side, StreamKind};
use prost::Message as _;
use tracing::warn;

use super::pcapng;
use super::scrub::{fit, scrub};

/// The largest record we write: Wireshark's 256 KiB packet limit, less room
/// for the exported-PDU tags and the packet block around it.
const MAX_RECORD: usize = 262_144 - 128;

/// The daemon's fake address. Bytes 10–11 are 1, so it never equals a
/// client address.
pub(crate) const DAEMON_ADDR: Ipv6Addr = Ipv6Addr::new(0xfd69, 0x7273, 0, 0, 0, 1, 0, 0);

/// The fake address of the client end of connection `connection`: its
/// number in the last 32 bits.
pub(crate) fn client_addr(connection: u32) -> Ipv6Addr {
    let [a, b] = [(connection >> 16) as u16, connection as u16];
    Ipv6Addr::new(0xfd69, 0x7273, 0, 0, 0, 0, a, b)
}

/// The fake port, on both ends, of QUIC stream `quic_stream_id`: never 0,
/// wrapping past 65,535.
pub(crate) fn port_for(quic_stream_id: u64) -> u32 {
    (quic_stream_id % 65_535) as u32 + 1
}

/// The record as one pcapng packet: trimmed to fit, between the fake
/// address of the side that sent it and the other.
fn packet_of(ts_micros: u64, mut record: Record) -> Vec<u8> {
    fit(&mut record, MAX_RECORD);
    let client = client_addr(record.connection);
    let (me, peer) = match record.side() {
        Side::Daemon => (DAEMON_ADDR, client),
        Side::Client | Side::Unspecified => (client, DAEMON_ADDR),
    };
    let (src, dst) = match record.direction() {
        Direction::Received => (peer, me),
        Direction::Sent | Direction::Unspecified => (me, peer),
    };
    let port = match record.stream() {
        StreamKind::Unspecified => 0,
        _ => port_for(record.quic_stream_id),
    };
    let pdu = pcapng::exported_pdu(src, dst, port, port, &record.encode_to_vec());
    pcapng::packet(ts_micros, &pdu)
}

struct Job {
    ts_micros: u64,
    record: Record,
}

/// Records submitted but not yet written and flushed.
#[derive(Default)]
struct Pending {
    count: Mutex<u64>,
    idle: Condvar,
}

/// Writes records to one capture file from a background thread.
pub(crate) struct Recorder {
    #[cfg(test)]
    path: PathBuf,
    tx: mpsc::Sender<Job>,
    base_wall_micros: u64,
    base_tokio: tokio::time::Instant,
    pending: Arc<Pending>,
}

/// Opens `path` for writing, mode 0600 if it is new, without truncating,
/// and takes its lock; `None` if another holds the lock. A symlink at
/// `path` is an error: following it would empty and chmod its target.
fn open_locked(path: &Path) -> io::Result<Option<File>> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits())
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e),
    }
}

/// `<stem>.<pid>.pcapng` next to `path`.
fn pid_path(path: &Path) -> PathBuf {
    let mut name = OsString::from(path.file_stem().unwrap_or("capture".as_ref()));
    name.push(format!(".{}.pcapng", std::process::id()));
    path.with_file_name(name)
}

impl Recorder {
    /// Opens the capture at `path`, or at `<stem>.<pid>.pcapng` next to it
    /// when another process holds `path`, and starts it over.
    pub(crate) fn open(path: impl AsRef<Path>) -> io::Result<Recorder> {
        let wanted = path.as_ref();
        let (file, path) = match open_locked(wanted)? {
            Some(f) => (f, wanted.to_owned()),
            None => {
                let alt = pid_path(wanted);
                let f = open_locked(&alt)?.ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::WouldBlock,
                        format!("{} and {} are both in use", wanted.display(), alt.display()),
                    )
                })?;
                (f, alt)
            }
        };
        // Only the lock holder truncates, so a capture in use is never cut.
        file.set_len(0)?;
        file.set_permissions(Permissions::from_mode(0o600))?;
        warn!(
            "writing a protocol capture to {}; it holds paths and file names",
            path.display()
        );
        let base_wall_micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_micros() as u64);
        let base_tokio = tokio::time::Instant::now();
        let (tx, rx) = mpsc::channel();
        let pending = Arc::new(Pending::default());
        let (p, shown) = (pending.clone(), path.clone());
        std::thread::Builder::new()
            .name("irsync-capture".into())
            .spawn(move || write_loop(file, &shown, &rx, &p))?;
        Ok(Recorder {
            #[cfg(test)]
            path,
            tx,
            base_wall_micros,
            base_tokio,
            pending,
        })
    }

    /// The file being written.
    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Queues `record` for the file, its file content taken out first. The
    /// timestamp is taken now, on tokio's clock, so a paused test clock
    /// shows simulated time.
    pub(crate) fn submit(&self, mut record: Record) {
        scrub(&mut record);
        let elapsed = tokio::time::Instant::now().saturating_duration_since(self.base_tokio);
        let ts_micros = self
            .base_wall_micros
            .saturating_add(u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX));
        *self.pending.count.lock().unwrap() += 1;
        if self.tx.send(Job { ts_micros, record }).is_err() {
            // The writer is gone; nothing will write this one.
            self.pending.done(1);
        }
    }

    /// Waits until every record submitted so far is written and flushed,
    /// or `timeout` passes (a writer that panicked never catches up).
    /// Returns whether it caught up.
    pub(crate) fn drain(&self, timeout: Duration) -> bool {
        let count = self.pending.count.lock().unwrap();
        let (count, _) = self
            .pending
            .idle
            .wait_timeout_while(count, timeout, |c| *c > 0)
            .unwrap();
        *count == 0
    }
}

impl Pending {
    fn done(&self, n: u64) {
        let mut count = self.count.lock().unwrap();
        *count -= n;
        if *count == 0 {
            self.idle.notify_all();
        }
    }
}

/// The capture file behind a buffer; after the first error it warns once
/// and writes nothing more.
struct Out<'a> {
    out: BufWriter<File>,
    path: &'a Path,
    failed: bool,
}

impl Out<'_> {
    fn check(&mut self, r: io::Result<()>) {
        if let Err(e) = r {
            warn!(
                "cannot write the protocol capture {}: {e}",
                self.path.display()
            );
            self.failed = true;
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        if !self.failed {
            let r = self.out.write_all(bytes);
            self.check(r);
        }
    }

    fn flush(&mut self) {
        if !self.failed {
            let r = self.out.flush();
            self.check(r);
        }
    }
}

/// The writer thread: the file's header blocks, then each record as it
/// comes, flushed whenever no more are waiting. Ends when the recorder goes.
fn write_loop(file: File, path: &Path, rx: &mpsc::Receiver<Job>, pending: &Pending) {
    let mut out = Out {
        out: BufWriter::with_capacity(1 << 20, file),
        path,
        failed: false,
    };
    out.write(&pcapng::section_header());
    out.write(&pcapng::interface(pcapng::LINKTYPE_WIRESHARK_UPPER_PDU, 0));
    while let Ok(job) = rx.recv() {
        let mut n = 1;
        out.write(&packet_of(job.ts_micros, job.record));
        while let Ok(job) = rx.try_recv() {
            out.write(&packet_of(job.ts_micros, job.record));
            n += 1;
        }
        out.flush();
        pending.done(n);
    }
    out.flush();
}

static RECORDER: OnceLock<Option<Recorder>> = OnceLock::new();

/// The process's recorder: opened on first use when `IRSYNC_CAPTURE` names
/// a file, `None` otherwise or when it cannot be opened.
pub(crate) fn recorder() -> Option<&'static Recorder> {
    RECORDER
        .get_or_init(|| {
            let path = std::env::var_os("IRSYNC_CAPTURE").filter(|p| !p.is_empty())?;
            Recorder::open(&path)
                .inspect_err(|e| {
                    let path = Path::new(&path).display();
                    warn!("cannot write a protocol capture to {path}: {e}");
                })
                .ok()
        })
        .as_ref()
}

/// Before the process exits: waits up to `timeout` for the capture to reach
/// the disk. Costs nothing, and never opens a capture, when none is open.
pub fn drain(timeout: Duration) {
    if let Some(r) = RECORDER.get().and_then(Option::as_ref) {
        if !r.drain(timeout) {
            warn!("the protocol capture may be missing its last records");
        }
    }
}

/// Hands `record` to the process's recorder, if there is one.
pub(crate) fn submit(record: Record) {
    if let Some(r) = recorder() {
        r.submit(record);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use irsync_proto::capture::v1::{record::Msg, Direction, PathChange, Side, StreamKind};
    use irsync_proto::v1::{control, Control, DataChunk, NeedChunks};

    fn u32_le(b: &[u8]) -> u32 {
        u32::from_le_bytes(b[..4].try_into().unwrap())
    }

    /// One packet of a capture file: its timestamp, the exported-PDU tags
    /// as `(tag, value)` and the record.
    struct Packet {
        ts: u64,
        tags: Vec<(u16, Vec<u8>)>,
        record: Record,
    }

    impl Packet {
        fn tag(&self, t: u16) -> &[u8] {
            &self.tags.iter().find(|(k, _)| *k == t).unwrap().1
        }
        fn addrs(&self) -> (Ipv6Addr, Ipv6Addr) {
            let a = |t| Ipv6Addr::from(<[u8; 16]>::try_from(self.tag(t)).unwrap());
            (a(22), a(23))
        }
        fn ports(&self) -> (u32, u32) {
            let p = |t| u32::from_be_bytes(self.tag(t).try_into().unwrap());
            (p(25), p(26))
        }
    }

    /// Parses a whole capture file block by block, checking every block.
    fn parse(file: &[u8]) -> Vec<Packet> {
        let mut blocks = Vec::new();
        let mut at = 0;
        while at < file.len() {
            let kind = u32_le(&file[at..]);
            let len = u32_le(&file[at + 4..]) as usize;
            assert_eq!(len % 4, 0);
            assert_eq!(
                u32_le(&file[at + len - 4..]) as usize,
                len,
                "trailing length"
            );
            blocks.push((kind, &file[at + 8..at + len - 4]));
            at += len;
        }
        assert_eq!(at, file.len());
        assert_eq!(blocks[0].0, 0x0a0d_0d0a);
        assert_eq!(blocks[1].0, 1);
        assert_eq!(u16::from_le_bytes([blocks[1].1[0], blocks[1].1[1]]), 252);
        blocks[2..]
            .iter()
            .map(|(kind, b)| {
                assert_eq!(*kind, 6);
                let ts = (u64::from(u32_le(&b[4..])) << 32) | u64::from(u32_le(&b[8..]));
                let cap = u32_le(&b[12..]) as usize;
                assert!(cap + 32 <= 262_144);
                let mut pdu = &b[20..20 + cap];
                let mut tags = Vec::new();
                loop {
                    let tag = u16::from_be_bytes([pdu[0], pdu[1]]);
                    let len = u16::from_be_bytes([pdu[2], pdu[3]]) as usize;
                    assert_eq!(len % 4, 0);
                    let value = pdu[4..4 + len].to_vec();
                    pdu = &pdu[4 + len..];
                    if tag == 0 {
                        break;
                    }
                    tags.push((tag, value));
                }
                Packet {
                    ts,
                    tags,
                    record: Record::decode(pdu).expect("every record decodes"),
                }
            })
            .collect()
    }

    fn read_packets(r: &Recorder) -> Vec<Packet> {
        assert!(r.drain(Duration::from_secs(30)));
        parse(&std::fs::read(r.path()).unwrap())
    }

    fn need(conn: u32, side: Side, dir: Direction, stream_id: u64, file_id: u64) -> Record {
        Record {
            side: side.into(),
            direction: dir.into(),
            stream: StreamKind::Control.into(),
            quic_stream_id: stream_id,
            connection: conn,
            file_id: Some(file_id),
            msg: Some(Msg::Control(Control {
                msg: Some(control::Msg::Need(NeedChunks {
                    file_id,
                    chunk_idx: vec![1, 2, 3],
                })),
            })),
            ..Default::default()
        }
    }

    #[test]
    fn locked_file_falls_back_to_pid_name() {
        let dir = tempfile::tempdir().unwrap();
        let wanted = dir.path().join("f.pcapng");
        let holder = std::fs::File::create(&wanted).unwrap();
        holder.try_lock().unwrap();
        let r = Recorder::open(&wanted).unwrap();
        let pid = std::process::id();
        assert_eq!(r.path(), dir.path().join(format!("f.{pid}.pcapng")));
        r.submit(need(1, Side::Client, Direction::Sent, 0, 1));
        assert_eq!(read_packets(&r).len(), 1);
        assert_eq!(std::fs::metadata(&wanted).unwrap().len(), 0);
    }

    #[test]
    fn reopening_an_unlocked_file_starts_it_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.pcapng");
        std::fs::write(&path, vec![7; 10_000]).unwrap();
        let r = Recorder::open(&path).unwrap();
        assert_eq!(r.path(), path);
        r.submit(need(1, Side::Client, Direction::Sent, 0, 1));
        assert_eq!(read_packets(&r).len(), 1);
    }

    #[test]
    fn symlink_at_the_path_is_refused() {
        // Following it would empty and chmod whatever file it points to.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("precious");
        std::fs::write(&target, b"keep me").unwrap();
        std::fs::set_permissions(&target, Permissions::from_mode(0o644)).unwrap();
        let link = dir.path().join("f.pcapng");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(Recorder::open(&link).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep me");
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644);
    }

    #[test]
    fn file_is_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let r = Recorder::open(dir.path().join("f.pcapng")).unwrap();
        let mode = std::fs::metadata(r.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn records_reach_disk_without_close() {
        let dir = tempfile::tempdir().unwrap();
        let r = Recorder::open(dir.path().join("f.pcapng")).unwrap();
        for i in 0..3 {
            r.submit(need(1, Side::Client, Direction::Sent, 0, i));
        }
        let got = read_packets(&r);
        assert_eq!(got.len(), 3);
        let ids: Vec<_> = got.iter().map(|p| p.record.file_id()).collect();
        assert_eq!(ids, [0, 1, 2]);
        for p in &got {
            assert_eq!(p.tag(12), b"protobuf");
            assert_eq!(p.tag(24), &[0, 0, 0, 3]);
            assert_eq!(p.tag(33), b"irsync\0\0");
        }
        drop(r);
    }

    #[test]
    fn concurrent_writers_produce_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let r = Recorder::open(dir.path().join("f.pcapng")).unwrap();
        std::thread::scope(|s| {
            for t in 0..8u32 {
                let r = &r;
                s.spawn(move || {
                    for i in 0..500 {
                        r.submit(need(t + 1, Side::Daemon, Direction::Received, 4, i));
                    }
                });
            }
        });
        let got = read_packets(&r);
        assert_eq!(got.len(), 4000);
        for t in 1..=8 {
            let mine: Vec<_> = got.iter().filter(|p| p.record.connection == t).collect();
            assert_eq!(mine.len(), 500);
        }
    }

    #[test]
    fn content_never_reaches_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let r = Recorder::open(dir.path().join("f.pcapng")).unwrap();
        let secret = b"top secret file content, quite unique".repeat(100);
        r.submit(Record {
            side: Side::Client.into(),
            direction: Direction::Sent.into(),
            stream: StreamKind::Data.into(),
            quic_stream_id: 2,
            connection: 1,
            msg: Some(Msg::DataChunk(DataChunk {
                data: secret.clone(),
                ops: vec![],
            })),
            ..Default::default()
        });
        assert!(r.drain(Duration::from_secs(30)));
        let file = std::fs::read(r.path()).unwrap();
        assert!(!file.windows(24).any(|w| w == &secret[..24]));
        let got = parse(&file);
        assert_eq!(got[0].record.trimmed.as_ref().unwrap().content[0].len, 3700);
    }

    #[test]
    fn huge_record_is_trimmed_to_one_packet() {
        let dir = tempfile::tempdir().unwrap();
        let r = Recorder::open(dir.path().join("f.pcapng")).unwrap();
        let mut rec = need(1, Side::Client, Direction::Sent, 0, 1);
        let Some(Msg::Control(Control {
            msg: Some(control::Msg::Need(n)),
        })) = &mut rec.msg
        else {
            panic!()
        };
        n.chunk_idx = (1_000_000..1_100_000).collect();
        r.submit(rec);
        let got = read_packets(&r);
        assert!(got[0].record.trimmed.as_ref().unwrap().dropped_entries > 0);
    }

    #[test]
    fn addresses_follow_the_sender() {
        let dir = tempfile::tempdir().unwrap();
        let r = Recorder::open(dir.path().join("f.pcapng")).unwrap();
        r.submit(need(5, Side::Client, Direction::Sent, 8, 1));
        r.submit(need(5, Side::Client, Direction::Received, 8, 1));
        r.submit(need(5, Side::Daemon, Direction::Sent, 8, 1));
        r.submit(need(5, Side::Daemon, Direction::Received, 8, 1));
        r.submit(Record {
            side: Side::Client.into(),
            connection: 5,
            msg: Some(Msg::PathChange(PathChange {
                remote: "ip:192.0.2.1:7".into(),
            })),
            ..Default::default()
        });
        let got = read_packets(&r);
        let (c, d) = (client_addr(5), DAEMON_ADDR);
        let addrs: Vec<_> = got.iter().map(Packet::addrs).collect();
        assert_eq!(addrs, [(c, d), (d, c), (d, c), (c, d), (c, d)]);
        assert_eq!(got[0].ports(), (9, 9));
        assert_eq!(got[4].ports(), (0, 0));
        assert!(got.windows(2).all(|w| w[0].ts <= w[1].ts));
    }

    #[test]
    fn timestamps_are_wall_clock() {
        let dir = tempfile::tempdir().unwrap();
        let r = Recorder::open(dir.path().join("f.pcapng")).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64;
        r.submit(need(1, Side::Client, Direction::Sent, 0, 1));
        let ts = read_packets(&r)[0].ts;
        assert!(ts.abs_diff(now) < 60_000_000, "{ts} vs {now}");
    }

    #[test]
    fn drain_puts_every_record_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let r = Recorder::open(dir.path().join("f.pcapng")).unwrap();
        for i in 0..1000 {
            r.submit(need(1, Side::Client, Direction::Sent, 0, i));
        }
        assert!(r.drain(Duration::from_secs(30)));
        let got = parse(&std::fs::read(r.path()).unwrap());
        assert_eq!(got.len(), 1000);
        assert_eq!(got[999].record.file_id(), 999);
    }

    #[test]
    fn drain_without_a_recorder_returns_at_once() {
        // No IRSYNC_CAPTURE in tests: nothing to wait for, nothing opened.
        let t = std::time::Instant::now();
        drain(Duration::from_secs(10));
        assert!(t.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test(start_paused = true)]
    async fn paused_clock_shows_simulated_time() {
        let dir = tempfile::tempdir().unwrap();
        let r = Recorder::open(dir.path().join("f.pcapng")).unwrap();
        r.submit(need(1, Side::Client, Direction::Sent, 0, 1));
        tokio::time::advance(Duration::from_secs(3600)).await;
        r.submit(need(1, Side::Client, Direction::Sent, 0, 2));
        let got = read_packets(&r);
        assert_eq!(got[1].ts - got[0].ts, 3_600_000_000);
    }

    #[test]
    fn addresses_and_ports() {
        assert_eq!(client_addr(3), "fd69:7273::3".parse::<Ipv6Addr>().unwrap());
        assert_eq!(
            client_addr(65_536),
            "fd69:7273::1:0".parse::<Ipv6Addr>().unwrap()
        );
        assert_eq!(DAEMON_ADDR, "fd69:7273::1:0:0".parse::<Ipv6Addr>().unwrap());
        assert_ne!(client_addr(65_535), DAEMON_ADDR);
        assert_ne!(client_addr(u32::MAX), DAEMON_ADDR);
        assert_eq!(port_for(0), 1);
        assert_eq!(port_for(65_535), 1);
        assert_eq!(port_for(4), 5);
    }
}
