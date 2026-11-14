//! Just enough pcapng to write Wireshark "exported PDU" packets.

use std::net::Ipv6Addr;

/// Wireshark's "upper PDU" link type: each packet starts with tags naming
/// its decoder.
pub(crate) const LINKTYPE_WIRESHARK_UPPER_PDU: u16 = 252;

/// A block: type, total length, `body` padded to 4, total length again.
fn block(kind: u32, body: &[u8]) -> Vec<u8> {
    let padded = body.len().next_multiple_of(4);
    let total = u32::try_from(12 + padded).expect("a pcapng block fits in u32");
    let mut out = Vec::with_capacity(12 + padded);
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(body);
    out.resize(8 + padded, 0);
    out.extend_from_slice(&total.to_le_bytes());
    out
}

/// The Section Header Block that starts the file: little-endian, version
/// 1.0, section length unknown, no options.
pub(crate) fn section_header() -> Vec<u8> {
    let mut body = Vec::with_capacity(16);
    body.extend_from_slice(&0x1a2b_3c4d_u32.to_le_bytes());
    body.extend_from_slice(&1u16.to_le_bytes());
    body.extend_from_slice(&0u16.to_le_bytes());
    body.extend_from_slice(&(-1i64).to_le_bytes());
    block(0x0a0d_0d0a, &body)
}

/// An Interface Description Block with no options, so timestamps are in
/// microseconds. A `snaplen` of 0 means no limit.
pub(crate) fn interface(linktype: u16, snaplen: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(8);
    body.extend_from_slice(&linktype.to_le_bytes());
    body.extend_from_slice(&0u16.to_le_bytes());
    body.extend_from_slice(&snaplen.to_le_bytes());
    block(1, &body)
}

/// An Enhanced Packet Block on interface 0 holding all of `payload`.
pub(crate) fn packet(ts_micros: u64, payload: &[u8]) -> Vec<u8> {
    let len = u32::try_from(payload.len()).expect("a packet fits in u32");
    let mut body = Vec::with_capacity(20 + payload.len());
    body.extend_from_slice(&0u32.to_le_bytes());
    body.extend_from_slice(&((ts_micros >> 32) as u32).to_le_bytes());
    body.extend_from_slice(&(ts_micros as u32).to_le_bytes());
    body.extend_from_slice(&len.to_le_bytes());
    body.extend_from_slice(&len.to_le_bytes());
    body.extend_from_slice(payload);
    block(6, &body)
}

// Wireshark's exported-PDU tags (`wsutil/exported_pdu_tlvs.h`).
const TAG_END: u16 = 0;
const TAG_DISSECTOR_NAME: u16 = 12;
const TAG_IPV6_SRC: u16 = 22;
const TAG_IPV6_DST: u16 = 23;
const TAG_PORT_TYPE: u16 = 24;
const TAG_SRC_PORT: u16 = 25;
const TAG_DST_PORT: u16 = 26;
const TAG_COL_PROT_TEXT: u16 = 33;
const PORT_TYPE_UDP: u32 = 3;

/// One tag: `u16` tag and length, big-endian, then the value padded with
/// zeros to 4 bytes; the length counts the padding.
fn tag(out: &mut Vec<u8>, tag: u16, value: &[u8]) {
    let padded = value.len().next_multiple_of(4);
    out.extend_from_slice(&tag.to_be_bytes());
    out.extend_from_slice(&u16::try_from(padded).expect("a short tag").to_be_bytes());
    out.extend_from_slice(value);
    out.resize(out.len() + padded - value.len(), 0);
}

/// An exported-PDU packet: tags that hand `body` to Wireshark's protobuf
/// decoder, between fake UDP endpoints, then `body`.
pub(crate) fn exported_pdu(
    src: Ipv6Addr,
    dst: Ipv6Addr,
    src_port: u32,
    dst_port: u32,
    body: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(96 + body.len());
    tag(&mut out, TAG_DISSECTOR_NAME, b"protobuf");
    tag(&mut out, TAG_IPV6_SRC, &src.octets());
    tag(&mut out, TAG_IPV6_DST, &dst.octets());
    tag(&mut out, TAG_PORT_TYPE, &PORT_TYPE_UDP.to_be_bytes());
    tag(&mut out, TAG_SRC_PORT, &src_port.to_be_bytes());
    tag(&mut out, TAG_DST_PORT, &dst_port.to_be_bytes());
    tag(&mut out, TAG_COL_PROT_TEXT, b"irsync");
    tag(&mut out, TAG_END, &[]);
    out.extend_from_slice(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Ipv6Addr = Ipv6Addr::new(0xfd69, 0x7273, 0, 0, 0, 0, 0, 1);
    const B: Ipv6Addr = Ipv6Addr::new(0xfd69, 0x7273, 0, 0, 0, 1, 0, 0);

    fn u32_le(b: &[u8]) -> u32 {
        u32::from_le_bytes(b[..4].try_into().unwrap())
    }

    #[test]
    fn exported_pdu_tags_are_padded() {
        let p = exported_pdu(A, B, 1, 2, b"xy");
        assert_eq!(&p[..4], &[0, 12, 0, 8]); // "protobuf" is already 8
        assert_eq!(&p[4..12], b"protobuf");
        assert_eq!(&p[12..16], &[0, 22, 0, 16]);
        assert_eq!(&p[16..32], &A.octets());
        assert_eq!(&p[32..36], &[0, 23, 0, 16]);
        assert_eq!(&p[36..52], &B.octets());
        assert_eq!(&p[52..60], &[0, 24, 0, 4, 0, 0, 0, 3]);
        assert_eq!(&p[60..68], &[0, 25, 0, 4, 0, 0, 0, 1]);
        assert_eq!(&p[68..76], &[0, 26, 0, 4, 0, 0, 0, 2]);
        assert_eq!(&p[76..80], &[0, 33, 0, 8]); // "irsync" padded to 8
        assert_eq!(&p[80..88], b"irsync\0\0");
        assert!(p.ends_with(&[0, 0, 0, 0, b'x', b'y']));
        assert_eq!(p.len(), 94);
    }

    #[test]
    fn packet_block_lengths_match() {
        for n in 0..9 {
            let payload = vec![0xab; n];
            let b = packet(0x1_0000_0002, &payload);
            assert_eq!(b.len() % 4, 0);
            assert_eq!(b.len(), 32 + n.div_ceil(4) * 4);
            assert_eq!(u32_le(&b[0..]), 6);
            assert_eq!(u32_le(&b[4..]) as usize, b.len());
            assert_eq!(u32_le(&b[b.len() - 4..]) as usize, b.len());
            assert_eq!(u32_le(&b[8..]), 0); // interface 0
            assert_eq!(u32_le(&b[12..]), 1); // timestamp, high word
            assert_eq!(u32_le(&b[16..]), 2); // timestamp, low word
            assert_eq!(u32_le(&b[20..]) as usize, n);
            assert_eq!(u32_le(&b[24..]) as usize, n);
            assert_eq!(&b[28..28 + n], &payload[..]);
        }
    }

    #[test]
    fn section_and_interface_blocks() {
        let s = section_header();
        assert_eq!(s.len(), 28);
        assert_eq!(&s[..4], &[0x0a, 0x0d, 0x0d, 0x0a]);
        assert_eq!(u32_le(&s[8..]), 0x1a2b_3c4d);
        assert_eq!(u32_le(&s[24..]), 28);
        let i = interface(252, 0);
        assert_eq!(i.len(), 20);
        assert_eq!(u32_le(&i[0..]), 1);
        assert_eq!(&i[8..12], &[252, 0, 0, 0]);
        assert_eq!(u32_le(&i[16..]), 20);
    }
}
