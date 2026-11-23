//! Every call into the operating system: metadata, names, links, times,
//! holes, ownership, special files and interrupt signals. The rest of the
//! crate names no platform API (`tests/platform_rule.rs` holds it to that),
//! so a new platform is one new file here.
//!
//! The interface is plain functions and constants; one implementation
//! exists per build. On Unix each constant leaves filegen as it always
//! was: the branches it guards elsewhere do nothing there.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

/// The kind of system a build runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
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

/// The type of a file system object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    File,
    Dir,
    Symlink,
    Fifo,
    CharDev,
    BlockDev,
    Socket,
    Other,
}

/// The metadata of one object, as [`lstat`] reads it.
#[derive(Clone, Debug)]
pub struct Stat {
    pub kind: FileKind,
    pub size: u64,
    /// The mode as the system reports it: the permission bits (with
    /// setuid, setgid and sticky), and on Unix the file type bits above
    /// them. Mask with `0o7777` for the permissions alone.
    pub mode: u32,
    /// The modification time in nanoseconds since the epoch.
    pub mtime_ns: i128,
    pub ino: u64,
    pub dev: u64,
    pub nlink: u64,
    pub uid: u32,
    /// The file attributes (`FILE_ATTRIBUTE_*`), for [`may_change`].
    #[cfg(windows)]
    attributes: u32,
}

/// What a symlink points to, for a platform that must know when it makes
/// one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    File,
    Dir,
}
