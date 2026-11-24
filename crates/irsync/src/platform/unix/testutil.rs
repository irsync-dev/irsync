use std::ffi::OsStr;
use std::fs::{FileType, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use nix::libc;

/// Makes `link` a symlink to `target`.
pub fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// Sets the permission bits of `path` (`chmod`, which follows a link).
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    super::set_mode_path_following(path, mode)
}

/// Sets the owner and the group of `path` (`chown`, which follows a link);
/// `None` leaves one as it is.
pub fn chown(path: &Path, uid: Option<u32>, gid: Option<u32>) -> io::Result<()> {
    std::os::unix::fs::chown(path, uid, gid)
}

/// Sets the access and the modification time of `path` to `sec` seconds
/// and `nsec` nanoseconds since the epoch (`utimensat`); a final link is
/// followed only when `follow`.
pub fn set_times(path: &Path, sec: i64, nsec: i64, follow: bool) -> io::Result<()> {
    use nix::sys::stat::{utimensat, UtimensatFlags};
    let t = nix::sys::time::TimeSpec::new(sec as _, nsec as _);
    let flag = if follow {
        UtimensatFlags::FollowSymlink
    } else {
        UtimensatFlags::NoFollowSymlink
    };
    utimensat(nix::fcntl::AT_FDCWD, path, &t, &t, flag).map_err(super::errno_of)
}

/// Makes a FIFO at `path` with permission bits `mode` (less the umask).
pub fn mkfifo(path: &Path, mode: u32) -> io::Result<()> {
    nix::unistd::mkfifo(path, nix::sys::stat::Mode::from_bits_truncate(mode as _))
        .map_err(super::errno_of)
}

/// Binds a Unix domain socket at `path`; the socket file stays while the
/// listener lives.
pub fn bind_unix_socket(path: &Path) -> io::Result<std::os::unix::net::UnixListener> {
    std::os::unix::net::UnixListener::bind(path)
}

/// Whether `t` is a FIFO.
pub fn is_fifo(t: FileType) -> bool {
    std::os::unix::fs::FileTypeExt::is_fifo(&t)
}

/// How many 512-byte blocks the file `m` describes has on disk.
pub fn blocks_of(m: &Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::blocks(m)
}

/// The device number of device `major`, `minor`.
// The argument and result types differ by platform.
#[allow(clippy::unnecessary_cast)]
pub fn makedev(major: u32, minor: u32) -> u64 {
    libc::makedev(major as _, minor as _) as u64
}

/// The real user id of this process.
pub fn real_uid() -> u32 {
    nix::unistd::getuid().as_raw()
}

/// The real group id of this process.
pub fn real_gid() -> u32 {
    nix::unistd::getgid().as_raw()
}

/// The stored bytes of `name`, borrowed.
pub fn name_bytes(name: &OsStr) -> &[u8] {
    std::os::unix::ffi::OsStrExt::as_bytes(name)
}

/// The name that `bytes` stand for, borrowed: any bytes are a name.
pub fn name_from_bytes(bytes: &[u8]) -> &OsStr {
    std::os::unix::ffi::OsStrExt::from_bytes(bytes)
}

/// The soft and the hard limit on open files (`RLIMIT_NOFILE`).
// `rlim_t` is signed on FreeBSD.
#[allow(clippy::unnecessary_cast)]
pub fn open_file_limits() -> io::Result<(u64, u64)> {
    use nix::sys::resource::{getrlimit, Resource};
    getrlimit(Resource::RLIMIT_NOFILE)
        .map(|(soft, hard)| (soft as u64, hard as u64))
        .map_err(super::errno_of)
}

/// Sets the soft and the hard limit on open files (`RLIMIT_NOFILE`).
pub fn set_open_file_limits(soft: u64, hard: u64) -> io::Result<()> {
    use nix::sys::resource::{setrlimit, Resource};
    setrlimit(Resource::RLIMIT_NOFILE, soft as _, hard as _).map_err(super::errno_of)
}

/// A signal [`send_signal`] sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// `SIGINT`.
    Interrupt,
    /// `SIGKILL`.
    Kill,
}

/// Sends `sig` to the process `pid` (`kill`).
pub fn send_signal(pid: u32, sig: Signal) -> io::Result<()> {
    use nix::sys::signal::{kill, Signal as S};
    let sig = match sig {
        Signal::Interrupt => S::SIGINT,
        Signal::Kill => S::SIGKILL,
    };
    kill(nix::unistd::Pid::from_raw(pid as i32), sig).map_err(super::errno_of)
}

/// Error numbers, for tests that make or expect an operating-system error.
pub mod errno {
    pub use nix::libc::{
        EACCES, EAGAIN, EBUSY, EILSEQ, EINVAL, EIO, EMFILE, ENFILE, ENOBUFS, ENOENT, ENOMEM,
        ENOSPC, EPERM,
    };
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

/// Counts the descriptors this process has open the slow, sure way: asks
/// about each number in turn.
pub fn open_descriptor_count() -> u64 {
    use nix::libc::{c_int, c_long, fcntl, sysconf, _SC_OPEN_MAX, F_GETFD};
    // Descriptor numbers stay below the process's open-file limit,
    // which sysconf reports. It can be "none" or in the millions; this
    // test process has only a few descriptors, so stop at 65536 to
    // keep the walk short.
    const CAP: c_long = 1 << 16;
    // SAFETY: sysconf takes a constant name, reads a limit and returns
    // it as a number; it touches no memory of ours.
    let limit = match unsafe { sysconf(_SC_OPEN_MAX) } {
        n if n > 0 => n.min(CAP),
        _ => CAP,
    };
    (0..limit as c_int)
        // SAFETY: F_GETFD takes no third argument and only returns the
        // flags of `fd`, or fails with EBADF when no such descriptor is
        // open. It changes nothing, so asking about a descriptor some
        // other code owns is harmless.
        .filter(|&fd| unsafe { fcntl(fd, F_GETFD) } != -1)
        .count() as u64
}
