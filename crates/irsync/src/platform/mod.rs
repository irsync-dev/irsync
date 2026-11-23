//! Every call into the operating system: what its errors mean and how names
//! become bytes. The rest of the crate is moving here; once it has,
//! `tests/platform_rule.rs` holds the rest of the crate to naming no
//! platform API, so a new platform is one new file here.
//!
//! The interface is plain functions and [`Dir`], a directory held open
//! that every operation below a root goes through by a single name; one
//! implementation exists per build.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

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
}

/// What an operating-system error means to the caller, as [`cause`] reads
/// it. Callers match on these instead of on error numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// A link where the call was told not to follow one.
    LinkRefused,
    /// No such name.
    NotThere,
    /// The name exists already.
    AlreadyThere,
    /// A component that should be a directory is not one.
    NotADirectory,
    /// A directory where something else was expected.
    IsADirectory,
    /// A directory to remove still holds entries.
    NotEmpty,
    /// A rename or link across file systems.
    CrossDevice,
    /// This process, or the system, is out of open files for the moment.
    OutOfHandles,
    /// Another handle holds the file in a way that forbids the call. Never
    /// on Unix.
    InUse,
    /// The file system or the system does not do this.
    NotSupported,
    /// Anything else.
    Other,
}

/// What a name in a directory is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    Fifo,
    CharDev,
    BlockDev,
    Socket,
    Other,
}

/// What [`Dir::stat`] and [`fstat`] say about a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stat {
    pub kind: Kind,
    /// The permission bits (`0o7777`), without the kind.
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub nlink: u64,
    pub ino: u64,
    pub dev: u64,
    pub rdev: u64,
    /// `(seconds, nanoseconds)` since the Unix epoch.
    pub mtime: (i64, u32),
    /// The inode change time, as `mtime`.
    pub ctime: (i64, u32),
    /// A regular file with fewer blocks than its size needs.
    pub sparse: bool,
}

/// Whether [`Dir::open_file`] creates the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Create {
    /// Only an existing file is opened.
    No,
    /// Created when missing, opened when there.
    IfMissing,
    /// Created; an existing name is an error ([`Cause::AlreadyThere`]).
    New,
}

/// How [`Dir::open_file`] opens a file. A link at the name is never
/// followed.
#[derive(Clone, Copy, Debug)]
pub struct Open {
    pub read: bool,
    pub write: bool,
    pub create: Create,
    /// Cut an existing file to length 0.
    pub truncate: bool,
    /// Never wait in the open itself: a FIFO planted at the name cannot
    /// block it. No effect on a regular file.
    pub nonblock: bool,
    /// The permission bits of a new file (less the umask).
    pub mode: u32,
}

/// What a symlink points at, for systems that make the two differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    File,
    Dir,
}
