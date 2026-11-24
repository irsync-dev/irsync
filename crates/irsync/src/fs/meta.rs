//! POSIX metadata: `lstat` into a `FileEntry`, and symlink-safe application.

use crate::fs::paths::RelPath;
use crate::platform::{self, Dir, Stat};
use irsync_proto::v1::{file_entry::Kind, FileEntry};
use std::fs::File;
use std::io;
use std::path::Path;

/// Which metadata `apply_metadata` should set.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApplyOpts {
    pub perms: bool,
    pub times: bool,
    pub owner: bool,
    pub group: bool,
}

/// The listing's kind of a platform kind: anything that is no other kind
/// counts as a regular file.
fn kind_of(kind: platform::Kind) -> Kind {
    match kind {
        platform::Kind::Dir => Kind::Directory,
        platform::Kind::Symlink => Kind::Symlink,
        platform::Kind::CharDev => Kind::CharDev,
        platform::Kind::BlockDev => Kind::BlockDev,
        platform::Kind::Fifo => Kind::Fifo,
        platform::Kind::Socket => Kind::Socket,
        platform::Kind::File | platform::Kind::Other => Kind::Regular,
    }
}

/// The platform kind a special entry is made as; [`platform::Kind::Other`]
/// (refused) for any kind that is not special.
fn special_kind(kind: Kind) -> platform::Kind {
    match kind {
        Kind::Fifo => platform::Kind::Fifo,
        Kind::CharDev => platform::Kind::CharDev,
        Kind::BlockDev => platform::Kind::BlockDev,
        Kind::Socket => platform::Kind::Socket,
        _ => platform::Kind::Other,
    }
}

/// `lstat` the entry at `root/rel` (never following a final symlink).
/// Fills everything except `whole_hash`.
pub fn lstat_entry(root: &Path, rel: &RelPath) -> io::Result<FileEntry> {
    lstat_entry_with_ctime(root, rel).map(|(e, _)| e)
}

/// [`lstat_entry_with_ctime`] for a path the other side named: no symlink
/// below `base` is followed on the way to it (see
/// [`open_parent_beneath`](crate::fs::safe::open_parent_beneath)). The
/// root itself is `lstat`ed by name.
pub fn lstat_beneath(base: &Path, rel: &RelPath) -> io::Result<(FileEntry, (i64, u32))> {
    if rel.as_bytes().is_empty() {
        return lstat_entry_with_ctime(base, rel);
    }
    let (dir, name) = crate::fs::safe::open_parent_beneath(base, rel)?;
    lstat_at(&dir, &name, rel)
}

/// `lstat` of `name` in the open directory `dir`, as the entry of `rel`.
pub fn lstat_at(dir: &Dir, name: &[u8], rel: &RelPath) -> io::Result<(FileEntry, (i64, u32))> {
    let st = dir.stat(name)?;
    let link_target = if st.kind == platform::Kind::Symlink {
        dir.read_link(name)?
    } else {
        Vec::new()
    };
    Ok(entry_of(&st, rel, link_target))
}

/// Like [`lstat_entry`], also returning the inode change time `(sec, nsec)`.
pub fn lstat_entry_with_ctime(root: &Path, rel: &RelPath) -> io::Result<(FileEntry, (i64, u32))> {
    let full = if rel.as_bytes().is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel.to_os())
    };
    let st = platform::lstat(&full)?;
    let link_target = if st.kind == platform::Kind::Symlink {
        platform::os_to_bytes(std::fs::read_link(&full)?.as_os_str())
    } else {
        Vec::new()
    };
    Ok(entry_of(&st, rel, link_target))
}

/// [`lstat_entry_with_ctime`] of the file open as `file` (never a
/// symlink), for the entry at `rel`.
pub fn fstat_entry_with_ctime(file: &File, rel: &RelPath) -> io::Result<(FileEntry, (i64, u32))> {
    Ok(stat_entry_with_ctime(&platform::fstat(file)?, rel))
}

/// [`fstat_entry_with_ctime`] from an `fstat` already taken.
pub fn stat_entry_with_ctime(st: &Stat, rel: &RelPath) -> (FileEntry, (i64, u32)) {
    entry_of(st, rel, Vec::new())
}

fn entry_of(st: &Stat, rel: &RelPath, link_target: Vec<u8>) -> (FileEntry, (i64, u32)) {
    // The whole `st_mode`: the kind's bits and the permission bits.
    let mode = platform::mode_of_kind(st.kind) | st.mode;
    let kind = kind_of(st.kind);
    let entry = FileEntry {
        path: rel.as_bytes().to_vec(),
        kind: kind as i32,
        size: st.size,
        mtime_sec: st.mtime.0,
        mtime_nsec: st.mtime.1,
        mode,
        uid: st.uid,
        gid: st.gid,
        link_target,
        rdev: st.rdev,
        dev: st.dev,
        inode: st.ino,
        nlink: st.nlink as u32,
        whole_hash: None,
        sparse: st.sparse,
        // Attached by the listing (see `fs::ids`), not here.
        user: String::new(),
        group: String::new(),
    };
    (entry, st.ctime)
}

/// The supplementary groups of this process, empty if they can't be read.
pub fn supplementary_groups() -> Vec<u32> {
    platform::supplementary_groups()
}

/// Whether this process runs as root, and the groups it is a member of
/// (effective and supplementary).
struct ProcessIds {
    root: bool,
    groups: Vec<u32>,
}

/// [`ProcessIds`], read once: irsync never changes its ids, and asking
/// the kernel for every file placed costs two syscalls each time.
fn process_ids() -> &'static ProcessIds {
    static IDS: std::sync::OnceLock<ProcessIds> = std::sync::OnceLock::new();
    IDS.get_or_init(|| {
        let groups = platform::groups();
        ProcessIds {
            root: platform::is_root(),
            groups,
        }
    })
}

/// Whether this process may chown to `gid`: as root, or as a member of it.
fn may_chgrp(gid: u32) -> bool {
    let ids = process_ids();
    ids.root || ids.groups.contains(&gid)
}

/// Where [`apply_at`] sets metadata: a name in an open directory, or a
/// path the caller trusts. Neither follows a symlink at the final name.
enum Target<'a> {
    At(&'a Dir, &'a [u8]),
    Path(&'a Path),
}

impl Target<'_> {
    fn set_owner(&self, uid: Option<u32>, gid: Option<u32>) -> io::Result<()> {
        match self {
            Target::At(dir, name) => dir.set_owner(name, uid, gid),
            Target::Path(path) => platform::set_owner_path(path, uid, gid),
        }
    }

    fn set_mode(&self, mode: u32) -> io::Result<()> {
        match self {
            Target::At(dir, name) => dir.set_mode(name, mode),
            Target::Path(path) => platform::set_mode_path(path, mode),
        }
    }

    fn set_mtime(&self, mtime: (i64, u32)) -> io::Result<()> {
        match self {
            Target::At(dir, name) => dir.set_mtime(name, mtime),
            Target::Path(path) => platform::set_mtime_path(path, mtime),
        }
    }
}

/// Apply the selected metadata of `e` to `path`, without following symlinks.
/// Ownership is applied first (chown clears setuid bits), then mode, then times.
///
/// The ids are the entry's as the receiver localized them (the sender's
/// names mapped to this side's ids unless `--numeric-ids`). Like rsync
/// without privilege, the owner is set only when running as root and the
/// group only as root or a member of it; otherwise they are silently left
/// alone.
pub fn apply_metadata(path: &Path, e: &FileEntry, opts: &ApplyOpts) -> io::Result<()> {
    apply_at(Target::Path(path), e, opts)
}

/// [`apply_metadata`] to `name` in the open directory `dir`. With a `dir`
/// from [`open_parent_beneath`](crate::fs::safe::open_parent_beneath)
/// nothing is reached through a symlink: not on the way, and not at the
/// name itself.
pub fn apply_metadata_at(
    dir: &Dir,
    name: &[u8],
    e: &FileEntry,
    opts: &ApplyOpts,
) -> io::Result<()> {
    apply_at(Target::At(dir, name), e, opts)
}

fn apply_at(at: Target<'_>, e: &FileEntry, opts: &ApplyOpts) -> io::Result<()> {
    let owner = opts.owner && process_ids().root;
    let group = opts.group && may_chgrp(e.gid);
    if owner || group {
        at.set_owner(owner.then_some(e.uid), group.then_some(e.gid))?;
    }
    if opts.perms && e.kind() != Kind::Symlink {
        at.set_mode(e.mode & 0o7777)?;
    }
    if opts.times {
        at.set_mtime((e.mtime_sec, e.mtime_nsec))?;
    }
    Ok(())
}

/// [`apply_metadata`] through an open regular file (never by path, so
/// nothing can be swapped in underneath).
pub fn apply_metadata_fd(file: &File, e: &FileEntry, opts: &ApplyOpts) -> io::Result<()> {
    apply_fd(file, e, opts, None)
}

/// [`apply_metadata_fd`] on a file whose owner and group are known to be
/// `now` (`(uid, gid)`): an owner or group it already has is not set again,
/// so a file that already has both needs no `fchown`.
pub fn apply_metadata_fd_owned(
    file: &File,
    e: &FileEntry,
    opts: &ApplyOpts,
    now: (u32, u32),
) -> io::Result<()> {
    apply_fd(file, e, opts, Some(now))
}

/// The mode a new regular file gets under this process's umask: what a
/// file created private is given when it is placed without `-p`.
pub fn default_file_mode() -> u32 {
    static MODE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    // Reading the umask changes it for a moment (see `platform::umask`).
    // `main` asks once before it starts any thread.
    *MODE.get_or_init(|| 0o666 & !platform::umask())
}

fn apply_fd(
    file: &File,
    e: &FileEntry,
    opts: &ApplyOpts,
    now: Option<(u32, u32)>,
) -> io::Result<()> {
    let owner = opts.owner && process_ids().root && now.is_none_or(|(uid, _)| uid != e.uid);
    let group = opts.group && now.is_none_or(|(_, gid)| gid != e.gid) && may_chgrp(e.gid);
    if owner || group {
        platform::set_owner_file(file, owner.then_some(e.uid), group.then_some(e.gid))?;
    }
    if opts.perms {
        platform::set_mode_file(file, e.mode & 0o7777)?;
    }
    if opts.times {
        platform::set_mtime_file(file, (e.mtime_sec, e.mtime_nsec))?;
    }
    Ok(())
}

/// Create a FIFO or device node at `path` from `e` (mode permission bits and rdev).
/// `mkfifo` and `mknod` are umask-subject, so callers follow with `apply_metadata` to set exact mode.
pub fn create_special(path: &Path, e: &FileEntry) -> io::Result<()> {
    platform::make_device_path(path, special_kind(e.kind()), e.mode & 0o7777, e.rdev)
}

/// [`create_special`] at `name` in the open directory `dir`.
pub fn create_special_at(dir: &Dir, name: &[u8], e: &FileEntry) -> io::Result<()> {
    dir.make_device(name, special_kind(e.kind()), e.mode & 0o7777, e.rdev)
}
