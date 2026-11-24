//! The Unix implementation: `nix`, `libc` and `std::os::unix`.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

use nix::fcntl::{openat, readlinkat, renameat, AtFlags, OFlag, AT_FDCWD};
use nix::libc;
use nix::sys::stat::{fchmodat, fstatat, mkdirat, utimensat, FchmodatFlags, FileStat, Mode};
use nix::sys::stat::{SFlag, UtimensatFlags};
use nix::sys::time::TimeSpec;
use nix::unistd::{fchownat, linkat, symlinkat, unlinkat, Gid, Uid, UnlinkatFlags};

use super::{Cause, Create, Kind, LinkKind, Open, Stat};

/// What `e` means, from its raw error number. An error without one is
/// [`Cause::Other`].
pub fn cause(e: &io::Error) -> Cause {
    let Some(n) = e.raw_os_error() else {
        return Cause::Other;
    };
    // Comparisons, not a `match`: some of these numbers are equal on some
    // systems (`ENOTSUP` and `EOPNOTSUPP` on Linux and FreeBSD), which a
    // `match` would report as an unreachable pattern.
    let is = |codes: &[i32]| codes.contains(&n);
    // An `O_NOFOLLOW` open of a symlink fails with `ELOOP`, except on
    // FreeBSD, which says `EMLINK`.
    if n == libc::ELOOP || (cfg!(target_os = "freebsd") && n == libc::EMLINK) {
        Cause::LinkRefused
    } else if n == libc::ENOENT {
        Cause::NotThere
    } else if n == libc::EEXIST {
        Cause::AlreadyThere
    } else if n == libc::ENOTDIR {
        Cause::NotADirectory
    } else if n == libc::EISDIR {
        Cause::IsADirectory
    } else if n == libc::ENOTEMPTY {
        Cause::NotEmpty
    } else if n == libc::EXDEV {
        Cause::CrossDevice
    } else if is(&[libc::EMFILE, libc::ENFILE]) {
        Cause::OutOfHandles
    } else if is(&[libc::EOPNOTSUPP, libc::ENOTSUP, libc::ENOSYS]) {
        Cause::NotSupported
    } else {
        Cause::Other
    }
}

/// Whether `e` is a momentary shortage worth waiting out that no [`Cause`]
/// names: no resources for a non-blocking call (`EAGAIN`), no memory, no
/// buffer space.
pub fn is_transient_extra(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::EAGAIN | libc::ENOMEM | libc::ENOBUFS)
    )
}

/// The stored bytes of a name.
pub fn os_to_bytes(name: &OsStr) -> Vec<u8> {
    name.as_bytes().to_vec()
}

/// The name that stored bytes stand for. Never fails on Unix: any bytes
/// are a name.
pub fn bytes_to_os(bytes: &[u8]) -> io::Result<OsString> {
    Ok(OsString::from_vec(bytes.to_vec()))
}

fn errno_of(e: nix::Error) -> io::Error {
    io::Error::from_raw_os_error(e as i32)
}

/// `name` if it is one component: not empty, `.` or `..`, and without `/`
/// or NUL. Checked before any call, so no operation of [`Dir`] can reach
/// past the directory it holds.
fn one_name(name: &[u8]) -> io::Result<&[u8]> {
    if name.is_empty() || name == b"." || name == b".." || name.contains(&b'/') || name.contains(&0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "not a single file name: {:?}",
                String::from_utf8_lossy(name)
            ),
        ));
    }
    Ok(name)
}

/// The kind of an `st_mode`.
fn kind_of_mode(mode: u32) -> Kind {
    let fmt = SFlag::from_bits_truncate(mode as _) & SFlag::S_IFMT;
    if fmt == SFlag::S_IFREG {
        Kind::File
    } else if fmt == SFlag::S_IFDIR {
        Kind::Dir
    } else if fmt == SFlag::S_IFLNK {
        Kind::Symlink
    } else if fmt == SFlag::S_IFIFO {
        Kind::Fifo
    } else if fmt == SFlag::S_IFCHR {
        Kind::CharDev
    } else if fmt == SFlag::S_IFBLK {
        Kind::BlockDev
    } else if fmt == SFlag::S_IFSOCK {
        Kind::Socket
    } else {
        Kind::Other
    }
}

/// The `st_mode` bits of a kind; none for [`Kind::Other`]. With
/// [`Stat::mode`] they make the whole `st_mode` a listing carries.
// `mode_t` is narrower on macOS and FreeBSD.
#[allow(clippy::unnecessary_cast)]
pub fn mode_of_kind(kind: Kind) -> u32 {
    let fmt = match kind {
        Kind::File => SFlag::S_IFREG,
        Kind::Dir => SFlag::S_IFDIR,
        Kind::Symlink => SFlag::S_IFLNK,
        Kind::Fifo => SFlag::S_IFIFO,
        Kind::CharDev => SFlag::S_IFCHR,
        Kind::BlockDev => SFlag::S_IFBLK,
        Kind::Socket => SFlag::S_IFSOCK,
        Kind::Other => SFlag::empty(),
    };
    fmt.bits() as u32
}

// The `stat` field types differ by platform; some casts are no-ops here.
#[allow(clippy::unnecessary_cast)]
fn stat_of(st: &FileStat) -> Stat {
    let kind = kind_of_mode(st.st_mode as u32);
    Stat {
        kind,
        mode: st.st_mode as u32 & 0o7777,
        uid: st.st_uid,
        gid: st.st_gid,
        size: st.st_size as u64,
        nlink: st.st_nlink as u64,
        ino: st.st_ino as u64,
        dev: st.st_dev as u64,
        rdev: st.st_rdev as u64,
        mtime: (st.st_mtime as i64, st.st_mtime_nsec as u32),
        ctime: (st.st_ctime as i64, st.st_ctime_nsec as u32),
        // As `fs::meta` has always computed it.
        sparse: kind == Kind::File && (st.st_blocks as i64) * 512 < st.st_size as i64,
    }
}

/// A directory held open. Every operation takes one name (stored bytes,
/// one component) and never follows a link.
#[derive(Debug)]
pub struct Dir(OwnedFd);

/// How a directory is opened: read-only, as a directory, not inherited.
fn dir_flags() -> OFlag {
    OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC
}

/// The flags of an [`Open`]; always `O_NOFOLLOW | O_CLOEXEC`.
fn open_flags(how: Open) -> OFlag {
    let mut flags = match (how.read, how.write) {
        (_, false) => OFlag::O_RDONLY,
        (false, true) => OFlag::O_WRONLY,
        (true, true) => OFlag::O_RDWR,
    };
    match how.create {
        Create::No => {}
        Create::IfMissing => flags |= OFlag::O_CREAT,
        Create::New => flags |= OFlag::O_CREAT | OFlag::O_EXCL,
    }
    if how.truncate {
        flags |= OFlag::O_TRUNC;
    }
    if how.nonblock {
        flags |= OFlag::O_NONBLOCK;
    }
    flags | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC
}

impl Dir {
    /// Opens `path` by name. Links in `path` are followed: the caller trusts it.
    pub fn open_root(path: &Path) -> io::Result<Dir> {
        openat(AT_FDCWD, path, dir_flags(), Mode::empty())
            .map(Dir)
            .map_err(errno_of)
    }

    /// Opens `path` by name, like [`Dir::open_root`], but a link as its
    /// final name is refused ([`Cause::LinkRefused`], or on Linux
    /// [`Cause::NotADirectory`]). Links before it are followed: the caller
    /// trusts them.
    pub fn open_root_nofollow(path: &Path) -> io::Result<Dir> {
        openat(
            AT_FDCWD,
            path,
            dir_flags() | OFlag::O_NOFOLLOW,
            Mode::empty(),
        )
        .map(Dir)
        .map_err(errno_of)
    }

    /// Opens the directory `name`. A link there is [`Cause::LinkRefused`],
    /// anything else that is not a directory [`Cause::NotADirectory`].
    // `st_mode` is narrower on macOS.
    #[allow(clippy::unnecessary_cast)]
    pub fn open_dir(&self, name: &[u8]) -> io::Result<Dir> {
        let name = one_name(name)?;
        match openat(
            &self.0,
            name,
            dir_flags() | OFlag::O_NOFOLLOW,
            Mode::empty(),
        ) {
            Ok(fd) => Ok(Dir(fd)),
            // Linux checks `O_DIRECTORY` before `O_NOFOLLOW`, so a link
            // there fails as not a directory. Only then is the name looked
            // at, to say which it was.
            Err(nix::Error::ENOTDIR) => {
                match fstatat(&self.0, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
                    Ok(st) if kind_of_mode(st.st_mode as u32) == Kind::Symlink => {
                        Err(io::Error::from_raw_os_error(libc::ELOOP))
                    }
                    _ => Err(errno_of(nix::Error::ENOTDIR)),
                }
            }
            Err(e) => Err(errno_of(e)),
        }
    }

    /// Makes the directory `name` with `mode` (less the umask).
    pub fn create_dir(&self, name: &[u8], mode: u32) -> io::Result<()> {
        let name = one_name(name)?;
        mkdirat(&self.0, name, Mode::from_bits_truncate(mode as _)).map_err(errno_of)
    }

    /// Opens the file `name` as `how` says.
    pub fn open_file(&self, name: &[u8], how: Open) -> io::Result<File> {
        let name = one_name(name)?;
        openat(
            &self.0,
            name,
            open_flags(how),
            Mode::from_bits_truncate(how.mode as _),
        )
        .map(File::from)
        .map_err(errno_of)
    }

    /// What is at `name`; a link is described, not followed.
    pub fn stat(&self, name: &[u8]) -> io::Result<Stat> {
        let name = one_name(name)?;
        fstatat(&self.0, name, AtFlags::AT_SYMLINK_NOFOLLOW)
            .map(|st| stat_of(&st))
            .map_err(errno_of)
    }

    /// The directory itself.
    pub fn stat_self(&self) -> io::Result<Stat> {
        nix::sys::stat::fstat(&self.0)
            .map(|st| stat_of(&st))
            .map_err(errno_of)
    }

    /// The target of the link `name`, as stored.
    pub fn read_link(&self, name: &[u8]) -> io::Result<Vec<u8>> {
        let name = one_name(name)?;
        readlinkat(&self.0, name)
            .map(OsString::into_vec)
            .map_err(errno_of)
    }

    /// The names in the directory, without `.` and `..`, in no order.
    pub fn list(&self) -> io::Result<Vec<Vec<u8>>> {
        let mut names = Vec::new();
        self.list_while(|n| {
            names.push(n.to_vec());
            true
        })?;
        Ok(names)
    }

    /// Calls `f` with each name in the directory (without `.` and `..`),
    /// in the order the directory gives them, and stops reading as soon as
    /// `f` returns false. A failed read ends the listing with its error.
    pub fn list_while(&self, mut f: impl FnMut(&[u8]) -> bool) -> io::Result<()> {
        // A second descriptor of the same open directory: reading moves
        // the position both share, and the iterator rewinds it when done.
        let mut dir = nix::dir::Dir::from_fd(self.0.try_clone()?).map_err(errno_of)?;
        for entry in dir.iter() {
            let entry = entry.map_err(errno_of)?;
            let n = entry.file_name().to_bytes();
            if n == b"." || n == b".." {
                continue;
            }
            if !f(n) {
                break;
            }
        }
        Ok(())
    }

    /// Removes `name`, which is not a directory (a link is removed, not
    /// followed).
    pub fn remove_file(&self, name: &[u8]) -> io::Result<()> {
        let name = one_name(name)?;
        unlinkat(&self.0, name, UnlinkatFlags::NoRemoveDir).map_err(errno_of)
    }

    /// Removes the empty directory `name`.
    pub fn remove_dir(&self, name: &[u8]) -> io::Result<()> {
        let name = one_name(name)?;
        unlinkat(&self.0, name, UnlinkatFlags::RemoveDir).map_err(errno_of)
    }

    /// Replaces what is at `to_name`, as `rename` does.
    pub fn rename(&self, name: &[u8], to: &Dir, to_name: &[u8]) -> io::Result<()> {
        let (name, to_name) = (one_name(name)?, one_name(to_name)?);
        renameat(&self.0, name, &to.0, to_name).map_err(errno_of)
    }

    /// Makes `name` a second name of the file `from_name` in `from`. A
    /// link at `from_name` is linked itself, never followed.
    pub fn hard_link(&self, name: &[u8], from: &Dir, from_name: &[u8]) -> io::Result<()> {
        let (name, from_name) = (one_name(name)?, one_name(from_name)?);
        linkat(&from.0, from_name, &self.0, name, AtFlags::empty()).map_err(errno_of)
    }

    /// Makes `name` a symlink to `target`. Unix links have no kind.
    pub fn symlink(&self, name: &[u8], target: &[u8], _kind: LinkKind) -> io::Result<()> {
        let name = one_name(name)?;
        symlinkat(target, &self.0, name).map_err(errno_of)
    }

    /// Sets the modification time of `name`, a link's own, never its
    /// target's; the access time is set to the same.
    pub fn set_mtime(&self, name: &[u8], mtime: (i64, u32)) -> io::Result<()> {
        let name = one_name(name)?;
        set_mtime_at(self.0.as_fd(), as_path(name), mtime)
    }

    /// `chmod` of `name` that never changes the target of a symlink
    /// swapped in at `name`. Where the system cannot do that in one call
    /// (an older Linux without `/proc`), the name is checked first, then
    /// changed.
    pub fn set_mode(&self, name: &[u8], mode: u32) -> io::Result<()> {
        let name = one_name(name)?;
        set_mode_at(self.0.as_fd(), as_path(name), mode)
    }

    /// Sets the owner and the group of `name` (a link's own); `None`
    /// leaves one as it is.
    pub fn set_owner(&self, name: &[u8], uid: Option<u32>, gid: Option<u32>) -> io::Result<()> {
        let name = one_name(name)?;
        set_owner_at(self.0.as_fd(), as_path(name), uid, gid)
    }

    /// Makes the FIFO `name` with `mode` (less the umask).
    pub fn make_fifo(&self, name: &[u8], mode: u32) -> io::Result<()> {
        let name = one_name(name)?;
        make_fifo_at(self.0.as_fd(), as_path(name), mode)
    }

    /// Makes the device node (or socket, or FIFO) `name` of `kind`, with
    /// `mode` (less the umask) and device number `rdev`.
    pub fn make_device(&self, name: &[u8], kind: Kind, mode: u32, rdev: u64) -> io::Result<()> {
        if kind == Kind::Fifo {
            // mkfifo, not mknod: FreeBSD's mknod makes device nodes only.
            return self.make_fifo(name, mode);
        }
        let fmt = device_format(kind)?;
        let name = one_name(name)?;
        make_node_at(self.0.as_fd(), as_path(name), fmt, mode, rdev)
    }
}

/// A name as the path the `*at` calls take.
fn as_path(name: &[u8]) -> &Path {
    Path::new(OsStr::from_bytes(name))
}

/// [`Dir::set_mtime`] of `name` in `dir`, or of a path (`AT_FDCWD`).
fn set_mtime_at(dir: BorrowedFd<'_>, name: &Path, mtime: (i64, u32)) -> io::Result<()> {
    let t = TimeSpec::new(mtime.0 as _, mtime.1 as _);
    utimensat(dir, name, &t, &t, UtimensatFlags::NoFollowSymlink).map_err(errno_of)
}

/// [`Dir::set_mode`] of `name` in `dir`, or of a path (`AT_FDCWD`).
// `st_mode` is narrower on macOS.
#[allow(clippy::unnecessary_cast)]
fn set_mode_at(dir: BorrowedFd<'_>, name: &Path, mode: u32) -> io::Result<()> {
    let mode = Mode::from_bits_truncate(mode as _);
    match fchmodat(dir, name, mode, FchmodatFlags::NoFollowSymlink) {
        Ok(()) => Ok(()),
        Err(nix::Error::EOPNOTSUPP | nix::Error::ENOSYS | nix::Error::EINVAL) => {
            let st = fstatat(dir, name, AtFlags::AT_SYMLINK_NOFOLLOW).map_err(errno_of)?;
            if kind_of_mode(st.st_mode as u32) == Kind::Symlink {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("refusing to follow symlink {}", name.display()),
                ));
            }
            fchmodat(dir, name, mode, FchmodatFlags::FollowSymlink).map_err(errno_of)
        }
        Err(e) => Err(errno_of(e)),
    }
}

/// [`Dir::set_owner`] of `name` in `dir`, or of a path (`AT_FDCWD`).
fn set_owner_at(
    dir: BorrowedFd<'_>,
    name: &Path,
    uid: Option<u32>,
    gid: Option<u32>,
) -> io::Result<()> {
    fchownat(
        dir,
        name,
        uid.map(Uid::from_raw),
        gid.map(Gid::from_raw),
        AtFlags::AT_SYMLINK_NOFOLLOW,
    )
    .map_err(errno_of)
}

/// The `st_mode` kind bits [`make_node_at`] gives `mknod` for `kind`: a
/// device or a socket, nothing else.
fn device_format(kind: Kind) -> io::Result<SFlag> {
    match kind {
        Kind::CharDev => Ok(SFlag::S_IFCHR),
        Kind::BlockDev => Ok(SFlag::S_IFBLK),
        Kind::Socket => Ok(SFlag::S_IFSOCK),
        _ => Err(not_special()),
    }
}

/// [`Dir::make_fifo`] of `name` in `dir`, or of a path (`AT_FDCWD`).
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
fn make_fifo_at(dir: BorrowedFd<'_>, name: &Path, mode: u32) -> io::Result<()> {
    nix::unistd::mkfifoat(dir, name, Mode::from_bits_truncate(mode as _)).map_err(errno_of)
}

/// [`Dir::make_fifo`] of `name` in `dir`, or of a path (`AT_FDCWD`). The
/// library irsync uses for system calls leaves `mkfifoat` out on macOS,
/// so it is called directly.
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn make_fifo_at(dir: BorrowedFd<'_>, name: &Path, mode: u32) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let c = name_cstring(name)?;
    // SAFETY: `c` is a valid NUL-terminated string and `dir` an open
    // descriptor (or `AT_FDCWD`) for the length of the call.
    let r = unsafe { libc::mkfifoat(dir.as_raw_fd(), c.as_ptr(), (mode & 0o7777) as libc::mode_t) };
    if r == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `mknod` of `name` in `dir` (or of a path, `AT_FDCWD`) as `fmt`, with
/// `mode` (less the umask) and device number `rdev`.
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
fn make_node_at(
    dir: BorrowedFd<'_>,
    name: &Path,
    fmt: SFlag,
    mode: u32,
    rdev: u64,
) -> io::Result<()> {
    let perm = Mode::from_bits_truncate(mode as _);
    nix::sys::stat::mknodat(dir, name, fmt, perm, rdev as _).map_err(errno_of)
}

/// `mknod` of `name` in `dir` (or of a path, `AT_FDCWD`) as `fmt`, with
/// `mode` (less the umask) and device number `rdev`. The library irsync
/// uses for system calls leaves `mknodat` out on macOS, so it is called
/// directly.
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn make_node_at(
    dir: BorrowedFd<'_>,
    name: &Path,
    fmt: SFlag,
    mode: u32,
    rdev: u64,
) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let c = name_cstring(name)?;
    let perm = (mode & 0o7777) as libc::mode_t;
    // SAFETY: `c` is a valid NUL-terminated string and `dir` an open
    // descriptor (or `AT_FDCWD`) for the length of the call.
    let r = unsafe { libc::mknodat(dir.as_raw_fd(), c.as_ptr(), fmt.bits() | perm, rdev as _) };
    if r == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn not_special() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "not a special file kind")
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn name_cstring(name: &Path) -> io::Result<std::ffi::CString> {
    std::ffi::CString::new(name.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in a file name"))
}

/// What the open `file` is.
pub fn fstat(file: &File) -> io::Result<Stat> {
    nix::sys::stat::fstat(file)
        .map(|st| stat_of(&st))
        .map_err(errno_of)
}

/// Reads at `offset` without moving the file's position.
pub fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, offset)
}

/// Writes at `offset` without moving the file's position.
pub fn write_at(file: &File, buf: &[u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::write_at(file, buf, offset)
}

/// Fills `buf` from `offset` without moving the file's position; a file
/// that ends first is an `UnexpectedEof` error.
pub fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

/// Writes all of `buf` at `offset` without moving the file's position.
pub fn write_all_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, buf, offset)
}

/// Which file `m` describes: its device and inode numbers.
pub fn file_id(m: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (m.dev(), m.ino())
}

/// The modification time of the file `m` describes, `(seconds,
/// nanoseconds)`.
pub fn mtime_of(m: &std::fs::Metadata) -> (i64, i64) {
    use std::os::unix::fs::MetadataExt;
    (m.mtime(), m.mtime_nsec())
}

/// The inode change time of the file `m` describes, `(seconds,
/// nanoseconds)`.
pub fn ctime_of(m: &std::fs::Metadata) -> (i64, i64) {
    use std::os::unix::fs::MetadataExt;
    (m.ctime(), m.ctime_nsec())
}

/// Sets the permission bits of `path` as the standard library does
/// (`chmod`): a link at `path` is FOLLOWED, unlike [`set_mode_path`].
pub fn set_mode_path_following(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// `secs` since the epoch as local time, `YYYY-MM-DD HH:MM:SS`; `@secs`
/// when the system cannot convert it.
pub fn local_time(secs: i64) -> String {
    // `time_t`, inferred: naming the alias is deprecated on musl.
    let t = secs as _;
    // SAFETY: `tm` is a plain C struct of integers (and, on some systems, a
    // pointer to the zone name, which may be null), so all zero bytes is a
    // valid value.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers come from references to locals that outlive
    // the call. localtime_r reads `t` and returns its result only through
    // `tm`, not in a static buffer as localtime does. It also reads TZ and
    // updates libc's own time-zone state, which libc guards with a lock;
    // nothing in irsync changes the environment while it runs.
    let ok = unsafe { !libc::localtime_r(&t, &mut tm).is_null() };
    if !ok {
        return format!("@{secs}");
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// The owner and group (`(uid, gid)`) of the file `m` describes.
pub fn owner_of(m: &std::fs::Metadata) -> (u32, u32) {
    use std::os::unix::fs::MetadataExt;
    (m.uid(), m.gid())
}

/// Adds owner read and write to the permissions of the open `file`, as
/// its metadata gives them.
pub fn make_owner_writable(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = file.metadata()?.permissions().mode();
    file.set_permissions(std::fs::Permissions::from_mode(mode | 0o600))
}

/// Sets the permission bits of the open `file` as the standard library
/// does (`fchmod`).
pub fn set_permissions_file(file: &File, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(std::fs::Permissions::from_mode(mode))
}

/// The error a write gets on a full disk (`ENOSPC`), for a test that
/// makes one fail as if it had.
pub fn no_space_error() -> io::Error {
    io::Error::from_raw_os_error(libc::ENOSPC)
}

/// Opens the file at `path` for reading. A link as the final name is
/// refused ([`Cause::LinkRefused`]); links before it are followed: the
/// caller trusts them. A FIFO there cannot block the open.
pub fn open_read_nofollow(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
}

/// TEMPORARY bridge for `fs/clone.rs`, the one caller not yet converted
/// (removed by Task 4): a `nix` error as an `io::Error`.
#[doc(hidden)]
pub fn errno(e: nix::Error) -> io::Error {
    errno_of(e)
}

/// TEMPORARY bridge (see [`errno`]): whether a `nix` error is
/// [`Cause::LinkRefused`].
#[doc(hidden)]
pub fn is_symlink_errno(e: nix::Error) -> bool {
    cause(&errno_of(e)) == Cause::LinkRefused
}

/// What is at `path`; a final link is described, not followed. Links
/// before it are followed: the caller trusts them.
pub fn lstat(path: &Path) -> io::Result<Stat> {
    nix::sys::stat::lstat(path)
        .map(|st| stat_of(&st))
        .map_err(errno_of)
}

/// [`Dir::set_mtime`] of a path the caller trusts; a final link's own
/// time is set.
pub fn set_mtime_path(path: &Path, mtime: (i64, u32)) -> io::Result<()> {
    set_mtime_at(AT_FDCWD, path, mtime)
}

/// [`Dir::set_mode`] of a path the caller trusts; a final link is never
/// followed.
pub fn set_mode_path(path: &Path, mode: u32) -> io::Result<()> {
    set_mode_at(AT_FDCWD, path, mode)
}

/// [`Dir::set_owner`] of a path the caller trusts; a final link's own
/// owner is set.
pub fn set_owner_path(path: &Path, uid: Option<u32>, gid: Option<u32>) -> io::Result<()> {
    set_owner_at(AT_FDCWD, path, uid, gid)
}

/// [`Dir::make_device`] at a path the caller trusts.
pub fn make_device_path(path: &Path, kind: Kind, mode: u32, rdev: u64) -> io::Result<()> {
    if kind == Kind::Fifo {
        // mkfifo, not mknod: FreeBSD's mknod makes device nodes only.
        return make_fifo_at(AT_FDCWD, path, mode);
    }
    let fmt = device_format(kind)?;
    make_node_at(AT_FDCWD, path, fmt, mode, rdev)
}

/// Sets the modification time of the open `file`; the access time is set
/// to the same.
pub fn set_mtime_file(file: &File, mtime: (i64, u32)) -> io::Result<()> {
    let t = TimeSpec::new(mtime.0 as _, mtime.1 as _);
    nix::sys::stat::futimens(file, &t, &t).map_err(errno_of)
}

/// Sets the permission bits of the open `file`.
pub fn set_mode_file(file: &File, mode: u32) -> io::Result<()> {
    nix::sys::stat::fchmod(file, Mode::from_bits_truncate(mode as _)).map_err(errno_of)
}

/// Sets the owner and the group of the open `file`; `None` leaves one as
/// it is.
pub fn set_owner_file(file: &File, uid: Option<u32>, gid: Option<u32>) -> io::Result<()> {
    nix::unistd::fchown(file, uid.map(Uid::from_raw), gid.map(Gid::from_raw)).map_err(errno_of)
}

/// The process's umask. It can only be read by setting it: for the moment
/// between the two calls, files other threads create are private, never
/// more open.
pub fn umask() -> u32 {
    let old = nix::sys::stat::umask(Mode::from_bits_truncate(0o077));
    nix::sys::stat::umask(old);
    old.bits() as u32
}

/// The effective user id of this process.
pub fn euid() -> u32 {
    nix::unistd::geteuid().as_raw()
}

/// The effective group id of this process.
pub fn egid() -> u32 {
    nix::unistd::getegid().as_raw()
}

/// Whether this process runs as root.
pub fn is_root() -> bool {
    nix::unistd::geteuid().is_root()
}

/// The supplementary groups of this process, empty if they can't be read.
/// Calls libc directly because nix leaves `getgroups` out on Apple targets.
pub fn supplementary_groups() -> Vec<u32> {
    #[cfg(not(target_os = "macos"))]
    use libc::getgroups;
    use libc::{c_int, gid_t};
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

/// The groups this process is a member of: the supplementary ones, then
/// the effective one.
pub fn groups() -> Vec<u32> {
    let mut groups = supplementary_groups();
    groups.push(egid());
    groups
}

/// The name of the user `uid`, if it has one.
pub fn user_name(uid: u32) -> Option<String> {
    nix::unistd::User::from_uid(Uid::from_raw(uid))
        .ok()
        .flatten()
        .map(|u| u.name)
}

/// The name of the group `gid`, if it has one.
pub fn group_name(gid: u32) -> Option<String> {
    nix::unistd::Group::from_gid(Gid::from_raw(gid))
        .ok()
        .flatten()
        .map(|g| g.name)
}

/// The id of the user called `name`, if there is one.
pub fn user_id(name: &str) -> Option<u32> {
    nix::unistd::User::from_name(name)
        .ok()
        .flatten()
        .map(|u| u.uid.as_raw())
}

/// The id of the group called `name`, if there is one.
pub fn group_id(name: &str) -> Option<u32> {
    nix::unistd::Group::from_name(name)
        .ok()
        .flatten()
        .map(|g| g.gid.as_raw())
}

/// Helpers for tests, in this crate and in `tests/`: what a test needs
/// from the system goes through here.
#[doc(hidden)]
pub mod testutil {
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread::JoinHandle;
    use std::time::Duration;

    /// Makes `link` a symlink to `target`.
    pub fn symlink(target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }
    /// How long the swapping thread leaves each state (the link, then the
    /// directory) in place: long enough that an operation running in the
    /// other thread meets it, short enough for many swaps a second.
    const HOLD: Duration = Duration::from_micros(100);

    /// A thread that swaps `dir` for a link to `outside` and back until the
    /// returned [`Swap`] is dropped. Dropping it waits for the thread, which
    /// always stops with `dir` back in its place.
    pub fn swap_for_link(dir: &Path, outside: &Path) -> Swap {
        let mut aside = dir.as_os_str().to_owned();
        aside.push(".swapped-aside");
        let (dir, aside, outside) = (
            dir.to_path_buf(),
            PathBuf::from(aside),
            outside.to_path_buf(),
        );
        let stop = Arc::new(AtomicBool::new(false));
        let swaps = Arc::new(AtomicUsize::new(0));
        let thread = std::thread::spawn({
            let (stop, swaps) = (stop.clone(), swaps.clone());
            move || {
                // `stop` is only looked at here, with `dir` in its place.
                while !stop.load(Ordering::Relaxed) {
                    // Fails while the other thread holds the tree in some
                    // way a rename cannot pass; then try again.
                    if std::fs::rename(&dir, &aside).is_err() {
                        std::thread::yield_now();
                        continue;
                    }
                    // Fails when the other thread made a directory at the
                    // free name meanwhile: no swap this time.
                    if symlink(&outside, &dir).is_ok() {
                        swaps.fetch_add(1, Ordering::Relaxed);
                        std::thread::sleep(HOLD);
                    }
                    put_back(&dir, &aside);
                    std::thread::sleep(HOLD);
                }
            }
        });
        Swap {
            stop,
            swaps,
            thread: Some(thread),
        }
    }

    /// Puts `aside` back at `dir`, removing what is there: the link, or a
    /// directory the other thread made at the name while it was free.
    fn put_back(dir: &Path, aside: &Path) {
        loop {
            match std::fs::symlink_metadata(dir) {
                Ok(m) if m.is_dir() => {
                    let _ = std::fs::remove_dir_all(dir);
                }
                Ok(_) => {
                    let _ = std::fs::remove_file(dir);
                }
                Err(_) => {}
            }
            if std::fs::rename(aside, dir).is_ok() {
                return;
            }
            std::thread::yield_now();
        }
    }

    /// The running swap of [`swap_for_link`].
    #[must_use = "the swapping stops when this is dropped"]
    pub struct Swap {
        stop: Arc<AtomicBool>,
        swaps: Arc<AtomicUsize>,
        thread: Option<JoinHandle<()>>,
    }

    impl Swap {
        /// How many times the link has been put in place so far.
        pub fn swaps(&self) -> usize {
            self.swaps.load(Ordering::Relaxed)
        }
    }

    impl Drop for Swap {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(t) = self.thread.take() {
                if t.join().is_err() && !std::thread::panicking() {
                    panic!("the thread that swaps a directory for a link panicked");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn causes_from_error_numbers() {
        let freebsd = cfg!(target_os = "freebsd");
        let cases = [
            (libc::ELOOP, Cause::LinkRefused),
            (
                libc::EMLINK,
                if freebsd {
                    Cause::LinkRefused
                } else {
                    Cause::Other
                },
            ),
            (libc::ENOENT, Cause::NotThere),
            (libc::EEXIST, Cause::AlreadyThere),
            (libc::ENOTDIR, Cause::NotADirectory),
            (libc::EISDIR, Cause::IsADirectory),
            (libc::ENOTEMPTY, Cause::NotEmpty),
            (libc::EXDEV, Cause::CrossDevice),
            (libc::EMFILE, Cause::OutOfHandles),
            (libc::ENFILE, Cause::OutOfHandles),
            (libc::EOPNOTSUPP, Cause::NotSupported),
            (libc::ENOTSUP, Cause::NotSupported),
            (libc::ENOSYS, Cause::NotSupported),
            (libc::EACCES, Cause::Other),
            (libc::EAGAIN, Cause::Other),
            (libc::ENOSPC, Cause::Other),
        ];
        for (n, want) in cases {
            assert_eq!(cause(&io::Error::from_raw_os_error(n)), want, "{n}");
        }
        assert_eq!(cause(&io::Error::other("x")), Cause::Other);
    }

    #[test]
    fn extra_transient_errors_are_the_shortages_no_cause_names() {
        for n in [libc::EAGAIN, libc::ENOMEM, libc::ENOBUFS] {
            assert!(is_transient_extra(&io::Error::from_raw_os_error(n)), "{n}");
        }
        for n in [libc::EMFILE, libc::ENFILE, libc::EACCES, libc::ENOSPC] {
            assert!(!is_transient_extra(&io::Error::from_raw_os_error(n)), "{n}");
        }
        assert!(!is_transient_extra(&io::Error::other("x")));
    }

    #[test]
    fn names_and_bytes_round_trip() {
        for b in [&b"plain"[..], b"caf\xe9", b"\xff\xfe", b""] {
            let os = bytes_to_os(b).unwrap();
            assert_eq!(os_to_bytes(&os), b);
        }
    }

    const READ: Open = Open {
        read: true,
        write: false,
        create: Create::No,
        truncate: false,
        nonblock: false,
        mode: 0,
    };

    fn sorted(mut names: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
        names.sort();
        names
    }

    #[test]
    fn every_operation_takes_one_name_and_follows_no_link() {
        let (t, outside) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let root = t.path().join("d");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("f"), b"f").unwrap();
        std::fs::write(root.join("sub/g"), b"g").unwrap();
        std::fs::create_dir(outside.path().join("dir")).unwrap();
        std::fs::write(outside.path().join("f"), b"outside").unwrap();
        testutil::symlink(&outside.path().join("dir"), &root.join("link_to_dir")).unwrap();
        testutil::symlink(&outside.path().join("f"), &root.join("link_to_file")).unwrap();

        let d = Dir::open_root(&root).unwrap();
        assert_eq!(
            sorted(d.list().unwrap()),
            [&b"f"[..], b"link_to_dir", b"link_to_file", b"sub"]
        );
        let e = d.open_dir(b"link_to_dir").unwrap_err();
        assert_eq!(cause(&e), Cause::LinkRefused, "{e}");
        let e = d.open_file(b"link_to_file", READ).unwrap_err();
        assert_eq!(cause(&e), Cause::LinkRefused, "{e}");
        assert_eq!(d.stat(b"link_to_file").unwrap().kind, Kind::Symlink);
        assert_eq!(d.stat(b"f").unwrap().kind, Kind::File);
        assert_eq!(d.stat(b"sub").unwrap().kind, Kind::Dir);
        assert_eq!(d.stat_self().unwrap().kind, Kind::Dir);
        assert_eq!(
            d.read_link(b"link_to_file").unwrap(),
            outside.path().join("f").as_os_str().as_bytes()
        );

        // The link goes, what it leads to stays.
        d.remove_file(b"link_to_file").unwrap();
        assert!(std::fs::symlink_metadata(root.join("link_to_file")).is_err());
        assert_eq!(std::fs::read(outside.path().join("f")).unwrap(), b"outside");

        // A rename replaces what is at the new name.
        let sub = d.open_dir(b"sub").unwrap();
        d.rename(b"f", &sub, b"g").unwrap();
        assert!(!root.join("f").exists());
        assert_eq!(std::fs::read(root.join("sub/g")).unwrap(), b"f");
        assert_eq!(sorted(sub.list().unwrap()), [&b"g"[..]]);

        // A second name of the same file.
        d.hard_link(b"h", &sub, b"g").unwrap();
        let (h, g) = (d.stat(b"h").unwrap(), sub.stat(b"g").unwrap());
        assert_eq!((h.nlink, g.nlink), (2, 2));
        assert_eq!((h.ino, h.dev), (g.ino, g.dev));

        // A link's own time changes, not its target's.
        let before = std::fs::metadata(outside.path().join("dir"))
            .unwrap()
            .modified()
            .unwrap();
        d.set_mtime(b"link_to_dir", (1_000_000_000, 5)).unwrap();
        assert_eq!(d.stat(b"link_to_dir").unwrap().mtime.0, 1_000_000_000);
        let after = std::fs::metadata(outside.path().join("dir"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after);

        // The rest of the operations, on names inside.
        d.create_dir(b"new", 0o755).unwrap();
        d.open_dir(b"new").unwrap();
        d.remove_dir(b"new").unwrap();
        d.symlink(b"l", b"sub/g", LinkKind::File).unwrap();
        assert_eq!(d.read_link(b"l").unwrap(), b"sub/g");
        d.set_mode(b"h", 0o640).unwrap();
        assert_eq!(d.stat(b"h").unwrap().mode, 0o640);
        let st = d.stat(b"h").unwrap();
        d.set_owner(b"h", Some(st.uid), Some(st.gid)).unwrap();
        d.make_fifo(b"p", 0o600).unwrap();
        assert_eq!(d.stat(b"p").unwrap().kind, Kind::Fifo);
        let write = Open {
            read: true,
            write: true,
            create: Create::New,
            truncate: false,
            nonblock: false,
            mode: 0o600,
        };
        let file = d.open_file(b"w", write).unwrap();
        assert_eq!(write_at(&file, b"abc", 2).unwrap(), 3);
        let mut buf = [0u8; 5];
        assert_eq!(read_at(&file, &mut buf, 0).unwrap(), 5);
        assert_eq!(&buf, b"\0\0abc");
        let st = fstat(&file).unwrap();
        assert_eq!((st.kind, st.size), (Kind::File, 5));
        assert_eq!(st, d.stat(b"w").unwrap());
    }

    #[test]
    fn list_while_stops_when_told() {
        let t = tempfile::tempdir().unwrap();
        for n in ["a", "b", "c"] {
            std::fs::write(t.path().join(n), n).unwrap();
        }
        let d = Dir::open_root(t.path()).unwrap();
        // Told to stop at the first name: called once.
        let mut calls = 0;
        d.list_while(|_| {
            calls += 1;
            false
        })
        .unwrap();
        assert_eq!(calls, 1);
        // Never told to stop: every name, without `.` and `..`.
        let mut names = Vec::new();
        d.list_while(|n| {
            names.push(n.to_vec());
            true
        })
        .unwrap();
        assert_eq!(sorted(names), [&b"a"[..], b"b", b"c"]);
        // Stopped after the second name: called twice, and a later listing
        // starts from the first name again.
        let mut calls = 0;
        d.list_while(|_| {
            calls += 1;
            calls < 2
        })
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(sorted(d.list().unwrap()), [&b"a"[..], b"b", b"c"]);
    }

    #[test]
    fn error_meanings() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("sub")).unwrap();
        std::fs::write(t.path().join("sub/g"), b"g").unwrap();
        std::fs::write(t.path().join("f"), b"f").unwrap();
        let d = Dir::open_root(t.path()).unwrap();
        let cause_of = |r: io::Result<()>| cause(&r.unwrap_err());
        assert_eq!(cause_of(d.open_dir(b"missing").map(drop)), Cause::NotThere);
        assert_eq!(cause_of(d.open_dir(b"f").map(drop)), Cause::NotADirectory);
        assert_eq!(cause_of(d.create_dir(b"sub", 0o755)), Cause::AlreadyThere);
        let new = Open {
            write: true,
            create: Create::New,
            mode: 0o600,
            ..READ
        };
        assert_eq!(
            cause_of(d.open_file(b"f", new).map(drop)),
            Cause::AlreadyThere
        );
        assert_eq!(cause_of(d.remove_dir(b"sub")), Cause::NotEmpty);
        let write = Open {
            read: false,
            write: true,
            ..READ
        };
        assert_eq!(
            cause_of(d.open_file(b"sub", write).map(drop)),
            Cause::IsADirectory
        );
    }

    #[test]
    fn a_name_with_a_separator_or_nul_is_refused() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("a")).unwrap();
        std::fs::write(t.path().join("a/b"), b"b").unwrap();
        let d = Dir::open_root(t.path()).unwrap();
        let invalid = |r: io::Result<()>| r.unwrap_err().kind() == io::ErrorKind::InvalidInput;
        for bad in [&b"a/b"[..], b"", b".", b"..", b"a\0"] {
            let ops: [(&str, io::Result<()>); 19] = [
                ("open_dir", d.open_dir(bad).map(drop)),
                ("create_dir", d.create_dir(bad, 0o755)),
                ("open_file", d.open_file(bad, READ).map(drop)),
                ("stat", d.stat(bad).map(drop)),
                ("read_link", d.read_link(bad).map(drop)),
                ("remove_file", d.remove_file(bad)),
                ("remove_dir", d.remove_dir(bad)),
                ("rename from", d.rename(bad, &d, b"x")),
                ("rename to", d.rename(b"a", &d, bad)),
                ("hard_link name", d.hard_link(bad, &d, b"a")),
                ("hard_link from", d.hard_link(b"x", &d, bad)),
                ("symlink", d.symlink(bad, b"a", LinkKind::File)),
                ("set_mtime", d.set_mtime(bad, (0, 0))),
                ("set_mode", d.set_mode(bad, 0o644)),
                ("set_owner", d.set_owner(bad, None, None)),
                ("make_fifo", d.make_fifo(bad, 0o644)),
                ("make_device", d.make_device(bad, Kind::Fifo, 0o644, 0)),
                (
                    "make_device socket",
                    d.make_device(bad, Kind::Socket, 0o644, 0),
                ),
                (
                    "make_device char",
                    d.make_device(bad, Kind::CharDev, 0o644, 0),
                ),
            ];
            for (op, r) in ops {
                assert!(invalid(r), "{op} with {:?}", String::from_utf8_lossy(bad));
            }
        }
        // Nothing was touched: `a` is still the directory with `b` in it.
        assert_eq!(std::fs::read(t.path().join("a/b")).unwrap(), b"b");
        assert_eq!(sorted(d.list().unwrap()), [&b"a"[..]]);
    }
}
