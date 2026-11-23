//! Wire protocol types for irsync, generated from `proto/irsync/v1`.

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
pub const PROTOCOL_VERSION: u32 = 5;

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[test]
    fn config_message_has_optional_keys_and_comments() {
        let c = config::Config::default();
        assert_eq!(c.unsafe_config_allowed, None);
        assert!(c.hosts.is_empty() && c.header.is_empty());
        let h = config::Host {
            name: "nas".into(),
            ..Default::default()
        };
        assert_eq!(h.endpoint_id, None);
        assert_eq!(h.comment, None);
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
