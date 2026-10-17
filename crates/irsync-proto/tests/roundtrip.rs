#[test]
fn hello_roundtrips() {
    use irsync_proto::v1::{hello::Direction, Hello};
    use prost::Message;
    let h = Hello {
        protocol_version: irsync_proto::PROTOCOL_VERSION,
        session_id: vec![7; 16],
        module: "photos".into(),
        path: b"2024/\xff".to_vec(),
        direction: Direction::DaemonSends as i32,
        chunk_min: 65536,
        chunk_avg: 262144,
        chunk_max: 1048576,
        contents_only: true,
    };
    let back = Hello::decode(h.encode_to_vec().as_slice()).unwrap();
    assert_eq!(back, h);
    assert_eq!(irsync_proto::ALPN, b"irsync/1");
}

#[test]
fn control_oneof_roundtrips() {
    use irsync_proto::v1::{
        control::Msg, file_ack::Status, plan_item::Action, ChunkList, ChunkRecord, Control, Error,
        FileAck, FileEntry, Hash, ListingEnd, ListingEntry, NeedChunks, PlanItem, SessionDone,
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
        }),
        Msg::ChunkList(ChunkList {
            file_id: 3,
            size: 2048,
            whole_hash: Some(h(9)),
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
        Msg::Ack(FileAck {
            file_id: 3,
            status: Status::HashMismatch as i32,
            message: "bad".into(),
        }),
        Msg::Done(SessionDone { files: 1, bytes: 2 }),
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
    };
    assert_eq!(DataChunk::decode(c.encode_to_vec().as_slice()).unwrap(), c);
}
