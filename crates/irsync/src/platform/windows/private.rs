//! Files and directories for their owner only: irsync's own state, keys,
//! captures and configuration. Windows has no modes: a file made here gets
//! the access its directory passes on, which inside the user's profile is
//! the user's alone. Who else can read a file is not checked: a known gap.

use std::fs::File;
use std::io;
use std::path::Path;

use crate::platform::{Create, Open};

/// [`std::fs::create_dir_all`]: each directory gets the access its parent
/// passes on.
pub fn create_private_dir_all(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Writes `contents` to `path`, replacing what is there ([`std::fs::write`]).
pub fn write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    std::fs::write(path, contents)
}

/// Opens `path` as `how` says (`how.mode` and `how.nonblock` mean nothing
/// here). A link (any reparse point) as the final name is opened itself
/// and refused ([`crate::platform::Cause::LinkRefused`]), never followed,
/// and never truncated: `how.truncate` cuts the file only once it is known
/// to be a regular file. Links before it are followed: the caller trusts
/// them.
pub fn open_private(path: &Path, how: Open) -> io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options
        .read(how.read)
        .write(how.write)
        .create(how.create == Create::IfMissing)
        .create_new(how.create == Create::New);
    let file = super::open_nofollow(options, path)?;
    if how.truncate {
        file.set_len(0)?;
    }
    Ok(file)
}

/// What is wrong with who can reach a file only its owner should reach:
/// never anything on Windows, where it is not checked.
pub fn open_file_problem(_meta: &std::fs::Metadata) -> Option<String> {
    None
}

/// Whether others can read or write the key file `meta` describes: not
/// checked on Windows.
pub fn key_file_open_to_others(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Does nothing on Windows: the saved file `to` keeps the access its
/// directory gives it, as the file it replaces had.
pub fn set_saved_permissions(_to: &Path, _path: &Path) -> io::Result<()> {
    Ok(())
}

/// `builder` as it is: a temporary file gets the access its directory
/// passes on.
pub fn set_temp_file_mode<'a, 'b, 'c>(
    builder: &'c mut tempfile::Builder<'a, 'b>,
    _mode: u32,
) -> &'c mut tempfile::Builder<'a, 'b> {
    builder
}
