//! A push with `IRSYNC_CAPTURE` set records every protocol message, from
//! both ends, in the capture file.
//!
//! The recorder opens from the environment on first use, so this file holds
//! one test only: it sets the variable before anything touches a tap.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{archive_opts, sync_trees, Flow};
use irsync_proto::capture::v1::{record::Msg, Direction, Record, Side};
use prost::Message as _;

fn u32_le(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}

/// Every record of a capture file: walks the pcapng blocks, skips the
/// exported-PDU tags of each packet and decodes what follows.
fn read_capture(path: &Path) -> Vec<Record> {
    let file = std::fs::read(path).unwrap();
    let mut records = Vec::new();
    let mut at = 0;
    while at < file.len() {
        let kind = u32_le(&file[at..]);
        let len = u32_le(&file[at + 4..]) as usize;
        let body = &file[at + 8..at + len - 4];
        at += len;
        if kind != 6 {
            continue; // section header, interface
        }
        let cap = u32_le(&body[12..]) as usize;
        let mut pdu = &body[20..20 + cap];
        loop {
            let tag = u16::from_be_bytes([pdu[0], pdu[1]]);
            let tlen = u16::from_be_bytes([pdu[2], pdu[3]]) as usize;
            pdu = &pdu[4 + tlen..];
            if tag == 0 {
                break;
            }
        }
        records.push(Record::decode(pdu).expect("every record decodes"));
    }
    assert_eq!(at, file.len());
    records
}

fn is_event(r: &Record) -> bool {
    matches!(r.msg, Some(Msg::StreamEvent(_)) | Some(Msg::PathChange(_)))
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
}
