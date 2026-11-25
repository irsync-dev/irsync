//! The `pbwalk` binary: its exit status and what it says on stderr.

use std::net::Ipv6Addr;
use std::path::Path;
use std::process::{Command, Output};

use irsync_proto::capture::v1::{record::Msg, Record};
use irsync_proto::v1;
use prost::Message as _;

const A: Ipv6Addr = Ipv6Addr::new(0xfd69, 0x7273, 0, 0, 0, 0, 0, 1);

fn hello_record(encoded_len: Option<u32>) -> Record {
    let hello = v1::Hello {
        protocol_version: 5,
        request: Some(v1::hello::Request::Probe(v1::Probe {
            path: b"m".to_vec(),
        })),
        ..Default::default()
    };
    Record {
        side: 1,
        direction: 1,
        encoded_len: encoded_len.unwrap_or(hello.encoded_len() as u32),
        msg: Some(Msg::Hello(hello)),
        ..Default::default()
    }
}

fn capture(records: &[Record]) -> Vec<u8> {
    let mut f = pbcap::write::section_header();
    f.extend(pbcap::write::interface(
        pbcap::LINKTYPE_WIRESHARK_UPPER_PDU,
        0,
    ));
    for r in records {
        let pdu = pbcap::write::exported_pdu("irsync", A, A, 1, 1, &r.encode_to_vec());
        f.extend(pbcap::write::packet(1, &pdu));
    }
    f
}

fn pbwalk(file: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pbwalk"))
        .args(extra)
        .arg(file)
        .output()
        .unwrap()
}

fn write(dir: &Path, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.join("c.pcapng");
    std::fs::write(&path, bytes).unwrap();
    path
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn good_capture_exits_0() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), &capture(&[hello_record(None)]));
    let out = pbwalk(&file, &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("protocol_version = 5"));
    assert!(out.stderr.is_empty(), "{}", stderr(&out));
}

#[test]
fn mismatch_exits_1_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), &capture(&[hello_record(Some(1))]));
    let out = pbwalk(&file, &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("do not add up"), "{}", stderr(&out));
}

#[test]
fn truncated_exits_1_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let whole = capture(&[hello_record(None), hello_record(None)]);
    let file = write(dir.path(), &whole[..whole.len() - 5]);
    let out = pbwalk(&file, &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("cut off"), "{}", stderr(&out));
}

#[test]
fn bad_type_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), &capture(&[hello_record(None)]));
    let out = pbwalk(&file, &["--type", "no.such.Type"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("no.such.Type"), "{}", stderr(&out));
    assert!(out.stdout.is_empty());
}

#[test]
fn missing_file_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    let out = pbwalk(&dir.path().join("none.pcapng"), &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("none.pcapng"), "{}", stderr(&out));
    assert!(out.stdout.is_empty());
}

#[test]
fn malformed_block_exits_1_and_says_where_it_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = capture(&[]);
    // Where the first packet should be: a block whose two lengths differ.
    bytes.extend_from_slice(&6u32.to_le_bytes());
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&20u32.to_le_bytes());
    let file = write(dir.path(), &bytes);
    let out = pbwalk(&file, &[]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.contains(
            "pbwalk: stopped reading: not a capture file this tool reads: \
             a block's two lengths differ; totals cover what was read"
        ),
        "{err}"
    );
    assert!(!err.contains("cut off"), "{err}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("By message"));
}
