//! Unwraps one capture packet from its wrapper record and accounts for
//! every byte the message took on the wire, including what the capture
//! left out.
//!
//! The wrapper follows `pbcap.v1.Envelope`: fields 1 to 19 are its own
//! (2 `direction`, 10 `encoded_len`, 11 `trimmed`), 20 to 39 the message,
//! 40 and up events.

use std::fmt::Write as _;

use pbcap::v1::{Direction, Envelope};
use prost::Message as _;
use prost_reflect::{FieldDescriptor, MessageDescriptor};

use crate::walk::{walk, Field, Value};

/// The wrapper's field numbers that are not printed on the header.
const ENCODED_LEN: u32 = 10;
const TRIMMED: u32 = 11;

/// How a message is framed on the wire.
#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub enum Prefix {
    /// A varint holding the message's length.
    Varint,
    /// Nothing.
    None,
}

/// What a packet holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// A message that was on the wire.
    Message,
    /// Something the recorder saw that was never on the wire.
    Event,
    /// The packet as it is in the file, not unwrapped.
    Raw,
}

/// One packet, accounted for.
#[derive(Clone, PartialEq, Debug)]
pub struct Entry<'a> {
    pub kind: Kind,
    /// Short name of the payload's type ("Hello"); the wrapper's for Raw;
    /// "" when there is no payload.
    pub type_name: String,
    /// The wrapper's own fields as (name, printed value), in file order,
    /// without encoded_len and trimmed. Empty for Raw.
    pub header: Vec<(String, String)>,
    /// Unspecified for Raw.
    pub direction: Direction,
    /// The payload's fields; the packet's for Raw.
    pub fields: Vec<Field<'a>>,
    /// Payload bytes in the file.
    pub present: u64,
    /// The message's size on the wire without prefix: encoded_len; present
    /// for Raw; 0 for Event.
    pub wire_len: u64,
    /// The length prefix on the wire.
    pub prefix: u64,
    /// File content the capture cut, itemised in `trimmed.content`.
    pub cut_content: u64,
    /// How many pieces of content were cut.
    pub cut_pieces: u64,
    /// Bytes the capture cut that `trimmed` does not itemise.
    pub cut_other: u64,
    /// Repeated entries the capture removed.
    pub dropped_entries: u64,
    /// The whole message, when the capture left it out.
    pub not_captured: u64,
    /// Bytes missing from the capture with no `trimmed` to say why.
    pub unexplained: u64,
    /// Why the numbers do not add up, if they do not.
    pub mismatch: Option<String>,
}

impl Entry<'_> {
    /// present + cut_content + cut_other + not_captured + unexplained == wire_len
    pub fn adds_up(&self) -> bool {
        [
            self.present,
            self.cut_content,
            self.cut_other,
            self.not_captured,
            self.unexplained,
        ]
        .iter()
        .try_fold(0u64, |sum, &n| sum.checked_add(n))
            == Some(self.wire_len)
    }

    /// The printed value of wrapper field `name`; its default's printed
    /// form when the record does not carry it. `None` when the wrapper has
    /// no such field among those the header lists.
    pub fn header_value(&self, wrapper: &MessageDescriptor, name: &str) -> Option<String> {
        if let Some((_, value)) = self.header.iter().find(|(k, _)| k == name) {
            return Some(value.clone());
        }
        let field = wrapper.get_field_by_name(name)?;
        is_header_field(field.number()).then(|| print_default(&field))
    }
}

/// One packet, unwrapped by the envelope rules.
pub fn unwrap<'a>(wrapper: &MessageDescriptor, body: &'a [u8], prefix: Prefix) -> Entry<'a> {
    let walked = walk(wrapper, body);
    let mut problems = Vec::new();

    let envelope = match Envelope::decode(body) {
        Ok(envelope) => Some(envelope),
        Err(e) => {
            problems.push(format!("the record does not decode as an envelope: {e}"));
            None
        }
    };
    if matches!(walked.last(), Some(f) if matches!(f.value, Value::Undecodable(_))) {
        problems.push("the record ends in bytes that do not parse".to_owned());
    }

    let header = walked
        .iter()
        .filter(|f| is_header_field(f.number))
        .map(|f| {
            let name = f.name.clone().unwrap_or_else(|| f.number.to_string());
            (
                name,
                print_header_value(wrapper.get_field(f.number), &f.value),
            )
        })
        .collect();

    let mut payloads = walked.into_iter().filter_map(|f| match f.value {
        Value::Message { type_name, fields } if f.number >= *pbcap::MESSAGE_FIELDS.start() => {
            Some((f.number, f.sizes.value, type_name, fields))
        }
        _ => None,
    });
    let payload = payloads.next();
    let extra = payloads.count();
    if extra > 0 {
        problems.push(format!(
            "the record holds {} messages, not one; the first is shown",
            extra + 1
        ));
    }

    let encoded_len = envelope.as_ref().map_or(0, |e| u64::from(e.encoded_len));
    let mut entry = Entry {
        kind: Kind::Message,
        type_name: String::new(),
        header,
        direction: envelope
            .as_ref()
            .map_or(Direction::Unspecified, |e| e.direction()),
        fields: Vec::new(),
        present: 0,
        wire_len: encoded_len,
        prefix: 0,
        cut_content: 0,
        cut_pieces: 0,
        cut_other: 0,
        dropped_entries: 0,
        not_captured: 0,
        unexplained: 0,
        mismatch: None,
    };

    match payload {
        Some((number, present, type_name, fields)) => {
            entry.type_name = short_name(&type_name).to_owned();
            entry.fields = fields;
            if pbcap::MESSAGE_FIELDS.contains(&number) {
                entry.present = present;
                if problems.is_empty() {
                    account_gap(&mut entry, envelope.as_ref(), encoded_len, &mut problems);
                }
            } else {
                // From FIRST_EVENT_FIELD up: never on the wire, counts nothing.
                entry.kind = Kind::Event;
                entry.wire_len = 0;
            }
        }
        None if encoded_len > 0 => entry.not_captured = encoded_len,
        None if envelope.is_some() => entry.kind = Kind::Event,
        None => {}
    }

    if entry.kind == Kind::Message && prefix == Prefix::Varint && envelope.is_some() {
        entry.prefix = prost::encoding::encoded_len_varint(encoded_len) as u64;
    }
    if !problems.is_empty() {
        entry.mismatch = Some(problems.join("; "));
    }
    entry
}

/// Splits what is missing between `present` and `encoded_len` into what
/// `trimmed` explains and what it does not.
fn account_gap(
    entry: &mut Entry,
    envelope: Option<&Envelope>,
    encoded_len: u64,
    problems: &mut Vec<String>,
) {
    let Some(gap) = encoded_len.checked_sub(entry.present) else {
        problems.push(format!(
            "encoded_len {encoded_len} is smaller than the {} bytes in the file",
            entry.present
        ));
        return;
    };
    match envelope.and_then(|e| e.trimmed.as_ref()) {
        Some(trimmed) => {
            let cut_content: u64 = trimmed.content.iter().map(|c| u64::from(c.len)).sum();
            if cut_content > gap {
                problems.push(format!(
                    "the cut content, {cut_content} bytes, is more than the {gap} bytes missing"
                ));
                return;
            }
            entry.cut_content = cut_content;
            entry.cut_pieces = trimmed.content.len() as u64;
            entry.cut_other = gap - cut_content;
            entry.dropped_entries = u64::from(trimmed.dropped_entries);
        }
        None => entry.unexplained = gap,
    }
}

/// One packet as it is in the file.
pub fn raw<'a>(desc: &MessageDescriptor, body: &'a [u8]) -> Entry<'a> {
    let len = body.len() as u64;
    Entry {
        kind: Kind::Raw,
        type_name: desc.name().to_owned(),
        header: Vec::new(),
        direction: Direction::Unspecified,
        fields: walk(desc, body),
        present: len,
        wire_len: len,
        prefix: 0,
        cut_content: 0,
        cut_pieces: 0,
        cut_other: 0,
        dropped_entries: 0,
        not_captured: 0,
        unexplained: 0,
        mismatch: None,
    }
}

/// A wrapper field the header lists: its own, except encoded_len and trimmed.
fn is_header_field(number: u32) -> bool {
    number >= 1
        && number < *pbcap::MESSAGE_FIELDS.start()
        && number != ENCODED_LEN
        && number != TRIMMED
}

/// The last component of a full type name: `irsync.v1.Hello` is `Hello`.
fn short_name(full_name: &str) -> &str {
    full_name.rsplit('.').next().unwrap_or(full_name)
}

/// A header value as the walker prints it; bytes as lowercase hex, a
/// string as its text, a nested message as its type's short name.
fn print_header_value(field: Option<FieldDescriptor>, value: &Value) -> String {
    match value {
        Value::Scalar(text) => text.clone(),
        Value::Bytes(b) => {
            let is_string = field.is_some_and(|f| matches!(f.kind(), prost_reflect::Kind::String));
            if is_string {
                String::from_utf8_lossy(b).into_owned()
            } else {
                hex(b)
            }
        }
        Value::Message { type_name, .. } => short_name(type_name).to_owned(),
        Value::Packed(elements) => elements.join(","),
        Value::Unknown { raw, .. } | Value::Undecodable(raw) => hex(raw),
    }
}

/// The printed form of a field's default value.
fn print_default(field: &FieldDescriptor) -> String {
    use prost_reflect::Value as V;
    match field.default_value() {
        V::Bool(b) => b.to_string(),
        V::I32(n) => n.to_string(),
        V::I64(n) => n.to_string(),
        V::U32(n) => n.to_string(),
        V::U64(n) => n.to_string(),
        V::F32(n) => n.to_string(),
        V::F64(n) => n.to_string(),
        V::String(s) => s,
        V::Bytes(b) => hex(&b),
        V::EnumNumber(n) => match field.kind().as_enum().and_then(|e| e.get_value(n)) {
            Some(value) => format!("{}({n})", value.name()),
            None => n.to_string(),
        },
        V::Message(_) | V::List(_) | V::Map(_) => String::new(),
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Schema;
    use irsync_proto::capture::v1::{record::Msg, ContentSummary, Record, StreamEvent, Trimmed};
    use irsync_proto::v1;
    use prost::Message;

    fn wrapper() -> MessageDescriptor {
        Schema::builtin()
            .message("irsync.capture.v1.Record")
            .unwrap()
    }

    fn hello() -> v1::Hello {
        v1::Hello {
            protocol_version: 5,
            path: b"export".to_vec(),
            ..Default::default()
        }
    }

    /// A Hello record whose encoded_len and trimmed are as given.
    fn hello_record(encoded_len: u32, trimmed: Option<Trimmed>) -> Vec<u8> {
        Record {
            side: 1,
            direction: 1,
            encoded_len,
            trimmed,
            msg: Some(Msg::Hello(hello())),
            ..Default::default()
        }
        .encode_to_vec()
    }

    fn hello_len() -> u64 {
        hello().encoded_len() as u64
    }

    #[test]
    fn plain_message() {
        let n = hello_len();
        let body = hello_record(n as u32, None);
        let e = unwrap(&wrapper(), &body, Prefix::Varint);
        assert_eq!((e.kind, e.type_name.as_str()), (Kind::Message, "Hello"));
        assert_eq!((e.present, e.wire_len, e.prefix), (n, n, 1));
        assert_eq!(e.direction, pbcap::v1::Direction::Sent);
        assert_eq!(e.fields[0].name.as_deref(), Some("protocol_version"));
        assert!(e.adds_up() && e.mismatch.is_none());
        assert!(e.header.contains(&("side".into(), "SIDE_CLIENT(1)".into())));
        assert!(!e
            .header
            .iter()
            .any(|(k, _)| k == "encoded_len" || k == "trimmed"));
    }

    #[test]
    fn cut_content() {
        let body = Record {
            direction: 1,
            encoded_len: 262_163,
            trimmed: Some(Trimmed {
                original_len: 262_163,
                dropped_entries: 0,
                content: vec![ContentSummary {
                    len: 262_144,
                    blake3_prefix: vec![1; 8],
                }],
            }),
            msg: Some(Msg::DataChunk(v1::DataChunk {
                data: Vec::new(),
                ops: Vec::new(),
            })),
            ..Default::default()
        }
        .encode_to_vec();
        let e = unwrap(&wrapper(), &body, Prefix::Varint);
        assert_eq!(e.type_name, "DataChunk");
        assert_eq!((e.cut_content, e.cut_pieces), (262_144, 1));
        assert_eq!(e.cut_other, 262_163 - 262_144 - e.present);
        assert_eq!(e.prefix, 3);
        assert!(e.adds_up() && e.mismatch.is_none());
    }

    #[test]
    fn dropped_entries() {
        let n = hello_len();
        let body = hello_record(
            n as u32 + 30,
            Some(Trimmed {
                original_len: n as u32 + 30,
                dropped_entries: 7,
                content: Vec::new(),
            }),
        );
        let e = unwrap(&wrapper(), &body, Prefix::Varint);
        assert_eq!((e.cut_other, e.dropped_entries, e.cut_content), (30, 7, 0));
        assert_eq!(e.unexplained, 0);
        assert!(e.adds_up() && e.mismatch.is_none());
    }

    #[test]
    fn message_not_captured() {
        let body = Record {
            direction: 2,
            encoded_len: 500,
            ..Default::default()
        }
        .encode_to_vec();
        let e = unwrap(&wrapper(), &body, Prefix::Varint);
        assert_eq!((e.kind, e.type_name.as_str()), (Kind::Message, ""));
        assert_eq!((e.present, e.not_captured, e.wire_len), (0, 500, 500));
        assert_eq!(e.prefix, 2);
        assert!(e.fields.is_empty());
        assert!(e.adds_up() && e.mismatch.is_none());
    }

    #[test]
    fn unexplained_gap() {
        let n = hello_len();
        let body = hello_record(n as u32 + 9, None);
        let e = unwrap(&wrapper(), &body, Prefix::Varint);
        assert_eq!(e.unexplained, 9);
        assert_eq!((e.cut_content, e.cut_other), (0, 0));
        assert!(e.adds_up() && e.mismatch.is_none());
    }

    #[test]
    fn event_counts_nothing() {
        let body = Record {
            direction: 1,
            msg: Some(Msg::StreamEvent(StreamEvent {
                kind: 1,
                detail: String::new(),
            })),
            ..Default::default()
        }
        .encode_to_vec();
        let e = unwrap(&wrapper(), &body, Prefix::Varint);
        assert_eq!((e.kind, e.type_name.as_str()), (Kind::Event, "StreamEvent"));
        assert_eq!((e.present, e.wire_len, e.prefix), (0, 0, 0));
        assert_eq!(e.fields[0].name.as_deref(), Some("kind"));
        assert!(e.adds_up() && e.mismatch.is_none());
    }

    #[test]
    fn record_with_nothing_is_an_empty_event() {
        let body = Record {
            side: 2,
            ..Default::default()
        }
        .encode_to_vec();
        let e = unwrap(&wrapper(), &body, Prefix::Varint);
        assert_eq!((e.kind, e.type_name.as_str()), (Kind::Event, ""));
        assert!(e.fields.is_empty());
        assert_eq!((e.wire_len, e.prefix), (0, 0));
        assert!(e.adds_up() && e.mismatch.is_none());
    }

    #[test]
    fn encoded_len_too_small_is_a_mismatch() {
        let n = hello_len();
        let body = hello_record(n as u32 - 1, None);
        let e = unwrap(&wrapper(), &body, Prefix::Varint);
        assert!(e.mismatch.is_some());
        assert_eq!((e.unexplained, e.cut_other, e.cut_content), (0, 0, 0));
        assert!(!e.adds_up());
    }

    #[test]
    fn cut_larger_than_gap_is_a_mismatch() {
        let n = hello_len();
        let body = hello_record(
            n as u32 + 5,
            Some(Trimmed {
                original_len: n as u32 + 5,
                dropped_entries: 0,
                content: vec![ContentSummary {
                    len: 10,
                    blake3_prefix: Vec::new(),
                }],
            }),
        );
        let e = unwrap(&wrapper(), &body, Prefix::Varint);
        assert!(e.mismatch.is_some());
        assert_eq!((e.cut_content, e.cut_pieces, e.cut_other), (0, 0, 0));
        assert!(!e.adds_up());
    }

    #[test]
    fn two_payloads_is_a_mismatch() {
        let n = hello_len();
        let mut body = hello_record(n as u32, None);
        // data_chunk (field 28), empty.
        body.extend_from_slice(&[0xe2, 0x01, 0x00]);
        let e = unwrap(&wrapper(), &body, Prefix::Varint);
        assert_eq!(e.type_name, "Hello");
        assert!(e.mismatch.is_some());
    }

    #[test]
    fn garbage_body_is_a_mismatch() {
        let body = [0xff; 8];
        let e = unwrap(&wrapper(), &body, Prefix::Varint);
        assert!(e.mismatch.is_some());
        assert_eq!(
            (e.cut_content, e.cut_other, e.unexplained, e.not_captured),
            (0, 0, 0, 0)
        );
    }

    #[test]
    fn garbage_tail_is_a_mismatch() {
        let n = hello_len();
        let mut body = hello_record(n as u32, None);
        body.extend_from_slice(&[0xff; 3]);
        let e = unwrap(&wrapper(), &body, Prefix::Varint);
        assert!(e.mismatch.is_some());
    }

    #[test]
    fn prefix_none_counts_no_prefix() {
        let n = hello_len();
        let body = hello_record(n as u32, None);
        let e = unwrap(&wrapper(), &body, Prefix::None);
        assert_eq!((e.prefix, e.wire_len), (0, n));
        assert!(e.adds_up());
    }

    #[test]
    fn header_prints_bytes_as_hex() {
        let body = Record {
            session_id: vec![0xab, 0x01],
            file_id: Some(0),
            encoded_len: 0,
            ..Default::default()
        }
        .encode_to_vec();
        let e = unwrap(&wrapper(), &body, Prefix::Varint);
        assert!(e.header.contains(&("session_id".into(), "ab01".into())));
        assert!(e.header.contains(&("file_id".into(), "0".into())));
    }

    #[test]
    fn header_value_gives_the_default_when_absent() {
        let w = wrapper();
        let body = Record {
            side: 2,
            ..Default::default()
        }
        .encode_to_vec();
        let e = unwrap(&w, &body, Prefix::Varint);
        assert_eq!(
            e.header_value(&w, "side").as_deref(),
            Some("SIDE_DAEMON(2)")
        );
        assert_eq!(
            e.header_value(&w, "direction").as_deref(),
            Some("DIRECTION_UNSPECIFIED(0)")
        );
        assert_eq!(e.header_value(&w, "file_id").as_deref(), Some("0"));
        assert_eq!(e.header_value(&w, "session_id").as_deref(), Some(""));
        assert_eq!(e.header_value(&w, "no_such_field"), None);
    }

    #[test]
    fn raw_lists_the_wrapper_itself() {
        let n = hello_len();
        let body = hello_record(n as u32, None);
        let e = raw(&wrapper(), &body);
        assert_eq!((e.kind, e.type_name.as_str()), (Kind::Raw, "Record"));
        assert_eq!(e.fields[0].name.as_deref(), Some("side"));
        assert_eq!(
            (e.present, e.wire_len),
            (body.len() as u64, body.len() as u64)
        );
        assert!(e.header.is_empty());
        assert_eq!(e.direction, pbcap::v1::Direction::Unspecified);
        assert!(e.adds_up());
    }
}
