//! irsync's protocol crate: the wire messages (`v1`), the capture messages
//! and the config file's message (`config`), all generated from `proto/`,
//! and the content-defined chunking both sides must agree on (`chunk`).

#![forbid(unsafe_code)]

pub mod chunk;

pub mod v1 {
    include!(concat!(env!("OUT_DIR"), "/irsync.v1.rs"));
}

/// Capture-only messages. Never sent on the wire.
pub mod capture {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/irsync.capture.v1.rs"));
    }
}

/// The configuration of irsync, in memory. Never encoded.
pub mod config {
    include!(concat!(env!("OUT_DIR"), "/irsync.config.rs"));
}

/// Encoded `FileDescriptorSet` for the `irsync.v1`, `irsync.capture.v1` and `irsync.config` packages.
pub static FILE_DESCRIPTOR_SET: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/descriptor.bin"));

/// ALPN identifier for the irsync protocol.
pub const ALPN: &[u8] = b"irsync/1";
/// Current protocol version.
pub const PROTOCOL_VERSION: u32 = 7;
/// The most bytes of a chunk one `DataChunk` frame carries: a chunk's bytes
/// travel in pieces of this size, so a connection that drops mid-chunk
/// still leaves the receiver the pieces that arrived.
pub const DATA_PIECE: usize = 64 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[test]
    fn config_message_has_optional_keys_and_comments() {
        let c = config::Config::default();
        assert_eq!(c.unsafe_config_allowed, None);
        assert_eq!(c.config_version, None);
        assert!(c.servers.is_empty() && c.clients.is_empty() && c.header.is_empty());
        let s = config::ServerEntry {
            name: "nas".into(),
            ..Default::default()
        };
        assert!(s.endpoint_id.is_empty());
        assert_eq!(s.comment, None);
        let e = config::Entry {
            value: "laptop".into(),
            comment: None,
        };
        assert_eq!(e.value, "laptop");
    }

    #[test]
    fn descriptor_set_has_all_three_packages() {
        let fds = prost_types::FileDescriptorSet::decode(FILE_DESCRIPTOR_SET).unwrap();
        let pkgs: Vec<_> = fds.file.iter().filter_map(|f| f.package.clone()).collect();
        assert!(pkgs.iter().any(|p| p == "irsync.v1"));
        assert!(pkgs.iter().any(|p| p == "irsync.capture.v1"));
        assert!(pkgs.iter().any(|p| p == "irsync.config"));
    }
}
