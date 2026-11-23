//! Symlink-safe file access: opening regular files without following a
//! final symlink, and resolving directories below a root component by
//! component without following any symlink on the way.

use std::fs::File;
use std::io;
use std::os::fd::OwnedFd;
use std::path::Path;

use nix::fcntl::{openat, OFlag, AT_FDCWD};
use nix::sys::stat::{fstat, FileStat, Mode, SFlag};

use crate::fs::paths::RelPath;
use crate::platform::{self, bytes_to_os, Cause};

pub(crate) fn errno(e: nix::Error) -> io::Error {
    io::Error::from_raw_os_error(e as i32)
}

/// True for the error an `O_NOFOLLOW` open gives when the final component
/// is a symlink: [`Cause::LinkRefused`] (`ELOOP`, except on FreeBSD, which
/// says `EMLINK`).
pub(crate) fn is_symlink_errno(e: nix::Error) -> bool {
    platform::cause(&errno(e)) == Cause::LinkRefused
}

/// Turns the error of an `O_NOFOLLOW` open of a symlink (see
/// [`is_symlink_errno`]) into one that says so.
pub fn nofollow_error(e: io::Error, what: &Path) -> io::Error {
    if platform::cause(&e) == Cause::LinkRefused {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to follow symlink {}", what.display()),
        )
    } else {
        e
    }
}

/// Whether an `fstat` mode is a regular file.
pub fn is_regular_mode(mode: u32) -> bool {
    SFlag::from_bits_truncate(mode as _) & SFlag::S_IFMT == SFlag::S_IFREG
}

/// Whether an `fstat` mode is a directory.
pub fn is_dir_mode(mode: u32) -> bool {
    SFlag::from_bits_truncate(mode as _) & SFlag::S_IFMT == SFlag::S_IFDIR
}

/// Fails unless the open file is a regular file.
pub fn require_regular(f: &File, what: &Path) -> io::Result<()> {
    require_regular_stat(f, what).map(drop)
}

/// [`require_regular`], returning the file's `fstat`.
fn require_regular_stat(f: &File, what: &Path) -> io::Result<FileStat> {
    let st = fstat(f).map_err(errno)?;
    if !is_regular_mode(st.st_mode as u32) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", what.display()),
        ));
    }
    Ok(st)
}

/// Opens `path` read-only, refusing a symlink as the final component and
/// anything that is not a regular file (`O_NONBLOCK` so a FIFO planted
/// there cannot block the open; it has no effect on regular files). A
/// momentary descriptor shortage is waited out (see [`super::transient`]).
pub fn open_regular_nofollow(path: &Path) -> io::Result<File> {
    open_regular_nofollow_stat(path).map(|(f, _)| f)
}

/// [`open_regular_nofollow`], also returning the open file's `fstat`.
pub fn open_regular_nofollow_stat(path: &Path) -> io::Result<(File, FileStat)> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = super::transient::retry(|| {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
            .open(path)
    })
    .map_err(|e| nofollow_error(e, path))?;
    let st = require_regular_stat(&f, path)?;
    Ok((f, st))
}

/// Splits a non-root relative path into its parent and final name.
pub fn split_last(rel: &RelPath) -> io::Result<(RelPath, Vec<u8>)> {
    let b = rel.as_bytes();
    if b.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty relative path",
        ));
    }
    let (parent, name) = match b.iter().rposition(|&c| c == b'/') {
        Some(i) => (&b[..i], &b[i + 1..]),
        None => (&b[..0], b),
    };
    let parent = RelPath::new(parent)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid relative path"))?;
    Ok((parent, name.to_vec()))
}

/// Opens the directory `root/dir`: `root` itself is opened normally (it
/// may be reached through symlinks the user chose), every component of
/// `dir` with `O_NOFOLLOW`, so a symlink anywhere below `root` is refused.
pub fn open_dir_beneath(root: &Path, dir: &RelPath) -> io::Result<OwnedFd> {
    let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC;
    let mut fd = openat(AT_FDCWD, root, flags, Mode::empty()).map_err(errno)?;
    for c in dir.components() {
        let owned = bytes_to_os(c)?;
        let name = owned.as_os_str();
        fd = openat(&fd, name, flags | OFlag::O_NOFOLLOW, Mode::empty()).map_err(|e| {
            if is_symlink_errno(e) || e == nix::Error::ENOTDIR {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "{}: a path component is a symlink or not a directory",
                        String::from_utf8_lossy(dir.as_bytes())
                    ),
                )
            } else {
                errno(e)
            }
        })?;
    }
    Ok(fd)
}

/// The directories on the way to the entry a walk is at, held open.
///
/// A walk visits a tree in path order, so most entries share their
/// directory, or most of the way to it, with the one before. This keeps
/// the chain of directories from `base` down to the current one open,
/// each opened from its parent without following a symlink, and reopens
/// only what differs for the next entry.
pub struct DirStack {
    base: OwnedFd,
    below: Vec<(Vec<u8>, OwnedFd)>,
}

impl DirStack {
    /// `base` itself is opened by name (it may be reached through symlinks
    /// the user chose).
    pub fn new(base: &Path) -> io::Result<DirStack> {
        let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC;
        Ok(DirStack {
            base: openat(AT_FDCWD, base, flags, Mode::empty()).map_err(errno)?,
            below: Vec::new(),
        })
    }

    /// The directory `dir` below the base, with no symlink followed.
    pub fn dir(&mut self, dir: &RelPath) -> io::Result<std::os::fd::BorrowedFd<'_>> {
        use std::os::fd::AsFd;
        let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW;
        let comps: Vec<&[u8]> = dir.components().collect();
        let keep = self
            .below
            .iter()
            .zip(&comps)
            .take_while(|((have, _), want)| have == *want)
            .count();
        self.below.truncate(keep);
        for c in &comps[keep..] {
            let parent = self
                .below
                .last()
                .map_or(self.base.as_fd(), |(_, fd)| fd.as_fd());
            let name = bytes_to_os(c)?;
            let fd = openat(parent, name.as_os_str(), flags, Mode::empty()).map_err(|e| {
                if is_symlink_errno(e) || e == nix::Error::ENOTDIR {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "a path component is a symlink or not a directory",
                    )
                } else {
                    errno(e)
                }
            })?;
            self.below.push((c.to_vec(), fd));
        }
        Ok(self
            .below
            .last()
            .map_or(self.base.as_fd(), |(_, fd)| fd.as_fd()))
    }
}

/// Opens the directory that holds `base/rel` (see [`open_dir_beneath`]:
/// no symlink below `base` is followed) and gives `rel`'s final name.
///
/// A path checked once and used by name later can lead somewhere else by
/// then: whoever can change the tree, the other side of a second session
/// included, may have swapped a directory on the way for a symlink.
/// Everything a session does to a path from the other side therefore
/// starts here and goes on from the descriptor.
pub fn open_parent_beneath(
    base: &Path,
    rel: &RelPath,
) -> io::Result<(OwnedFd, std::ffi::OsString)> {
    let (parent, name) = split_last(rel)?;
    let dir = open_dir_beneath(base, &parent)?;
    Ok((dir, bytes_to_os(&name)?))
}

/// Creates the directory `root/dir` and what is missing above it, without
/// following a symlink below `root`: a symlink where a directory should be
/// is an error, never a place to go on from. New directories get `mode`
/// (less the umask).
pub fn create_dir_all_beneath(root: &Path, dir: &RelPath, mode: u32) -> io::Result<OwnedFd> {
    let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC;
    let mut fd = openat(AT_FDCWD, root, flags, Mode::empty()).map_err(errno)?;
    for c in dir.components() {
        let owned = bytes_to_os(c)?;
        let name = owned.as_os_str();
        let open = |fd: &OwnedFd| openat(fd, name, flags | OFlag::O_NOFOLLOW, Mode::empty());
        let next = match open(&fd) {
            Err(nix::Error::ENOENT) => {
                match nix::sys::stat::mkdirat(&fd, name, Mode::from_bits_truncate(mode as _)) {
                    // Made by someone else meanwhile: fine, if it is a directory.
                    Ok(()) | Err(nix::Error::EEXIST) => open(&fd),
                    Err(e) => Err(e),
                }
            }
            r => r,
        };
        fd = next.map_err(|e| {
            if is_symlink_errno(e) || e == nix::Error::ENOTDIR {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "{}: a path component is a symlink or not a directory",
                        String::from_utf8_lossy(dir.as_bytes())
                    ),
                )
            } else {
                errno(e)
            }
        })?;
    }
    Ok(fd)
}

/// A place a session writes to: `rel` below `root`. `root` is a name this
/// side trusts (the export's directory, the user's destination); `rel`
/// comes from the other side. Every operation here opens `rel`'s directory
/// anew from `root` without following a symlink (see
/// [`open_parent_beneath`]) and works on the final name from that
/// descriptor, so a directory swapped for a symlink makes the operation
/// fail instead of leading it elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Beneath {
    pub root: std::path::PathBuf,
    pub rel: RelPath,
}

impl Beneath {
    pub fn new(root: &Path, rel: RelPath) -> Beneath {
        Beneath {
            root: root.to_path_buf(),
            rel,
        }
    }

    /// The place as a path name, for messages and logs only.
    pub fn path(&self) -> std::path::PathBuf {
        if self.rel.as_bytes().is_empty() {
            self.root.clone()
        } else {
            self.root.join(self.rel.to_os())
        }
    }

    /// The directory that holds the place, and the place's name in it.
    pub fn parent(&self) -> io::Result<(OwnedFd, std::ffi::OsString)> {
        open_parent_beneath(&self.root, &self.rel)
    }

    /// Makes the directory that holds the place, and what is missing
    /// above, the root included (a name this side trusts, made by name).
    pub fn make_parent(&self) -> io::Result<()> {
        let (dir, _) = split_last(&self.rel)?;
        if !self.root.is_dir() {
            std::fs::create_dir_all(&self.root)?;
        }
        create_dir_all_beneath(&self.root, &dir, 0o777).map(drop)
    }

    /// `lstat` of the place.
    pub fn lstat(&self) -> io::Result<FileStat> {
        let (dir, name) = self.parent()?;
        nix::sys::stat::fstatat(
            &dir,
            name.as_os_str(),
            nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW,
        )
        .map_err(errno)
    }

    /// Opens the place with `flags` (never following a symlink there).
    pub fn open(&self, flags: OFlag, mode: u32) -> io::Result<File> {
        let (dir, name) = self.parent()?;
        openat(
            &dir,
            name.as_os_str(),
            flags | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(mode as _),
        )
        .map(File::from)
        .map_err(|e| nofollow_error(errno(e), &self.path()))
    }

    /// Removes the file at the place.
    pub fn unlink(&self) -> io::Result<()> {
        let (dir, name) = self.parent()?;
        nix::unistd::unlinkat(
            &dir,
            name.as_os_str(),
            nix::unistd::UnlinkatFlags::NoRemoveDir,
        )
        .map_err(errno)
    }

    /// Renames what is at the place to `to`.
    pub fn rename_to(&self, to: &Beneath) -> io::Result<()> {
        let (from_dir, from_name) = self.parent()?;
        let (to_dir, to_name) = to.parent()?;
        nix::fcntl::renameat(
            &from_dir,
            from_name.as_os_str(),
            &to_dir,
            to_name.as_os_str(),
        )
        .map_err(errno)
    }
}

/// [`open_regular_nofollow`] for `base/rel`, with no symlink followed in
/// any component of `rel` (see [`open_parent_beneath`]).
pub fn open_regular_beneath(base: &Path, rel: &RelPath) -> io::Result<File> {
    open_regular_beneath_stat(base, rel).map(|(f, _)| f)
}

/// [`open_regular_beneath`], also returning the open file's `fstat`.
pub fn open_regular_beneath_stat(base: &Path, rel: &RelPath) -> io::Result<(File, FileStat)> {
    let what = base.join(rel.to_os());
    let flags = OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC;
    let fd = super::transient::retry(|| {
        let (dir, name) = open_parent_beneath(base, rel)?;
        openat(&dir, name.as_os_str(), flags, Mode::empty()).map_err(errno)
    })
    .map_err(|e| nofollow_error(e, &what))?;
    let f = File::from(fd);
    let st = require_regular_stat(&f, &what)?;
    Ok((f, st))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_regular_beneath_follows_no_symlink_on_the_way() {
        let (d, outside) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::create_dir_all(d.path().join("a/b")).unwrap();
        std::fs::write(d.path().join("a/b/f"), b"inside").unwrap();
        std::fs::write(outside.path().join("f"), b"outside").unwrap();
        let rel = |p: &str| RelPath::new(p.as_bytes()).unwrap();
        let read = |r: &str| {
            use std::io::Read;
            let mut s = String::new();
            open_regular_beneath(d.path(), &rel(r)).map(|mut f| {
                f.read_to_string(&mut s).unwrap();
                s
            })
        };
        assert_eq!(read("a/b/f").unwrap(), "inside");
        // The directory is swapped for a link to one outside.
        std::fs::rename(d.path().join("a/b"), d.path().join("a/moved")).unwrap();
        std::os::unix::fs::symlink(outside.path(), d.path().join("a/b")).unwrap();
        assert_eq!(
            std::fs::read(d.path().join("a/b/f")).unwrap(),
            b"outside",
            "by name, the path now leads outside"
        );
        let e = read("a/b/f").unwrap_err();
        assert!(e.to_string().contains("symlink"), "{e}");
        // A link as the final name, a directory, and the root are refused.
        std::os::unix::fs::symlink(outside.path().join("f"), d.path().join("l")).unwrap();
        assert!(read("l").unwrap_err().to_string().contains("symlink"));
        assert!(read("a").is_err());
        assert!(open_regular_beneath(d.path(), &RelPath::root()).is_err());
    }

    #[test]
    fn a_place_beneath_a_root_is_never_reached_through_a_symlink() {
        let (d, outside) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let rel = |p: &str| RelPath::new(p.as_bytes()).unwrap();
        let at = |p: &str| Beneath::new(d.path(), rel(p));
        let rw = OFlag::O_RDWR | OFlag::O_CREAT;

        // Missing directories are made; the file is created and renamed.
        at("a/b/part").make_parent().unwrap();
        at("a/b/part").open(rw, 0o600).unwrap();
        assert!(at("a/b/part").lstat().is_ok());
        at("a/b/part").rename_to(&at("a/f")).unwrap();
        assert!(d.path().join("a/f").is_file());
        at("a/f").unlink().unwrap();
        assert!(!d.path().join("a/f").exists());

        // `a/b` becomes a link out of the tree: nothing goes through it.
        std::fs::remove_dir(d.path().join("a/b")).unwrap();
        std::os::unix::fs::symlink(outside.path(), d.path().join("a/b")).unwrap();
        std::fs::write(outside.path().join("victim"), b"keep").unwrap();
        assert!(at("a/b/part").open(rw, 0o600).is_err());
        assert!(at("a/b/new/part").make_parent().is_err());
        assert!(at("a/b/victim").lstat().is_err());
        assert!(at("a/b/victim").unlink().is_err());
        std::fs::write(d.path().join("a/src"), b"new").unwrap();
        assert!(at("a/src").rename_to(&at("a/b/victim")).is_err());
        assert!(at("a/b/victim").rename_to(&at("a/stolen")).is_err());
        let left: Vec<_> = std::fs::read_dir(outside.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, ["victim"]);
        assert_eq!(
            std::fs::read(outside.path().join("victim")).unwrap(),
            b"keep"
        );

        // A link as the final name is not opened either.
        std::os::unix::fs::symlink(outside.path().join("victim"), d.path().join("l")).unwrap();
        assert!(at("l").open(OFlag::O_RDWR, 0).is_err());
    }

    #[test]
    fn open_regular_nofollow_refuses_symlinks_and_non_files() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("f"), b"x").unwrap();
        std::os::unix::fs::symlink(d.path().join("f"), d.path().join("l")).unwrap();
        assert!(open_regular_nofollow(&d.path().join("f")).is_ok());
        let e = open_regular_nofollow(&d.path().join("l")).unwrap_err();
        assert!(e.to_string().contains("refusing to follow symlink"), "{e}");
        assert!(open_regular_nofollow(d.path()).is_err());
    }

    #[test]
    fn open_dir_beneath_refuses_symlinked_components() {
        let d = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("a/b")).unwrap();
        std::os::unix::fs::symlink(out.path(), d.path().join("esc")).unwrap();
        let r = |s: &[u8]| RelPath::new(s).unwrap();
        assert!(open_dir_beneath(d.path(), &r(b"a/b")).is_ok());
        assert!(open_dir_beneath(d.path(), &RelPath::root()).is_ok());
        assert!(open_dir_beneath(d.path(), &r(b"esc")).is_err());
        assert!(open_dir_beneath(d.path(), &r(b"a/missing")).is_err());
        let (p, n) = split_last(&r(b"a/b/c")).unwrap();
        assert_eq!((p.as_bytes(), &n[..]), (&b"a/b"[..], &b"c"[..]));
        let (p, n) = split_last(&r(b"c")).unwrap();
        assert_eq!((p.as_bytes(), &n[..]), (&b""[..], &b"c"[..]));
    }
}
