//! POSIX metadata: `lstat` into a `FileEntry`, and symlink-safe application.

use crate::fs::paths::RelPath;
use irsync_proto::v1::{file_entry::Kind, FileEntry};
use nix::fcntl::{AtFlags, AT_FDCWD};
use nix::sys::stat::{
    fchmodat, lstat, mknod, utimensat, FchmodatFlags, Mode, SFlag, UtimensatFlags,
};
use nix::sys::time::TimeSpec;
use nix::unistd::{fchownat, Gid, Uid};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Which metadata `apply_metadata` should set.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApplyOpts {
    pub perms: bool,
    pub times: bool,
    pub owner: bool,
    pub group: bool,
}

fn kind_of(mode: u32) -> Kind {
    let fmt = SFlag::from_bits_truncate(mode as _) & SFlag::S_IFMT;
    if fmt == SFlag::S_IFDIR {
        Kind::Directory
    } else if fmt == SFlag::S_IFLNK {
        Kind::Symlink
    } else if fmt == SFlag::S_IFCHR {
        Kind::CharDev
    } else if fmt == SFlag::S_IFBLK {
        Kind::BlockDev
    } else if fmt == SFlag::S_IFIFO {
        Kind::Fifo
    } else if fmt == SFlag::S_IFSOCK {
        Kind::Socket
    } else {
        Kind::Regular
    }
}

/// `lstat` the entry at `root/rel` (never following a final symlink).
/// Fills everything except `whole_hash`.
pub fn lstat_entry(root: &Path, rel: &RelPath) -> io::Result<FileEntry> {
    let full = root.join(rel.to_os());
    let st = lstat(&full)?;
    let mode = st.st_mode as u32;
    let kind = kind_of(mode);
    let link_target = if kind == Kind::Symlink {
        std::fs::read_link(&full)?.as_os_str().as_bytes().to_vec()
    } else {
        Vec::new()
    };
    // Linux and macOS both expose st_mtime / st_mtime_nsec through nix's libc FileStat.
    Ok(FileEntry {
        path: rel.0.clone(),
        kind: kind as i32,
        size: st.st_size as u64,
        mtime_sec: st.st_mtime as i64,
        mtime_nsec: st.st_mtime_nsec as u32,
        mode,
        uid: st.st_uid,
        gid: st.st_gid,
        link_target,
        rdev: st.st_rdev as u64,
        dev: st.st_dev as u64,
        inode: st.st_ino,
        nlink: st.st_nlink as u32,
        whole_hash: None,
        sparse: (st.st_blocks as i64) * 512 < st.st_size as i64,
    })
}

fn errno(e: nix::Error) -> io::Error {
    io::Error::from_raw_os_error(e as i32)
}

/// Apply the selected metadata of `e` to `path`, without following symlinks.
/// Ownership is applied first (chown clears setuid bits), then mode, then times.
pub fn apply_metadata(path: &Path, e: &FileEntry, opts: &ApplyOpts) -> io::Result<()> {
    if opts.owner || opts.group {
        let uid = opts.owner.then(|| Uid::from_raw(e.uid));
        let gid = opts.group.then(|| Gid::from_raw(e.gid));
        fchownat(AT_FDCWD, path, uid, gid, AtFlags::AT_SYMLINK_NOFOLLOW).map_err(errno)?;
    }
    if opts.perms && e.kind() != Kind::Symlink {
        let mode = Mode::from_bits_truncate((e.mode & 0o7777) as _);
        fchmodat(AT_FDCWD, path, mode, FchmodatFlags::FollowSymlink).map_err(errno)?;
    }
    if opts.times {
        let t = TimeSpec::new(e.mtime_sec as _, e.mtime_nsec as _);
        utimensat(AT_FDCWD, path, &t, &t, UtimensatFlags::NoFollowSymlink).map_err(errno)?;
    }
    Ok(())
}

/// Create a FIFO or device node at `path` from `e` (mode permission bits and rdev).
pub fn create_special(path: &Path, e: &FileEntry) -> io::Result<()> {
    let kind = match e.kind() {
        Kind::Fifo => SFlag::S_IFIFO,
        Kind::CharDev => SFlag::S_IFCHR,
        Kind::BlockDev => SFlag::S_IFBLK,
        Kind::Socket => SFlag::S_IFSOCK,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a special file kind",
            ))
        }
    };
    let perm = Mode::from_bits_truncate((e.mode & 0o7777) as _);
    mknod(path, kind, perm, e.rdev as _).map_err(errno)
}
