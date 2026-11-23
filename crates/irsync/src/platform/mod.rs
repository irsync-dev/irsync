//! Every call into the operating system: what its errors mean and how names
//! become bytes. The rest of the crate is moving here; once it has,
//! `tests/platform_rule.rs` holds the rest of the crate to naming no
//! platform API, so a new platform is one new file here.
//!
//! The interface is plain functions; one implementation exists per build.

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
