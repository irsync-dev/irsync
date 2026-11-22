//! The protocol capture file format: pcapng packets holding Wireshark
//! "exported PDU" tags and a protobuf body. `write` builds the blocks
//! irsync writes; `Reader` reads them back.

#![forbid(unsafe_code)]

mod read;
pub mod write;

pub use read::{Error, Packet, Reader};

/// Wireshark refuses a packet larger than this.
pub const MAX_PACKET: usize = 262_144;

/// Wireshark's "upper PDU" link type: each packet starts with tags naming
/// its decoder.
pub const LINKTYPE_WIRESHARK_UPPER_PDU: u16 = 252;
