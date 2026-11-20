//! Small helpers for symlink-safe file access: the error a refused
//! symlink gives, the regular-file check and splitting a relative path.
//! Files and folders below a root are opened through [`super::beneath`].

use std::fs::File;
use std::io;
use std::path::Path;

use nix::sys::stat::{fstat, FileStat, SFlag};

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
pub(crate) fn require_regular_stat(f: &File, what: &Path) -> io::Result<FileStat> {
    let st = fstat(f).map_err(errno)?;
    if !is_regular_mode(st.st_mode as u32) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", what.display()),
        ));
    }
    Ok(st)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_last_splits_off_the_final_name() {
        let r = |s: &[u8]| RelPath::new(s).unwrap();
        let (p, n) = split_last(&r(b"a/b/c")).unwrap();
        assert_eq!((p.as_bytes(), &n[..]), (&b"a/b"[..], &b"c"[..]));
        let (p, n) = split_last(&r(b"c")).unwrap();
        assert_eq!((p.as_bytes(), &n[..]), (&b""[..], &b"c"[..]));
        assert!(split_last(&RelPath::root()).is_err());
    }
}
