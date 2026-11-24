//! Walks encoded protobuf bytes and reports every field with its exact size.
//!
//! The decoding libraries do not say where a field sits or how many bytes
//! it takes, so this reads the wire format itself. It never panics: bytes
//! that do not parse become an [`Value::Undecodable`] field, so the sizes
//! of a message's fields always add up to the message's length.

use prost_reflect::{FieldDescriptor, Kind, MessageDescriptor};

/// The bytes one field takes on the wire.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Sizes {
    /// The field key (number and wire type).
    pub tag: u64,
    /// The length prefix of a length-delimited field, else 0.
    pub len: u64,
    /// The rest: the value itself.
    pub value: u64,
}

impl Sizes {
    /// All the bytes of the field.
    pub fn total(&self) -> u64 {
        self.tag + self.len + self.value
    }
}

/// One field as it appears in the bytes.
#[derive(Clone, PartialEq, Debug)]
pub struct Field<'a> {
    /// The field number; 0 for [`Value::Undecodable`].
    pub number: u32,
    /// The field's name; `None` when the type has no such field.
    pub name: Option<String>,
    pub sizes: Sizes,
    pub value: Value<'a>,
    /// The bytes of this field's own line: its tag and length for a
    /// [`Value::Message`], whose fields carry the rest; else the whole
    /// field.
    pub own: &'a [u8],
}

/// Every field's own bytes, depth first: the input of the walk.
pub fn own_bytes(fields: &[Field]) -> Vec<u8> {
    fn collect(fields: &[Field], out: &mut Vec<u8>) {
        for f in fields {
            out.extend_from_slice(f.own);
            if let Value::Message { fields, .. } = &f.value {
                collect(fields, out);
            }
        }
    }
    let mut out = Vec::new();
    collect(fields, &mut out);
    out
}

/// What a field holds.
#[derive(Clone, PartialEq, Debug)]
pub enum Value<'a> {
    /// A number, bool or enum, already printed: "5", "true", "SIDE_DAEMON(2)".
    Scalar(String),
    /// A `bytes` or `string` field, or a message nested deeper than [`MAX_DEPTH`].
    Bytes(&'a [u8]),
    /// A nested message and its fields.
    Message {
        type_name: String,
        fields: Vec<Field<'a>>,
    },
    /// A packed repeated field: every element, printed.
    Packed(Vec<String>),
    /// A field number the type does not have, or a field whose wire type
    /// contradicts its declared type; the raw value bytes.
    Unknown { wire_type: u8, raw: &'a [u8] },
    /// Bytes from here to the end of the enclosing message do not parse.
    Undecodable(&'a [u8]),
}

/// Nested messages deeper than this are reported as [`Value::Bytes`].
pub const MAX_DEPTH: usize = 64;

const VARINT: u8 = 0;
const FIXED64: u8 = 1;
const LEN: u8 = 2;
const FIXED32: u8 = 5;

/// Every field of `bytes`, in the order it appears. Never panics.
pub fn walk<'a>(desc: &MessageDescriptor, bytes: &'a [u8]) -> Vec<Field<'a>> {
    walk_at(desc, bytes, 0)
}

fn walk_at<'a>(desc: &MessageDescriptor, bytes: &'a [u8], depth: usize) -> Vec<Field<'a>> {
    let mut fields = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        match field_at(desc, rest, depth) {
            Some((field, used)) => {
                fields.push(field);
                rest = &rest[used..];
            }
            None => {
                fields.push(Field {
                    number: 0,
                    name: None,
                    sizes: Sizes {
                        tag: 0,
                        len: 0,
                        value: rest.len() as u64,
                    },
                    value: Value::Undecodable(rest),
                    own: rest,
                });
                break;
            }
        }
    }
    fields
}

/// The field at the start of `rest` and the bytes it takes, or `None` when
/// the bytes there do not parse as a field.
fn field_at<'a>(
    desc: &MessageDescriptor,
    rest: &'a [u8],
    depth: usize,
) -> Option<(Field<'a>, usize)> {
    let (key, tag) = read_varint(rest)?;
    let key = u32::try_from(key).ok()?;
    let number = key >> 3;
    if number == 0 {
        return None;
    }
    let wire_type = (key & 7) as u8;
    let after = rest.get(tag..)?;
    let (len, raw) = match wire_type {
        VARINT => {
            let (_, n) = read_varint(after)?;
            (0, after.get(..n)?)
        }
        FIXED64 => (0, after.get(..8)?),
        LEN => {
            let (value_len, n) = read_varint(after)?;
            let end = n.checked_add(usize::try_from(value_len).ok()?)?;
            (n, after.get(n..end)?)
        }
        FIXED32 => (0, after.get(..4)?),
        // Groups (3, 4) and the undefined wire types 6 and 7.
        _ => return None,
    };
    let field = desc.get_field(number);
    let value = field
        .as_ref()
        .and_then(|f| known_value(f, wire_type, raw, depth))
        .unwrap_or(Value::Unknown { wire_type, raw });
    let sizes = Sizes {
        tag: tag as u64,
        len: len as u64,
        value: raw.len() as u64,
    };
    let used = tag + len + raw.len();
    // A nested message's fields carry its value themselves.
    let own = match value {
        Value::Message { .. } => &rest[..tag + len],
        _ => &rest[..used],
    };
    let field = Field {
        number,
        name: field.map(|f| f.name().to_owned()),
        sizes,
        value,
        own,
    };
    Some((field, used))
}

/// The value of a field the type declares, or `None` when the bytes do not
/// fit its declared type.
fn known_value<'a>(
    field: &FieldDescriptor,
    wire_type: u8,
    raw: &'a [u8],
    depth: usize,
) -> Option<Value<'a>> {
    let kind = field.kind();
    match (&kind, wire_type) {
        (Kind::Message(_), _) if field.is_group() => None,
        (Kind::Message(m), LEN) => {
            if depth >= MAX_DEPTH {
                Some(Value::Bytes(raw))
            } else {
                Some(Value::Message {
                    type_name: m.full_name().to_owned(),
                    fields: walk_at(m, raw, depth + 1),
                })
            }
        }
        (Kind::String | Kind::Bytes, LEN) => Some(Value::Bytes(raw)),
        (Kind::Message(_) | Kind::String | Kind::Bytes, _) => None,
        (_, LEN) if field.is_list() => packed(&kind, raw).map(Value::Packed),
        _ if scalar_wire_type(&kind) == wire_type => {
            let (text, used) = read_scalar(&kind, raw)?;
            (used == raw.len()).then_some(Value::Scalar(text))
        }
        _ => None,
    }
}

/// Every element of a packed field, or `None` unless they fill `raw` exactly.
fn packed(kind: &Kind, raw: &[u8]) -> Option<Vec<String>> {
    let mut elements = Vec::new();
    let mut rest = raw;
    while !rest.is_empty() {
        let (text, used) = read_scalar(kind, rest)?;
        elements.push(text);
        rest = &rest[used..];
    }
    Some(elements)
}

/// The wire type a scalar of this kind is written with.
fn scalar_wire_type(kind: &Kind) -> u8 {
    match kind {
        Kind::Double | Kind::Fixed64 | Kind::Sfixed64 => FIXED64,
        Kind::Float | Kind::Fixed32 | Kind::Sfixed32 => FIXED32,
        Kind::String | Kind::Bytes | Kind::Message(_) => LEN,
        _ => VARINT,
    }
}

/// One scalar of `kind` from the start of `bytes`, printed, and the bytes
/// it takes.
fn read_scalar(kind: &Kind, bytes: &[u8]) -> Option<(String, usize)> {
    match scalar_wire_type(kind) {
        FIXED64 => {
            let b: [u8; 8] = bytes.get(..8)?.try_into().ok()?;
            let text = match kind {
                Kind::Double => f64::from_le_bytes(b).to_string(),
                Kind::Sfixed64 => i64::from_le_bytes(b).to_string(),
                _ => u64::from_le_bytes(b).to_string(),
            };
            Some((text, 8))
        }
        FIXED32 => {
            let b: [u8; 4] = bytes.get(..4)?.try_into().ok()?;
            let text = match kind {
                Kind::Float => f32::from_le_bytes(b).to_string(),
                Kind::Sfixed32 => i32::from_le_bytes(b).to_string(),
                _ => u32::from_le_bytes(b).to_string(),
            };
            Some((text, 4))
        }
        VARINT => {
            let (v, used) = read_varint(bytes)?;
            let text = match kind {
                // 32-bit values are the low 32 bits of the varint, as protobuf decodes them.
                Kind::Int32 => (v as u32 as i32).to_string(),
                Kind::Uint32 => (v as u32).to_string(),
                Kind::Int64 => (v as i64).to_string(),
                Kind::Sint32 => {
                    let n = v as u32;
                    ((n >> 1) as i32 ^ -((n & 1) as i32)).to_string()
                }
                Kind::Sint64 => ((v >> 1) as i64 ^ -((v & 1) as i64)).to_string(),
                Kind::Bool => (v != 0).to_string(),
                Kind::Enum(e) => {
                    let n = v as u32 as i32;
                    match e.get_value(n) {
                        Some(value) => format!("{}({n})", value.name()),
                        None => n.to_string(),
                    }
                }
                _ => v.to_string(),
            };
            Some((text, used))
        }
        _ => None,
    }
}

/// A varint from the start of `bytes` and the bytes it takes: at most 10,
/// and a value that fits in 64 bits.
fn read_varint(bytes: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    for (i, &b) in bytes.iter().take(10).enumerate() {
        if i == 9 && b > 1 {
            return None;
        }
        value |= u64::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use irsync_proto::v1::{self, control::Msg, delta_op, hello};
    use prost::Message;
    use prost_reflect::DescriptorPool;

    fn total(fields: &[Field]) -> u64 {
        fields.iter().map(|f| f.sizes.total()).sum()
    }

    fn desc(name: &str) -> MessageDescriptor {
        DescriptorPool::decode(irsync_proto::FILE_DESCRIPTOR_SET)
            .unwrap()
            .get_message_by_name(name)
            .unwrap()
    }

    /// Every Message's value is the sum of its children, all the way down,
    /// and (for bytes prost wrote) nothing is unknown or undecodable.
    fn check_clean(fields: &[Field]) {
        for f in fields {
            match &f.value {
                Value::Message {
                    fields: children, ..
                } => {
                    assert_eq!(f.sizes.value, total(children), "field {}", f.number);
                    check_clean(children);
                }
                Value::Unknown { .. } | Value::Undecodable(_) => {
                    panic!("field {} did not decode: {:?}", f.number, f.value)
                }
                _ => assert!(f.name.is_some()),
            }
        }
    }

    fn hash(b: u8) -> Option<v1::Hash> {
        Some(v1::Hash { bytes: vec![b; 32] })
    }

    fn entry() -> v1::FileEntry {
        v1::FileEntry {
            path: b"dir/file".to_vec(),
            kind: 7,
            size: 1 << 40,
            mtime_sec: -5,
            mtime_nsec: 999,
            mode: 0o100644,
            uid: 1000,
            gid: 1000,
            link_target: b"other".to_vec(),
            whole_hash: hash(3),
            sparse: true,
            user: "mike".into(),
            group: "staff".into(),
            ..Default::default()
        }
    }

    fn stat() -> v1::SourceStat {
        v1::SourceStat {
            size: 12,
            mtime_sec: 1,
            mtime_nsec: 2,
            ctime_sec: 3,
            ctime_nsec: 4,
            inode: 5,
            mode: 6,
            uid: 7,
            gid: 8,
        }
    }

    fn path_error() -> v1::PathError {
        v1::PathError {
            path: b"x".to_vec(),
            message: "no".into(),
        }
    }

    fn totals() -> Option<v1::ListTotals> {
        Some(v1::ListTotals {
            reg: 1,
            dir: 2,
            link: 3,
            special: 4,
            total_size: 5,
        })
    }

    fn control_variants() -> Vec<Msg> {
        vec![
            Msg::Listing(v1::ListingEntry {
                entry: Some(entry()),
            }),
            Msg::ListingEnd(v1::ListingEnd {
                incomplete: true,
                count: 300,
            }),
            Msg::Plan(v1::PlanItem {
                file_id: 1,
                path: b"p".to_vec(),
                action: 2,
                append_from: 77,
            }),
            Msg::Need(v1::NeedChunks {
                file_id: 2,
                chunk_idx: vec![0, 1, 300, u32::MAX],
            }),
            Msg::Ack(v1::FileAck {
                file_id: 3,
                status: 1,
                message: "bad".into(),
                matched: 10,
            }),
            Msg::Done(v1::SessionDone {
                files: 1,
                bytes: 2,
                errors: vec![path_error(), path_error()],
                deleted: vec![b"a".to_vec(), b"b".to_vec()],
                would_delete: vec![b"c".to_vec()],
                max_delete_reached: true,
                deleted_omitted: 3,
                would_delete_omitted: 4,
                created: 5,
                transferred_size: 6,
                listed: totals(),
            }),
            Msg::Error(v1::Error {
                code: 4,
                message: "bad path".into(),
            }),
            Msg::Itemized(v1::Itemized {
                path: b"f".to_vec(),
                code: ">f+++++++++".into(),
            }),
            Msg::Heartbeat(v1::Heartbeat {
                walking: Some(Vec::new()),
                skipped_bytes: 1,
                deleted: 2,
                delete_total: 3,
                listed: totals(),
                listed_settled: 4,
                received_bytes: 5,
            }),
            Msg::ChunkListPart(v1::ChunkListPart {
                file_id: 4,
                size: 5,
                whole_hash: hash(1),
                total: 2,
                first: 0,
                chunks: vec![
                    v1::ChunkRecord {
                        offset: 0,
                        len: 3,
                        hash: hash(2),
                    },
                    v1::ChunkRecord {
                        offset: 3,
                        len: 2,
                        hash: hash(4),
                    },
                ],
                last: true,
                mtime_sec: -1,
                mtime_nsec: 1,
                mode: 2,
                uid: 3,
                gid: 4,
                user: "u".into(),
                group: "g".into(),
                ctime_sec: 5,
                ctime_nsec: 6,
                inode: 7,
            }),
            Msg::ChunkBasis(v1::ChunkBasis {
                file_id: 5,
                chunk_idx: 6,
                offset: 7,
                block: 512,
                weak: vec![1, 0xdead_beef],
                strong: vec![9; 16],
            }),
            Msg::ListingError(path_error()),
            Msg::ListRoom(v1::ListRoom { records: 64 }),
            Msg::MayPlace(v1::MayPlace { file_id: 6, gen: 1 }),
            Msg::PlaceVerdict(v1::PlaceVerdict {
                file_id: 6,
                changed: true,
                gen: 1,
            }),
            Msg::Replan(v1::Replan {
                path: b"r".to_vec(),
                entry: Some(entry()),
            }),
            Msg::Settle(v1::Settle {}),
            Msg::Settled(v1::Settled {
                vanished: vec![b"v".to_vec()],
                errors: vec![path_error()],
                vanished_omitted: 1,
            }),
            Msg::CheckLink(v1::CheckLink {
                path: b"l".to_vec(),
                want: Some(stat()),
            }),
            Msg::CheckLinksEnd(v1::CheckLinksEnd {}),
            Msg::Unlinked(v1::Unlinked {
                path: b"u".to_vec(),
                entry: Some(entry()),
            }),
            Msg::LinksChecked(v1::LinksChecked {}),
        ]
    }

    fn hello() -> v1::Hello {
        v1::Hello {
            protocol_version: 5,
            session_id: vec![7; 16],
            path: b"export/sub".to_vec(),
            direction: 1,
            chunk_min: 1,
            chunk_avg: 2,
            chunk_max: 3,
            contents_only: true,
            delete_mode: 2,
            delete_excluded: true,
            max_delete: Some(0),
            dry_run: true,
            ignore_errors: true,
            itemize: true,
            update_only: true,
            checksum: true,
            inplace: true,
            append: 2,
            build: Some(v1::BuildInfo {
                version: "1.0".into(),
                commit: "abc".into(),
                dirty: Some(false),
            }),
            name: b"n".to_vec(),
            preserve: Some(hello::Preserve {
                links: true,
                hard_links: true,
                dirs: true,
                ..Default::default()
            }),
            filters: vec![
                hello::FilterRule {
                    include: true,
                    pattern: b"*.rs".to_vec(),
                },
                hello::FilterRule {
                    include: false,
                    pattern: b"\xff".to_vec(),
                },
            ],
            partial_dir: b".partial".to_vec(),
            numeric_ids: true,
            traceparent: "00-abc".into(),
            tracestate: "k=v".into(),
            no_local_reuse: true,
            fsync: true,
            paranoid: true,
            single_file: true,
            bwlimit: Some(1 << 20),
        }
    }

    #[test]
    fn sizes_add_up_for_every_message_type() {
        let mut cases: Vec<(&str, Vec<u8>)> = vec![
            ("irsync.v1.Hello", hello().encode_to_vec()),
            (
                "irsync.v1.HelloAck",
                v1::HelloAck {
                    session_id: vec![1; 16],
                    resumed: true,
                    protocol_version: 5,
                    build: Some(v1::BuildInfo {
                        version: "2".into(),
                        commit: "unknown".into(),
                        dirty: None,
                    }),
                    single_name: b"s".to_vec(),
                }
                .encode_to_vec(),
            ),
            (
                "irsync.v1.Error",
                v1::Error {
                    code: 1,
                    message: "denied".into(),
                }
                .encode_to_vec(),
            ),
            (
                "irsync.v1.ExportList",
                v1::ExportList {
                    exports: vec![
                        v1::ExportEntry {
                            name: "a".into(),
                            path: b"/srv/a".to_vec(),
                            access: 1,
                        },
                        v1::ExportEntry {
                            name: "b".into(),
                            path: b"/srv/b".to_vec(),
                            access: 0,
                        },
                    ],
                }
                .encode_to_vec(),
            ),
            (
                "irsync.v1.PathInfo",
                v1::PathInfo { kind: Some(0) }.encode_to_vec(),
            ),
            (
                "irsync.v1.ListingStreamHeader",
                v1::ListingStreamHeader {
                    session_id: vec![2; 16],
                }
                .encode_to_vec(),
            ),
            (
                "irsync.v1.DataStreamHeader",
                v1::DataStreamHeader {
                    session_id: vec![3; 16],
                    file_id: 1,
                    offset: 1 << 33,
                    length: 4096,
                    first_chunk_idx: 9,
                }
                .encode_to_vec(),
            ),
            (
                "irsync.v1.DataChunk",
                v1::DataChunk {
                    data: vec![0xaa; 300],
                    ops: vec![
                        v1::DeltaOp {
                            op: Some(delta_op::Op::Literal(b"lit".to_vec())),
                        },
                        v1::DeltaOp {
                            op: Some(delta_op::Op::Copy(v1::CopyOld {
                                offset: 100,
                                len: 50,
                            })),
                        },
                    ],
                }
                .encode_to_vec(),
            ),
        ];
        for msg in control_variants() {
            cases.push((
                "irsync.v1.Control",
                v1::Control { msg: Some(msg) }.encode_to_vec(),
            ));
        }
        for (name, bytes) in &cases {
            let fields = walk(&desc(name), bytes);
            assert!(!fields.is_empty(), "{name}");
            assert_eq!(total(&fields), bytes.len() as u64, "{name}");
            check_clean(&fields);
        }
    }

    #[test]
    fn hello_fields_are_named_and_sized() {
        let bytes = v1::Hello {
            protocol_version: 5,
            path: b"m".into(),
            ..Default::default()
        }
        .encode_to_vec();
        let fields = walk(&desc("irsync.v1.Hello"), &bytes);
        assert_eq!(
            fields,
            vec![
                Field {
                    number: 1,
                    name: Some("protocol_version".into()),
                    sizes: Sizes {
                        tag: 1,
                        len: 0,
                        value: 1
                    },
                    value: Value::Scalar("5".into()),
                    own: &bytes[0..2],
                },
                Field {
                    number: 4,
                    name: Some("path".into()),
                    sizes: Sizes {
                        tag: 1,
                        len: 1,
                        value: 1
                    },
                    value: Value::Bytes(b"m"),
                    own: &bytes[2..5],
                },
            ]
        );
    }

    #[test]
    fn enum_prints_name_and_number() {
        let d = desc("irsync.v1.Hello");
        let known = v1::Hello {
            direction: 2,
            ..Default::default()
        }
        .encode_to_vec();
        assert_eq!(
            walk(&d, &known)[0].value,
            Value::Scalar("LIST_EXPORTS(2)".into())
        );
        let unknown = v1::Hello {
            direction: 9,
            ..Default::default()
        }
        .encode_to_vec();
        assert_eq!(walk(&d, &unknown)[0].value, Value::Scalar("9".into()));
    }

    #[test]
    fn packed_field_is_one_field() {
        let bytes = v1::NeedChunks {
            file_id: 0,
            chunk_idx: vec![0, 1, 300],
        }
        .encode_to_vec();
        let fields = walk(&desc("irsync.v1.NeedChunks"), &bytes);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].number, 2);
        assert_eq!(fields[0].name.as_deref(), Some("chunk_idx"));
        assert_eq!(
            fields[0].sizes,
            Sizes {
                tag: 1,
                len: 1,
                value: 4
            }
        );
        assert_eq!(
            fields[0].value,
            Value::Packed(vec!["0".into(), "1".into(), "300".into()])
        );
    }

    #[test]
    fn packed_field_that_does_not_parse_is_unknown() {
        // chunk_idx, 2 bytes: one varint, then one that never ends.
        let bytes = [0x12, 0x02, 0x01, 0x80];
        let fields = walk(&desc("irsync.v1.NeedChunks"), &bytes);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name.as_deref(), Some("chunk_idx"));
        assert_eq!(
            fields[0].value,
            Value::Unknown {
                wire_type: 2,
                raw: &[0x01, 0x80]
            }
        );
    }

    #[test]
    fn wrong_wire_type_is_unknown() {
        // path (field 4, bytes) sent as a varint.
        let bytes = [0x20, 0x05];
        let fields = walk(&desc("irsync.v1.Hello"), &bytes);
        assert_eq!(fields[0].name.as_deref(), Some("path"));
        assert_eq!(
            fields[0].value,
            Value::Unknown {
                wire_type: 0,
                raw: &[0x05]
            }
        );
        assert_eq!(total(&fields), 2);
    }

    #[test]
    fn unknown_field_is_kept_with_its_size() {
        let mut bytes = v1::Hello {
            protocol_version: 5,
            ..Default::default()
        }
        .encode_to_vec();
        bytes.extend_from_slice(&[0x98, 0x06, 0x01]);
        let fields = walk(&desc("irsync.v1.Hello"), &bytes);
        let last = fields.last().unwrap();
        assert_eq!(last.number, 99);
        assert_eq!(last.name, None);
        assert_eq!(
            last.value,
            Value::Unknown {
                wire_type: 0,
                raw: &[0x01]
            }
        );
        assert_eq!(
            last.sizes,
            Sizes {
                tag: 2,
                len: 0,
                value: 1
            }
        );
        assert_eq!(total(&fields), bytes.len() as u64);
    }

    #[test]
    fn garbage_tail_is_undecodable() {
        let mut bytes = hello().encode_to_vec();
        let good = bytes.len();
        bytes.extend_from_slice(&[0xff; 3]);
        let fields = walk(&desc("irsync.v1.Hello"), &bytes);
        let last = fields.last().unwrap();
        assert_eq!(last.number, 0);
        assert_eq!(last.value, Value::Undecodable(&bytes[good..]));
        assert_eq!(
            last.sizes,
            Sizes {
                tag: 0,
                len: 0,
                value: 3
            }
        );
        assert_eq!(total(&fields), bytes.len() as u64);
    }

    #[test]
    fn length_past_the_end_is_undecodable() {
        let mut bytes = v1::Hello {
            protocol_version: 5,
            ..Default::default()
        }
        .encode_to_vec();
        // path, length 5, but only one byte follows.
        bytes.extend_from_slice(&[0x22, 0x05, b'a']);
        let fields = walk(&desc("irsync.v1.Hello"), &bytes);
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[1].value, Value::Undecodable(&[0x22, 0x05, b'a']));
        assert_eq!(total(&fields), bytes.len() as u64);
    }

    #[test]
    fn groups_and_field_zero_are_undecodable() {
        let d = desc("irsync.v1.Hello");
        for bytes in [[0x0b, 0x0c], [0x02, 0x00], [0x0f, 0x00]] {
            let fields = walk(&d, &bytes);
            assert_eq!(fields, vec![undecodable(&bytes)], "{bytes:x?}");
        }
    }

    fn undecodable(bytes: &[u8]) -> Field<'_> {
        Field {
            number: 0,
            name: None,
            sizes: Sizes {
                tag: 0,
                len: 0,
                value: bytes.len() as u64,
            },
            value: Value::Undecodable(bytes),
            own: bytes,
        }
    }

    #[test]
    fn every_truncation_still_adds_up() {
        let bytes = v1::Control {
            msg: Some(Msg::ChunkListPart(match &control_variants()[9] {
                Msg::ChunkListPart(p) => p.clone(),
                _ => unreachable!(),
            })),
        }
        .encode_to_vec();
        let d = desc("irsync.v1.Control");
        for end in 0..bytes.len() {
            assert_eq!(total(&walk(&d, &bytes[..end])), end as u64);
        }
    }

    #[test]
    fn deep_nesting_stops_at_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("node.proto"),
            "syntax = \"proto3\";\nmessage Node { Node child = 1; }\n",
        )
        .unwrap();
        let fds = protox::compile(["node.proto"], [dir.path()]).unwrap();
        let pool = DescriptorPool::from_file_descriptor_set(fds).unwrap();
        let node = pool.get_message_by_name("Node").unwrap();

        let mut bytes: Vec<u8> = Vec::new();
        for _ in 0..100 {
            let mut outer = vec![0x0a];
            prost::encoding::encode_varint(bytes.len() as u64, &mut outer);
            outer.extend_from_slice(&bytes);
            bytes = outer;
        }

        let fields = walk(&node, &bytes);
        assert_eq!(total(&fields), bytes.len() as u64);
        let mut messages = 0;
        let mut level = &fields;
        loop {
            assert_eq!(level.len(), 1);
            match &level[0].value {
                Value::Message { fields, .. } => {
                    assert_eq!(level[0].sizes.value, total(fields));
                    messages += 1;
                    level = fields;
                }
                Value::Bytes(_) => break,
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(messages, MAX_DEPTH);
    }

    #[test]
    fn scalars_print_as_their_declared_type() {
        use prost::encoding as enc;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("s.proto"),
            "syntax = \"proto3\";\n\
             message S {\n\
               sint32 a = 1; sint64 b = 2; int32 c = 3; int64 d = 4;\n\
               fixed32 e = 5; fixed64 f = 6; sfixed32 g = 7; sfixed64 h = 8;\n\
               float i = 9; double j = 10;\n\
             }\n",
        )
        .unwrap();
        let fds = protox::compile(["s.proto"], [dir.path()]).unwrap();
        let pool = DescriptorPool::from_file_descriptor_set(fds).unwrap();
        let s = pool.get_message_by_name("S").unwrap();

        let mut bytes = Vec::new();
        enc::sint32::encode(1, &i32::MIN, &mut bytes);
        enc::sint64::encode(2, &-3, &mut bytes);
        enc::int32::encode(3, &-1, &mut bytes);
        enc::int64::encode(4, &i64::MIN, &mut bytes);
        enc::fixed32::encode(5, &u32::MAX, &mut bytes);
        enc::fixed64::encode(6, &u64::MAX, &mut bytes);
        enc::sfixed32::encode(7, &-7, &mut bytes);
        enc::sfixed64::encode(8, &i64::MIN, &mut bytes);
        enc::float::encode(9, &-1.5, &mut bytes);
        enc::double::encode(10, &f64::NEG_INFINITY, &mut bytes);

        let printed: Vec<_> = walk(&s, &bytes)
            .into_iter()
            .map(|f| match f.value {
                Value::Scalar(text) => text,
                other => panic!("field {}: {other:?}", f.number),
            })
            .collect();
        assert_eq!(
            printed,
            [
                "-2147483648",
                "-3",
                "-1",
                "-9223372036854775808",
                "4294967295",
                "18446744073709551615",
                "-7",
                "-9223372036854775808",
                "-1.5",
                "-inf",
            ]
        );
    }

    #[test]
    fn empty_message_has_no_fields() {
        assert!(walk(&desc("irsync.v1.Hello"), &[]).is_empty());
        let bytes = v1::Control {
            msg: Some(Msg::Settle(v1::Settle {})),
        }
        .encode_to_vec();
        let fields = walk(&desc("irsync.v1.Control"), &bytes);
        let Value::Message { type_name, fields } = &fields[0].value else {
            panic!("not a message: {:?}", fields[0].value);
        };
        assert_eq!(type_name, "irsync.v1.Settle");
        assert!(fields.is_empty());
    }

    #[test]
    fn own_bytes_are_the_input() {
        let bytes = v1::Hello {
            protocol_version: 5,
            path: b"m".into(),
            build: Some(v1::BuildInfo {
                version: "0.1.0".into(),
                commit: "abc".into(),
                dirty: Some(false),
            }),
            ..Default::default()
        }
        .encode_to_vec();
        let fields = walk(&desc("irsync.v1.Hello"), &bytes);
        assert_eq!(own_bytes(&fields), bytes);
        let named = |name: &str| {
            fields
                .iter()
                .find(|f| f.name.as_deref() == Some(name))
                .unwrap()
        };
        // A nested message's own bytes are its tag and length only.
        let build = named("build");
        assert_eq!(build.own.len() as u64, build.sizes.tag + build.sizes.len);
        let path = named("path");
        assert_eq!(path.own.len() as u64, path.sizes.total());
    }

    #[test]
    fn own_bytes_of_malformed_input() {
        let d = desc("irsync.v1.Hello");
        let mut tail = vec![0x08, 0x05]; // protocol_version = 5
        tail.extend([0x0b; 20]); // then a group tag: 20 bytes that do not parse
        for bytes in [
            &[0x0b, 0x0c][..],
            &[0x02, 0x00],
            &[0x08, 0x05, 0x22, 0x09, 0x61],
            &tail,
        ] {
            assert_eq!(own_bytes(&walk(&d, bytes)), bytes, "{bytes:x?}");
        }
    }

    #[test]
    fn own_bytes_cover_a_packed_field() {
        // NeedChunks.chunk_idx is packed; 20 elements are over 16 bytes.
        let need = v1::NeedChunks {
            file_id: 1,
            chunk_idx: (0..20).collect(),
        };
        let bytes = need.encode_to_vec();
        let fields = walk(&desc("irsync.v1.NeedChunks"), &bytes);
        let packed = fields
            .iter()
            .find(|f| matches!(f.value, Value::Packed(_)))
            .unwrap();
        assert!(packed.own.len() > 16);
        assert_eq!(packed.own.len() as u64, packed.sizes.total());
        assert_eq!(own_bytes(&fields), bytes);
    }

    #[test]
    fn own_bytes_of_the_session_capture() {
        let file = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fuzz/seeds/pbwalk/session.pcapng"
        ))
        .unwrap();
        let d = desc("irsync.capture.v1.Record");
        let mut packets = 0;
        for packet in pbcap::Reader::new(&file[..]).unwrap() {
            let body = packet.unwrap().body;
            assert_eq!(own_bytes(&walk(&d, &body)), body);
            packets += 1;
        }
        assert!(packets > 0);
    }
}
