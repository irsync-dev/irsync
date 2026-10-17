//! Wire protocol types for irsync, generated from `proto/irsync/v1`.

pub mod v1 {
    include!(concat!(env!("OUT_DIR"), "/irsync.v1.rs"));
}

/// ALPN identifier for the irsync protocol.
pub const ALPN: &[u8] = b"irsync/1";
/// Current protocol version.
pub const PROTOCOL_VERSION: u32 = 1;
