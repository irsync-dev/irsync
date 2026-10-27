#[test]
fn hello_roundtrips() {
    use irsync_proto::v1::{hello::Direction, Hello};
    use prost::Message;
    let h = Hello {
        protocol_version: irsync_proto::PROTOCOL_VERSION,
        session_id: vec![7; 16],
        export: "photos".into(),
        path: b"2024/\xff".to_vec(),
        direction: Direction::DaemonSends as i32,
        chunk_min: 65536,
        chunk_avg: 262144,
        chunk_max: 1048576,
        contents_only: true,
        delete_mode: irsync_proto::v1::hello::DeleteMode::DeleteAfter as i32,
        delete_excluded: true,
        max_delete: Some(0),
        dry_run: true,
        ignore_errors: true,
        itemize: true,
        update_only: true,
        checksum: true,
        inplace: true,
        append: irsync_proto::v1::hello::AppendMode::AppendVerify as i32,
        build: Some(irsync_proto::v1::BuildInfo {
            version: "0.1.0".into(),
            commit: "abc1234".into(),
            dirty: Some(true),
        }),
    };
    let back = Hello::decode(h.encode_to_vec().as_slice()).unwrap();
    assert_eq!(back, h);
    assert_eq!(irsync_proto::ALPN, b"irsync/1");
}

#[test]
fn control_oneof_roundtrips() {
    use irsync_proto::v1::{
        control::Msg, file_ack::Status, plan_item::Action, ChunkBasis, ChunkListPart, ChunkRecord,
        Control, Error, FileAck, FileEntry, Hash, ListingEnd, ListingEntry, NeedChunks, PlanItem,
        SessionDone,
    };
    use prost::Message;
    let h = |b: u8| Hash { bytes: vec![b; 32] };
    let msgs = vec![
        Msg::Listing(ListingEntry {
            entry: Some(FileEntry {
                path: b"dir/\xff\xfe".to_vec(),
                size: 42,
                ..Default::default()
            }),
        }),
        Msg::ListingEnd(ListingEnd {
            incomplete: true,
            count: 9,
        }),
        Msg::Plan(PlanItem {
            file_id: 3,
            path: b"a/\xff".to_vec(),
            action: Action::Delta as i32,
            append_from: 0,
        }),
        Msg::Plan(PlanItem {
            file_id: 4,
            path: b"log".to_vec(),
            action: Action::Append as i32,
            append_from: 1 << 35,
        }),
        Msg::ChunkListPart(ChunkListPart {
            file_id: 3,
            size: 2048,
            whole_hash: Some(h(9)),
            total: 2,
            first: 0,
            last: true,
            chunks: vec![
                ChunkRecord {
                    offset: 0,
                    len: 1024,
                    hash: Some(h(1)),
                },
                ChunkRecord {
                    offset: 1024,
                    len: 1024,
                    hash: Some(h(2)),
                },
            ],
        }),
        Msg::Need(NeedChunks {
            file_id: 3,
            chunk_idx: vec![0, 5, 7],
        }),
        Msg::ChunkBasis(ChunkBasis {
            file_id: 3,
            chunk_idx: 5,
            offset: 1 << 33,
            block: 1024,
            weak: vec![1, 2, 0xffff_ffff],
            strong: vec![9; 24],
        }),
        Msg::Ack(FileAck {
            file_id: 3,
            status: Status::HashMismatch as i32,
            message: "bad".into(),
        }),
        Msg::Done(SessionDone {
            files: 1,
            bytes: 2,
            errors: vec![irsync_proto::v1::PathError {
                path: b"d/\xff".to_vec(),
                message: "mkdir: denied".into(),
            }],
            deleted: vec![b"old".to_vec()],
            would_delete: vec![b"x/\xff".to_vec()],
            max_delete_reached: true,
            deleted_omitted: 3,
            would_delete_omitted: 4,
            created: 5,
            transferred_size: 6,
        }),
        Msg::Error(Error {
            code: 7,
            message: "boom".into(),
        }),
    ];
    for m in msgs {
        let c = Control { msg: Some(m) };
        let back = Control::decode(c.encode_to_vec().as_slice()).unwrap();
        assert_eq!(back, c);
    }
}

#[test]
fn data_header_roundtrips() {
    use irsync_proto::v1::{DataChunk, DataStreamHeader};
    use prost::Message;
    let hd = DataStreamHeader {
        session_id: vec![1; 16],
        file_id: 4,
        offset: 1 << 40,
        length: 1 << 20,
        first_chunk_idx: 12,
    };
    assert_eq!(
        DataStreamHeader::decode(hd.encode_to_vec().as_slice()).unwrap(),
        hd
    );
    let c = DataChunk {
        data: vec![0xab; 1 << 20],
        ops: vec![],
    };
    assert_eq!(DataChunk::decode(c.encode_to_vec().as_slice()).unwrap(), c);
    use irsync_proto::v1::{delta_op::Op, CopyOld, DeltaOp};
    let d = DataChunk {
        data: vec![],
        ops: vec![
            DeltaOp {
                op: Some(Op::Literal(vec![1, 2, 3])),
            },
            DeltaOp {
                op: Some(Op::Copy(CopyOld {
                    offset: 1 << 40,
                    len: 4096,
                })),
            },
        ],
    };
    assert_eq!(DataChunk::decode(d.encode_to_vec().as_slice()).unwrap(), d);
}
