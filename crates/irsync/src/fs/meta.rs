//! POSIX metadata: `lstat` into a `FileEntry`, and symlink-safe application.

use crate::fs::beneath::{Missing, Root};
use crate::fs::paths::RelPath;
use irsync_proto::v1::{file_entry::Kind, FileEntry};
use nix::sys::stat::{fstat, FileStat, Mode, SFlag};
use nix::sys::time::TimeSpec;
use nix::unistd::{Gid, Uid};
use std::io;
use std::path::Path;

/// Which metadata [`apply_metadata_fd`] should set.
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

/// `lstat` the entry at `root/rel` (never following a final symlink),
/// opening `root` for this one call. Fills everything except `whole_hash`.
/// Opening a root costs a few system calls each time, so this is not for
/// hot paths: keep an open [`Root`] and use [`lstat_entry_with_ctime`].
pub fn lstat_entry(root: &Path, rel: &RelPath) -> io::Result<FileEntry> {
    let root = Root::open(root)?;
    lstat_entry_with_ctime(&root, rel).map(|(e, _)| e)
}

/// The entry at `rel` below the open `root` (never following a final
/// symlink) and its inode change time `(sec, nsec)`. The folders on the
/// way are opened one at a time and a symlink among them is refused. `rel`
/// may be the root itself.
pub fn lstat_entry_with_ctime(root: &Root, rel: &RelPath) -> io::Result<(FileEntry, (i64, u32))> {
    if rel.as_bytes().is_empty() {
        let dir = root.dir(rel, Missing::Refuse)?;
        return Ok(stat_entry_with_ctime(fstat(&dir).map_err(errno)?, rel));
    }
    let (dir, name) = root.parent(rel, Missing::Refuse)?;
    dir.entry(&name, rel)
}

/// [`lstat_entry_with_ctime`] of the file open as `file` (never a
/// symlink), for the entry at `rel`.
pub fn fstat_entry_with_ctime(
    file: &std::fs::File,
    rel: &RelPath,
) -> io::Result<(FileEntry, (i64, u32))> {
    Ok(stat_entry_with_ctime(fstat(file)?, rel))
}

/// [`fstat_entry_with_ctime`] from an `fstat` already taken.
pub fn stat_entry_with_ctime(st: FileStat, rel: &RelPath) -> (FileEntry, (i64, u32)) {
    entry_of(st, rel, Vec::new())
}

// The `stat` field types differ by platform; some casts are no-ops here.
#[allow(clippy::unnecessary_cast)]
pub(crate) fn entry_of(
    st: FileStat,
    rel: &RelPath,
    link_target: Vec<u8>,
) -> (FileEntry, (i64, u32)) {
    let mode = st.st_mode as u32;
    let kind = kind_of(mode);
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
        // Attached by the listing (see `fs::ids`), not here.
        user: String::new(),
        group: String::new(),
    };
    (entry, ctime)
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
    // SAFETY: with a size of 0, getgroups writes nothing and only returns
    // the number of groups, so a null buffer is allowed.
    let n = unsafe { getgroups(0, std::ptr::null_mut()) };
    if n <= 0 {
        return Vec::new();
    }
    // gid_t is u32 on Linux and macOS alike.
    let mut buf: Vec<gid_t> = vec![0; n as usize];
    // SAFETY: `buf` holds `n` writable gid_t slots and getgroups writes at
    // most the size it is given. If the groups grew since the first call,
    // it fails with EINVAL and writes nothing; the result is then empty.
    let n = unsafe { getgroups(n as c_int, buf.as_mut_ptr()) };
    buf.truncate(n.max(0) as usize);
    buf
}

/// Whether this process runs as root, and the groups it is a member of
/// (effective and supplementary).
pub(crate) struct ProcessIds {
    pub(crate) root: bool,
    groups: Vec<u32>,
}

/// [`ProcessIds`], read once: irsync never changes its ids, and asking
/// the kernel for every file placed costs two syscalls each time.
pub(crate) fn process_ids() -> &'static ProcessIds {
    static IDS: std::sync::OnceLock<ProcessIds> = std::sync::OnceLock::new();
    IDS.get_or_init(|| {
        let mut groups = supplementary_groups();
        groups.push(nix::unistd::getegid().as_raw());
        ProcessIds {
            root: nix::unistd::geteuid().is_root(),
            groups,
        }
    })
}

/// Whether this process may chown to `gid`: as root, or as a member of it.
pub(crate) fn may_chgrp(gid: u32) -> bool {
    let ids = process_ids();
    ids.root || ids.groups.contains(&gid)
}

/// Apply the selected metadata of `e` through an open descriptor (never by
/// path, so nothing can be swapped in underneath). Ownership is applied
/// first (chown clears setuid bits), then mode, then times. A symlink or
/// special file is done by name with
/// [`Dir::apply_node_metadata`](crate::fs::beneath::Dir::apply_node_metadata).
///
/// The ids are the entry's as the receiver localized them (the sender's
/// names mapped to this side's ids unless `--numeric-ids`). Like rsync
/// without privilege, the owner is set only when running as root and the
/// group only as root or a member of it; otherwise they are silently left
/// alone.
pub fn apply_metadata_fd(
    fd: impl std::os::fd::AsFd,
    e: &FileEntry,
    opts: &ApplyOpts,
) -> io::Result<()> {
    apply_fd(fd.as_fd(), e, opts, None)
}

/// [`apply_metadata_fd`] on a file whose owner and group are known to be
/// `now` (`(uid, gid)`): an owner or group it already has is not set again,
/// so a file that already has both needs no `fchown`.
pub fn apply_metadata_fd_owned(
    fd: impl std::os::fd::AsFd,
    e: &FileEntry,
    opts: &ApplyOpts,
    now: (u32, u32),
) -> io::Result<()> {
    apply_fd(fd.as_fd(), e, opts, Some(now))
}

pub(crate) fn apply_fd(
    fd: std::os::fd::BorrowedFd<'_>,
    e: &FileEntry,
    opts: &ApplyOpts,
    now: Option<(u32, u32)>,
) -> io::Result<()> {
    use nix::sys::stat::{fchmod, futimens};
    use nix::unistd::fchown;
    let owner = opts.owner && process_ids().root && now.is_none_or(|(uid, _)| uid != e.uid);
    let group = opts.group && now.is_none_or(|(_, gid)| gid != e.gid) && may_chgrp(e.gid);
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
