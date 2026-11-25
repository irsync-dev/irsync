//! Every call into the operating system: what its errors mean and how names
//! become bytes. `tests/platform_rule.rs` holds the rest of the crate to
//! naming no platform API, so a new platform is one new file here.
//!
//! The interface is plain functions and [`Dir`], a directory held open
//! that every operation below a root goes through by a single name; one
//! implementation exists per build.
//!
//! Positional reads and writes (`read_at`, `write_at`, `read_exact_at`,
//! `write_all_at`) take their offset and never rely on the file's
//! position. On Unix they leave it as it was; on Windows they move it.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

/// The tests of [`Dir`], on every platform.
#[cfg(test)]
mod dir_tests;

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

    /// Whether files carry a permission mode a receiver can set (`-p`).
    /// Windows has none, so a receiver there leaves modes alone, as the
    /// user did not ask for them; setting one stays an error in the
    /// platform calls ([`Dir::set_mode`] and the rest), never a silent
    /// success.
    pub const fn has_modes(self) -> bool {
        match self {
            Platform::Unix => true,
            Platform::Windows => false,
        }
    }

    /// The separator this kind of system writes between the components of
    /// a path: `/` on Unix, `\` on Windows.
    pub const fn separator(self) -> u8 {
        match self {
            Platform::Unix => b'/',
            Platform::Windows => b'\\',
        }
    }

    /// Whether two different spellings may name one directory: on Windows
    /// a name in another case (NTFS folds case by default) or an 8.3 short
    /// name. Where they may, the daemon also checks the directories a
    /// request passes through by identity (see `daemon::inner_export`).
    /// Not on Unix, where irsync makes no such check (a case-insensitive
    /// macOS volume has the same gap; recorded, not handled here).
    pub const fn names_may_alias(self) -> bool {
        match self {
            Platform::Unix => false,
            Platform::Windows => true,
        }
    }

    /// Whether the byte `b` separates the components of a path on this kind
    /// of system: `/` on Unix; `\` and `/` on Windows. Both are ASCII, so
    /// they never occur inside a longer UTF-8 or WTF-8 sequence.
    pub const fn is_separator(self, b: u8) -> bool {
        match self {
            Platform::Unix => b == b'/',
            Platform::Windows => b == b'/' || b == b'\\',
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

/// The `st_mode` kind bits of `kind` (`S_IFREG` and the rest); none for
/// [`Kind::Other`]. With [`Stat::mode`] they make the whole `st_mode` a
/// listing carries, so they are part of the wire format: the same on every
/// system, Windows included. They are POSIX's traditional values, which
/// Linux, macOS and FreeBSD all use (a Unix test holds them to `libc`'s).
pub fn mode_of_kind(kind: Kind) -> u32 {
    match kind {
        Kind::File => 0o100000,
        Kind::Dir => 0o040000,
        Kind::Symlink => 0o120000,
        Kind::Fifo => 0o010000,
        Kind::CharDev => 0o020000,
        Kind::BlockDev => 0o060000,
        Kind::Socket => 0o140000,
        Kind::Other => 0,
    }
}

/// Where irsync keeps its files by default (see `default_locations`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locations {
    /// The config file, `irsync.toml`; the default keys sit beside it.
    pub config: std::path::PathBuf,
    /// The state directory (indexes, locks, the daemon's log).
    pub state: std::path::PathBuf,
    /// The key tried when the config's own client key does not exist yet
    /// (never generated); `None` without a home.
    pub key: Option<std::path::PathBuf>,
}

/// This process's resource usage so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// CPU time in user mode.
    pub cpu_user: std::time::Duration,
    /// CPU time in the kernel.
    pub cpu_sys: std::time::Duration,
    /// The peak resident memory, in bytes.
    pub max_rss: u64,
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
