//! Symlink-safe file access: opening regular files without following a
//! final symlink, and resolving directories below a root component by
//! component without following any symlink on the way.

use std::fs::File;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use nix::fcntl::{openat, OFlag, AT_FDCWD};
use nix::sys::stat::{fstat, Mode, SFlag};

use crate::fs::paths::RelPath;

pub(crate) fn errno(e: nix::Error) -> io::Error {
    io::Error::from_raw_os_error(e as i32)
}

/// `ELOOP` from an `O_NOFOLLOW` open means the final component is a symlink.
pub fn nofollow_error(e: io::Error, what: &Path) -> io::Error {
    if e.raw_os_error() == Some(nix::libc::ELOOP) {
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
    let st = fstat(f).map_err(errno)?;
    if !is_regular_mode(st.st_mode as u32) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", what.display()),
        ));
    }
    Ok(())
}

/// Opens `path` read-only, refusing a symlink as the final component and
/// anything that is not a regular file (`O_NONBLOCK` so a FIFO planted
/// there cannot block the open; it has no effect on regular files).
pub fn open_regular_nofollow(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| nofollow_error(e, path))?;
    require_regular(&f, path)?;
    Ok(f)
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
            if matches!(e, nix::Error::ELOOP | nix::Error::ENOTDIR) {
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

#[cfg(test)]
mod tests {
    use super::*;

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
