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
    lstat_entry_with_ctime(root, rel).map(|(e, _)| e)
}

/// Like [`lstat_entry`], also returning the inode change time `(sec, nsec)`.
pub fn lstat_entry_with_ctime(root: &Path, rel: &RelPath) -> io::Result<(FileEntry, (i64, u32))> {
    let full = if rel.as_bytes().is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel.to_os())
    };
    let st = lstat(&full)?;
    let mode = st.st_mode as u32;
    let kind = kind_of(mode);
    let link_target = if kind == Kind::Symlink {
        std::fs::read_link(&full)?.as_os_str().as_bytes().to_vec()
    } else {
        Vec::new()
    };
    // Linux and macOS both expose st_mtime / st_mtime_nsec through nix's libc FileStat.
    let ctime = (st.st_ctime as i64, st.st_ctime_nsec as u32);
    let entry = FileEntry {
        path: rel.as_bytes().to_vec(),
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
        sparse: kind == Kind::Regular && (st.st_blocks as i64) * 512 < st.st_size as i64,
    };
    Ok((entry, ctime))
}

fn errno(e: nix::Error) -> io::Error {
    io::Error::from_raw_os_error(e as i32)
}

/// The supplementary groups of this process, empty if they can't be read.
/// Calls libc directly because nix leaves `getgroups` out on Apple targets.
pub fn supplementary_groups() -> Vec<u32> {
    #[cfg(not(target_os = "macos"))]
    use nix::libc::getgroups;
    use nix::libc::{c_int, gid_t};
    // The plain macOS getgroups stops at 16 groups; this variant returns
    // all of them.
    #[cfg(target_os = "macos")]
    extern "C" {
        #[link_name = "getgroups$DARWIN_EXTSN"]
        fn getgroups(size: c_int, list: *mut gid_t) -> c_int;
    }
    // SAFETY: a zero count with a null buffer only asks for the group count.
    let n = unsafe { getgroups(0, std::ptr::null_mut()) };
    if n <= 0 {
        return Vec::new();
    }
    // gid_t is u32 on Linux and macOS alike.
    let mut buf: Vec<gid_t> = vec![0; n as usize];
    // SAFETY: `buf` holds `n` writable gid_t slots.
    let n = unsafe { getgroups(n as c_int, buf.as_mut_ptr()) };
    buf.truncate(n.max(0) as usize);
    buf
}

/// Whether this process may chown to `gid`: as root, or as a member of it.
fn may_chgrp(gid: u32) -> bool {
    use nix::unistd::{getegid, geteuid};
    geteuid().is_root() || getegid().as_raw() == gid || supplementary_groups().contains(&gid)
}

/// Apply the selected metadata of `e` to `path`, without following symlinks.
/// Ownership is applied first (chown clears setuid bits), then mode, then times.
///
/// Ids are numeric (`--numeric-ids` is always on in v1). Like rsync without
/// privilege, the owner is set only when running as root and the group only
/// as root or a member of it; otherwise they are silently left alone.
pub fn apply_metadata(path: &Path, e: &FileEntry, opts: &ApplyOpts) -> io::Result<()> {
    let owner = opts.owner && nix::unistd::geteuid().is_root();
    let group = opts.group && may_chgrp(e.gid);
    if owner || group {
        let uid = owner.then(|| Uid::from_raw(e.uid));
        let gid = group.then(|| Gid::from_raw(e.gid));
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

/// [`apply_metadata`] through an open descriptor of a regular file (never
/// by path, so nothing can be swapped in underneath).
pub fn apply_metadata_fd(
    fd: impl std::os::fd::AsFd,
    e: &FileEntry,
    opts: &ApplyOpts,
) -> io::Result<()> {
    use nix::sys::stat::{fchmod, futimens};
    use nix::unistd::fchown;
    let fd = fd.as_fd();
    let owner = opts.owner && nix::unistd::geteuid().is_root();
    let group = opts.group && may_chgrp(e.gid);
    if owner || group {
        let uid = owner.then(|| Uid::from_raw(e.uid));
        let gid = group.then(|| Gid::from_raw(e.gid));
        fchown(fd, uid, gid).map_err(errno)?;
    }
    if opts.perms {
        fchmod(fd, Mode::from_bits_truncate((e.mode & 0o7777) as _)).map_err(errno)?;
    }
    if opts.times {
        let t = TimeSpec::new(e.mtime_sec as _, e.mtime_nsec as _);
        futimens(fd, &t, &t).map_err(errno)?;
    }
    Ok(())
}

/// Create a FIFO or device node at `path` from `e` (mode permission bits and rdev).
/// `mknod` is umask-subject, so callers follow with `apply_metadata` to set exact mode.
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
