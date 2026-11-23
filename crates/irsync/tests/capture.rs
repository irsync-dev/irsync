//! A push with `IRSYNC_CAPTURE` set records every protocol message, from
//! both ends, in the capture file.
//!
//! The recorder opens from the environment on first use, so this file holds
//! one test only: it sets the variable before anything touches a tap.

mod common;

use std::fs::File;
use std::path::Path;
use std::time::Duration;

use common::{archive_opts, sync_trees, Flow};
use irsync_proto::capture::v1::{record::Msg, Direction, Record, Side, StreamKind};
use pbwalk::render::hex_blocks;
use pbwalk::Parser as _;
use prost::encoding::encoded_len_varint;
use prost::Message as _;

/// Every record of a capture file, decoded from the body of each packet.
fn read_capture(path: &Path) -> Vec<Record> {
    let file = std::fs::read(path).unwrap();
    pbcap::Reader::new(&file[..])
        .expect("a capture file")
        .map(|p| {
            Record::decode(&p.expect("every block is whole").body[..])
                .expect("every record decodes")
        })
        .collect()
}

/// Copies the repository's `proto/` tree to `to`. tshark reads the copy:
/// Ubuntu's AppArmor profile for tshark denies it the home directory, where
/// the clone usually is, and Wireshark then decodes nothing, without an error.
fn copy_protos(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_protos(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// With `tshark` on `PATH`, every packet must decode as a `Record` from the
/// `.proto` files, the settings the docs give. Skipped silently without it,
/// and with a note where tshark is not allowed to read the capture.
fn check_with_tshark(path: &Path, records: usize) {
    let protos = path.with_file_name("proto");
    copy_protos(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../proto"),
        &protos,
    );
    let Ok(out) = std::process::Command::new("tshark")
        .arg("-r")
        .arg(path)
        .arg("-o")
        .arg("protobuf.default_type:irsync.capture.v1.Record")
        .arg("-o")
        .arg(format!(
            "uat:protobuf_search_paths:\"{}\",\"TRUE\"",
            protos.display()
        ))
        .args([
            "-T",
            "fields",
            "-e",
            "protobuf.message.name",
            "-Y",
            "protobuf",
        ])
        .output()
    else {
        return;
    };
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("You don't have permission to read the file") {
        // Ubuntu confines tshark with AppArmor to /tmp, /var/tmp and ~/tmp;
        // the file itself is ours and readable. Happens with TMPDIR elsewhere.
        eprintln!("skipping tshark: it may not read {}", path.display());
        return;
    }
    assert!(out.status.success(), "tshark failed: {stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<_> = stdout.lines().collect();
    assert_eq!(lines.len(), records, "tshark stderr: {stderr}");
    // Each line lists the packet's message names, outermost first; an
    // unknown type leaves it empty.
    for line in lines {
        assert!(
            line.starts_with("irsync.capture.v1.Record"),
            "not decoded: {line:?}; tshark stderr: {stderr}"
        );
    }
}

fn is_event(r: &Record) -> bool {
    matches!(r.msg, Some(Msg::StreamEvent(_)) | Some(Msg::PathChange(_)))
}

/// A record of the daemon's half of the listing stream: sent by the daemon
/// or received by the client.
fn is_daemon_listing(r: &Record) -> bool {
    r.stream() == StreamKind::Listing
        && matches!(
            (r.side(), r.direction()),
            (Side::Daemon, Direction::Sent) | (Side::Client, Direction::Received)
        )
}

/// The bytes on the last line, `total` or `sum`, of pbwalk's table `title`.
fn table_total(out: &str, title: &str) -> u64 {
    let table = out
        .split(&format!("\n{title}\n"))
        .nth(1)
        .unwrap_or_else(|| panic!("no {title} table in\n{out}"));
    let line = table
        .lines()
        .find(|l| {
            let l = l.trim_start();
            l.starts_with("total ") || l.starts_with("sum ")
        })
        .unwrap_or_else(|| panic!("no total line under {title}"));
    let words: Vec<&str> = line.split_whitespace().collect();
    let b = words.iter().position(|w| *w == "B").expect(line);
    words[b - 1].replace(',', "").parse().expect(line)
}

/// pbwalk accounts for the whole capture: nothing fails to add up, and its
/// totals are the records' own sizes.
fn check_with_pbwalk(path: &Path, records: &[Record]) {
    let args = pbwalk::Args::parse_from(["pbwalk", path.to_str().unwrap()]);
    let mut out = Vec::new();
    let s = pbwalk::run(&args, File::open(path).unwrap(), &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    let wire: Vec<&Record> = records.iter().filter(|r| !is_event(r)).collect();
    assert_eq!(s.packets, records.len() as u64);
    assert_eq!(s.messages, wire.len() as u64);
    assert_eq!(
        s.wire_bytes,
        wire.iter().map(|r| u64::from(r.encoded_len)).sum::<u64>()
    );
    assert_eq!(
        s.prefix_bytes,
        wire.iter()
            .map(|r| encoded_len_varint(u64::from(r.encoded_len)) as u64)
            .sum::<u64>()
    );
    assert_eq!((s.mismatches, s.truncated, s.stopped), (0, false, None));
    for bad in [
        "unexplained",
        "not decodable",
        "type not in the protos",
        "DOES NOT ADD UP",
    ] {
        let line = out.lines().find(|l| l.contains(bad));
        assert_eq!(line, None, "pbwalk printed {bad:?}");
    }
    assert_eq!(table_total(&out, "By message"), s.wire_bytes);
    assert_eq!(table_total(&out, "By field"), s.wire_bytes);
    let with_prefixes = s.wire_bytes + s.prefix_bytes;
    assert_eq!(table_total(&out, "Overhead"), with_prefixes);
    assert_eq!(
        table_total(&out, "By direction (with prefixes)"),
        with_prefixes
    );
    check_hex_columns(path);
}

/// The bytes of the message or event inside a record, as they are in
/// `body`, the record's packet; empty for a record without one.
fn payload(body: &[u8]) -> Vec<u8> {
    use prost::bytes::Buf as _;
    use prost::encoding::{decode_key, decode_varint, skip_field, DecodeContext, WireType};
    let Some(msg) = Record::decode(body).expect("a record").msg else {
        return Vec::new();
    };
    // The field number of the message: that of a record holding only it.
    let only = Record {
        msg: Some(msg),
        ..Default::default()
    }
    .encode_to_vec();
    let (number, _) = decode_key(&mut &only[..]).unwrap();
    let mut buf = body;
    while buf.has_remaining() {
        let (n, wire_type) = decode_key(&mut buf).unwrap();
        if n == number && wire_type == WireType::LengthDelimited {
            let len = decode_varint(&mut buf).unwrap() as usize;
            return buf[..len].to_vec();
        }
        skip_field(wire_type, n, &mut buf, DecodeContext::default()).unwrap();
    }
    panic!("the record's message is not in its packet")
}

/// pbwalk's hex columns are the capture: put back together they are each
/// packet byte for byte, and, unwrapped, the message inside it.
fn check_hex_columns(path: &Path) {
    let listing = |extra: &[&str]| {
        let args = pbwalk::Args::parse_from(
            ["pbwalk", "-C", "--no-totals"]
                .iter()
                .chain(extra)
                .copied()
                .chain([path.to_str().unwrap()]),
        );
        let mut out = Vec::new();
        pbwalk::run(&args, File::open(path).unwrap(), &mut out).unwrap();
        String::from_utf8(out).unwrap()
    };
    let bodies: Vec<Vec<u8>> = pbcap::Reader::new(&std::fs::read(path).unwrap()[..])
        .expect("a capture file")
        .map(|p| p.expect("every block is whole").body)
        .collect();
    assert_eq!(
        hex_blocks(&listing(&["--no-unwrap"])),
        bodies,
        "the hex column is not the capture"
    );
    let unwrapped = hex_blocks(&listing(&[]));
    let payloads: Vec<Vec<u8>> = bodies.iter().map(|b| payload(b)).collect();
    assert_eq!(unwrapped, payloads, "the hex column is not the messages");
    assert!(unwrapped.iter().any(|b| !b.is_empty()));
}

fn write_files(dir: &Path, n: usize, size: usize) {
    for i in 0..n {
        let data: Vec<u8> = (0..size).map(|j| (i * 31 + j * 7) as u8).collect();
        std::fs::write(dir.join(format!("f{i}.bin")), data).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_capture_has_both_sides_of_every_message() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.pcapng");
    std::env::set_var("IRSYNC_CAPTURE", &path);
    let (out, _h) = sync_trees(
        |s| write_files(s, 3, 300_000),
        |_| {},
        &archive_opts(),
        Flow::Push,
    )
    .await;
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    irsync::net::tap::drain_capture(Duration::from_secs(30));
    let recs = read_capture(&path);
    if let Some(keep) = std::env::var_os("IRSYNC_KEEP_CAPTURE") {
        // For looking at the capture by hand, see tools/wireshark/README.md.
        std::fs::create_dir_all(&keep).unwrap();
        std::fs::copy(&path, Path::new(&keep).join("c.pcapng")).unwrap();
    }

    // Stream events are counted apart: a finish is reported as sent by the
    // writer and as received by the reader, but they are not one message.
    let wire = |dir: Direction, daemon_listing: bool| -> Vec<&Record> {
        recs.iter()
            .filter(|r| {
                r.direction() == dir
                    && r.msg.is_some()
                    && !is_event(r)
                    && is_daemon_listing(r) == daemon_listing
            })
            .collect()
    };
    let (sent, recv) = (
        wire(Direction::Sent, false),
        wire(Direction::Received, false),
    );
    assert!(!sent.is_empty());
    // In one process each message is recorded twice: by its sender and by
    // its receiver.
    assert_eq!(sent.len(), recv.len());
    // But the client, the sender of a push, stops reading the daemon's
    // listing once the session is done (`run_role` in session/engine.rs),
    // so the end of it, `ListingEnd` at least, may never be read: what the
    // client received is the start of what the daemon sent.
    let (sent, recv) = (wire(Direction::Sent, true), wire(Direction::Received, true));
    assert!(recv.len() <= sent.len(), "{} of {}", recv.len(), sent.len());
    for (s, r) in sent.iter().zip(&recv) {
        assert_eq!(s.msg, r.msg);
    }
    assert!(recs
        .iter()
        .any(|r| matches!(r.msg, Some(Msg::Hello(_))) && r.side() == Side::Client));
    assert!(recs
        .iter()
        .any(|r| matches!(r.msg, Some(Msg::HelloAck(_))) && r.side() == Side::Daemon));
    let chunks: Vec<_> = recs
        .iter()
        .filter_map(|r| match &r.msg {
            Some(Msg::DataChunk(d)) => Some((r, d)),
            _ => None,
        })
        .collect();
    assert!(!chunks.is_empty());
    assert!(chunks
        .iter()
        .all(|(r, d)| d.data.is_empty() && r.file_id.is_some()));
    // Hello, its reply and stream events may come before the session id is
    // known; every session message carries it.
    assert!(recs
        .iter()
        .filter(|r| matches!(
            r.msg,
            Some(Msg::Control(_))
                | Some(Msg::DataStreamHeader(_))
                | Some(Msg::DataChunk(_))
                | Some(Msg::ListingStreamHeader(_))
        ))
        .all(|r| r.session_id.len() == 16));
    // Both sides know their own endpoint id.
    assert!(recs
        .iter()
        .filter(|r| r.msg.is_some() && !is_event(r))
        .all(|r| r.local_endpoint_id.len() == 32));
    check_with_tshark(&path, recs.len());
    check_with_pbwalk(&path, &recs);
}
