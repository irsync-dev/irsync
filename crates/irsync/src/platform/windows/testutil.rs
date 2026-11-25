use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use windows_sys::Win32::Foundation::ERROR_PRIVILEGE_NOT_HELD;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
    FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA,
};

/// Makes `link` a symlink to `target`: a directory symlink if `target` is
/// a directory now, else a file symlink. Needs the symlink privilege or
/// Developer Mode; see [`symlinks_available`].
pub fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    let base = link.parent().unwrap_or(Path::new("."));
    if base.join(target).is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

/// Sets the access and the modification time of `path` to `sec` seconds
/// and `nsec` nanoseconds since the epoch (in NTFS's steps of 100 ns); a
/// final link is followed only when `follow`.
pub fn set_times(path: &Path, sec: i64, nsec: i64, follow: bool) -> io::Result<()> {
    let nsec = u32::try_from(nsec)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "nanoseconds out of range"))?;
    let t = super::file_time((sec, nsec))?;
    let flags = if follow {
        FILE_FLAG_BACKUP_SEMANTICS
    } else {
        FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT
    };
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
        .custom_flags(flags)
        .open(path)?;
    super::native::set(
        file.as_handle(),
        &windows_sys::Wdk::Storage::FileSystem::FILE_BASIC_INFORMATION {
            LastAccessTime: t,
            LastWriteTime: t,
            ..Default::default()
        },
    )
}

/// What is at `path`, a final link followed, read by handle (`std`'s
/// `Metadata` has no file index or link count on Windows).
fn stat_following(path: &Path) -> io::Result<super::Stat> {
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    super::fstat(&file)
}

/// Which file is at `path`, a final link followed: its volume serial
/// number and file index, as [`super::Stat`] gives them.
pub fn file_id(path: &Path) -> io::Result<(u64, u64)> {
    stat_following(path).map(|st| (st.dev, st.ino))
}

/// How many names (hard links) the file at `path` has, a final link
/// followed.
pub fn nlink(path: &Path) -> io::Result<u64> {
    stat_following(path).map(|st| st.nlink)
}

/// Whether this process may make symlinks (the privilege, or Developer
/// Mode). A test that needs them returns early when not, and says so.
/// With `CI` set, not being able to is a failure: CI must run them.
pub fn symlinks_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let dir = tempfile::tempdir().expect("a temporary directory");
        match std::os::windows::fs::symlink_file("target", dir.path().join("link")) {
            Ok(()) => true,
            Err(e) if e.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD as i32) => {
                assert!(
                    std::env::var_os("CI").is_none(),
                    "CI is set, but this process may not make symlinks \
                     (no privilege, no Developer Mode)"
                );
                eprintln!("skipping: this process may not make symlinks");
                false
            }
            Err(e) => panic!("making a test symlink: {e}"),
        }
    })
}

/// Makes `link` a junction to the directory `target`: a new directory
/// carrying a mount-point reparse point. Needs no privilege.
pub fn junction(target: &Path, link: &Path) -> io::Result<()> {
    std::fs::create_dir(link)?;
    let made = make_junction(target, link);
    if made.is_err() {
        let _ = std::fs::remove_dir(link);
    }
    made
}

/// Turns the existing empty directory `dir` into a junction to `target`,
/// through a handle of its own: what an attacker can do to a directory
/// that someone else holds open.
pub fn make_junction(target: &Path, dir: &Path) -> io::Result<()> {
    let data = super::junction_reparse(&absolute_units(target)?)?;
    set_reparse_at(dir, &data)
}

/// Turns the existing empty directory `dir` into a directory symlink to
/// `target` (absolute), as [`make_junction`] does. Needs the symlink
/// privilege or Developer Mode, see [`symlinks_available`].
pub fn make_dir_symlink(target: &Path, dir: &Path) -> io::Result<()> {
    let data = super::symlink_reparse(&absolute_units(target)?)?;
    set_reparse_at(dir, &data)
}

/// `path` made absolute, as UTF-16 units.
fn absolute_units(path: &Path) -> io::Result<Vec<u16>> {
    Ok(std::path::absolute(path)?
        .as_os_str()
        .encode_wide()
        .collect())
}

/// Sets the reparse point `data` on the directory `dir`, opened by path
/// without following it.
fn set_reparse_at(dir: &Path, data: &[u8]) -> io::Result<()> {
    let handle = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | FILE_WRITE_DATA)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(dir)?;
    super::native::set_reparse(handle.as_handle(), data)
}

/// How long the swapping thread leaves each state (the link, then the
/// directory) in place: long enough that an operation running in the
/// other thread meets it, short enough for many swaps a second.
const HOLD: Duration = Duration::from_micros(100);

/// A thread that swaps `dir` for a junction to `outside` and back until
/// the returned [`Swap`] is dropped. Dropping it waits for the thread,
/// which always stops with `dir` back in its place. A junction, not a
/// symlink: it needs no privilege, and `Dir` must refuse it the same.
pub fn swap_for_link(dir: &Path, outside: &Path) -> Swap {
    let mut aside = dir.as_os_str().to_owned();
    aside.push(".swapped-aside");
    let mut link = dir.as_os_str().to_owned();
    link.push(".swapped-link");
    let (dir, aside, link, outside) = (
        dir.to_path_buf(),
        PathBuf::from(aside),
        PathBuf::from(link),
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
                // Made beside it, then renamed into place, so the name is
                // a junction at once, as a Unix symlink is (`junction`
                // makes a plain directory first). The rename fails when
                // the other thread made a directory at the free name
                // meanwhile: no swap this time.
                let _ = std::fs::remove_dir(&link);
                if junction(&outside, &link).is_ok() && std::fs::rename(&link, &dir).is_ok() {
                    swaps.fetch_add(1, Ordering::Relaxed);
                    std::thread::sleep(HOLD);
                }
                put_back(&dir, &aside);
                std::thread::sleep(HOLD);
            }
            let _ = std::fs::remove_dir(&link);
        }
    });
    Swap {
        stop,
        swaps,
        thread: Some(thread),
    }
}

/// Puts `aside` back at `dir`, removing what is there: the junction (a
/// link: removed itself, never what it leads to), or a directory the
/// other thread made at the name while it was free.
fn put_back(dir: &Path, aside: &Path) {
    loop {
        match std::fs::symlink_metadata(dir) {
            // A junction or a symlink: `remove_dir` removes a directory
            // link itself, `remove_file` a file link.
            Ok(m) if m.is_symlink() => {
                if std::fs::remove_dir(dir).is_err() {
                    let _ = std::fs::remove_file(dir);
                }
            }
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
