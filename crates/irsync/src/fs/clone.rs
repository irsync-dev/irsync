//! Reflink clones of source files, for `--paranoid`: a clone shares the
//! source's blocks at one moment and never changes after, so hashing and
//! sending it sends a state the source really held, whatever writes the
//! source sees meanwhile. No root needed: btrfs, xfs with reflink, ZFS 2.2+
//! with block cloning, bcachefs (`FICLONE`), APFS (`clonefile`).
//!
//! Clones live in a hidden per-user folder, `.irsync-clones-<uid>` (mode
//! 0700), as far up the file's filesystem as the user can write: from its
//! mount point down, the first directory on the way to the file that is
//! writable. Each session works in its own subfolder `<session>/`, whose
//! `lock` file it holds locked (`flock`) for its life. The first use of a
//! folder removes the subfolders of sessions that are gone (their lock is
//! free), which is how a crash's leftovers go; a session removes its own at
//! its end. The walk never lists these folders ([`CLONE_DIR_PREFIX`]).

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use nix::sys::stat::FileStat;
use nix::unistd::{access, AccessFlags};

use crate::fs::paths::RelPath;
use crate::hasher::Freshness;

/// The start of a clone folder's name; the walk prunes any directory whose
/// name starts with it.
pub const CLONE_DIR_PREFIX: &str = ".irsync-clones-";

/// The file a session holds locked in its subfolder.
const LOCK: &str = "lock";

/// Never follow a symlink, never block on a FIFO, never leak to a child.
const NOFOLLOW: i32 = nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC;

/// Makes `dst` share `src`'s extents (`FICLONE`): constant time, no new
/// space. Fails where the filesystem, or a cross-device pair, cannot.
#[cfg(target_os = "linux")]
pub fn reflink(src: &File, dst: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: FICLONE takes the source descriptor as its integer argument
    // and touches no memory of ours.
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
    pub path: PathBuf,
    /// The source's stat at the moment of cloning (the clone's own inode
    /// and times are of no interest).
    pub stat: Freshness,
}

impl Drop for Cloned {
    fn drop(&mut self) {
        // Gone already when its session's folder went first.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// One session's subfolder in a clone folder, locked while it lives.
/// Dropping it removes the subfolder, and the clone folder if that is then
/// empty.
struct Area {
    /// The clone folder (`.irsync-clones-<uid>`).
    root: PathBuf,
    /// This session's subfolder in it.
    dir: PathBuf,
    _lock: File,
}

impl Drop for Area {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
        // Only if no other session uses it.
        let _ = std::fs::remove_dir(&self.root);
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
    /// when no clone can be made here: the filesystem cannot clone, no
    /// clone folder can be made, or the source kept changing while it was
    /// cloned. Blocking.
    pub fn clone_file(&self, full: &Path) -> io::Result<Option<Cloned>> {
        for _ in 0..2 {
            match self.try_clone(full)? {
                Attempt::Cloned(c) => return Ok(Some(c)),
                Attempt::NoClone => return Ok(None),
                Attempt::Changed => {}
            }
        }
        Ok(None)
    }

    fn try_clone(&self, full: &Path) -> io::Result<Attempt> {
        if self.closed.load(Ordering::SeqCst) {
            return Ok(Attempt::NoClone);
        }
        let (src, s1) = crate::fs::safe::open_regular_nofollow_stat(full)?;
        // `st_dev` is narrower on macOS.
        #[allow(clippy::unnecessary_cast)]
        let dev = s1.st_dev as u64;
        let Some(area) = self.area(full, dev) else {
            return Ok(Attempt::NoClone);
        };
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let path = area.dir.join(n.to_string());
        let file = match make_clone(&src, full, &path) {
            Ok(f) => f,
            Err(e) if cannot_clone(&e) => {
                tracing::debug!(error = %e, "this filesystem cannot clone; hashing again instead");
                let _ = std::fs::remove_file(&path);
                // No more tries here: the area goes (with its folder).
                self.lock_areas().insert(dev, None);
                return Ok(Attempt::NoClone);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                return Err(e);
            }
        };
        let cloned = Cloned {
            file,
            path,
            stat: freshness(s1),
        };
        // A write while cloning may have given a clone of no real state.
        let s2 = nix::sys::stat::fstat(&src).map_err(crate::fs::safe::errno)?;
        if freshness(s2) != cloned.stat {
            return Ok(Attempt::Changed);
        }
        Ok(Attempt::Cloned(cloned))
    }

    fn lock_areas(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Option<Arc<Area>>>> {
        self.areas.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The area on device `dev` (the device of `full`), made on first use.
    fn area(&self, full: &Path, dev: u64) -> Option<Arc<Area>> {
        let mut areas = self.lock_areas();
        if let Some(a) = areas.get(&dev) {
            return a.clone();
        }
        let made =
            area_dir_below(full, dev, self.within.as_deref()).and_then(|top| {
                match make_area(&top, &self.session) {
                    Ok(a) => Some(Arc::new(a)),
                    Err(e) => {
                        tracing::debug!(dir = %top.display(), error = %e, "no clone folder");
                        None
                    }
                }
            });
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

/// Errors that say the clone cannot be made here at all.
fn cannot_clone(e: &io::Error) -> bool {
    use nix::libc::{EINVAL, ENOSYS, ENOTTY, EOPNOTSUPP, EXDEV};
    e.kind() == io::ErrorKind::Unsupported
        || matches!(
            e.raw_os_error(),
            Some(c) if c == EOPNOTSUPP || c == EXDEV || c == EINVAL || c == ENOTTY || c == ENOSYS
        )
}

fn freshness(st: FileStat) -> Freshness {
    let (entry, ctime) = crate::fs::meta::stat_entry_with_ctime(st, &RelPath::root());
    Freshness::of_entry(&entry, ctime)
}

/// Clones `src` (open, at `src_path`) as the new file `dst`, and opens it.
#[cfg(not(target_os = "macos"))]
fn make_clone(src: &File, _src_path: &Path, dst: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(NOFOLLOW)
        .open(dst)?;
    reflink(src, &file)?;
    Ok(file)
}

/// macOS: `clonefile` works on paths, not descriptors; the clone must
/// then be of the file open as `src` (the path may have been replaced).
/// Not tested here (no macOS runner).
#[cfg(target_os = "macos")]
fn make_clone(src: &File, src_path: &Path, dst: &Path) -> io::Result<File> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    extern "C" {
        fn clonefile(
            src: *const std::os::raw::c_char,
            dst: *const std::os::raw::c_char,
            flags: u32,
        ) -> std::os::raw::c_int;
    }
    const CLONE_NOFOLLOW: u32 = 0x0001;
    let s = CString::new(src_path.as_os_str().as_bytes())?;
    let d = CString::new(dst.as_os_str().as_bytes())?;
    // SAFETY: both are valid NUL-terminated paths that outlive the call.
    if unsafe { clonefile(s.as_ptr(), d.as_ptr(), CLONE_NOFOLLOW) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let opened = nix::sys::stat::fstat(src).map_err(crate::fs::safe::errno)?;
    let now = nix::sys::stat::lstat(src_path).map_err(crate::fs::safe::errno)?;
    if (now.st_dev, now.st_ino) != (opened.st_dev, opened.st_ino) {
        let _ = std::fs::remove_file(dst);
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the source was replaced while it was cloned",
        ));
    }
    OpenOptions::new()
        .read(true)
        .custom_flags(NOFOLLOW)
        .open(dst)
}

/// Where the clone folder for `file` (on device `dev`) goes: of the
/// directories from the file's filesystem's mount point down to the file,
/// the first the user can write to (`access(W_OK|X_OK)`). `None` if none.
pub fn area_dir(file: &Path, dev: u64) -> Option<PathBuf> {
    area_dir_below(file, dev, None)
}

/// [`area_dir`], looking no higher than `within` when given.
fn area_dir_below(file: &Path, dev: u64, within: Option<&Path>) -> Option<PathBuf> {
    let parent = std::fs::canonicalize(file.parent()?).ok()?;
    let within = match within {
        Some(w) => Some(std::fs::canonicalize(w).ok()?),
        None => None,
    };
    let mut chain: Vec<&Path> = Vec::new();
    for dir in parent.ancestors() {
        if within.as_deref().is_some_and(|w| !dir.starts_with(w)) {
            break;
        }
        match std::fs::metadata(dir) {
            Ok(m) if m.dev() == dev => chain.push(dir),
            _ => break,
        }
    }
    chain
        .into_iter()
        .rev()
        .find(|d| access(*d, AccessFlags::W_OK | AccessFlags::X_OK).is_ok())
        .map(Path::to_path_buf)
}

/// Makes (or takes) the clone folder in `top`, sweeps it, and makes this
/// session's locked subfolder in it.
fn make_area(top: &Path, session: &str) -> io::Result<Area> {
    let uid = nix::unistd::getuid().as_raw();
    let root = top.join(format!("{CLONE_DIR_PREFIX}{uid}"));
    // Another session may remove an empty clone folder at any moment.
    let mut last = None;
    for _ in 0..3 {
        match std::fs::DirBuilder::new().mode(0o700).create(&root) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        check_own_dir(&root, uid)?;
        if let Err(e) = sweep_stale(&root) {
            tracing::debug!(dir = %root.display(), error = %e, "sweeping old clones failed");
        }
        match lock_session(&root, session) {
            Ok(a) => return Ok(a),
            Err(e) if e.kind() == io::ErrorKind::NotFound => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("clone folder keeps vanishing")))
}

/// Refuses a clone folder that is not a real directory of ours, 0700.
fn check_own_dir(dir: &Path, uid: u32) -> io::Result<()> {
    let m = std::fs::symlink_metadata(dir)?;
    if !m.is_dir() || m.uid() != uid || m.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a private folder of ours", dir.display()),
        ));
    }
    Ok(())
}

/// Makes `<root>/<session>/` and takes its lock.
fn lock_session(root: &Path, session: &str) -> io::Result<Area> {
    let dir = root.join(session);
    for _ in 0..3 {
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let Some(lock) = take_lock(&dir)? else {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("{} is in use", dir.display()),
            ));
        };
        // A sweep may have removed the folder between our mkdir and our
        // lock: only a lock file still in place counts.
        if still_linked(&lock, &dir.join(LOCK)) {
            return Ok(Area {
                root: root.to_path_buf(),
                dir,
                _lock: lock,
            });
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("{} keeps vanishing", dir.display()),
    ))
}

/// Opens (making it if need be) `dir`'s lock file and locks it without
/// waiting: `None` if another holds it.
fn take_lock(dir: &Path) -> io::Result<Option<File>> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(NOFOLLOW)
        .open(dir.join(LOCK))?;
    match lock.try_lock() {
        Ok(()) => Ok(Some(lock)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

fn still_linked(open: &File, path: &Path) -> bool {
    match (open.metadata(), std::fs::symlink_metadata(path)) {
        (Ok(a), Ok(b)) => (a.dev(), a.ino()) == (b.dev(), b.ino()),
        _ => false,
    }
}

/// Removes every session subfolder of the clone folder `clone_root` whose
/// lock is free (its session is gone); returns how many. A live session's
/// subfolder, whose lock is held, stays.
pub fn sweep_stale(clone_root: &Path) -> io::Result<usize> {
    let mut removed = 0;
    for entry in std::fs::read_dir(clone_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let dir = entry.path();
        // Held while the folder goes, so no session takes it meanwhile;
        // one that took it just before finds its lock unlinked after.
        let lock = match take_lock(&dir) {
            Ok(Some(l)) => l,
            Ok(None) => continue,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        drop(lock);
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let t = tempfile::tempdir().unwrap();
        let root = clone_root(&t);
        let stale = session(&root, "stale");
        // A folder a crash left before it made its lock.
        std::fs::create_dir(root.join("unlocked")).unwrap();
        assert_eq!(sweep_stale(&root).unwrap(), 2);
        assert!(!stale.exists());
        assert!(!root.join("unlocked").exists());
    }

    #[test]
    fn live_clone_session_is_kept() {
        let t = tempfile::tempdir().unwrap();
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
        let t = tempfile::tempdir().unwrap();
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
            assert!(c.path.starts_with(root.join("me")));
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
        let t = tempfile::tempdir().unwrap();
        let deep = t.path().join("a/b");
        std::fs::create_dir_all(&deep).unwrap();
        let file = deep.join("f");
        std::fs::write(&file, b"x").unwrap();
        let dev = std::fs::metadata(&file).unwrap().dev();
        let bound = std::fs::canonicalize(t.path().join("a")).unwrap();
        assert_eq!(
            area_dir_below(&file, dev, Some(&bound)),
            Some(bound.clone())
        );
        // Unbounded, it is an ancestor at or above the bound (only
        // computed; nothing is made there).
        let top = area_dir(&file, dev).unwrap();
        assert!(std::fs::canonicalize(&deep).unwrap().starts_with(&top));
        // A read-only directory is passed over for the next one down.
        let ro = t.path().join("a");
        std::fs::set_permissions(&ro, std::os::unix::fs::PermissionsExt::from_mode(0o555)).unwrap();
        let got = area_dir_below(&file, dev, Some(&bound));
        std::fs::set_permissions(&ro, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        assert_eq!(got, Some(std::fs::canonicalize(&deep).unwrap()));
    }

    /// Where the filesystem clones (the build's `target/`): the clone holds
    /// the source's content and stat at cloning, survives a later write to
    /// the source, and goes when dropped.
    #[test]
    fn a_clone_is_frozen_and_removed_when_dropped() {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
        std::fs::create_dir_all(&target).unwrap();
        let t = tempfile::tempdir_in(&target).unwrap();
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
}
