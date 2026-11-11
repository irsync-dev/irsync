//! Pins the exact wire bytes, so a field-number or wire-type change fails here
//! (a roundtrip test passes whatever the numbers are).

use irsync_proto::v1::{
    control::Msg, ChunkListPart, ChunkRecord, Control, DataStreamHeader, Hash, Heartbeat, ListRoom,
    MayPlace, PlaceVerdict,
};
use prost::Message;

#[test]
fn protocol_version_is_pinned() {
    assert_eq!(irsync_proto::PROTOCOL_VERSION, 3);
}

fn enc(m: Msg) -> Vec<u8> {
    Control { msg: Some(m) }.encode_to_vec()
}

#[test]
fn control_heartbeat_golden_bytes() {
    // heartbeat = 10, walking = 1, skipped_bytes = 2
    let hb = enc(Msg::Heartbeat(Heartbeat {
        walking: Some(b"a".to_vec()),
        skipped_bytes: 5,
        deleted: 0,
        delete_total: 0,
    }));
    assert_eq!(hb, [0x52, 0x05, 0x0A, 0x01, 0x61, 0x10, 0x05]);
}

#[test]
fn control_list_room_golden_bytes() {
    // list_room = 14, records = 1 (varint 300)
    let room = enc(Msg::ListRoom(ListRoom { records: 300 }));
    assert_eq!(room, [0x72, 0x03, 0x08, 0xAC, 0x02]);
}

#[test]
fn control_may_place_golden_bytes() {
    // may_place = 15, file_id = 1 (varint 300), gen = 2
    let m = enc(Msg::MayPlace(MayPlace {
        file_id: 300,
        gen: 2,
    }));
    assert_eq!(m, [0x7A, 0x05, 0x08, 0xAC, 0x02, 0x10, 0x02]);
}

#[test]
fn control_place_verdict_golden_bytes() {
    // place_verdict = 16 (two-byte tag), file_id = 1, changed = 2, gen = 3
    let v = enc(Msg::PlaceVerdict(PlaceVerdict {
        file_id: 7,
        changed: true,
        gen: 4,
    }));
    assert_eq!(v, [0x82, 0x01, 0x06, 0x08, 0x07, 0x10, 0x01, 0x18, 0x04]);
}

#[test]
fn control_chunk_list_part_golden_bytes() {
    // chunk_list_part = 11, with a nested Hash and a nested ChunkRecord
    let part = enc(Msg::ChunkListPart(ChunkListPart {
        file_id: 1,
        size: 2,
        whole_hash: Some(Hash {
            bytes: vec![0xAB; 2],
        }),
        total: 3,
        first: 0,
        chunks: vec![ChunkRecord {
            offset: 4,
            len: 5,
            hash: Some(Hash { bytes: vec![0xCD] }),
        }],
        last: true,
        mtime_sec: 6,
        mtime_nsec: 7,
    }));
    assert_eq!(
        part,
        [
            0x5A, 0x1D, 0x08, 0x01, 0x10, 0x02, 0x1A, 0x04, 0x0A, 0x02, 0xAB, 0xAB, 0x20, 0x03,
            0x32, 0x09, 0x08, 0x04, 0x10, 0x05, 0x1A, 0x03, 0x0A, 0x01, 0xCD, 0x38, 0x01, 0x40,
            0x06, 0x48, 0x07
        ]
    );
}

#[test]
fn data_stream_header_golden_bytes() {
    let h = DataStreamHeader {
        session_id: vec![1, 2],
        file_id: 3,
        offset: 4,
        length: 5,
        first_chunk_idx: 6,
    };
    assert_eq!(
        h.encode_to_vec(),
        [0x0A, 0x02, 0x01, 0x02, 0x10, 0x03, 0x18, 0x04, 0x20, 0x05, 0x28, 0x06]
    );
}
