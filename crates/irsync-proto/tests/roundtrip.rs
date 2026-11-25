#[test]
fn hello_roundtrips() {
    use irsync_proto::v1::{hello, sync_request, Hello, SyncRequest};
    use prost::Message;
    let req = SyncRequest {
        session_id: vec![7; 16],
        name: b"photos".to_vec(),
        path: b"2024/\xff".to_vec(),
        chunk_min: 65536,
        chunk_avg: 262144,
        chunk_max: 1048576,
        contents_only: true,
        delete_mode: sync_request::DeleteMode::DeleteAfter as i32,
        delete_excluded: true,
        max_delete: Some(0),
        dry_run: true,
        ignore_errors: true,
        itemize: true,
        update_only: true,
        checksum: true,
        inplace: true,
        fsync: true,
        paranoid: true,
        bwlimit: Some(1 << 20),
        single_file: true,
        append: sync_request::AppendMode::AppendVerify as i32,
        preserve: Some(sync_request::Preserve {
            recursive: false,
            dirs: true,
            links: true,
            perms: false,
            times: true,
            group: false,
            owner: true,
            devices: false,
            specials: true,
            hard_links: false,
            sparse: true,
        }),
        filters: vec![
            sync_request::FilterRule {
                include: true,
                pattern: "keep.o".into(),
            },
            sync_request::FilterRule {
                include: false,
                pattern: "*.o".into(),
            },
        ],
        partial_dir: b".p".to_vec(),
        numeric_ids: true,
        no_local_reuse: true,
    };
    for request in [
        hello::Request::Push(req.clone()),
        hello::Request::Pull(req),
        hello::Request::Probe(irsync_proto::v1::Probe {
            path: b"photos/a".to_vec(),
        }),
    ] {
        let h = Hello {
            protocol_version: irsync_proto::PROTOCOL_VERSION,
            build: Some(irsync_proto::v1::BuildInfo {
                version: "0.1.0".into(),
                commit: "abc1234".into(),
                dirty: Some(true),
            }),
            traceparent: "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".into(),
            tracestate: "k=v".into(),
            request: Some(request),
        };
        let back = Hello::decode(h.encode_to_vec().as_slice()).unwrap();
        assert_eq!(back, h);
    }
    assert_eq!(irsync_proto::ALPN, b"irsync/1");
}

/// Every reply to Hello decodes as itself and as nothing else: an empty
/// export listing and a refusal no longer look alike.
#[test]
fn hello_reply_arms_stay_apart() {
    use irsync_proto::v1::{
        error::Code, hello_reply::Reply, path_info, AdminReady, Error, ExportList, HelloAck,
        HelloReply, PathInfo,
    };
    use prost::Message;
    let replies = [
        Reply::Error(Error {
            code: Code::BadRequest as i32,
            message: String::new(),
        }),
        Reply::Ack(HelloAck {
            session_id: vec![1; 16],
            ..Default::default()
        }),
        Reply::Exports(ExportList::default()),
        Reply::PathInfo(PathInfo {
            kind: path_info::Kind::Missing as i32,
        }),
        Reply::AdminReady(AdminReady {}),
    ];
    for r in replies {
        let m = HelloReply { reply: Some(r) };
        let back = HelloReply::decode(m.encode_to_vec().as_slice()).unwrap();
        assert_eq!(back, m);
    }
}

#[test]
fn control_oneof_roundtrips() {
    use irsync_proto::v1::{
        control::Msg, file_ack::Status, plan_item::Action, ChunkBasis, ChunkListMore,
        ChunkListStart, ChunkRecord, Control, Error, FileAck, Hash, MayPlace, NeedChunks,
        PlaceVerdict, PlanItem, SessionDone, SourceStat, WithdrawChunkList,
    };
    use prost::Message;
    let h = |b: u8| Hash { bytes: vec![b; 32] };
    let msgs = vec![
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
        Msg::ChunkListStart(ChunkListStart {
            file_id: 3,
            whole_hash: Some(h(9)),
            total: 3,
            stat: Some(SourceStat {
                size: 3072,
                mtime_sec: -5,
                mtime_nsec: 999_999_999,
                ctime_sec: 1_700_000_000,
                ctime_nsec: 1,
                inode: 1 << 40,
                mode: 0o100644,
                uid: 1000,
                gid: 100,
            }),
            user: "alice".into(),
            group: "users".into(),
            chunks: vec![
                ChunkRecord {
                    len: 1024,
                    hash: Some(h(1)),
                },
                ChunkRecord {
                    len: 1024,
                    hash: Some(h(2)),
                },
            ],
        }),
        Msg::ChunkListMore(ChunkListMore {
            file_id: 3,
            first: 2,
            chunks: vec![ChunkRecord {
                len: 1024,
                hash: Some(h(3)),
            }],
        }),
        Msg::WithdrawChunkList(WithdrawChunkList { file_id: 3 }),
        Msg::Need(NeedChunks {
            file_id: 3,
            chunk_idx: vec![0, 5, 7],
            held: [(5, 70_000)].into(),
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
            matched: 7,
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
            listed: Some(irsync_proto::v1::ListTotals {
                reg: 7,
                dir: 8,
                link: 9,
                special: 10,
                total_size: 11,
            }),
        }),
        Msg::Error(Error {
            code: 6,
            message: "boom".into(),
        }),
        Msg::MayPlace(MayPlace {
            file_id: 1 << 40,
            gen: 3,
        }),
        Msg::PlaceVerdict(PlaceVerdict {
            file_id: 9,
            changed: true,
            gen: 3,
        }),
    ];
    for m in msgs {
        let c = Control { msg: Some(m) };
        let back = Control::decode(c.encode_to_vec().as_slice()).unwrap();
        assert_eq!(back, c);
    }
}

#[test]
fn listing_frame_oneof_roundtrips() {
    use irsync_proto::v1::{listing_frame::Frame, FileEntry, ListingEnd, ListingFrame, PathError};
    use prost::Message;
    let frames = [
        Frame::Entry(FileEntry {
            path: b"dir/\xff\xfe".to_vec(),
            size: 42,
            ..Default::default()
        }),
        Frame::Error(PathError {
            path: b"dir".to_vec(),
            message: "unreadable".into(),
        }),
        Frame::End(ListingEnd {
            incomplete: true,
            count: 9,
        }),
    ];
    for f in frames {
        let m = ListingFrame { frame: Some(f) };
        assert_eq!(
            ListingFrame::decode(m.encode_to_vec().as_slice()).unwrap(),
            m
        );
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

#[test]
fn admin_messages_round_trip() {
    use irsync_proto::v1::Ack;
    use irsync_proto::v1::{
        admin_reply, admin_request, change, error::Code, read, AdminReply, AdminRequest, Change,
        ClientAdd, ClientRemove, ClientRename, ClientSet, ClientView, Commit, ConfigText, Error,
        ExportAdd, ExportRemove, ExportView, GrantAdd, GrantRemove, Nack, Read, ReadConfig,
        ReadView, View,
    };
    use prost::Message;

    fn rt<M: Message + Default + PartialEq + std::fmt::Debug>(m: M) {
        let back = M::decode(m.encode_to_vec().as_slice()).unwrap();
        assert_eq!(back, m);
    }

    let ops = vec![
        change::Op::ExportAdd(ExportAdd {
            target: "photos:~/Photos".into(),
            read_only: true,
            warm_index: true,
        }),
        change::Op::ExportRemove(ExportRemove {
            name: "photos".into(),
        }),
        change::Op::GrantAdd(GrantAdd {
            export: "photos".into(),
            who: "myclient".into(),
            ro: true,
        }),
        change::Op::GrantRemove(GrantRemove {
            export: "photos".into(),
            who: "all".into(),
        }),
        change::Op::ClientAdd(ClientAdd {
            name: "myclient".into(),
            id: vec![9; 32],
        }),
        change::Op::ClientRemove(ClientRemove {
            name: "myclient".into(),
            force: true,
        }),
        change::Op::ClientRename(ClientRename {
            old: "a".into(),
            new: "b".into(),
        }),
        change::Op::ClientSet(ClientSet {
            name: "myclient".into(),
            settings: vec!["admin=ro".into(), "all_paths=rw".into()],
        }),
    ];
    for op in ops {
        let ch = Change { op: Some(op) };
        rt(ch.clone());
        rt(AdminRequest {
            msg: Some(admin_request::Msg::Change(ch)),
        });
    }
    rt(AdminRequest {
        msg: Some(admin_request::Msg::Commit(Commit {})),
    });
    for what in [
        read::What::View(ReadView {}),
        read::What::Config(ReadConfig { key: "x".into() }),
    ] {
        rt(AdminRequest {
            msg: Some(admin_request::Msg::Read(Read { what: Some(what) })),
        });
    }

    let view = View {
        daemon_id: vec![1; 32],
        allow_all_paths: true,
        exports: vec![ExportView {
            name: "photos".into(),
            path: "/srv/photos".into(),
            read_only: true,
            warm_index: true,
            fsync: true,
            is_auto: true,
            rw: vec!["myclient".into()],
            ro: vec!["other".into()],
        }],
        clients: vec![ClientView {
            name: "myclient".into(),
            id: vec![2; 32],
            rights: [("admin".to_string(), "rw".to_string())].into(),
        }],
    };
    for msg in [
        admin_reply::Msg::Ack(Ack {
            results: vec!["added".into()],
        }),
        admin_reply::Msg::Nack(Nack {
            change: 3,
            reason: "busy".into(),
        }),
        admin_reply::Msg::View(view),
        admin_reply::Msg::Config(ConfigText {
            lines: vec!["a = 1".into()],
        }),
        admin_reply::Msg::Refused(Error {
            code: Code::Unauthorized as i32,
            message: "not allowed".into(),
        }),
    ] {
        rt(AdminReply { msg: Some(msg) });
    }
}
