//! Read-only or read-write: the access a grant or a right gives, as the
//! config file spells it.

/// The access a grant or a right gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    ReadOnly,
    ReadWrite,
}

impl Access {
    pub(crate) fn permits(self, write: bool) -> bool {
        !write || self == Access::ReadWrite
    }

    /// The word the file spells it with.
    pub fn as_str(self) -> &'static str {
        match self {
            Access::ReadWrite => "rw",
            Access::ReadOnly => "ro",
        }
    }

    /// `"rw"` or `"ro"`; anything else is not an access.
    pub fn parse(s: &str) -> Option<Access> {
        match s {
            "rw" => Some(Access::ReadWrite),
            "ro" => Some(Access::ReadOnly),
            _ => None,
        }
    }

    /// The access a message value of `pb::Access` stands for; `None` for
    /// unspecified or a number the message does not know.
    pub fn from_message(n: i32) -> Option<Access> {
        match irsync_proto::config::Access::try_from(n) {
            Ok(irsync_proto::config::Access::Rw) => Some(Access::ReadWrite),
            Ok(irsync_proto::config::Access::Ro) => Some(Access::ReadOnly),
            _ => None,
        }
    }
}

impl From<Access> for irsync_proto::config::Access {
    fn from(a: Access) -> Self {
        match a {
            Access::ReadWrite => irsync_proto::config::Access::Rw,
            Access::ReadOnly => irsync_proto::config::Access::Ro,
        }
    }
}
