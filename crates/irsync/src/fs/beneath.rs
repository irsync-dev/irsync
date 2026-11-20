//! Folders opened below a root, one component at a time.
//!
//! A [`Root`] holds an open handle on the export or destination folder. Every
//! folder below it is opened with `openat` and `O_NOFOLLOW` from its parent, so
//! a symlink anywhere below the root is refused, and a folder that is swapped
//! for a symlink later cannot lead out: work goes through the handle that was
//! opened, not through the name. The root itself may be reached through
//! symlinks the user chose.
//!
//! Opened folders are kept in a small least-recently-used cache, so the
//! next file in the same folder does not walk the path again. A [`Dir`] a
//! caller holds stays valid after the cache drops it. A cached handle follows
//! the folder, not its name, so callers call [`Root::forget`] after removing
//! or renaming a folder.
//!
//! A [`Dir`] also carries the file operations that act on one name inside it
//! (`openat`, `unlinkat`, and friends), so no operation walks a path again.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use irsync_proto::v1::{file_entry::Kind, FileEntry};
use nix::errno::Errno;
use nix::fcntl::{openat, readlinkat, renameat, AtFlags, OFlag, AT_FDCWD};
use nix::sys::stat::{fstatat, mkdirat, utimensat, FileStat, Mode, SFlag, UtimensatFlags};
use nix::sys::time::TimeSpec;
use nix::unistd::{fchownat, symlinkat, unlinkat, Gid, Uid, UnlinkatFlags};

use crate::fs::meta::{apply_fd, entry_of, may_chgrp, process_ids, ApplyOpts};
use crate::fs::paths::RelPath;
use crate::fs::safe::{errno, is_symlink_errno, nofollow_error, require_regular_stat, split_last};
use crate::fs::transient::retry;

/// How many folder handles a [`Root`] keeps by default.
pub const DIR_CACHE: usize = 64;

/// What to do with a missing folder on the way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// Fail with `NotFound`.
    Refuse,
    /// Make it, with mode `0o777` less the umask.
    Create,
}

/// An open folder below a [`Root`]. Cheap to clone; the handle closes when
/// the last clone and the cache have let go of it.
#[derive(Clone)]
pub struct Dir {
    fd: Arc<OwnedFd>,
    rel: RelPath,
}

impl AsFd for Dir {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

/// A name must be one path component, so it cannot lead out of the folder.
fn check_name(name: &OsStr) -> io::Result<()> {
    let b = name.as_bytes();
    if b.is_empty() || b == b"." || b == b".." || b.contains(&b'/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not a single file name: {}", name.to_string_lossy()),
        ));
    }
    Ok(())
}

/// Flags for every open of a file by name: never follow a symlink there,
/// never block on a FIFO there.
fn name_flags() -> OFlag {
    OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC
}

/// Makes a FIFO or device node.
#[cfg(not(target_vendor = "apple"))]
fn make_node(dir: &Dir, name: &OsStr, kind: Kind, perm: Mode, rdev: u64) -> io::Result<()> {
    let sflag = match kind {
        // mkfifoat, not mknodat: FreeBSD's mknod makes device nodes only.
        Kind::Fifo => return nix::unistd::mkfifoat(dir, name, perm).map_err(errno),
        Kind::CharDev => SFlag::S_IFCHR,
        Kind::BlockDev => SFlag::S_IFBLK,
        Kind::Socket => SFlag::S_IFSOCK,
        _ => return Err(io::Error::from(io::ErrorKind::InvalidInput)),
    };
    nix::sys::stat::mknodat(dir, name, sflag, perm, rdev as _).map_err(errno)
}

/// `mkfifoat` and `mknodat` exist only from macOS 13, and nix has neither on
/// Apple. They are looked up when first needed, so the binary still starts on
/// older systems; there, making a special file fails with `Unsupported`.
#[cfg(target_vendor = "apple")]
mod apple_at {
    use std::ffi::{c_char, c_int, c_void};
    use std::io;
    use std::sync::OnceLock;

    use nix::libc::{dev_t, mode_t};

    type MkfifoatFn = unsafe extern "C" fn(c_int, *const c_char, mode_t) -> c_int;
    type MknodatFn = unsafe extern "C" fn(c_int, *const c_char, mode_t, dev_t) -> c_int;

    fn lookup(name: &std::ffi::CStr) -> Option<*mut c_void> {
        // SAFETY: `name` is a valid NUL-terminated string; `dlsym` only reads it.
        let p = unsafe { nix::libc::dlsym(nix::libc::RTLD_DEFAULT, name.as_ptr()) };
        (!p.is_null()).then_some(p)
    }

    pub fn mkfifoat() -> io::Result<MkfifoatFn> {
        static F: OnceLock<Option<MkfifoatFn>> = OnceLock::new();
        let f = F.get_or_init(|| {
            // SAFETY: the symbol, when present, is the C function
            // `int mkfifoat(int fd, const char *path, mode_t mode)`.
            lookup(c"mkfifoat")
                .map(|p| unsafe { std::mem::transmute::<*mut c_void, MkfifoatFn>(p) })
        });
        f.ok_or_else(unsupported)
    }

    pub fn mknodat() -> io::Result<MknodatFn> {
        static F: OnceLock<Option<MknodatFn>> = OnceLock::new();
        let f = F.get_or_init(|| {
            // SAFETY: the symbol, when present, is the C function
            // `int mknodat(int fd, const char *path, mode_t mode, dev_t dev)`.
            lookup(c"mknodat").map(|p| unsafe { std::mem::transmute::<*mut c_void, MknodatFn>(p) })
        });
        f.ok_or_else(unsupported)
    }

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "special files need macOS 13 or newer",
        )
    }
}

#[cfg(target_vendor = "apple")]
fn make_node(dir: &Dir, name: &OsStr, kind: Kind, perm: Mode, rdev: u64) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let c = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let fd = dir.as_fd().as_raw_fd();
    let sflag = match kind {
        Kind::Fifo => {
            let f = apple_at::mkfifoat()?;
            // SAFETY: `f` is `mkfifoat(int, const char *, mode_t)`; `c` is a
            // valid NUL-terminated string that outlives the call, and `fd` is
            // an open folder handle held by `dir`.
            let rc = unsafe { f(fd, c.as_ptr(), perm.bits()) };
            return if rc == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            };
        }
        Kind::CharDev => SFlag::S_IFCHR,
        Kind::BlockDev => SFlag::S_IFBLK,
        Kind::Socket => SFlag::S_IFSOCK,
        _ => return Err(io::Error::from(io::ErrorKind::InvalidInput)),
    };
    let f = apple_at::mknodat()?;
    // SAFETY: `f` is `mknodat(int, const char *, mode_t, dev_t)`; the string and
    // folder handle are valid as above.
    let rc = unsafe { f(fd, c.as_ptr(), sflag.bits() | perm.bits(), rdev as _) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

impl Dir {
    /// Where this folder is, relative to the root it came from (for messages).
    pub fn rel(&self) -> &RelPath {
        &self.rel
    }

    /// `name` as a path below the root, for messages.
    fn shown(&self, name: &OsStr) -> PathBuf {
        PathBuf::from(OsStr::from_bytes(self.rel.as_bytes())).join(name)
    }

    /// Opens `name` with `flags`, never following a symlink there.
    fn open_name(&self, name: &OsStr, flags: OFlag, mode: Mode) -> io::Result<File> {
        check_name(name)?;
        let fd = retry(|| openat(self, name, flags | name_flags(), mode).map_err(errno))
            .map_err(|e| nofollow_error(e, &self.shown(name)))?;
        Ok(File::from(fd))
    }

    /// Opens `name` read-only. It must be a regular file; a symlink, FIFO or
    /// folder is refused with the same messages as `open_regular_nofollow`.
    pub fn open_regular(&self, name: &OsStr) -> io::Result<(File, FileStat)> {
        let f = self.open_name(name, OFlag::O_RDONLY, Mode::empty())?;
        let st = require_regular_stat(&f, &self.shown(name))?;
        Ok((f, st))
    }

    /// Opens `name` for reading and writing; `create` makes it if missing.
    /// A symlink at the name is refused, not followed.
    pub fn open_rw(&self, name: &OsStr, create: bool) -> io::Result<File> {
        let mut flags = OFlag::O_RDWR;
        if create {
            flags |= OFlag::O_CREAT;
        }
        self.open_name(name, flags, Mode::from_bits_truncate(0o666))
    }

    /// Opens `name` read-only, or write-only if the mode does not allow
    /// reading. Never creates it.
    pub fn open_any(&self, name: &OsStr) -> io::Result<File> {
        match self.open_name(name, OFlag::O_RDONLY, Mode::empty()) {
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                self.open_name(name, OFlag::O_WRONLY, Mode::empty())
            }
            r => r,
        }
    }

    /// `lstat` of `name`.
    pub fn stat(&self, name: &OsStr) -> io::Result<FileStat> {
        check_name(name)?;
        fstatat(self, name, AtFlags::AT_SYMLINK_NOFOLLOW).map_err(errno)
    }

    /// Where the symlink `name` points.
    pub fn read_link(&self, name: &OsStr) -> io::Result<Vec<u8>> {
        check_name(name)?;
        let t = readlinkat(self, name).map_err(errno)?;
        Ok(t.as_bytes().to_vec())
    }

    /// The entry and ctime of `name`, as `lstat_entry_with_ctime` gives for
    /// `rel`.
    pub fn entry(&self, name: &OsStr, rel: &RelPath) -> io::Result<(FileEntry, (i64, u32))> {
        let st = self.stat(name)?;
        let link = if is_link(&st) {
            self.read_link(name)?
        } else {
            Vec::new()
        };
        Ok(entry_of(st, rel, link))
    }

    /// Makes `name` a folder, removing a non-folder in the way (a symlink is
    /// removed, not followed).
    pub fn mkdir_replacing(&self, name: &OsStr) -> io::Result<()> {
        check_name(name)?;
        let make = || mkdirat(self, name, Mode::from_bits_truncate(0o777)).map_err(errno);
        match fstatat(self, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
            Ok(st) if is_dir(&st) => Ok(()),
            Ok(_) => {
                unlinkat(self, name, UnlinkatFlags::NoRemoveDir).map_err(errno)?;
                make()
            }
            Err(Errno::ENOENT) => make(),
            Err(e) => Err(errno(e)),
        }
    }

    pub fn symlink(&self, target: &OsStr, name: &OsStr) -> io::Result<()> {
        check_name(name)?;
        symlinkat(target, self, name).map_err(errno)
    }

    /// Makes a FIFO or device node from the entry, as `fs::meta::create_special`.
    /// The mode is subject to the umask; follow with [`Dir::apply_node_metadata`].
    pub fn create_special(&self, name: &OsStr, e: &FileEntry) -> io::Result<()> {
        check_name(name)?;
        let kind = e.kind();
        if !matches!(
            kind,
            Kind::Fifo | Kind::CharDev | Kind::BlockDev | Kind::Socket
        ) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a special file kind",
            ));
        }
        let perm = Mode::from_bits_truncate((e.mode & 0o7777) as _);
        make_node(self, name, kind, perm, e.rdev)
    }

    /// Moves `name` to `to_name` in the folder `to`.
    pub fn rename(&self, name: &OsStr, to: &Dir, to_name: &OsStr) -> io::Result<()> {
        check_name(name)?;
        check_name(to_name)?;
        renameat(self, name, to, to_name).map_err(errno)
    }

    /// Removes `name` (not a folder).
    pub fn unlink(&self, name: &OsStr) -> io::Result<()> {
        check_name(name)?;
        unlinkat(self, name, UnlinkatFlags::NoRemoveDir).map_err(errno)
    }

    /// Sets owner, mode and times of this folder itself, through its handle.
    pub fn apply_metadata(&self, e: &FileEntry, opts: &ApplyOpts) -> io::Result<()> {
        apply_fd(self.as_fd(), e, opts, None)
    }

    /// Sets owner, mode and times of a symlink or special file at `name`.
    /// Never follows a symlink: if `name` is one, only the link itself is
    /// changed, and its mode is left alone.
    pub fn apply_node_metadata(
        &self,
        name: &OsStr,
        e: &FileEntry,
        opts: &ApplyOpts,
    ) -> io::Result<()> {
        check_name(name)?;
        let owner = opts.owner && process_ids().root;
        let group = opts.group && may_chgrp(e.gid);
        if owner || group {
            let uid = owner.then(|| Uid::from_raw(e.uid));
            let gid = group.then(|| Gid::from_raw(e.gid));
            fchownat(self, name, uid, gid, AtFlags::AT_SYMLINK_NOFOLLOW).map_err(errno)?;
        }
        if opts.perms && e.kind() != Kind::Symlink {
            self.chmod_node(name, Mode::from_bits_truncate((e.mode & 0o7777) as _))?;
        }
        if opts.times {
            let t = TimeSpec::new(e.mtime_sec as _, e.mtime_nsec as _);
            utimensat(self, name, &t, &t, UtimensatFlags::NoFollowSymlink).map_err(errno)?;
        }
        Ok(())
    }

    /// `chmod` of `name` that cannot reach through a symlink.
    #[cfg(not(target_os = "linux"))]
    fn chmod_node(&self, name: &OsStr, mode: Mode) -> io::Result<()> {
        use nix::sys::stat::{fchmodat, FchmodatFlags};
        fchmodat(self, name, mode, FchmodatFlags::NoFollowSymlink).map_err(errno)
    }

    /// Linux cannot `chmod` a name without following it, so open the node
    /// with `O_PATH` (which does not follow), refuse a symlink, and chmod
    /// that open handle through `/proc`. Without `/proc` this fails, and
    /// nothing is followed.
    #[cfg(target_os = "linux")]
    fn chmod_node(&self, name: &OsStr, mode: Mode) -> io::Result<()> {
        use nix::sys::stat::fstat;
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::PermissionsExt;
        let fd = openat(
            self,
            name,
            OFlag::O_PATH | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map_err(errno)?;
        if is_link(&fstat(&fd).map_err(errno)?) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("refusing to follow symlink {}", self.shown(name).display()),
            ));
        }
        let proc = format!("/proc/self/fd/{}", fd.as_raw_fd());
        std::fs::set_permissions(proc, std::fs::Permissions::from_mode(mode.bits() as u32))
    }
}

fn is_link(st: &FileStat) -> bool {
    SFlag::from_bits_truncate(st.st_mode as _) & SFlag::S_IFMT == SFlag::S_IFLNK
}

fn is_dir(st: &FileStat) -> bool {
    SFlag::from_bits_truncate(st.st_mode as _) & SFlag::S_IFMT == SFlag::S_IFDIR
}

/// Least recently used first, most recent last. Small, so a plain list is
/// enough.
struct Cache {
    cap: usize,
    entries: Vec<(RelPath, Arc<OwnedFd>)>,
}

impl Cache {
    /// The cached handle for `rel`, now the most recent.
    fn get(&mut self, rel: &RelPath) -> Option<Arc<OwnedFd>> {
        let i = self.entries.iter().position(|(k, _)| k == rel)?;
        let e = self.entries.remove(i);
        let fd = e.1.clone();
        self.entries.push(e);
        Some(fd)
    }

    fn put(&mut self, rel: RelPath, fd: Arc<OwnedFd>) {
        self.entries.retain(|(k, _)| *k != rel);
        self.entries.push((rel, fd));
        while self.entries.len() > self.cap {
            self.entries.remove(0);
        }
    }
}

/// A folder everything else is opened below. Its own handle is never
/// evicted.
pub struct Root {
    path: PathBuf,
    fd: Arc<OwnedFd>,
    cap: usize,
    cache: Mutex<Cache>,
}

fn open_flags() -> OFlag {
    OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC
}

impl Root {
    /// Opens `path` (symlinks on the way are followed) with a cache of
    /// [`DIR_CACHE`] folders.
    pub fn open(path: &Path) -> io::Result<Arc<Root>> {
        Self::open_with_cache(path, DIR_CACHE)
    }

    pub fn open_with_cache(path: &Path, cap: usize) -> io::Result<Arc<Root>> {
        let fd = retry(|| openat(AT_FDCWD, path, open_flags(), Mode::empty()).map_err(errno))?;
        Ok(Self::from_fd(path.to_path_buf(), Arc::new(fd), cap))
    }

    fn from_fd(path: PathBuf, fd: Arc<OwnedFd>, cap: usize) -> Arc<Root> {
        Arc::new(Root {
            path,
            fd,
            cap,
            cache: Mutex::new(Cache {
                cap,
                entries: Vec::new(),
            }),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The folder `rel` below the root; `RelPath::root()` is the root itself.
    pub fn dir(&self, rel: &RelPath, missing: Missing) -> io::Result<Dir> {
        if rel.as_bytes().is_empty() {
            return Ok(Dir {
                fd: self.fd.clone(),
                rel: rel.clone(),
            });
        }
        let total = rel.components().count();
        // Start from the deepest folder already open.
        let mut start = (RelPath::root(), self.fd.clone());
        let mut skip = 0;
        {
            let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            let mut prefix = rel.clone();
            let mut depth = total;
            while !prefix.as_bytes().is_empty() {
                if let Some(fd) = cache.get(&prefix) {
                    start = (prefix, fd);
                    skip = depth;
                    break;
                }
                prefix = prefix.parent();
                depth -= 1;
            }
        }
        if skip == total {
            return Ok(Dir {
                fd: start.1,
                rel: rel.clone(),
            });
        }
        // Walk the rest without holding the lock: opens can wait.
        let mut opened = Vec::new();
        let (mut at, mut fd) = start;
        for name in rel.components().skip(skip) {
            at = at.join(name).expect("a component of a RelPath is valid");
            let next = Arc::new(open_child(&fd, name, rel, missing)?);
            opened.push((at.clone(), next.clone()));
            fd = next;
        }
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        for (k, v) in opened {
            cache.put(k, v);
        }
        Ok(Dir {
            fd,
            rel: rel.clone(),
        })
    }

    /// `rel`'s parent folder and final name.
    pub fn parent(&self, rel: &RelPath, missing: Missing) -> io::Result<(Dir, OsString)> {
        let (parent, name) = split_last(rel)?;
        let dir = self.dir(&parent, missing)?;
        Ok((dir, OsStr::from_bytes(&name).to_os_string()))
    }

    /// A new Root at the folder `rel`, with its own cache.
    pub fn sub(&self, rel: &RelPath, missing: Missing) -> io::Result<Arc<Root>> {
        let dir = self.dir(rel, missing)?;
        Ok(Self::from_fd(self.path.join(rel.to_os()), dir.fd, self.cap))
    }

    /// Drops cached handles at or below `rel`, so the next use opens the
    /// folder by name again (after it was removed and made anew).
    pub fn forget(&self, rel: &RelPath) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let b = rel.as_bytes();
        if b.is_empty() {
            cache.entries.clear();
            return;
        }
        cache.entries.retain(|(k, _)| {
            let k = k.as_bytes();
            !(k.starts_with(b) && (k.len() == b.len() || k[b.len()] == b'/'))
        });
    }

    /// Handles the cache holds now (tests, budget).
    pub fn cached(&self) -> usize {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .len()
    }
}

/// Opens the folder `name` inside `parent` without following a symlink.
/// `whole` is the path being resolved, for messages.
fn open_child(
    parent: &OwnedFd,
    name: &[u8],
    whole: &RelPath,
    missing: Missing,
) -> io::Result<OwnedFd> {
    let name = OsStr::from_bytes(name);
    let open = || {
        retry(|| {
            openat(
                parent,
                name,
                open_flags() | OFlag::O_NOFOLLOW,
                Mode::empty(),
            )
            .map_err(errno)
        })
    };
    let refused = |e: io::Error| match e.raw_os_error().map(Errno::from_raw) {
        Some(n) if is_symlink_errno(n) || n == Errno::ENOTDIR => io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{}: a path component is a symlink or not a directory",
                String::from_utf8_lossy(whole.as_bytes())
            ),
        ),
        _ => e,
    };
    match open() {
        Err(e) if missing == Missing::Create && e.kind() == io::ErrorKind::NotFound => {
            match mkdirat(parent, name, Mode::from_bits_truncate(0o777)) {
                Ok(()) | Err(Errno::EEXIST) => {}
                Err(e) => return Err(errno(e)),
            }
            open().map_err(refused)
        }
        r => r.map_err(refused),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::fcntl::AtFlags;
    use nix::sys::stat::fstatat;
    use nix::sys::time::TimeSpec;
    use std::os::unix::fs::{symlink, DirBuilderExt, MetadataExt, PermissionsExt};

    fn r(s: &[u8]) -> RelPath {
        RelPath::new(s).unwrap()
    }

    fn kind<T>(x: io::Result<T>) -> io::ErrorKind {
        x.err().expect("expected an error").kind()
    }

    #[test]
    fn dir_refuses_a_symlinked_component() {
        let d = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("a")).unwrap();
        std::fs::create_dir(out.path().join("x")).unwrap();
        symlink(out.path(), d.path().join("esc")).unwrap();
        symlink(out.path(), d.path().join("a/esc")).unwrap();
        let root = Root::open(d.path()).unwrap();
        for p in [&b"esc"[..], b"esc/x", b"a/esc"] {
            let e = root.dir(&r(p), Missing::Refuse).err().unwrap();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{p:?}");
            assert!(
                e.to_string().contains("a symlink or not a directory"),
                "{e}"
            );
        }
    }

    #[test]
    fn dir_create_makes_missing_folders_only() {
        let d = tempfile::tempdir().unwrap();
        let root = Root::open(d.path()).unwrap();
        assert_eq!(
            kind(root.dir(&r(b"a/b/c"), Missing::Refuse)),
            io::ErrorKind::NotFound
        );
        root.dir(&r(b"a/b/c"), Missing::Create).unwrap();
        let umask = {
            let probe = d.path().join("probe");
            std::fs::DirBuilder::new()
                .mode(0o777)
                .create(&probe)
                .unwrap();
            0o777 & !std::fs::metadata(&probe).unwrap().permissions().mode()
        };
        let m = std::fs::metadata(d.path().join("a/b/c")).unwrap();
        assert_eq!(m.permissions().mode() & 0o777, 0o777 & !umask);
        std::fs::write(d.path().join("a/file"), b"x").unwrap();
        assert_eq!(
            kind(root.dir(&r(b"a/file/z"), Missing::Create)),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            kind(root.dir(&r(b"a/file"), Missing::Create)),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn root_may_be_reached_through_a_symlink() {
        let d = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let link = out.path().join("link");
        symlink(d.path(), &link).unwrap();
        std::fs::create_dir(d.path().join("a")).unwrap();
        symlink(out.path(), d.path().join("esc")).unwrap();
        let root = Root::open(&link).unwrap();
        root.dir(&r(b"a"), Missing::Refuse).unwrap();
        assert_eq!(
            kind(root.dir(&r(b"esc"), Missing::Refuse)),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn cache_is_bounded_and_evicts_least_recent() {
        let d = tempfile::tempdir().unwrap();
        for n in ["a", "b", "c"] {
            std::fs::create_dir(d.path().join(n)).unwrap();
        }
        let root = Root::open_with_cache(d.path(), 2).unwrap();
        let a1 = root.dir(&r(b"a"), Missing::Refuse).unwrap();
        root.dir(&r(b"b"), Missing::Refuse).unwrap();
        root.dir(&r(b"c"), Missing::Refuse).unwrap();
        assert_eq!(root.cached(), 2);
        // "a" was the least recent, so it is opened again: a new handle.
        let a2 = root.dir(&r(b"a"), Missing::Refuse).unwrap();
        assert!(!Arc::ptr_eq(&a1.fd, &a2.fd));
        assert_eq!(root.cached(), 2);
        // "c" is still cached: the same handle comes back.
        let c1 = root.dir(&r(b"c"), Missing::Refuse).unwrap();
        let c2 = root.dir(&r(b"c"), Missing::Refuse).unwrap();
        assert!(Arc::ptr_eq(&c1.fd, &c2.fd));
    }

    #[test]
    fn a_held_dir_outlives_eviction() {
        let d = tempfile::tempdir().unwrap();
        for n in ["a", "b"] {
            std::fs::create_dir(d.path().join(n)).unwrap();
        }
        std::fs::write(d.path().join("a/f"), b"x").unwrap();
        let root = Root::open_with_cache(d.path(), 1).unwrap();
        let a = root.dir(&r(b"a"), Missing::Refuse).unwrap();
        root.dir(&r(b"b"), Missing::Refuse).unwrap();
        assert_eq!(root.cached(), 1);
        fstatat(&a, "f", AtFlags::AT_SYMLINK_NOFOLLOW).unwrap();
    }

    #[test]
    fn a_swapped_folder_stays_inside() {
        let d = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("d")).unwrap();
        let root = Root::open(d.path()).unwrap();
        let held = root.dir(&r(b"d"), Missing::Refuse).unwrap();
        std::fs::rename(d.path().join("d"), d.path().join("moved")).unwrap();
        symlink(out.path(), d.path().join("d")).unwrap();
        nix::sys::stat::mkdirat(&held, "made", Mode::from_bits_truncate(0o755)).unwrap();
        assert!(d.path().join("moved/made").is_dir());
        assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
    }

    #[test]
    fn forget_reopens_a_recreated_folder() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("d/sub")).unwrap();
        let root = Root::open(d.path()).unwrap();
        root.dir(&r(b"d/sub"), Missing::Refuse).unwrap();
        root.dir(&r(b"d"), Missing::Refuse).unwrap();
        std::fs::remove_dir(d.path().join("d/sub")).unwrap();
        std::fs::remove_dir(d.path().join("d")).unwrap();
        std::fs::create_dir(d.path().join("d")).unwrap();
        let new_ino = std::fs::metadata(d.path().join("d")).unwrap().ino();
        root.dir(&r(b"dd"), Missing::Create).unwrap();
        root.forget(&r(b"d"));
        // Only "d" and below went; the sibling "dd" stays cached.
        assert_eq!(root.cached(), 1);
        let fresh = root.dir(&r(b"d"), Missing::Refuse).unwrap();
        let got = nix::sys::stat::fstat(&fresh).unwrap().st_ino as u64;
        assert_eq!(got, new_ino);
    }

    #[test]
    fn a_tree_deeper_than_path_max_and_raw_byte_names() {
        let d = tempfile::tempdir().unwrap();
        let mut rel = Vec::new();
        let mut cur = Root::open(d.path()).unwrap();
        for i in 0..300 {
            // APFS refuses names that are not UTF-8, so macOS skips the raw name.
            let name: Vec<u8> = if i == 150 && !cfg!(target_os = "macos") {
                b"\xff\xfe".to_vec()
            } else {
                format!("{i:020}").into_bytes()
            };
            if !rel.is_empty() {
                rel.push(b'/');
            }
            rel.extend_from_slice(&name);
            // Build level by level with sub(), so no path longer than
            // PATH_MAX is ever handed to the kernel.
            let one = r(&name);
            cur = cur.sub(&one, Missing::Create).unwrap();
        }
        let root = Root::open(d.path()).unwrap();
        let bottom = root.dir(&r(&rel), Missing::Refuse).unwrap();
        assert_eq!(bottom.rel().as_bytes(), &rel[..]);
        assert!(rel.len() > 4096);
        // Same again with the cache cold, from a fresh root.
        let root2 = Root::open_with_cache(d.path(), 1).unwrap();
        root2.dir(&r(&rel), Missing::Refuse).unwrap();
    }

    fn os(s: &str) -> &OsStr {
        OsStr::new(s)
    }

    fn entry_with(kind: Kind, mode: u32, mtime: i64) -> FileEntry {
        FileEntry {
            kind: kind as i32,
            mode,
            mtime_sec: mtime,
            ..Default::default()
        }
    }

    /// A root over a fresh folder, and a second folder standing for "outside".
    fn setup() -> (tempfile::TempDir, tempfile::TempDir, Dir) {
        let d = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let dir = Root::open(d.path())
            .unwrap()
            .dir(&RelPath::root(), Missing::Refuse)
            .unwrap();
        (d, out, dir)
    }

    #[test]
    fn open_regular_refuses_symlink_fifo_and_folder() {
        let (d, out, dir) = setup();
        std::fs::write(d.path().join("f"), b"hi").unwrap();
        std::fs::write(out.path().join("o"), b"x").unwrap();
        symlink(out.path().join("o"), d.path().join("s")).unwrap();
        nix::unistd::mkfifo(&d.path().join("p"), Mode::from_bits_truncate(0o644)).unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        let (mut f, st) = dir.open_regular(os("f")).unwrap();
        assert_eq!(st.st_size, 2);
        let mut s = String::new();
        io::Read::read_to_string(&mut f, &mut s).unwrap();
        assert_eq!(s, "hi");
        let e = dir.open_regular(os("s")).err().unwrap();
        assert!(e.to_string().contains("refusing to follow symlink"), "{e}");
        for n in ["p", "sub"] {
            let e = dir.open_regular(os(n)).err().unwrap();
            assert!(e.to_string().contains("is not a regular file"), "{e}");
        }
    }

    #[test]
    fn open_rw_never_follows_a_symlink_at_the_name() {
        let (d, out, dir) = setup();
        std::fs::write(out.path().join("there"), b"x").unwrap();
        symlink(out.path().join("there"), d.path().join("a")).unwrap();
        symlink(out.path().join("missing"), d.path().join("b")).unwrap();
        for n in ["a", "b"] {
            for create in [false, true] {
                let e = dir.open_rw(os(n), create).err().unwrap();
                assert!(e.to_string().contains("symlink"), "{n} {create}: {e}");
            }
            assert!(dir.open_any(os(n)).is_err());
        }
        assert!(!out.path().join("missing").exists());
        assert_eq!(std::fs::read(out.path().join("there")).unwrap(), b"x");
        // Honest use: create, then reopen.
        dir.open_rw(os("new"), true).unwrap();
        dir.open_rw(os("new"), false).unwrap();
        dir.open_any(os("new")).unwrap();
        assert_eq!(
            kind(dir.open_rw(os("nope"), false)),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn names_with_a_slash_or_dots_are_refused() {
        let (d, _out, dir) = setup();
        let other = Root::open(d.path())
            .unwrap()
            .dir(&RelPath::root(), Missing::Refuse)
            .unwrap();
        let e = entry_with(Kind::Fifo, 0o644, 0);
        let rel = r(b"x");
        let opts = ApplyOpts::default();
        for n in ["a/b", "..", ".", ""] {
            let n = os(n);
            let bad = io::ErrorKind::InvalidInput;
            assert_eq!(kind(dir.open_regular(n)), bad, "{n:?}");
            assert_eq!(kind(dir.open_rw(n, true)), bad, "{n:?}");
            assert_eq!(kind(dir.open_any(n)), bad, "{n:?}");
            assert_eq!(kind(dir.stat(n)), bad, "{n:?}");
            assert_eq!(kind(dir.read_link(n)), bad, "{n:?}");
            assert_eq!(kind(dir.entry(n, &rel)), bad, "{n:?}");
            assert_eq!(kind(dir.mkdir_replacing(n)), bad, "{n:?}");
            assert_eq!(kind(dir.symlink(os("t"), n)), bad, "{n:?}");
            assert_eq!(kind(dir.create_special(n, &e)), bad, "{n:?}");
            assert_eq!(kind(dir.rename(n, &other, os("ok"))), bad, "{n:?}");
            assert_eq!(kind(dir.rename(os("ok"), &other, n)), bad, "{n:?}");
            assert_eq!(kind(dir.unlink(n)), bad, "{n:?}");
            assert_eq!(kind(dir.apply_node_metadata(n, &e, &opts)), bad, "{n:?}");
        }
    }

    #[test]
    fn mkdir_replacing_replaces_a_symlink_without_following() {
        let (d, out, dir) = setup();
        std::fs::write(out.path().join("keep"), b"x").unwrap();
        symlink(out.path(), d.path().join("n")).unwrap();
        std::fs::write(d.path().join("f"), b"x").unwrap();
        dir.mkdir_replacing(os("n")).unwrap();
        dir.mkdir_replacing(os("f")).unwrap();
        dir.mkdir_replacing(os("fresh")).unwrap();
        dir.mkdir_replacing(os("fresh")).unwrap();
        for n in ["n", "f", "fresh"] {
            let m = std::fs::symlink_metadata(d.path().join(n)).unwrap();
            assert!(m.is_dir(), "{n}");
        }
        assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 1);
        assert!(out.path().join("keep").exists());
    }

    #[test]
    fn rename_moves_between_two_dirs() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("a")).unwrap();
        std::fs::create_dir(d.path().join("b")).unwrap();
        std::fs::write(d.path().join("a/f"), b"x").unwrap();
        let root = Root::open(d.path()).unwrap();
        let a = root.dir(&r(b"a"), Missing::Refuse).unwrap();
        let b = root.dir(&r(b"b"), Missing::Refuse).unwrap();
        a.rename(os("f"), &b, os("g")).unwrap();
        assert!(!d.path().join("a/f").exists());
        assert_eq!(std::fs::read(d.path().join("b/g")).unwrap(), b"x");
        b.symlink(os("g"), os("l")).unwrap();
        assert_eq!(b.read_link(os("l")).unwrap(), b"g");
        b.unlink(os("l")).unwrap();
        b.unlink(os("g")).unwrap();
        assert_eq!(kind(b.unlink(os("g"))), io::ErrorKind::NotFound);
    }

    #[test]
    fn entry_matches_lstat_entry_with_ctime() {
        use crate::fs::meta::lstat_entry_with_ctime;
        let (d, out, dir) = setup();
        std::fs::write(d.path().join("file"), b"hello").unwrap();
        std::fs::create_dir(d.path().join("folder")).unwrap();
        symlink(out.path(), d.path().join("link")).unwrap();
        nix::unistd::mkfifo(&d.path().join("fifo"), Mode::from_bits_truncate(0o644)).unwrap();
        for n in ["file", "folder", "link", "fifo"] {
            let rel = r(n.as_bytes());
            let want = lstat_entry_with_ctime(d.path(), &rel).unwrap();
            assert_eq!(dir.entry(os(n), &rel).unwrap(), want, "{n}");
        }
        let link = dir.entry(os("link"), &r(b"link")).unwrap().0;
        assert_eq!(link.link_target, out.path().as_os_str().as_bytes());
    }

    #[test]
    fn create_special_makes_a_fifo() {
        let (d, _out, dir) = setup();
        let e = entry_with(Kind::Fifo, 0o644, 0);
        dir.create_special(os("p"), &e).unwrap();
        let m = std::fs::symlink_metadata(d.path().join("p")).unwrap();
        assert!(std::os::unix::fs::FileTypeExt::is_fifo(&m.file_type()));
        assert_eq!(
            kind(dir.create_special(os("q"), &entry_with(Kind::Regular, 0o644, 0))),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn apply_metadata_sets_the_folder_through_its_handle() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        let root = Root::open(d.path()).unwrap();
        let sub = root.dir(&r(b"sub"), Missing::Refuse).unwrap();
        let e = entry_with(Kind::Directory, 0o750, 1_600_000_000);
        let opts = ApplyOpts {
            perms: true,
            times: true,
            ..Default::default()
        };
        sub.apply_metadata(&e, &opts).unwrap();
        let m = std::fs::metadata(d.path().join("sub")).unwrap();
        assert_eq!(m.permissions().mode() & 0o7777, 0o750);
        assert_eq!(m.mtime(), 1_600_000_000);
    }

    #[test]
    fn apply_node_metadata_never_reaches_a_symlink_target() {
        let (d, out, dir) = setup();
        let target = out.path().join("o");
        std::fs::write(&target, b"x").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        nix::sys::stat::utimensat(
            AT_FDCWD,
            &target,
            &TimeSpec::new(1, 0),
            &TimeSpec::new(1, 0),
            nix::sys::stat::UtimensatFlags::FollowSymlink,
        )
        .unwrap();
        symlink(&target, d.path().join("n")).unwrap();
        let opts = ApplyOpts {
            perms: true,
            times: true,
            ..Default::default()
        };
        // The entry says FIFO, but the name is a symlink: nothing may follow it.
        let _ = dir.apply_node_metadata(os("n"), &entry_with(Kind::Fifo, 0o644, 2), &opts);
        let m = std::fs::metadata(&target).unwrap();
        assert_eq!(m.permissions().mode() & 0o7777, 0o600);
        assert_eq!(m.mtime(), 1);
        // Honest use: a real FIFO gets its mode and time.
        let fifo = entry_with(Kind::Fifo, 0o640, 5);
        dir.create_special(os("p"), &fifo).unwrap();
        dir.apply_node_metadata(os("p"), &fifo, &opts).unwrap();
        let m = std::fs::symlink_metadata(d.path().join("p")).unwrap();
        assert_eq!(m.permissions().mode() & 0o7777, 0o640);
        assert_eq!(m.mtime(), 5);
    }
}
