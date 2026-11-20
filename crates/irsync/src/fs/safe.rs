//! Symlink-safe file access: opening regular files without following a
//! final symlink, and resolving directories below a root component by
//! component without following any symlink on the way.

use std::fs::File;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use nix::fcntl::{openat, OFlag, AT_FDCWD};
use nix::sys::stat::{fstat, FileStat, Mode, SFlag};

use crate::fs::paths::RelPath;

pub(crate) fn errno(e: nix::Error) -> io::Error {
    io::Error::from_raw_os_error(e as i32)
}

/// True for the error an `O_NOFOLLOW` open gives when the final component
/// is a symlink: `ELOOP`, except on FreeBSD, which says `EMLINK`.
pub(crate) fn is_symlink_errno(e: nix::Error) -> bool {
    e == nix::Error::ELOOP || (cfg!(target_os = "freebsd") && e == nix::Error::EMLINK)
}

/// Turns the error of an `O_NOFOLLOW` open of a symlink (see
/// [`is_symlink_errno`]) into one that says so.
pub fn nofollow_error(e: io::Error, what: &Path) -> io::Error {
    if e.raw_os_error()
        .is_some_and(|n| is_symlink_errno(nix::Error::from_raw(n)))
    {
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
        let name = std::ffi::OsStr::from_bytes(c);
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
    Ok((dir, std::ffi::OsStr::from_bytes(&name).to_owned()))
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
