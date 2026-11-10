//! Where the daemon puts the automatic export for a path a client with the
//! immediate grant reached.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AutoRootError {
    /// Neither the path nor (for a push) its parent is there to export.
    #[error("no such file or directory")]
    NotFound,
}

/// The directory to export for `abs`: `abs` itself when it is a directory,
/// its parent when it is anything else (a file is never an export). A
/// missing `abs` is exported by its parent when `push` (the receiver
/// creates it, as rsync does) and that parent is a directory; otherwise
/// there is nothing to export.
pub fn auto_root(abs: &Path, push: bool) -> Result<PathBuf, AutoRootError> {
    let parent = || {
        abs.parent()
            .filter(|p| p.is_dir())
            .map(Path::to_path_buf)
            .ok_or(AutoRootError::NotFound)
    };
    match std::fs::metadata(abs) {
        Ok(m) if m.is_dir() => Ok(abs.to_path_buf()),
        Ok(_) => parent(),
        Err(_) if push => parent(),
        Err(_) => Err(AutoRootError::NotFound),
    }
}
