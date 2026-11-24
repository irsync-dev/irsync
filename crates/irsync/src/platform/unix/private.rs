//! Files and directories for their owner only: irsync's own state, keys,
//! captures and configuration.

use std::fs::File;
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use nix::libc;

use crate::platform::{Create, Open};

/// Like [`std::fs::create_dir_all`], but every directory it makes is for
/// the owner only (0700). One that exists keeps its mode.
pub fn create_private_dir_all(dir: &Path) -> io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

/// Writes `contents` to `path`, replacing what is there. A file it makes is
/// for the owner only (0600); one that exists keeps its mode.
pub fn write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    use std::io::Write;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?
        .write_all(contents)
}

/// Opens `path` as `how` says. A file it makes is for the owner only
/// (0600, whatever `how.mode` says). A symlink as the final name is refused
/// ([`crate::platform::Cause::LinkRefused`]); links before it are followed:
/// the caller trusts them.
pub fn open_private(path: &Path, how: Open) -> io::Result<File> {
    let mut flags = libc::O_NOFOLLOW;
    if how.nonblock {
        flags |= libc::O_NONBLOCK;
    }
    std::fs::OpenOptions::new()
        .read(how.read)
        .write(how.write)
        .create(how.create == Create::IfMissing)
        .create_new(how.create == Create::New)
        .truncate(how.truncate)
        .mode(0o600)
        .custom_flags(flags)
        .open(path)
}

/// What is wrong with the owner or mode of a file that only its owner
/// should reach, as sshd asks of a key: `None` when it belongs to this
/// user (or root) and neither its group nor anyone else may read, write or
/// run it. Root may use a file of any owner: it manages the daemon
/// account's files. Only a regular file is judged: a pipe or a device
/// stores nothing.
pub fn open_file_problem(meta: &std::fs::Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    if !meta.is_file() {
        return None;
    }
    let me = nix::unistd::geteuid().as_raw();
    if me != 0 && meta.uid() != me && meta.uid() != 0 {
        return Some(format!("is owned by another user (uid {})", meta.uid()));
    }
    let mode = meta.mode() & 0o7777;
    (mode & 0o077 != 0).then(|| format!("has mode {mode:04o}, which is open to other users"))
}

/// Whether the mode of the key file `meta` describes lets its group or
/// everyone read or write it. Whoever reads an unencrypted key file is
/// that identity.
pub fn key_file_open_to_others(meta: &std::fs::Metadata) -> bool {
    meta.permissions().mode() & 0o066 != 0
}

/// Gives the file `to`, about to replace the file at `path`, the
/// permissions a saved file gets: those the file at `path` has, so a save
/// never opens up a file its owner closed; 0600 for a new file.
pub fn set_saved_permissions(to: &Path, path: &Path) -> io::Result<()> {
    let mode = std::fs::metadata(path).map_or(0o600, |m| m.permissions().mode() & 0o777);
    std::fs::set_permissions(to, std::fs::Permissions::from_mode(mode))
}

/// Makes the temporary files `builder` creates with the mode bits `mode`.
pub fn set_temp_file_mode<'a, 'b, 'c>(
    builder: &'c mut tempfile::Builder<'a, 'b>,
    mode: u32,
) -> &'c mut tempfile::Builder<'a, 'b> {
    builder.permissions(std::fs::Permissions::from_mode(mode))
}
