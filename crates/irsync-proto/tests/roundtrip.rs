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
