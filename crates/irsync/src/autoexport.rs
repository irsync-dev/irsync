//! Where the daemon puts the automatic export for a path a client with the
//! `all_paths` right reached.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AutoExportError {
    /// Neither the path nor (for a push) its parent is there to export.
    #[error("no such file or directory")]
    NotFound,
    /// A push's missing directory could not be made; `error` is the OS's
    /// reason, as text.
    #[error("cannot create {}: {error}", path.display())]
    Create { path: PathBuf, error: String },
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
/// export. With `file_dest` (a push whose destination is a single file) a
/// missing `abs` is the file to be written, so its parent directory is
/// exported and nothing is created; the parent must be a directory. The
/// root must pass [`check_root`], before anything is created.
pub fn auto_export_dir(
    abs: &Path,
    push: bool,
    file_dest: bool,
) -> Result<PathBuf, AutoExportError> {
    let parent = || {
        let p = abs
            .parent()
            .filter(|p| crate::platform::native_path(p).is_ok_and(|p| p.is_dir()))
            .ok_or(AutoExportError::NotFound)?;
        check_root(p)?;
        Ok(p.to_path_buf())
    };
    // Opened as written (see `native_path`): on Windows `x.` is not `x`.
    let native = crate::platform::native_path(abs).map_err(|_| AutoExportError::NotFound)?;
    match std::fs::metadata(&native) {
        Ok(m) if m.is_dir() => {
            check_root(abs)?;
            Ok(abs.to_path_buf())
        }
        Ok(_) => parent(),
        Err(_) if push && file_dest => parent(),
        Err(_) if push => {
            if !abs
                .parent()
                .is_some_and(|p| crate::platform::native_path(p).is_ok_and(|p| p.is_dir()))
            {
                return Err(AutoExportError::NotFound);
            }
            check_root(abs)?;
            match std::fs::create_dir(&native) {
                Ok(()) => Ok(abs.to_path_buf()),
                // Created meanwhile by another connection.
                Err(_) if native.is_dir() => Ok(abs.to_path_buf()),
                Err(e) => Err(AutoExportError::Create {
                    path: abs.to_path_buf(),
                    error: e.to_string(),
                }),
            }
        }
        Err(_) => Err(AutoExportError::NotFound),
    }
}

/// Refuses `/` and a path that is not UTF-8 as an automatic export.
pub fn check_root(root: &Path) -> Result<(), AutoExportError> {
    if root.parent().is_none() {
        return Err(AutoExportError::Root);
    }
    if root.to_str().is_none() {
        return Err(AutoExportError::NotUtf8);
    }
    Ok(())
}
