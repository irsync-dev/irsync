//! Placement rules for a single file as the source of a copy, following
//! rsync: where the file lands depends on what the destination is.

use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::fs::paths::RelPath;

/// A one-file transfer. Each name is exactly one path component. `wire` is
/// the receiver's name for the file, `local` is this side's name; they are
/// equal on the receiver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SingleFile {
    pub wire: RelPath,
    pub local: RelPath,
}

/// Where a single file goes: the directory to use as the session root, and
/// the file's name inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePlace {
    pub root: PathBuf,
    pub name: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum PlaceError {
    #[error("{}: not a directory", .0.display())]
    NotADirectory(PathBuf),
    #[error("{}: is a directory", .0.display())]
    IsADirectory(PathBuf),
    #[error("{}: {}", .0.display(), .1)]
    Io(PathBuf, io::Error),
}

/// True for a non-empty name with no `/` and no NUL that is not `.` or `..`.
pub fn is_one_component(name: &[u8]) -> bool {
    !name.is_empty() && name != b"." && name != b".." && !name.contains(&b'/') && !name.contains(&0)
}

/// The parent (`.` for a bare name) and the last component of `path`.
pub fn split_last(path: &Path) -> (PathBuf, Vec<u8>) {
    match path.file_name() {
        Some(name) => {
            let parent = match path.parent() {
                Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
                _ => PathBuf::from("."),
            };
            (parent, name.as_bytes().to_vec())
        }
        None => (PathBuf::from("."), path.as_os_str().as_bytes().to_vec()),
    }
}

/// Decide where a single file named `src_name` lands for destination `dest`.
/// `follow` resolves a symlink at `dest` (the client does, the daemon does
/// not). A missing root directory is not checked here; callers create it.
pub fn place_file(
    dest: &Path,
    contents_only: bool,
    src_name: &[u8],
    follow: bool,
) -> Result<FilePlace, PlaceError> {
    let meta = if follow {
        std::fs::metadata(dest)
    } else {
        std::fs::symlink_metadata(dest)
    };
    let in_dir = match meta {
        Ok(m) if m.is_dir() => true,
        Ok(_) => {
            if contents_only {
                return Err(PlaceError::NotADirectory(dest.to_path_buf()));
            }
            false
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => contents_only,
        Err(e) => return Err(PlaceError::Io(dest.to_path_buf(), e)),
    };
    if !in_dir {
        let (root, name) = split_last(dest);
        return Ok(FilePlace { root, name });
    }
    let target = dest.join(OsStr::from_bytes(src_name));
    match std::fs::symlink_metadata(&target) {
        Ok(m) if m.is_dir() => return Err(PlaceError::IsADirectory(target)),
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(PlaceError::Io(target, e)),
    }
    Ok(FilePlace {
        root: dest.to_path_buf(),
        name: src_name.to_vec(),
    })
}

/// Whether `path` names a single file (not a directory). A missing path is
/// not single, so today's error path stays. With `contents_only` (trailing
/// slash) a non-directory is an error.
pub fn source_is_single(path: &Path, contents_only: bool) -> Result<bool, PlaceError> {
    match std::fs::metadata(path) {
        Ok(m) if m.is_dir() => Ok(false),
        Ok(_) if contents_only => Err(PlaceError::NotADirectory(path.to_path_buf())),
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(PlaceError::Io(path.to_path_buf(), e)),
    }
}
