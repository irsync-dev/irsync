//! Reflink clones of source files, for `--paranoid`: a clone shares the
//! source's blocks at one moment and never changes after, so hashing and
//! sending it sends a state the source really held, whatever writes the
//! source sees meanwhile. No root needed: btrfs, xfs with reflink, ZFS 2.2+
//! with block cloning, bcachefs (`FICLONE`), APFS (`fclonefileat`).
//!
//! Clones live in a hidden per-user folder, `.irsync-clones-<uid>` (mode
//! 0700), as far up the file's filesystem as the user can write: from its
//! mount point down, the first directory on the way to the file that is
//! writable and not shared without the sticky bit. Each session works in
//! its own subfolder `<session>/`, whose `lock` file it holds locked
//! (`flock`) for its life. The first use of a folder removes the subfolders
//! of sessions that are gone (their lock is free), which is how a crash's
//! leftovers go; a session removes its own at its end. The walk never lists
//! these folders ([`CLONE_DIR_PREFIX`]).
//!
//! The directories below the session's own (an export's directory, on a
//! daemon) are looked at through open directories, one name at a time and
//! never through a symlink, so a clone folder is never put where a link
//! leads. Everything below the directory chosen is done relative to open
//! directories too, never following a symlink: whoever else can write there
//! cannot swap a symlink in and steer a removal elsewhere.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nix::dir::Dir;
use nix::fcntl::{openat, AtFlags, OFlag, AT_FDCWD};
use nix::sys::stat::{fstat, fstatat, mkdirat, FileStat, Mode, SFlag};
use nix::unistd::{access, unlinkat, AccessFlags, UnlinkatFlags};

use crate::fs::paths::RelPath;
use crate::fs::safe::errno;
use crate::hasher::Freshness;

/// The start of a clone folder's name, followed by the uid; the walk
/// prunes a directory with exactly that form of name
/// ([`crate::scan::is_clone_dir`]).
pub const CLONE_DIR_PREFIX: &str = ".irsync-clones-";

/// The file a session holds locked in its subfolder.
const LOCK: &str = "lock";

/// How often, and how far apart, a session tries to take its subfolder's
/// lock while another session's sweep holds it for a moment.
const LOCK_TRIES: u32 = 10;
const LOCK_PAUSE: Duration = Duration::from_millis(20);

/// Opening a directory: never through a symlink.
fn dir_flags() -> OFlag {
    OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC
}

/// Opening a file: never through a symlink, never blocking on a FIFO.
fn file_flags() -> OFlag {
    OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC
}

/// Makes `dst` share `src`'s extents (`FICLONE`): constant time, no new
/// space. Fails where the filesystem, or a cross-device pair, cannot.
#[cfg(target_os = "linux")]
pub fn reflink(src: &File, dst: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: both descriptors belong to `File`s borrowed for the call, so
    // they stay open. FICLONE takes the source descriptor as a plain
    // integer, not a pointer, so the kernel reads and writes no memory of
    // ours.
    let r = unsafe { nix::libc::ioctl(dst.as_raw_fd(), nix::libc::FICLONE, src.as_raw_fd()) };
    if r == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
pub fn reflink(_src: &File, _dst: &File) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// A clone of a source file. Dropping it removes the clone.
#[derive(Debug)]
pub struct Cloned {
    /// The clone, open.
    pub file: File,
    /// Where it is (for messages; it is removed through `dir`).
    pub path: PathBuf,
    /// The source's stat at the moment of cloning (the clone's own inode
    /// and times are of no interest).
    pub stat: Freshness,
    /// Its session's subfolder, open, and its name there.
    dir: Arc<OwnedFd>,
    name: OsString,
}

impl Drop for Cloned {
    fn drop(&mut self) {
        // Gone already when its session's folder went first.
        let _ = unlinkat(
            &*self.dir,
            self.name.as_os_str(),
            UnlinkatFlags::NoRemoveDir,
        );
    }
}

/// One session's subfolder in a clone folder, locked while it lives.
/// Dropping it removes the subfolder, and the clone folder if that is then
/// empty.
struct Area {
    /// The directory the clone folder is in, open, and the clone folder's
    /// name there.
    top: OwnedFd,
    root_name: OsString,
    /// The clone folder (`.irsync-clones-<uid>`), open.
    root: OwnedFd,
    /// This session's subfolder in it: name, path (for messages), open.
    dir_name: OsString,
    dir_path: PathBuf,
    dir: Arc<OwnedFd>,
    _lock: File,
}

impl Drop for Area {
    fn drop(&mut self) {
        // With the lock still held (fields drop after this).
        if let Err(e) = remove_tree_at(&self.root, &self.dir_name) {
            tracing::debug!(dir = %self.dir_path.display(), error = %e, "removing clones failed");
        }
        // Only if no other session uses it (fails unless empty, and on
        // anything but a directory).
        let _ = unlinkat(
            &self.top,
            self.root_name.as_os_str(),
            UnlinkatFlags::RemoveDir,
        );
    }
}

/// A session's clones, with one area per filesystem (by `st_dev`).
pub struct Clones {
    session: String,
    /// Clone folders go no higher than this folder (tests).
    within: Option<PathBuf>,
    /// By device: the area, or `None` where none can be made or the
    /// filesystem cannot clone.
    areas: Mutex<HashMap<u64, Option<Arc<Area>>>>,
    next: AtomicU64,
    closed: AtomicBool,
}

impl Clones {
    pub fn new(session_id: &str) -> Clones {
        Clones {
            session: session_id.to_string(),
            within: None,
            areas: Mutex::default(),
            next: AtomicU64::new(0),
            closed: AtomicBool::new(false),
        }
    }

    /// Clone folders go no higher than `dir` (a test keeps them inside its
    /// own temporary folder).
    pub fn within(mut self, dir: PathBuf) -> Clones {
        self.within = Some(dir);
        self
    }

    /// Clones the regular file at `full` (never through a symlink). `None`
    /// when no clone can be made: the filesystem cannot clone, this file
    /// cannot be cloned, no clone folder can be made, or the source kept
    /// changing while it was cloned. Blocking.
    pub fn clone_file(&self, full: &Path) -> io::Result<Option<Cloned>> {
        let Some(dir) = full.parent() else {
            return Ok(None);
        };
        self.clone_opened(dir, &RelPath::root(), full, || {
            crate::fs::safe::open_regular_nofollow_stat(full)
        })
    }

    /// [`Clones::clone_file`] of `base/rel`, with no symlink followed in
    /// any component of `rel`.
    pub fn clone_file_beneath(&self, base: &Path, rel: &RelPath) -> io::Result<Option<Cloned>> {
        let full = base.join(rel.to_os());
        self.clone_opened(base, &rel.parent(), &full, || {
            crate::fs::safe::open_regular_beneath_stat(base, rel)
        })
    }

    /// `full` (for messages) is the file `open` opens, in `dir` below `base`.
    fn clone_opened(
        &self,
        base: &Path,
        dir: &RelPath,
        full: &Path,
        open: impl Fn() -> io::Result<(File, FileStat)>,
    ) -> io::Result<Option<Cloned>> {
        for _ in 0..2 {
            match self.try_clone(base, dir, full, &open)? {
                Attempt::Cloned(c) => return Ok(Some(c)),
                Attempt::NoClone => return Ok(None),
                Attempt::Changed => {}
            }
        }
        Ok(None)
    }

    fn try_clone(
        &self,
        base: &Path,
        dir: &RelPath,
        full: &Path,
        open: &impl Fn() -> io::Result<(File, FileStat)>,
    ) -> io::Result<Attempt> {
        if self.closed.load(Ordering::SeqCst) {
            return Ok(Attempt::NoClone);
        }
        let (src, s1) = open()?;
        // `st_dev` is narrower on macOS.
        #[allow(clippy::unnecessary_cast)]
        let dev = s1.st_dev as u64;
        let Some(area) = self.area(base, dir, dev) else {
            return Ok(Attempt::NoClone);
        };
        let name = OsString::from(self.next.fetch_add(1, Ordering::Relaxed).to_string());
        let file = match make_clone(&src, &area.dir, &name) {
            Ok(f) => f,
            Err(e) => {
                let _ = unlinkat(&*area.dir, name.as_os_str(), UnlinkatFlags::NoRemoveDir);
                return match clone_failure(&e) {
                    CloneFailure::Filesystem => {
                        tracing::debug!(error = %e, "this filesystem cannot clone; hashing again instead");
                        // No more tries here: the area goes (with its folder).
                        self.lock_areas().insert(dev, None);
                        Ok(Attempt::NoClone)
                    }
                    CloneFailure::File => {
                        tracing::debug!(path = %full.display(), error = %e, "this file cannot be cloned; hashing again instead");
                        Ok(Attempt::NoClone)
                    }
                    CloneFailure::Error => Err(e),
                };
            }
        };
        let cloned = Cloned {
            file,
            path: area.dir_path.join(&name),
            stat: freshness(s1),
            dir: area.dir.clone(),
            name,
        };
        // A write while cloning may have given a clone of no real state.
        let s2 = fstat(&src).map_err(errno)?;
        if freshness(s2) != cloned.stat {
            return Ok(Attempt::Changed);
        }
        Ok(Attempt::Cloned(cloned))
    }

    fn lock_areas(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Option<Arc<Area>>>> {
        self.areas.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The area on device `dev` (the device of a file in `dir` below
    /// `base`), made on first use.
    fn area(&self, base: &Path, dir: &RelPath, dev: u64) -> Option<Arc<Area>> {
        let mut areas = self.lock_areas();
        if let Some(a) = areas.get(&dev) {
            return a.clone();
        }
        let made = match area_top(base, dir, dev, self.within.as_deref())
            .map(|(fd, top)| (make_area(fd, &top, &self.session), top))
        {
            None => None,
            Some((Ok(a), _)) => Some(Arc::new(a)),
            // Out of file descriptors (each file in flight holds its clone
            // open): no clone for this file, but later ones may still get
            // one, so the filesystem is not written off.
            Some((Err(e), top)) if out_of_fds(&e) => {
                tracing::debug!(dir = %top.display(), error = %e, "no clone folder for now");
                return None;
            }
            Some((Err(e), top)) => {
                tracing::debug!(dir = %top.display(), error = %e, "no clone folder");
                None
            }
        };
        areas.insert(dev, made.clone());
        made
    }

    /// Removes this session's clone folders; no clones are made after.
    /// Clones still held are removed with them (an open one stays readable).
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let areas = std::mem::take(&mut *self.lock_areas());
        drop(areas);
    }
}

impl Drop for Clones {
    fn drop(&mut self) {
        self.close();
    }
}

enum Attempt {
    Cloned(Cloned),
    /// No clone is possible here.
    NoClone,
    /// The source changed while it was cloned.
    Changed,
}

/// What a failed clone says.
#[derive(Debug, PartialEq, Eq)]
enum CloneFailure {
    /// This filesystem (or this pair of devices) cannot clone at all.
    Filesystem,
    /// This one file cannot be cloned (btrfs: `EINVAL` when the source and
    /// the clone differ in `nodatacow` / `nodatasum`).
    File,
    /// Anything else.
    Error,
}

fn clone_failure(e: &io::Error) -> CloneFailure {
    use nix::libc::{EINVAL, ENOSYS, ENOTSUP, ENOTTY, EOPNOTSUPP, EXDEV};
    if e.kind() == io::ErrorKind::Unsupported {
        return CloneFailure::Filesystem;
    }
    match e.raw_os_error() {
        // `ENOTSUP` and `EOPNOTSUPP` differ on macOS, not on Linux.
        Some(c) if c == EOPNOTSUPP || c == ENOTSUP || c == ENOSYS || c == EXDEV => {
            CloneFailure::Filesystem
        }
        Some(c) if c == EINVAL || c == ENOTTY => CloneFailure::File,
        _ => CloneFailure::Error,
    }
}

/// Whether `e` says the process or system has no file descriptor left
/// (`EMFILE`, `ENFILE`): a passing shortage, not a property of the
/// filesystem.
fn out_of_fds(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(nix::libc::EMFILE | nix::libc::ENFILE)
    )
}

fn freshness(st: FileStat) -> Freshness {
    let (entry, ctime) = crate::fs::meta::stat_entry_with_ctime(st, &RelPath::root());
    Freshness::of_entry(&entry, ctime)
}

/// Clones `src` as the new file `name` in the open directory `dir`, and
/// opens the clone.
#[cfg(not(target_os = "macos"))]
fn make_clone(src: &File, dir: &OwnedFd, name: &OsStr) -> io::Result<File> {
    let fd = openat(
        dir,
        name,
        OFlag::O_RDWR | OFlag::O_CREAT | OFlag::O_EXCL | file_flags(),
        Mode::from_bits_truncate(0o600),
    )
    .map_err(errno)?;
    let file = File::from(fd);
    reflink(src, &file)?;
    Ok(file)
}

/// macOS: `fclonefileat` clones the file open as `src` into the open
/// directory `dir`. `tests/paranoid_clone.rs` covers it on macOS.
#[cfg(target_os = "macos")]
fn make_clone(src: &File, dir: &OwnedFd, name: &OsStr) -> io::Result<File> {
    use std::ffi::CString;
    use std::os::fd::AsRawFd;
    extern "C" {
        fn fclonefileat(
            srcfd: std::os::raw::c_int,
            dst_dirfd: std::os::raw::c_int,
            dst: *const std::os::raw::c_char,
            flags: u32,
        ) -> std::os::raw::c_int;
    }
    let d = CString::new(name.as_bytes())?;
    // SAFETY: `src` and `dir` are borrowed for the call, so both
    // descriptors stay open. `d` is a CString (CString::new refused an
    // inner NUL above), so NUL-terminated, and lives past the call;
    // fclonefileat only reads it. The declaration above matches the one
    // in macOS's <sys/clonefile.h>.
    if unsafe { fclonefileat(src.as_raw_fd(), dir.as_raw_fd(), d.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = openat(dir, name, OFlag::O_RDONLY | file_flags(), Mode::empty()).map_err(errno)?;
    Ok(File::from(fd))
}

/// Where the clone folder for a file in `dir` below `base` (on device
/// `dev`) goes: of the directories from the file's filesystem's mount
/// point down to the file, the first the user can write to
/// (`W_OK|X_OK`) that others cannot rename things in: not group- or
/// world-writable, unless it has the sticky bit. The directory, open, and
/// its path (for messages); `None` if none. Looks no higher than `within`
/// when given.
///
/// `base` and what is above it are names this side trusts and are looked
/// at by name. The directories of `dir` are opened one from the other
/// without following a symlink, and looked at through those descriptors:
/// a directory someone swapped for a symlink gives no clone folder, never
/// one where the link leads.
fn area_top(
    base: &Path,
    dir: &RelPath,
    dev: u64,
    within: Option<&Path>,
) -> Option<(OwnedFd, PathBuf)> {
    use nix::unistd::faccessat;
    let real = std::fs::canonicalize(base).ok()?;
    let within = match within {
        Some(w) => Some(std::fs::canonicalize(w).ok()?),
        None => None,
    };
    let inside = |p: &Path| within.as_deref().is_none_or(|w| p.starts_with(w));
    // `base` and the directories of `dir`, open, top first.
    let by_name = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC;
    let mut below: Vec<(Option<OwnedFd>, PathBuf)> = Vec::new();
    let mut path = real.clone();
    let mut fd = openat(AT_FDCWD, &real, by_name, Mode::empty()).ok()?;
    for c in dir.components() {
        let name = OsStr::from_bytes(c);
        let next = openat(&fd, name, dir_flags(), Mode::empty()).ok()?;
        below.push((Some(std::mem::replace(&mut fd, next)), path.clone()));
        path.push(name);
    }
    below.push((Some(fd), path));
    // From the file's directory up, as far as the device stays the same.
    let mut chain: Vec<(Option<OwnedFd>, PathBuf, u32)> = Vec::new();
    let mut whole = true;
    for (fd, path) in below.into_iter().rev() {
        let st = fd.as_ref().and_then(|fd| fstat(fd).ok());
        // `st_dev` and `st_mode` are narrower on macOS.
        #[allow(clippy::unnecessary_cast)]
        match st {
            Some(st) if st.st_dev as u64 == dev && inside(&path) => {
                chain.push((fd, path, st.st_mode as u32));
            }
            _ => {
                whole = false;
                break;
            }
        }
    }
    if whole {
        for up in real.ancestors().skip(1) {
            if !inside(up) {
                break;
            }
            match std::fs::metadata(up) {
                Ok(m) if m.dev() == dev => chain.push((None, up.to_path_buf(), m.mode())),
                _ => break,
            }
        }
    }
    let (fd, path, _) = chain.into_iter().rev().find(|(fd, path, mode)| {
        let shared = mode & 0o022 != 0 && mode & 0o1000 == 0;
        let want = AccessFlags::W_OK | AccessFlags::X_OK;
        let writable = match fd {
            Some(fd) => faccessat(fd, ".", want, AtFlags::empty()).is_ok(),
            None => access(path.as_path(), want).is_ok(),
        };
        !shared && writable
    })?;
    let fd = match fd {
        Some(fd) => fd,
        None => openat(AT_FDCWD, &path, dir_flags(), Mode::empty()).ok()?,
    };
    Some((fd, path))
}

/// Makes (or takes) the clone folder in the open directory `top_fd` (at
/// `top`, for messages), sweeps it, and makes this session's locked
/// subfolder in it.
fn make_area(top_fd: OwnedFd, top: &Path, session: &str) -> io::Result<Area> {
    let uid = nix::unistd::getuid().as_raw();
    let root_name = OsString::from(format!("{CLONE_DIR_PREFIX}{uid}"));
    // Another session may remove an empty clone folder at any moment.
    let mut last = None;
    for _ in 0..3 {
        match mkdirat(
            &top_fd,
            root_name.as_os_str(),
            Mode::from_bits_truncate(0o700),
        ) {
            Ok(()) | Err(nix::Error::EEXIST) => {}
            Err(e) => return Err(errno(e)),
        }
        // A symlink there is refused here, not followed.
        let root = match openat(&top_fd, root_name.as_os_str(), dir_flags(), Mode::empty()) {
            Ok(fd) => fd,
            Err(nix::Error::ENOENT) => {
                last = Some(errno(nix::Error::ENOENT));
                continue;
            }
            Err(e) => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "{}: not a folder of ours: {e}",
                        top.join(&root_name).display()
                    ),
                ))
            }
        };
        check_own_dir(&root, uid, &top.join(&root_name))?;
        if let Err(e) = sweep_at(&root) {
            tracing::debug!(dir = %top.join(&root_name).display(), error = %e, "sweeping old clones failed");
        }
        match lock_session(&root, session) {
            Ok((dir, lock)) => {
                let dir_path = top.join(&root_name).join(session);
                return Ok(Area {
                    top: top_fd,
                    root_name,
                    root,
                    dir_name: OsString::from(session),
                    dir_path,
                    dir: Arc::new(dir),
                    _lock: lock,
                });
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("clone folder keeps vanishing")))
}

/// Refuses an open clone folder that is not a directory of ours, 0700.
fn check_own_dir(dir: &OwnedFd, uid: u32, what: &Path) -> io::Result<()> {
    let st = fstat(dir).map_err(errno)?;
    let mode = st.st_mode as u32;
    let is_dir = SFlag::from_bits_truncate(st.st_mode) & SFlag::S_IFMT == SFlag::S_IFDIR;
    if !is_dir || st.st_uid != uid || mode & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a private folder of ours", what.display()),
        ));
    }
    Ok(())
}

/// Makes `<root>/<session>/` and takes its lock: the subfolder, open, and
/// the lock. A sweep holds a free lock for a moment: waited out.
fn lock_session(root: &OwnedFd, session: &str) -> io::Result<(OwnedFd, File)> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "session folder keeps vanishing");
    for _ in 0..LOCK_TRIES {
        match mkdirat(root, session, Mode::from_bits_truncate(0o700)) {
            Ok(()) | Err(nix::Error::EEXIST) => {}
            Err(e) => return Err(errno(e)),
        }
        let dir = match openat(root, session, dir_flags(), Mode::empty()) {
            Ok(fd) => fd,
            Err(nix::Error::ENOENT) => continue,
            Err(e) => return Err(errno(e)),
        };
        match take_lock(&dir) {
            Ok(Some(lock)) => {
                // A sweep may have removed the folder between our mkdir and
                // our lock: only a folder and a lock still in place count.
                if still_linked(&lock, &dir, OsStr::new(LOCK))
                    && still_linked(&dir, root, OsStr::new(session))
                {
                    return Ok((dir, lock));
                }
            }
            Ok(None) => {
                last = io::Error::new(io::ErrorKind::WouldBlock, format!("{session} is in use"));
                std::thread::sleep(LOCK_PAUSE);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

/// Opens (making it if need be) the lock file in the open directory `dir`
/// and locks it without waiting: `None` if another holds it.
fn take_lock(dir: &OwnedFd) -> io::Result<Option<File>> {
    let fd = openat(
        dir,
        LOCK,
        OFlag::O_RDWR | OFlag::O_CREAT | file_flags(),
        Mode::from_bits_truncate(0o600),
    )
    .map_err(errno)?;
    let lock = File::from(fd);
    match lock.try_lock() {
        Ok(()) => Ok(Some(lock)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// Whether `name` in the open directory `parent` is still the open `fd`.
fn still_linked(fd: &impl std::os::fd::AsFd, parent: &OwnedFd, name: &OsStr) -> bool {
    match (
        fstat(fd),
        fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW),
    ) {
        (Ok(a), Ok(b)) => (a.st_dev, a.st_ino) == (b.st_dev, b.st_ino),
        _ => false,
    }
}

/// The names in the open directory `dir` (but `.` and `..`).
fn names(dir: &OwnedFd) -> io::Result<Vec<OsString>> {
    // A fresh open of it: its own read position.
    let fd = openat(dir, ".", dir_flags(), Mode::empty()).map_err(errno)?;
    let mut out = Vec::new();
    for entry in Dir::from_fd(fd).map_err(errno)?.iter() {
        let entry = entry.map_err(errno)?;
        let n = entry.file_name().to_bytes();
        if n != b"." && n != b".." {
            out.push(OsStr::from_bytes(n).to_os_string());
        }
    }
    Ok(out)
}

/// Removes `name` from the open directory `parent`, with everything below
/// it if it is a directory, never following a symlink (one is removed
/// itself). Gone already is fine.
fn remove_tree_at(parent: &OwnedFd, name: &OsStr) -> io::Result<()> {
    let st = match fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(st) => st,
        Err(nix::Error::ENOENT) => return Ok(()),
        Err(e) => return Err(errno(e)),
    };
    let is_dir = SFlag::from_bits_truncate(st.st_mode) & SFlag::S_IFMT == SFlag::S_IFDIR;
    if is_dir {
        let dir = match openat(parent, name, dir_flags(), Mode::empty()) {
            Ok(fd) => fd,
            Err(nix::Error::ENOENT) => return Ok(()),
            Err(e) => return Err(errno(e)),
        };
        for n in names(&dir)? {
            remove_tree_at(&dir, &n)?;
        }
    }
    let flag = if is_dir {
        UnlinkatFlags::RemoveDir
    } else {
        UnlinkatFlags::NoRemoveDir
    };
    match unlinkat(parent, name, flag) {
        Ok(()) | Err(nix::Error::ENOENT) => Ok(()),
        Err(e) => Err(errno(e)),
    }
}

/// Removes every session subfolder of the clone folder `clone_root` whose
/// lock is free (its session is gone); returns how many. A live session's
/// subfolder, whose lock is held, stays. `clone_root` itself must not be a
/// symlink.
pub fn sweep_stale(clone_root: &Path) -> io::Result<usize> {
    let root = openat(AT_FDCWD, clone_root, dir_flags(), Mode::empty()).map_err(errno)?;
    sweep_at(&root)
}

/// [`sweep_stale`] of the open clone folder `root`.
fn sweep_at(root: &OwnedFd) -> io::Result<usize> {
    let mut removed = 0;
    for name in names(root)? {
        // Only real directories: a symlink is never opened.
        let dir = match openat(root, name.as_os_str(), dir_flags(), Mode::empty()) {
            Ok(fd) => fd,
            Err(nix::Error::ENOENT | nix::Error::ENOTDIR) => continue,
            Err(e) if crate::fs::safe::is_symlink_errno(e) => continue,
            Err(e) => return Err(errno(e)),
        };
        // Held while the folder goes, so no session takes it meanwhile;
        // one that took it just before finds its lock unlinked after.
        let lock = match take_lock(&dir) {
            Ok(Some(l)) => l,
            Ok(None) => continue,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            // One folder whose lock cannot be had (e.g. its lock file is
            // not ours to open) leaves the rest to be swept.
            Err(e) => {
                tracing::debug!(dir = ?name, error = %e, "cannot lock an old clone folder; skipping it");
                continue;
            }
        };
        // Still the folder we locked?
        if !still_linked(&dir, root, &name) {
            continue;
        }
        remove_tree_at(root, &name)?;
        removed += 1;
        drop(lock);
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    /// A temporary folder only we can write to (`tempfile` follows the
    /// umask, which may make it group-writable, and so passed over).
    fn private_tempdir_in(dir: &Path) -> tempfile::TempDir {
        let t = tempfile::tempdir_in(dir).unwrap();
        chmod(t.path(), 0o700);
        t
    }

    fn private_tempdir() -> tempfile::TempDir {
        private_tempdir_in(&std::env::temp_dir())
    }

    fn chmod(p: &Path, mode: u32) {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Where the clone folder for `file` goes, its own directory taken as
    /// a name we trust.
    fn area_of(file: &Path, dev: u64, within: Option<&Path>) -> Option<PathBuf> {
        area_top(file.parent()?, &RelPath::root(), dev, within).map(|(_, p)| p)
    }

    /// The directories below the base are looked at through open
    /// directories: one that became a symlink gives no clone folder, and
    /// nothing is made where the link leads.
    #[test]
    fn the_clone_folder_is_never_put_behind_a_symlink() {
        let t = private_tempdir();
        let outside = private_tempdir();
        let base = t.path().join("base");
        std::fs::create_dir_all(base.join("d")).unwrap();
        chmod(&base, 0o755);
        chmod(&base.join("d"), 0o755);
        std::fs::write(base.join("d/f"), b"x").unwrap();
        let dev = std::fs::metadata(base.join("d/f")).unwrap().dev();
        let bound = std::fs::canonicalize(&base).unwrap();
        let d = RelPath::new(b"d").unwrap();
        let top = |within: &Path| area_top(&base, &d, dev, Some(within)).map(|(_, p)| p);
        assert_eq!(top(&bound), Some(bound.clone()));
        // A read-only base is passed over for the directory below it.
        chmod(&base, 0o555);
        let got = top(&bound);
        chmod(&base, 0o755);
        assert_eq!(got, Some(bound.join("d")));

        std::fs::remove_dir_all(base.join("d")).unwrap();
        std::os::unix::fs::symlink(outside.path(), base.join("d")).unwrap();
        std::fs::write(outside.path().join("f"), b"x").unwrap();
        assert_eq!(top(&bound), None);
        let clones = Clones::new("me").within(bound.clone());
        let f = RelPath::new(b"d/f").unwrap();
        assert!(clones.clone_file_beneath(&base, &f).is_err());
        clones.close();
        let left: Vec<_> = std::fs::read_dir(outside.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, vec![OsString::from("f")]);
    }

    fn clone_root(t: &tempfile::TempDir) -> PathBuf {
        let r = t.path().join(".irsync-clones-test");
        std::fs::create_dir(&r).unwrap();
        r
    }

    fn session(root: &Path, name: &str) -> PathBuf {
        let d = root.join(name);
        std::fs::create_dir(&d).unwrap();
        std::fs::write(d.join(LOCK), b"").unwrap();
        std::fs::write(d.join("0"), b"a clone").unwrap();
        d
    }

    #[test]
    fn stale_clone_session_is_swept() {
        let t = private_tempdir();
        let root = clone_root(&t);
        let stale = session(&root, "stale");
        // A folder a crash left before it made its lock.
        std::fs::create_dir(root.join("unlocked")).unwrap();
        assert_eq!(sweep_stale(&root).unwrap(), 2);
        assert!(!stale.exists());
        assert!(!root.join("unlocked").exists());
    }

    /// A folder whose lock cannot be opened is skipped; the sweep goes on
    /// with the others. (Root opens any lock file, so it skips the test.)
    #[test]
    fn an_unlockable_folder_does_not_stop_the_sweep() {
        if nix::unistd::getuid().is_root() {
            return;
        }
        let t = private_tempdir();
        let root = clone_root(&t);
        let bad = session(&root, "bad");
        chmod(&bad.join(LOCK), 0o400);
        // Several, so that some come after the bad one in directory order.
        let stale: Vec<_> = (0..6).map(|i| session(&root, &format!("s{i}"))).collect();
        assert_eq!(sweep_stale(&root).unwrap(), 6);
        assert!(stale.iter().all(|d| !d.exists()), "a stale folder was left");
        assert!(bad.join("0").exists(), "the unlockable folder went");
    }

    #[test]
    fn live_clone_session_is_kept() {
        let t = private_tempdir();
        let root = clone_root(&t);
        let live = session(&root, "live");
        let stale = session(&root, "stale");
        let held = File::open(live.join(LOCK)).unwrap();
        held.lock().unwrap();
        assert_eq!(sweep_stale(&root).unwrap(), 1);
        assert!(live.join("0").exists(), "a live session's clone went");
        assert!(!stale.exists());
    }

    /// The first use of a clone folder sweeps it; the session's own
    /// subfolder is locked, and goes (with the then empty clone folder)
    /// when the session closes. Works where nothing can be cloned too: the
    /// folder is made before the first clone is tried.
    #[test]
    fn first_use_sweeps_and_close_removes() {
        let t = private_tempdir();
        let uid = nix::unistd::getuid().as_raw();
        let root = t.path().join(format!("{CLONE_DIR_PREFIX}{uid}"));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let stale = session(&root, "crashed");
        let src = t.path().join("f");
        std::fs::write(&src, b"content").unwrap();

        let clones = Clones::new("me").within(t.path().to_path_buf());
        let c = clones.clone_file(&src).unwrap();
        assert!(!stale.exists(), "the stale session was not swept");
        if let Some(c) = &c {
            // Clones go under the resolved folder (macOS: /var is /private/var).
            let real = std::fs::canonicalize(&root).unwrap();
            assert!(c.path.starts_with(real.join("me")), "{:?}", c.path);
        }
        // Our own lock is held: a sweep keeps it.
        if root.join("me").exists() {
            assert_eq!(sweep_stale(&root).unwrap(), 0);
            assert!(root.join("me").exists());
        }
        drop(c);
        clones.close();
        assert!(!root.exists(), "the clone folder is left");
        assert!(clones.clone_file(&src).unwrap().is_none());
    }

    #[test]
    fn area_dir_stays_within_its_bound() {
        let t = private_tempdir();
        let deep = t.path().join("a/b");
        std::fs::create_dir_all(&deep).unwrap();
        // Whatever the umask: neither is shared.
        chmod(&t.path().join("a"), 0o755);
        chmod(&deep, 0o755);
        let file = deep.join("f");
        std::fs::write(&file, b"x").unwrap();
        let dev = std::fs::metadata(&file).unwrap().dev();
        let bound = std::fs::canonicalize(t.path().join("a")).unwrap();
        assert_eq!(area_of(&file, dev, Some(&bound)), Some(bound.clone()));
        // Unbounded, it is an ancestor at or above the bound (only
        // computed; nothing is made there).
        let top = area_of(&file, dev, None).unwrap();
        assert!(std::fs::canonicalize(&deep).unwrap().starts_with(&top));
        // A read-only directory is passed over for the next one down.
        let ro = t.path().join("a");
        chmod(&ro, 0o555);
        let got = area_of(&file, dev, Some(&bound));
        chmod(&ro, 0o755);
        assert_eq!(got, Some(std::fs::canonicalize(&deep).unwrap()));
    }

    /// A directory others can write to without the sticky bit is passed
    /// over: someone else could rename the clone folder away and put a
    /// symlink in its place. With the sticky bit it is fine.
    #[test]
    fn a_shared_directory_without_sticky_bit_is_skipped() {
        let t = private_tempdir();
        let a = t.path().join("a");
        let deep = a.join("b");
        std::fs::create_dir_all(&deep).unwrap();
        chmod(&deep, 0o755);
        let file = deep.join("f");
        std::fs::write(&file, b"x").unwrap();
        let dev = std::fs::metadata(&file).unwrap().dev();
        let bound = std::fs::canonicalize(&a).unwrap();
        let deep = std::fs::canonicalize(&deep).unwrap();
        chmod(&a, 0o775);
        assert_eq!(area_of(&file, dev, Some(&bound)), Some(deep.clone()));
        chmod(&a, 0o757);
        assert_eq!(area_of(&file, dev, Some(&bound)), Some(deep));
        chmod(&a, 0o1777);
        assert_eq!(area_of(&file, dev, Some(&bound)), Some(bound));
        chmod(&a, 0o755);
    }

    /// A symlink put where the clone folder goes (by whoever else can write
    /// there) is refused, never followed: nothing it points at is swept.
    #[test]
    fn a_symlinked_clone_folder_is_refused() {
        let t = private_tempdir();
        let uid = nix::unistd::getuid().as_raw();
        let decoy = t.path().join("decoy");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&decoy)
            .unwrap();
        let victim = session(&decoy, "victim");
        std::os::unix::fs::symlink(&decoy, t.path().join(format!("{CLONE_DIR_PREFIX}{uid}")))
            .unwrap();
        let src = t.path().join("f");
        std::fs::write(&src, b"content").unwrap();
        let clones = Clones::new("me").within(t.path().to_path_buf());
        assert!(clones.clone_file(&src).unwrap().is_none());
        clones.close();
        assert!(victim.join(LOCK).exists());
        assert_eq!(std::fs::read(victim.join("0")).unwrap(), b"a clone");
        assert!(!decoy.join("me").exists());
        // `sweep_stale` refuses a symlinked folder too.
        assert!(sweep_stale(&t.path().join(format!("{CLONE_DIR_PREFIX}{uid}"))).is_err());
        assert!(victim.join("0").exists());
    }

    /// A symlink inside the clone folder is removed itself, never followed.
    #[test]
    fn a_symlink_inside_is_not_followed() {
        let t = private_tempdir();
        let root = clone_root(&t);
        let outside = t.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep"), b"keep").unwrap();
        let stale = session(&root, "stale");
        std::os::unix::fs::symlink(&outside, stale.join("link")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("toplink")).unwrap();
        assert_eq!(sweep_stale(&root).unwrap(), 1);
        assert!(!stale.exists());
        assert!(outside.join("keep").exists());
    }

    /// Only some failures turn cloning off for the whole filesystem; one
    /// file's `EINVAL` (btrfs `nodatacow` mismatch) falls back for it alone.
    #[test]
    fn clone_failures_are_told_apart() {
        use nix::libc::{EINVAL, EIO, ENOSYS, ENOTTY, EOPNOTSUPP, EXDEV};
        let e = io::Error::from_raw_os_error;
        for c in [EOPNOTSUPP, ENOSYS, EXDEV, nix::libc::ENOTSUP] {
            assert_eq!(clone_failure(&e(c)), CloneFailure::Filesystem, "{c}");
        }
        assert_eq!(
            clone_failure(&io::Error::from(io::ErrorKind::Unsupported)),
            CloneFailure::Filesystem
        );
        for c in [EINVAL, ENOTTY] {
            assert_eq!(clone_failure(&e(c)), CloneFailure::File, "{c}");
        }
        assert_eq!(clone_failure(&e(EIO)), CloneFailure::Error);
    }

    /// Running out of file descriptors is told apart from other errors.
    #[test]
    fn out_of_fds_is_told_apart() {
        let e = io::Error::from_raw_os_error;
        assert!(out_of_fds(&e(nix::libc::EMFILE)));
        assert!(out_of_fds(&e(nix::libc::ENFILE)));
        assert!(!out_of_fds(&e(nix::libc::EIO)));
        assert!(!out_of_fds(&io::Error::from(io::ErrorKind::Unsupported)));
    }

    /// A sweep holding our fresh subfolder's lock for a moment does not
    /// cost the session its clones: the lock is tried again.
    #[test]
    fn a_lock_held_for_a_moment_is_waited_for() {
        let t = private_tempdir();
        let root = clone_root(&t);
        let d = root.join("me");
        std::fs::create_dir(&d).unwrap();
        std::fs::write(d.join(LOCK), b"").unwrap();
        let held = File::open(d.join(LOCK)).unwrap();
        held.lock().unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            drop(held);
        });
        let fd = openat(AT_FDCWD, &root, dir_flags(), Mode::empty()).unwrap();
        let got = lock_session(&fd, "me");
        release.join().unwrap();
        assert!(got.is_ok(), "{:?}", got.err());
    }

    /// Where the filesystem clones (the build's `target/`): the clone holds
    /// the source's content and stat at cloning, survives a later write to
    /// the source, and goes when dropped.
    #[test]
    fn a_clone_is_frozen_and_removed_when_dropped() {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
        std::fs::create_dir_all(&target).unwrap();
        let t = private_tempdir_in(&target);
        let src = t.path().join("f");
        std::fs::write(&src, b"before").unwrap();
        let clones = Clones::new("me").within(t.path().to_path_buf());
        let Some(c) = clones.clone_file(&src).unwrap() else {
            eprintln!("skipping: {} cannot clone", target.display());
            return;
        };
        let (entry, ctime) =
            crate::fs::meta::lstat_entry_with_ctime(&src, &RelPath::root()).unwrap();
        assert_eq!(c.stat, Freshness::of_entry(&entry, ctime));
        std::fs::write(&src, b"after!").unwrap();
        assert_eq!(std::fs::read(&c.path).unwrap(), b"before");
        let path = c.path.clone();
        drop(c);
        assert!(!path.exists());
    }

    /// A write through a shared writable mapping, never flushed (no
    /// `msync`), is in the page cache but perhaps not on disk. Does a
    /// clone made right after include it? The docs claim what this shows.
    #[test]
    fn clone_includes_unflushed_mmap_writes() {
        use std::os::fd::AsRawFd;
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
        std::fs::create_dir_all(&target).unwrap();
        let t = private_tempdir_in(&target);
        let src = t.path().join("f");
        let len = 8192usize;
        std::fs::write(&src, vec![b'a'; len]).unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&src)
            .unwrap();
        // SAFETY: a null address lets the kernel pick a free spot, so the
        // new mapping cannot overlap memory Rust already uses. The file
        // was just written with `len` bytes, so every mapped page is
        // backed by the file. The result is checked against MAP_FAILED
        // below before any use.
        let map = unsafe {
            nix::libc::mmap(
                std::ptr::null_mut(),
                len,
                nix::libc::PROT_READ | nix::libc::PROT_WRITE,
                nix::libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        assert_ne!(map, nix::libc::MAP_FAILED, "mmap failed");
        // SAFETY: `map` is a writable mapping of `len` (8192) bytes, not
        // MAP_FAILED (asserted above); the two 100-byte writes land at
        // offsets 0 and `len - 100`, inside it. No Rust reference points
        // into the mapping.
        unsafe {
            std::ptr::write_bytes(map as *mut u8, b'z', 100);
            std::ptr::write_bytes((map as *mut u8).add(len - 100), b'y', 100);
        }
        let clones = Clones::new("me").within(t.path().to_path_buf());
        let cloned = clones.clone_file(&src).unwrap();
        let Some(c) = cloned else {
            // SAFETY: `map` and `len` are exactly the mapping made above,
            // still mapped, and nothing uses it after this.
            unsafe { nix::libc::munmap(map, len) };
            eprintln!("skipping: {} cannot clone", target.display());
            return;
        };
        let got = std::fs::read(&c.path).unwrap();
        // SAFETY: `map` and `len` are exactly the mapping made above,
        // still mapped (the other unmap returned early), and nothing uses
        // it after this.
        unsafe { nix::libc::munmap(map, len) };
        assert_eq!(&got[..100], &[b'z'; 100][..], "clone misses mmap writes");
        assert_eq!(
            &got[len - 100..],
            &[b'y'; 100][..],
            "clone misses mmap writes"
        );
        assert_eq!(&got[100..len - 100], &vec![b'a'; len - 200][..]);
    }
}
