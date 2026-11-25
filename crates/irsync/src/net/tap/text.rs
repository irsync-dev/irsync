//! The text trace: every protocol message as one readable line, logged under
//! the `irsync::wire` target.

use irsync_proto::capture::v1::{record, Direction, Record, Side, StreamKind};
use prost::Message as _;
use prost_reflect::{
    DescriptorPool, DynamicMessage, Kind, MessageDescriptor, ReflectMessage, Value,
};
use std::fmt::Write as _;
use std::sync::OnceLock;

fn pool() -> &'static DescriptorPool {
    static POOL: OnceLock<DescriptorPool> = OnceLock::new();
    POOL.get_or_init(|| {
        DescriptorPool::decode(irsync_proto::FILE_DESCRIPTOR_SET)
            .expect("the built-in descriptor set is valid")
    })
}

pub(super) fn descriptor(full_name: &str) -> MessageDescriptor {
    pool()
        .get_message_by_name(full_name)
        .unwrap_or_else(|| panic!("{full_name} is in the descriptor set"))
}

/// Renders a message as `Name { field: value, … }` on one line.
///
/// Fields at their default are left out; file content (`DataChunk.data`,
/// `DeltaOp.literal`) prints as its length and a short hash, never as bytes.
pub(crate) fn render(msg: &DynamicMessage) -> String {
    let mut out = String::new();
    message(&mut out, msg);
    out
}

fn message(out: &mut String, msg: &DynamicMessage) {
    let desc = msg.descriptor();
    out.push_str(desc.name());
    let mut any = false;
    for (field, value) in msg.fields() {
        out.push_str(if any { ", " } else { " { " });
        any = true;
        out.push_str(field.name());
        out.push_str(": ");
        let ctx = (desc.full_name(), field.name());
        if field.is_map() {
            if let Value::Map(m) = value {
                out.push('{');
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    let _ = write!(out, "{k:?}: ");
                    scalar(out, v, &field.kind(), ctx);
                }
                out.push('}');
            }
        } else {
            scalar(out, value, &field.kind(), ctx);
        }
    }
    out.push_str(if any { " }" } else { " {}" });
}

fn scalar(out: &mut String, value: &Value, kind: &Kind, ctx: (&str, &str)) {
    match value {
        Value::Bool(v) => {
            let _ = write!(out, "{v}");
        }
        Value::I32(v) => {
            let _ = write!(out, "{v}");
        }
        Value::I64(v) => {
            let _ = write!(out, "{v}");
        }
        Value::U32(v) => {
            let _ = write!(out, "{v}");
        }
        Value::U64(v) => {
            let _ = write!(out, "{v}");
        }
        Value::F32(v) => {
            let _ = write!(out, "{v}");
        }
        Value::F64(v) => {
            let _ = write!(out, "{v}");
        }
        Value::String(s) => {
            let _ = write!(out, "{s:?}");
        }
        Value::Bytes(b) => bytes(out, b, ctx),
        Value::EnumNumber(n) => match kind.as_enum().and_then(|e| e.get_value(*n)) {
            Some(v) => out.push_str(v.name()),
            None => {
                let _ = write!(out, "{n}");
            }
        },
        Value::Message(m) => message(out, m),
        Value::List(items) => {
            out.push('[');
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                scalar(out, v, kind, ctx);
            }
            out.push(']');
        }
        Value::Map(_) => out.push_str("{…}"),
    }
}

fn hex(out: &mut String, b: &[u8]) {
    for x in b {
        let _ = write!(out, "{x:02x}");
    }
}

fn bytes(out: &mut String, b: &[u8], ctx: (&str, &str)) {
    match ctx {
        ("irsync.v1.DataChunk", "data") | ("irsync.v1.DeltaOp", "literal") => {
            let h = blake3::hash(b);
            let _ = write!(out, "<{} bytes, blake3 ", b.len());
            hex(out, &h.as_bytes()[..8]);
            out.push('>');
        }
        ("irsync.v1.Hash", "bytes") => hex(out, b),
        _ => match std::str::from_utf8(b) {
            Ok(s) if !s.chars().any(char::is_control) => {
                let _ = write!(out, "{s:?}");
            }
            _ => {
                out.push_str("0x");
                hex(out, b);
            }
        },
    }
}

/// The record's inner message as a dynamic message, by its full name.
fn inner(msg: &record::Msg) -> DynamicMessage {
    use record::Msg as M;
    let (name, bytes) = match msg {
        M::Hello(m) => ("irsync.v1.Hello", m.encode_to_vec()),
        M::HelloReply(m) => ("irsync.v1.HelloReply", m.encode_to_vec()),
        M::Control(m) => ("irsync.v1.Control", m.encode_to_vec()),
        M::ListingStreamHeader(m) => ("irsync.v1.ListingStreamHeader", m.encode_to_vec()),
        M::DataStreamHeader(m) => ("irsync.v1.DataStreamHeader", m.encode_to_vec()),
        M::DataChunk(m) => ("irsync.v1.DataChunk", m.encode_to_vec()),
        M::AdminRequest(m) => ("irsync.v1.AdminRequest", m.encode_to_vec()),
        M::AdminReply(m) => ("irsync.v1.AdminReply", m.encode_to_vec()),
        M::StreamEvent(m) => ("irsync.capture.v1.StreamEvent", m.encode_to_vec()),
        M::PathChange(m) => ("irsync.capture.v1.PathChange", m.encode_to_vec()),
    };
    DynamicMessage::decode(descriptor(name), bytes.as_slice())
        .expect("a message we just encoded decodes")
}

/// Logs one record as a line under `irsync::wire` at TRACE.
pub(crate) fn emit(record: &Record) {
    let Some(msg) = &record.msg else { return };
    let rendered = render(&inner(msg));
    let side = Side::try_from(record.side).map_or("?", |s| s.as_str_name());
    let dir = Direction::try_from(record.direction).map_or("?", |s| s.as_str_name());
    let stream = StreamKind::try_from(record.stream).map_or("?", |s| s.as_str_name());
    let mut session = String::new();
    if record.session_id.is_empty() {
        session.push('-');
    } else {
        hex(&mut session, &record.session_id);
    }
    let file = record
        .file_id
        .map_or_else(|| "-".to_string(), |f| f.to_string());
    tracing::trace!(
        target: "irsync::wire",
        side,
        dir,
        stream,
        quic_stream = record.quic_stream_id,
        conn = record.connection,
        session = %session,
        file = %file,
        "{rendered}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use irsync_proto::v1::{
        control::Msg, Control, DataChunk, DeltaOp, FileEntry, ListingEntry, NeedChunks,
    };

    fn dyn_of<M: prost::Message>(m: &M) -> DynamicMessage {
        let name = {
            // Look the descriptor up by the type's unqualified name.
            let t = std::any::type_name::<M>().rsplit("::").next().unwrap();
            pool()
                .all_messages()
                .find(|d| d.name() == t && d.full_name().starts_with("irsync.v1."))
                .unwrap()
        };
        DynamicMessage::decode(name, m.encode_to_vec().as_slice()).unwrap()
    }

    #[test]
    fn renders_need_chunks() {
        let c = Control {
            msg: Some(Msg::Need(NeedChunks {
                file_id: 12,
                chunk_idx: vec![0, 1, 5],
                ..Default::default()
            })),
        };
        assert_eq!(
            render(&dyn_of(&c)),
            "Control { need: NeedChunks { file_id: 12, chunk_idx: [0, 1, 5] } }"
        );
    }

    #[test]
    fn data_bytes_never_print() {
        let d = DataChunk {
            data: vec![b'x'; 1000],
            ..Default::default()
        };
        let s = render(&dyn_of(&d));
        assert!(s.contains("<1000 bytes, blake3 "), "{s}");
        assert!(!s.contains("xxxx"), "{s}");
    }

    #[test]
    fn literal_bytes_never_print() {
        let d = DataChunk {
            ops: vec![DeltaOp {
                op: Some(irsync_proto::v1::delta_op::Op::Literal(vec![b'y'; 300])),
            }],
            ..Default::default()
        };
        let s = render(&dyn_of(&d));
        assert!(s.contains("<300 bytes, blake3 "), "{s}");
        assert!(!s.contains("yyyy"), "{s}");
    }

    #[test]
    fn non_utf8_path_prints_as_hex() {
        let e = ListingEntry {
            entry: Some(FileEntry {
                path: vec![0x66, 0xff],
                ..Default::default()
            }),
        };
        let c = Control {
            msg: Some(Msg::Listing(e)),
        };
        assert!(render(&dyn_of(&c)).contains("path: 0x66ff"));
    }

    #[test]
    fn stream_events_render_like_any_message() {
        let ev = record::Msg::StreamEvent(irsync_proto::capture::v1::StreamEvent {
            kind: irsync_proto::capture::v1::stream_event::Kind::Opened as i32,
            detail: String::new(),
        });
        assert_eq!(render(&inner(&ev)), "StreamEvent { kind: OPENED }");
    }

    #[test]
    fn every_record_kind_renders() {
        // One default of each kind, through the same lookup `emit` uses, so a
        // renamed proto message fails here rather than in a live trace. The
        // match has no `_` arm: a new kind does not compile until it is added.
        use record::Msg as M;
        let all = [
            M::Hello(Default::default()),
            M::HelloReply(Default::default()),
            M::Control(Default::default()),
            M::ListingStreamHeader(Default::default()),
            M::DataStreamHeader(Default::default()),
            M::DataChunk(Default::default()),
            M::AdminRequest(Default::default()),
            M::AdminReply(Default::default()),
            M::StreamEvent(Default::default()),
            M::PathChange(Default::default()),
        ];
        for msg in all {
            let name = match &msg {
                M::Hello(_) => "Hello",
                M::HelloReply(_) => "HelloReply",
                M::Control(_) => "Control",
                M::ListingStreamHeader(_) => "ListingStreamHeader",
                M::DataStreamHeader(_) => "DataStreamHeader",
                M::DataChunk(_) => "DataChunk",
                M::AdminRequest(_) => "AdminRequest",
                M::AdminReply(_) => "AdminReply",
                M::StreamEvent(_) => "StreamEvent",
                M::PathChange(_) => "PathChange",
            };
            assert_eq!(render(&inner(&msg)), format!("{name} {{}}"));
            emit(&Record {
                msg: Some(msg),
                ..Default::default()
            });
        }
    }

    #[test]
    fn utf8_path_prints_quoted_and_empty_message_is_braces() {
        let e = FileEntry {
            path: b"a/b".to_vec(),
            ..Default::default()
        };
        assert_eq!(render(&dyn_of(&e)), "FileEntry { path: \"a/b\" }");
        assert_eq!(render(&dyn_of(&DataChunk::default())), "DataChunk {}");
    }
}
