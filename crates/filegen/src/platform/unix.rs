//! The Unix implementation: `std::os::unix`, `nix`, `libc` and `xattr`.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, OnceLock};

use nix::fcntl::{AtFlags, AT_FDCWD};
use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};
use nix::sys::stat::{utimensat, Mode, SFlag, UtimensatFlags};
use nix::sys::time::TimeSpec;
use nix::unistd::{fchownat, Gid, Uid};

use super::{FileKind, Stat};
use crate::caps::Caps;

/// The longest path the system takes, in bytes.
pub const PATH_MAX: usize = libc::PATH_MAX as usize;
/// mtimes are stored to the nanosecond.
pub const MTIME_STEP_NS: u32 = 1;
/// Any mtime in seconds is stored.
pub const MIN_MTIME_SEC: i64 = i64::MIN;
/// No limit on the names of one file that filegen could reach.
pub const MAX_LINKS: u64 = u64::MAX;
/// The separator of the components of a symlink target.
pub const TARGET_SEP: u8 = b'/';
/// rename(2) replaces an empty directory too.
pub const RENAME_OVER_DIRS: bool = true;
/// rename(2) of one name of a file over another succeeds (doing nothing).
pub const RENAME_OVER_SAME_FILE: bool = true;
/// No names that are valid but awkward on this platform are drawn.
pub const EDGE_NAMES: bool = false;

fn stat_of(md: &fs::Metadata) -> Stat {
    let t = md.file_type();
    let kind = if t.is_file() {
        FileKind::File
    } else if t.is_dir() {
        FileKind::Dir
    } else if t.is_symlink() {
        FileKind::Symlink
    } else if t.is_fifo() {
        FileKind::Fifo
    } else if t.is_char_device() {
        FileKind::CharDev
    } else if t.is_block_device() {
        FileKind::BlockDev
    } else if t.is_socket() {
        FileKind::Socket
    } else {
        FileKind::Other
    };
    Stat {
        kind,
        size: md.size(),
        mode: md.mode(),
        mtime_ns: i128::from(md.mtime()) * 1_000_000_000 + i128::from(md.mtime_nsec()),
        ino: md.ino(),
        dev: md.dev(),
        nlink: md.nlink(),
        uid: md.uid(),
    }
}

/// Reads the metadata of `path` without following a final symlink.
pub fn lstat(path: &Path) -> io::Result<Stat> {
    fs::symlink_metadata(path).map(|md| stat_of(&md))
}

/// Reads the metadata of the open file `f`.
pub fn fstat(f: &File) -> io::Result<Stat> {
    f.metadata().map(|md| stat_of(&md))
}

/// Whether filegen may pick this entry as a target: always as root;
/// otherwise when it is owned by the effective uid, with owner read and
/// write on a file and read, write and search on a directory. Without them
/// an operation fails, and since a failed step rewinds the generator, fails
/// again on every resume; and a rename into a directory filegen cannot
/// list could replace entries the walk never saw.
///
/// The rules look only at ownership and the owner's permission bits, which
/// filegen's own operations never take away (`chmod` always keeps owner
/// rw, rwx on directories; giving an entry to another uid needs root), so a
/// later walk never puts an entry filegen made off limits.
pub fn may_change(stat: &Stat) -> bool {
    let euid = nix::unistd::geteuid();
    if euid.is_root() {
        return true;
    }
    let need = match stat.kind {
        FileKind::File => 0o600,
        FileKind::Dir => 0o700,
        _ => 0,
    };
    stat.uid == euid.as_raw() && stat.mode & need == need
}

/// Whether filegen runs as root, which may change anything.
pub fn is_root() -> bool {
    nix::unistd::geteuid().is_root()
}

/// The stored bytes of a name.
pub fn os_to_bytes(name: &OsStr) -> Vec<u8> {
    name.as_bytes().to_vec()
}

/// The name that stored bytes stand for.
pub fn bytes_to_os(bytes: &[u8]) -> io::Result<OsString> {
    Ok(OsString::from_vec(bytes.to_vec()))
}

/// The path below `root` of the stored, `/`-separated relative path `rel`.
pub fn rel_to_path(root: &Path, rel: &[u8]) -> io::Result<PathBuf> {
    Ok(root.join(OsStr::from_bytes(rel)))
}

/// The bytes of any path, as the log writes them.
pub fn path_to_bytes(path: &Path) -> Vec<u8> {
    path.as_os_str().as_bytes().to_vec()
}

/// Whether the system takes `name` as a file name, beyond the rules every
/// platform shares. Any name does here.
pub fn name_ok(_name: &str) -> bool {
    true
}

/// The length of `name` as the system counts it against its name limit:
/// bytes here.
pub fn name_units(name: &str) -> usize {
    name.len()
}

/// Sets the mtime and atime of `path` to `sec` seconds and `nsec`
/// nanoseconds after the epoch, never following a symlink.
pub fn set_mtime(path: &Path, sec: i64, nsec: u32) -> io::Result<()> {
    let spec = TimeSpec::new(sec as _, nsec as _);
    utimensat(
        AT_FDCWD,
        path,
        &spec,
        &spec,
        UtimensatFlags::NoFollowSymlink,
    )
    .map_err(io::Error::from)
}

/// Makes `link` a symlink holding `target`. `kind` is ignored here.
pub fn symlink(target: &[u8], link: &Path, _kind: super::LinkTarget) -> io::Result<()> {
    std::os::unix::fs::symlink(OsStr::from_bytes(target), link)
}

/// Removes the symlink `link`.
pub fn remove_symlink(link: &Path) -> io::Result<()> {
    fs::remove_file(link)
}

/// The target of the symlink `path`, as the bytes it holds.
pub fn read_link(path: &Path) -> io::Result<Vec<u8>> {
    Ok(fs::read_link(path)?.into_os_string().into_vec())
}

/// Deallocates `[offset, offset + len)` of `f`, keeping its size; `false`
/// if the filesystem cannot.
#[cfg(target_os = "linux")]
pub(crate) fn punch_hole(f: &File, offset: u64, len: u64) -> io::Result<bool> {
    use nix::errno::Errno;
    use nix::fcntl::{fallocate, FallocateFlags};
    let flags = FallocateFlags::FALLOC_FL_PUNCH_HOLE | FallocateFlags::FALLOC_FL_KEEP_SIZE;
    match fallocate(f, flags, offset as libc::off_t, len as libc::off_t) {
        Ok(()) => Ok(true),
        Err(Errno::EOPNOTSUPP | Errno::ENOSYS) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Deallocates `[offset, offset + len)` of `f`, keeping its size; `false`
/// if the filesystem cannot.
#[cfg(target_os = "macos")]
pub(crate) fn punch_hole(f: &File, offset: u64, len: u64) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    let arg = libc::fpunchhole_t {
        fp_flags: 0,
        reserved: 0,
        fp_offset: offset as libc::off_t,
        fp_length: len as libc::off_t,
    };
    // SAFETY: F_PUNCHHOLE reads a `fpunchhole_t` through its pointer
    // argument; `&arg` points to a fully initialised one on the stack that
    // outlives the call, and the kernel only reads it. `f` is borrowed, so
    // its descriptor stays open.
    let r = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_PUNCHHOLE, &arg) };
    Ok(r == 0)
}

/// Deallocates `[offset, offset + len)` of `f`, keeping its size; `false`
/// if the filesystem cannot, as always here.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn punch_hole(_: &File, _: u64, _: u64) -> io::Result<bool> {
    Ok(false)
}

/// Sets the permission bits of `path` (following a symlink).
pub fn chmod(path: &Path, mode: u32) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

/// Changes the owner and/or group of `path`, never following a symlink;
/// `None` keeps that id.
pub fn chown(path: &Path, uid: Option<u32>, gid: Option<u32>) -> io::Result<()> {
    fchownat(
        AT_FDCWD,
        path,
        uid.map(Uid::from_raw),
        gid.map(Gid::from_raw),
        AtFlags::AT_SYMLINK_NOFOLLOW,
    )
    .map_err(io::Error::from)
}

/// Creates a FIFO at `path` with the permission bits `mode`.
pub fn mkfifo(path: &Path, mode: u32) -> io::Result<()> {
    nix::unistd::mkfifo(path, Mode::from_bits_truncate(mode as libc::mode_t))
        .map_err(io::Error::from)
}

/// Creates a character (`char_dev`) or block device node at `path` with the
/// permission bits `mode` and the device number `major`, `minor`.
pub fn mknod(path: &Path, char_dev: bool, mode: u32, major: u32, minor: u32) -> io::Result<()> {
    let kind = if char_dev {
        SFlag::S_IFCHR
    } else {
        SFlag::S_IFBLK
    };
    let rdev = libc::makedev(major as _, minor as _);
    nix::sys::stat::mknod(
        path,
        kind,
        Mode::from_bits_truncate(mode as libc::mode_t),
        rdev,
    )
    .map_err(io::Error::from)
}

/// The value of the extended attribute `name` of `path`; `None` if unset.
pub fn get_xattr(path: &Path, name: &str) -> io::Result<Option<Vec<u8>>> {
    xattr::get(path, name)
}

/// Sets the extended attribute `name` of `path` to `value`.
pub fn set_xattr(path: &Path, name: &str, value: &[u8]) -> io::Result<()> {
    xattr::set(path, name, value)
}

/// Removes the extended attribute `name` of `path`.
pub fn remove_xattr(path: &Path, name: &str) -> io::Result<()> {
    xattr::remove(path, name)
}

/// The capabilities of this machine in `dir`: all of them on Unix, found
/// without touching anything.
pub fn probe_caps(_dir: &Path) -> io::Result<Caps> {
    Ok(Caps::ALL)
}

/// The effective user id.
pub fn euid() -> u32 {
    nix::unistd::geteuid().as_raw()
}

/// The effective group id.
pub fn egid() -> u32 {
    nix::unistd::getegid().as_raw()
}

/// The process-wide flag: 0, or the number of the first signal received.
static FLAG: OnceLock<Arc<AtomicI32>> = OnceLock::new();

extern "C" fn handler(sig: libc::c_int) {
    // Only an atomic load (`OnceLock::get` once initialised), a CAS and
    // `_exit`: all async-signal-safe.
    let Some(flag) = FLAG.get() else {
        return;
    };
    if flag
        .compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
        && sig == libc::SIGINT
    {
        // A second Ctrl-C while shutting down: leave now.
        // SAFETY: `_exit` is safe to call from a signal handler: it ends
        // the process without running destructors or atexit hooks, so no
        // Rust state the interrupted thread was in the middle of is
        // touched.
        unsafe { libc::_exit(130) };
    }
}

/// Installs the SIGINT and SIGTERM handlers and returns the flag they
/// set: 0 until a signal arrives, then that signal's number (the first
/// one wins). A second SIGINT while the flag is set exits the process at
/// once with status 130. Each call clears the flag. With `SA_RESTART`,
/// so the step in flight finishes rather than failing with `EINTR`.
pub fn install_interrupt() -> Arc<AtomicI32> {
    let flag = FLAG.get_or_init(|| Arc::new(AtomicI32::new(0)));
    flag.store(0, Ordering::SeqCst);
    let action = SigAction::new(
        SigHandler::Handler(handler),
        SaFlags::SA_RESTART,
        SigSet::empty(),
    );
    for sig in [Signal::SIGINT, Signal::SIGTERM] {
        // SAFETY: nix marks sigaction unsafe because the handler runs at
        // any point in any thread. `handler` reads `FLAG` with
        // `OnceLock::get` (an atomic load; it was set above), does one
        // compare-exchange and may call `_exit`, all safe in a signal
        // handler. It takes no lock and does not allocate.
        unsafe { sigaction(sig, &action) }.expect("installing a signal handler");
    }
    Arc::clone(flag)
}

/// The name of a signal number for the log, e.g. `SIGTERM`.
pub fn interrupt_name(sig: i32) -> String {
    Signal::try_from(sig).map_or_else(|_| format!("signal {sig}"), |s| s.as_str().to_string())
}

/// Helpers for the tests, here and in `tests/`: what they need of the system
/// beyond the interface above.
#[doc(hidden)]
pub mod testutil {
    use std::ffi::OsString;
    use std::io;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use nix::sys::signal::{
        kill, raise, sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal,
    };
    use nix::unistd::Pid;

    /// The interrupt from the terminal (Ctrl-C).
    pub const SIGINT: i32 = libc::SIGINT;
    /// The polite request to end.
    pub const SIGTERM: i32 = libc::SIGTERM;

    pub fn symlink(target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    /// False when this user may not create symlinks; a platform where that
    /// can happen panics instead when the environment variable `CI` is set.
    /// Any user may, on Unix.
    pub fn symlinks_available(_dir: &Path) -> bool {
        true
    }

    /// A file made unreadable; its permissions come back when this is dropped.
    pub struct Unreadable {
        path: PathBuf,
        permissions: std::fs::Permissions,
    }

    impl Drop for Unreadable {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.path, self.permissions.clone());
        }
    }

    /// Takes every permission from `path` until the result is dropped.
    /// `None` when the file cannot be made unreadable here: privileges that
    /// bypass file permissions (root) still open it.
    pub fn make_unreadable(path: &Path) -> Option<Unreadable> {
        let permissions = std::fs::metadata(path).unwrap().permissions();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = Unreadable {
            path: path.to_path_buf(),
            permissions,
        };
        if std::fs::File::open(path).is_ok() {
            return None;
        }
        Some(unreadable)
    }

    /// Creates a FIFO at `path`; false where there are none.
    pub fn mkfifo(path: &Path) -> bool {
        super::mkfifo(path, 0o600).unwrap_or_else(|e| panic!("mkfifo {}: {e}", path.display()));
        true
    }

    pub fn set_mode(path: &Path, mode: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// A Unix socket bound at `path`; it stays there while the result lives.
    pub fn bind_socket(path: &Path) -> std::os::unix::net::UnixListener {
        std::os::unix::net::UnixListener::bind(path).unwrap()
    }

    /// The owner and group of `path`, not following a symlink.
    pub fn owner(path: &Path) -> (u32, u32) {
        use std::os::unix::fs::MetadataExt;
        let md = std::fs::symlink_metadata(path).unwrap();
        (md.uid(), md.gid())
    }

    /// Whether `e` says the filesystem has no user xattrs.
    pub fn xattr_unsupported(e: &io::Error) -> bool {
        e.raw_os_error() == Some(libc::ENOTSUP) || e.raw_os_error() == Some(libc::EOPNOTSUPP)
    }

    /// Every extended attribute of `path`, sorted by name, the name made
    /// UTF-8 (lossily).
    pub fn xattrs(path: &Path) -> Vec<(String, Vec<u8>)> {
        let mut names: Vec<OsString> = xattr::list(path).unwrap().collect();
        names.sort();
        names
            .into_iter()
            .map(|n| {
                let v = xattr::get(path, &n).unwrap().unwrap();
                (n.to_string_lossy().into_owned(), v)
            })
            .collect()
    }

    fn signal(sig: i32) -> Signal {
        Signal::try_from(sig).unwrap_or_else(|e| panic!("signal {sig}: {e}"))
    }

    /// Sends the interrupt signal `sig` to this process.
    pub fn raise_interrupt(sig: i32) {
        raise(signal(sig)).unwrap();
    }

    /// Sends the interrupt signal `sig` to the process `pid`.
    pub fn send_interrupt(pid: u32, sig: i32) {
        kill(Pid::from_raw(pid as i32), signal(sig)).unwrap();
    }

    /// Whether the handler installed for `sig` restarts interrupted system
    /// calls (`SA_RESTART`). Swaps the action out and back, so nothing else
    /// in the process may raise `sig` meanwhile.
    pub fn interrupt_restarts_syscalls(sig: i32) -> bool {
        let sig = signal(sig);
        let probe = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
        // SAFETY: the default action runs no code of ours, so installing
        // it cannot break a handler's rules. No signal is raised before
        // the old action goes back on the next line, and the caller
        // promises nothing else in the process races it.
        let installed = unsafe { sigaction(sig, &probe) }.unwrap();
        // SAFETY: puts back the action that was installed (by
        // `install_interrupt`, whose handler is safe to run at any time;
        // see its own SAFETY comment).
        unsafe { sigaction(sig, &installed) }.unwrap();
        installed.flags().contains(SaFlags::SA_RESTART)
    }
}
