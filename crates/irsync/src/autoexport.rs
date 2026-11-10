//! Where the daemon puts the automatic export for a path a client with the
//! immediate grant reached.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AutoRootError {
    /// Neither the path nor (for a push) its parent is there to export.
    #[error("no such file or directory")]
    NotFound,
    /// The root would be `/`: the whole filesystem is never exported
    /// automatically.
    #[error("cannot export / automatically; add an export by hand")]
    Root,
    /// The config file holds paths as UTF-8 text.
    #[error("automatic exports need a UTF-8 path")]
    NotUtf8,
}

/// The directory to export for `abs`: `abs` itself when it is a directory,
/// its parent when it is anything else (a file is never an export). A
/// missing `abs` on a `push` whose parent is a directory is created here
/// (that one directory only) and exported; otherwise there is nothing to
/// export. The root must pass [`check_root`], before anything is created.
pub fn auto_root(abs: &Path, push: bool) -> Result<PathBuf, AutoRootError> {
    let parent = || {
        let p = abs
            .parent()
            .filter(|p| p.is_dir())
            .ok_or(AutoRootError::NotFound)?;
        check_root(p)?;
        Ok(p.to_path_buf())
    };
    match std::fs::metadata(abs) {
        Ok(m) if m.is_dir() => {
            check_root(abs)?;
            Ok(abs.to_path_buf())
        }
        Ok(_) => parent(),
        Err(_) if push => {
            if !abs.parent().is_some_and(Path::is_dir) {
                return Err(AutoRootError::NotFound);
            }
            check_root(abs)?;
            match std::fs::create_dir(abs) {
                Ok(()) => Ok(abs.to_path_buf()),
                // Created meanwhile by another connection.
                Err(_) if abs.is_dir() => Ok(abs.to_path_buf()),
                Err(_) => Err(AutoRootError::NotFound),
            }
        }
        Err(_) => Err(AutoRootError::NotFound),
    }
}

/// Refuses `/` and a path that is not UTF-8 as an automatic export.
pub fn check_root(root: &Path) -> Result<(), AutoRootError> {
    if root.parent().is_none() {
        return Err(AutoRootError::Root);
    }
    if root.to_str().is_none() {
        return Err(AutoRootError::NotUtf8);
    }
    Ok(())
}
