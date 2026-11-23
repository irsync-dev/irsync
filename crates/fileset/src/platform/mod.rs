//! Every call into the operating system: metadata, names, extended
//! attributes, opening files and CPU facts. The rest of the crate names no
//! platform API (`tests/platform_rule.rs` holds it to that), so a new
//! platform is one new file here.
//!
//! The interface is plain functions; one implementation exists per build.

use crate::model::{Kind, Timespec};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

/// The metadata of one object, as [`lstat`] reads it.
#[derive(Clone, Debug)]
pub struct Stat {
    pub kind: Kind,
    /// The permission bits, with setuid, setgid and sticky.
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub nlink: u64,
    pub ino: u64,
    pub dev: u64,
    pub rdev: u64,
    pub mtime: Timespec,
    pub ctime: Timespec,
}

/// Extended attributes: names and values, sorted by name.
pub type Xattrs = Vec<(Vec<u8>, Vec<u8>)>;

/// The kind of system a build runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Unix,
    Windows,
}

impl Platform {
    /// The platform of this build.
    #[cfg(unix)]
    pub const CURRENT: Platform = Platform::Unix;
    /// The platform of this build.
    #[cfg(windows)]
    pub const CURRENT: Platform = Platform::Windows;

    pub fn as_str(self) -> &'static str {
        match self {
            Platform::Unix => "unix",
            Platform::Windows => "windows",
        }
    }

    /// The platform named exactly `s`, as [`Platform::as_str`] writes it.
    pub fn parse(s: &str) -> Option<Platform> {
        match s {
            "unix" => Some(Platform::Unix),
            "windows" => Some(Platform::Windows),
            _ => None,
        }
    }
}
