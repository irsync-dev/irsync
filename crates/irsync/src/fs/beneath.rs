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
//! caller holds stays valid after the cache drops it.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use nix::errno::Errno;
use nix::fcntl::{openat, OFlag, AT_FDCWD};
use nix::sys::stat::{mkdirat, Mode};

use crate::fs::paths::RelPath;
use crate::fs::safe::{errno, is_symlink_errno, split_last};
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

impl Dir {
    /// Where this folder is, relative to the root it came from (for messages).
    pub fn rel(&self) -> &RelPath {
        &self.rel
    }
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
        let old = root.dir(&r(b"d"), Missing::Refuse).unwrap();
        let old_ino = nix::sys::stat::fstat(&old).unwrap().st_ino as u64;
        std::fs::remove_dir(d.path().join("d/sub")).unwrap();
        std::fs::remove_dir(d.path().join("d")).unwrap();
        std::fs::create_dir(d.path().join("d")).unwrap();
        let new_ino = std::fs::metadata(d.path().join("d")).unwrap().ino();
        root.forget(&r(b"d"));
        assert_eq!(root.cached(), 0);
        let fresh = root.dir(&r(b"d"), Missing::Refuse).unwrap();
        let got = nix::sys::stat::fstat(&fresh).unwrap().st_ino as u64;
        assert_eq!(got, new_ino);
        // The old inode may be reused by the filesystem; the point is that
        // the handle is the new folder's, which the line above proves.
        let _ = old_ino;
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
}
