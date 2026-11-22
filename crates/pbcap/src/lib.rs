//! The protocol capture file format: pcapng packets holding Wireshark
//! "exported PDU" tags and a protobuf body. `write` builds the blocks
//! irsync writes; `Reader` reads them back.

#![forbid(unsafe_code)]

mod read;
pub mod write;

pub use read::{Error, Packet, Reader};

/// The envelope every capture's wrapper record follows. Field numbers:
/// 1 to 19 are the wrapper's own (2 `direction`, 10 `encoded_len`, 11
/// `trimmed` are fixed, the rest are the project's); 20 to 39 are the one
/// message that was on the wire; 40 and up are events the recorder saw
/// that were never on the wire.
pub mod v1 {
    include!(concat!(env!("OUT_DIR"), "/pbcap.v1.rs"));
}

/// Encoded FileDescriptorSet for package pbcap.v1.
pub static FILE_DESCRIPTOR_SET: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/descriptor.bin"));

/// Field numbers of the one message that was on the wire.
pub const MESSAGE_FIELDS: std::ops::RangeInclusive<u32> = 20..=39;

/// Field numbers from here up are events, never on the wire.
pub const FIRST_EVENT_FIELD: u32 = 40;

/// Wireshark refuses a packet larger than this.
pub const MAX_PACKET: usize = 262_144;

/// Wireshark's "upper PDU" link type: each packet starts with tags naming
/// its decoder.
pub const LINKTYPE_WIRESHARK_UPPER_PDU: u16 = 252;

#[cfg(test)]
mod envelope_tests {
    use prost::Message;

    use super::v1;

    #[test]
    fn envelope_reads_a_bigger_record() {
        // bytes: field 1 varint 2, field 2 varint 1, field 10 varint 38,
        //        field 20 length-delimited "abc"
        let e = v1::Envelope::decode(
            &[0x08, 2, 0x10, 1, 0x50, 38, 0xa2, 0x01, 3, b'a', b'b', b'c'][..],
        )
        .unwrap();
        assert_eq!(
            (e.direction(), e.encoded_len, e.trimmed),
            (v1::Direction::Sent, 38, None)
        );
    }
}
