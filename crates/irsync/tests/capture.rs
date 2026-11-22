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
use irsync_proto::capture::v1::{record::Msg, Direction, Record, Side};
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

/// pbwalk accounts for the whole capture: nothing fails to add up, and its
/// totals are the records' own sizes.
fn check_with_pbwalk(path: &Path, records: &[Record]) {
    let args = pbwalk::Args::parse_from(["pbwalk", "--totals-only", path.to_str().unwrap()]);
    let s = pbwalk::run(&args, File::open(path).unwrap(), &mut Vec::new()).unwrap();
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
    assert_eq!((s.mismatches, s.truncated), (0, false));
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
    let sent = recs
        .iter()
        .filter(|r| r.direction() == Direction::Sent && r.msg.is_some() && !is_event(r))
        .count();
    let recv = recs
        .iter()
        .filter(|r| r.direction() == Direction::Received && !is_event(r))
        .count();
    assert!(sent > 0);
    // In one process each message is recorded twice: by its sender and by
    // its receiver.
    assert_eq!(sent, recv);
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
