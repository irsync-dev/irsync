//! Makes a record safe and small enough for the capture file.
//!
//! [`scrub`] takes file content out; [`fit`] drops repeated entries until the
//! record fits a Wireshark packet.

use irsync_proto::capture::v1::record::Msg;
use irsync_proto::capture::v1::{ContentSummary, Record};
use irsync_proto::v1::delta_op::Op;
use prost::encoding::encoded_len_varint;
use prost::Message as _;
use prost_reflect::{DynamicMessage, FieldDescriptor, Kind, ReflectMessage as _, Value};

use super::text::descriptor;

fn summary(bytes: &[u8]) -> ContentSummary {
    ContentSummary {
        len: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
        blake3_prefix: blake3::hash(bytes).as_bytes()[..8].to_vec(),
    }
}

/// Replaces `DataChunk.data` and every `DeltaOp.literal` with empty bytes,
/// adding a [`ContentSummary`] for each, in message order, to
/// `record.trimmed`. Empty `data` gets no summary; every literal does, so
/// the summaries line up with the literal ops.
pub(crate) fn scrub(record: &mut Record) {
    let Some(Msg::DataChunk(chunk)) = &mut record.msg else {
        return;
    };
    let mut content = Vec::new();
    if !chunk.data.is_empty() {
        content.push(summary(&std::mem::take(&mut chunk.data)));
    }
    for op in &mut chunk.ops {
        if let Some(Op::Literal(l)) = &mut op.op {
            content.push(summary(&std::mem::take(l)));
        }
    }
    if !content.is_empty() {
        record
            .trimmed
            .get_or_insert_default()
            .content
            .extend(content);
    }
}

/// While the encoded record is over `max` bytes, drops entries from the end
/// of the largest repeated field anywhere in its message (for `ChunkBasis`,
/// `weak` with the matching 8-byte slices of `strong`), counting them in
/// `trimmed.dropped_entries` and setting `trimmed.original_len`.
///
/// A record still too big with no repeated entries left loses its message
/// altogether; `trimmed.original_len` still says how big it was.
pub(crate) fn fit(record: &mut Record, max: usize) {
    let original = record.encoded_len();
    if original <= max {
        return;
    }
    let mut dropped = 0u64;
    while record.encoded_len() > max {
        let excess = record.encoded_len() - max;
        let mut dynamic = DynamicMessage::decode(
            descriptor("irsync.capture.v1.Record"),
            record.encode_to_vec().as_slice(),
        )
        .expect("a record we just encoded decodes");
        let n = drop_from_largest(&mut dynamic, excess);
        let mut last_resort = false;
        if n > 0 {
            dropped += n;
            *record = dynamic
                .transcode_to()
                .expect("a dynamic record converts back");
        } else if record.msg.is_some() {
            record.msg = None;
        } else {
            // Only the record's context and summaries are left.
            last_resort = true;
            record.trimmed.get_or_insert_default().content.clear();
        }
        let t = record.trimmed.get_or_insert_default();
        t.original_len = u32::try_from(original).unwrap_or(u32::MAX);
        t.dropped_entries = u32::try_from(dropped).unwrap_or(u32::MAX);
        if last_resort {
            break;
        }
    }
}

fn is_chunk_basis_weak(parent: &DynamicMessage, field: &FieldDescriptor) -> bool {
    parent.descriptor().full_name() == "irsync.v1.ChunkBasis" && field.name() == "weak"
}

/// About how many bytes one entry of a repeated field takes when encoded.
fn entry_len(parent: &DynamicMessage, field: &FieldDescriptor, v: &Value) -> usize {
    let framed = |n: usize| 1 + encoded_len_varint(n as u64) + n;
    let len = match v {
        Value::Message(m) => framed(m.encoded_len()),
        Value::Bytes(b) => framed(b.len()),
        Value::String(s) => framed(s.len()),
        _ => match field.kind() {
            Kind::Fixed32 | Kind::Sfixed32 | Kind::Float => 4,
            Kind::Fixed64 | Kind::Sfixed64 | Kind::Double => 8,
            _ => match v {
                Value::U32(x) => encoded_len_varint(u64::from(*x)),
                Value::U64(x) => encoded_len_varint(*x),
                Value::I32(x) => encoded_len_varint(*x as i64 as u64),
                Value::I64(x) => encoded_len_varint(*x as u64),
                Value::EnumNumber(x) => encoded_len_varint(*x as i64 as u64),
                _ => 1,
            },
        },
    };
    if is_chunk_basis_weak(parent, field) {
        len + 8
    } else {
        len
    }
}

/// The largest non-empty repeated field under `msg`, as `(size, path)`: the
/// path's last field is the repeated one, the ones before it lead to the
/// message holding it. Skips the record's own `trimmed`.
fn largest(
    msg: &DynamicMessage,
    path: &mut Vec<FieldDescriptor>,
    best: &mut Option<(usize, Vec<FieldDescriptor>)>,
) {
    for (field, value) in msg.fields() {
        if path.is_empty() && field.name() == "trimmed" {
            continue;
        }
        match value {
            Value::List(items) if !items.is_empty() => {
                let size: usize = items.iter().map(|v| entry_len(msg, &field, v)).sum();
                if best.as_ref().is_none_or(|(b, _)| size > *b) {
                    let mut p = path.clone();
                    p.push(field);
                    *best = Some((size, p));
                }
            }
            Value::Message(inner) => {
                path.push(field);
                largest(inner, path, best);
                path.pop();
            }
            _ => {}
        }
    }
}

/// Drops entries from the end of the largest repeated field until about
/// `excess` bytes are gone; returns how many it dropped.
fn drop_from_largest(record: &mut DynamicMessage, excess: usize) -> u64 {
    let mut best = None;
    largest(record, &mut Vec::new(), &mut best);
    let Some((_, path)) = best else { return 0 };
    let (field, to_parent) = path.split_last().expect("a path names a field");
    let mut parent = record;
    for f in to_parent {
        parent = match parent.get_field_mut(f) {
            Value::Message(m) => m,
            _ => unreachable!("the path leads through messages"),
        };
    }
    let Value::List(mut items) = parent.get_field(field).into_owned() else {
        unreachable!("the path ends at a repeated field")
    };
    let (mut removed, mut count) = (0, 0u64);
    while removed < excess {
        let Some(v) = items.pop() else { break };
        removed += entry_len(parent, field, &v);
        count += 1;
    }
    let kept = items.len();
    parent.set_field(field, Value::List(items));
    if is_chunk_basis_weak(parent, field) {
        if let Value::Bytes(strong) = parent
            .get_field_by_name_mut("strong")
            .expect("ChunkBasis has strong")
        {
            strong.truncate(8 * kept);
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use irsync_proto::capture::v1::record::Msg;
    use irsync_proto::v1::{
        control, delta_op, ChunkBasis, ChunkListPart, ChunkRecord, Control, CopyOld, DataChunk,
        DeltaOp, Hash, Heartbeat,
    };

    fn record_with(msg: Msg) -> Record {
        Record {
            connection: 1,
            msg: Some(msg),
            ..Default::default()
        }
    }

    fn control(m: control::Msg) -> Msg {
        Msg::Control(Control { msg: Some(m) })
    }

    fn data_of(r: &Record) -> &[u8] {
        match &r.msg {
            Some(Msg::DataChunk(c)) => &c.data,
            _ => panic!("not a DataChunk"),
        }
    }

    fn part_with_records(n: usize) -> control::Msg {
        control::Msg::ChunkListPart(ChunkListPart {
            file_id: 7,
            total: n as u32,
            chunks: (0..n)
                .map(|i| ChunkRecord {
                    offset: i as u64 * 65_536,
                    len: 65_536,
                    hash: Some(Hash {
                        bytes: vec![i as u8; 32],
                    }),
                })
                .collect(),
            ..Default::default()
        })
    }

    #[test]
    fn data_chunk_keeps_no_content() {
        let mut r = record_with(Msg::DataChunk(DataChunk {
            data: vec![1; 4096],
            ..Default::default()
        }));
        scrub(&mut r);
        assert!(data_of(&r).is_empty());
        let t = r.trimmed.unwrap();
        assert_eq!(t.content[0].len, 4096);
        assert_eq!(
            t.content[0].blake3_prefix,
            blake3::hash(&[1; 4096]).as_bytes()[..8]
        );
    }

    #[test]
    fn literals_go_and_copies_stay() {
        let lit = |b: &[u8]| DeltaOp {
            op: Some(delta_op::Op::Literal(b.to_vec())),
        };
        let copy = DeltaOp {
            op: Some(delta_op::Op::Copy(CopyOld { offset: 3, len: 9 })),
        };
        let mut r = record_with(Msg::DataChunk(DataChunk {
            data: vec![],
            ops: vec![lit(b"abc"), copy.clone(), lit(b"hello")],
        }));
        scrub(&mut r);
        let Some(Msg::DataChunk(c)) = &r.msg else {
            panic!()
        };
        assert_eq!(c.ops[1], copy);
        for i in [0, 2] {
            assert_eq!(c.ops[i].op, Some(delta_op::Op::Literal(vec![])));
        }
        let lens: Vec<u32> = r.trimmed.unwrap().content.iter().map(|s| s.len).collect();
        assert_eq!(lens, [3, 5]);
    }

    #[test]
    fn big_chunk_list_fits_and_says_so() {
        let mut r = record_with(control(part_with_records(32_768)));
        let before = r.encoded_len();
        fit(&mut r, 262_144);
        assert!(r.encoded_len() <= 262_144);
        let t = r.trimmed.clone().unwrap();
        assert_eq!(t.original_len as usize, before);
        assert!(t.dropped_entries > 0);
        let Some(Msg::Control(Control {
            msg: Some(control::Msg::ChunkListPart(p)),
        })) = &r.msg
        else {
            panic!()
        };
        // The first entries stay, and the record is not cut much shorter
        // than it has to be.
        assert_eq!(p.chunks.len() as u32 + t.dropped_entries, 32_768);
        assert_eq!(p.chunks[0].offset, 0);
        assert!(r.encoded_len() > 262_144 - 1024);
    }

    #[test]
    fn chunk_basis_drops_weak_and_strong_together() {
        let n = 40_000;
        let mut r = record_with(control(control::Msg::ChunkBasis(ChunkBasis {
            file_id: 1,
            weak: (0..n).collect(),
            strong: vec![9; 8 * n as usize],
            ..Default::default()
        })));
        fit(&mut r, 100_000);
        assert!(r.encoded_len() <= 100_000);
        let Some(Msg::Control(Control {
            msg: Some(control::Msg::ChunkBasis(b)),
        })) = &r.msg
        else {
            panic!()
        };
        assert_eq!(b.strong.len(), 8 * b.weak.len());
        assert_eq!(b.weak.len() as u32 + r.trimmed.unwrap().dropped_entries, n);
    }

    #[test]
    fn huge_message_without_lists_keeps_only_its_context() {
        let mut r = record_with(Msg::Hello(irsync_proto::v1::Hello {
            traceparent: "x".repeat(300_000),
            ..Default::default()
        }));
        fit(&mut r, 262_144);
        assert!(r.msg.is_none());
        assert_eq!(r.connection, 1);
        assert!(r.trimmed.unwrap().original_len > 300_000);
    }

    #[test]
    fn small_message_is_untouched() {
        let mut r = record_with(control(control::Msg::Heartbeat(Heartbeat {
            skipped_bytes: 5,
            ..Default::default()
        })));
        let before = r.clone();
        fit(&mut r, 262_144);
        assert_eq!(r, before);
        assert!(r.trimmed.is_none());
    }
}
