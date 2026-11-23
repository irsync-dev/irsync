//! The Unix implementation: `nix`, `libc` and `std::os::unix`.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use nix::libc;

use super::Cause;

/// What `e` means, from its raw error number. An error without one is
/// [`Cause::Other`].
pub fn cause(e: &io::Error) -> Cause {
    let Some(n) = e.raw_os_error() else {
        return Cause::Other;
    };
    // Comparisons, not a `match`: some of these numbers are equal on some
    // systems (`ENOTSUP` and `EOPNOTSUPP` on Linux and FreeBSD), which a
    // `match` would report as an unreachable pattern.
    let is = |codes: &[i32]| codes.contains(&n);
    // An `O_NOFOLLOW` open of a symlink fails with `ELOOP`, except on
    // FreeBSD, which says `EMLINK`.
    if n == libc::ELOOP || (cfg!(target_os = "freebsd") && n == libc::EMLINK) {
        Cause::LinkRefused
    } else if n == libc::ENOENT {
        Cause::NotThere
    } else if n == libc::EEXIST {
        Cause::AlreadyThere
    } else if n == libc::ENOTDIR {
        Cause::NotADirectory
    } else if n == libc::EISDIR {
        Cause::IsADirectory
    } else if n == libc::ENOTEMPTY {
        Cause::NotEmpty
    } else if n == libc::EXDEV {
        Cause::CrossDevice
    } else if is(&[libc::EMFILE, libc::ENFILE]) {
        Cause::OutOfHandles
    } else if is(&[libc::EOPNOTSUPP, libc::ENOTSUP, libc::ENOSYS]) {
        Cause::NotSupported
    } else {
        Cause::Other
    }
}

/// Whether `e` is a momentary shortage worth waiting out that no [`Cause`]
/// names: no resources for a non-blocking call (`EAGAIN`), no memory, no
/// buffer space.
pub fn is_transient_extra(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::EAGAIN | libc::ENOMEM | libc::ENOBUFS)
    )
}

/// The stored bytes of a name.
pub fn os_to_bytes(name: &OsStr) -> Vec<u8> {
    name.as_bytes().to_vec()
}

/// The name that stored bytes stand for. Never fails on Unix: any bytes
/// are a name.
pub fn bytes_to_os(bytes: &[u8]) -> io::Result<OsString> {
    Ok(OsString::from_vec(bytes.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn causes_from_error_numbers() {
        let freebsd = cfg!(target_os = "freebsd");
        let cases = [
            (libc::ELOOP, Cause::LinkRefused),
            (
                libc::EMLINK,
                if freebsd {
                    Cause::LinkRefused
                } else {
                    Cause::Other
                },
            ),
            (libc::ENOENT, Cause::NotThere),
            (libc::EEXIST, Cause::AlreadyThere),
            (libc::ENOTDIR, Cause::NotADirectory),
            (libc::EISDIR, Cause::IsADirectory),
            (libc::ENOTEMPTY, Cause::NotEmpty),
            (libc::EXDEV, Cause::CrossDevice),
            (libc::EMFILE, Cause::OutOfHandles),
            (libc::ENFILE, Cause::OutOfHandles),
            (libc::EOPNOTSUPP, Cause::NotSupported),
            (libc::ENOTSUP, Cause::NotSupported),
            (libc::ENOSYS, Cause::NotSupported),
            (libc::EACCES, Cause::Other),
            (libc::EAGAIN, Cause::Other),
            (libc::ENOSPC, Cause::Other),
        ];
        for (n, want) in cases {
            assert_eq!(cause(&io::Error::from_raw_os_error(n)), want, "{n}");
        }
        assert_eq!(cause(&io::Error::other("x")), Cause::Other);
    }

    #[test]
    fn extra_transient_errors_are_the_shortages_no_cause_names() {
        for n in [libc::EAGAIN, libc::ENOMEM, libc::ENOBUFS] {
            assert!(is_transient_extra(&io::Error::from_raw_os_error(n)), "{n}");
        }
        for n in [libc::EMFILE, libc::ENFILE, libc::EACCES, libc::ENOSPC] {
            assert!(!is_transient_extra(&io::Error::from_raw_os_error(n)), "{n}");
        }
        assert!(!is_transient_extra(&io::Error::other("x")));
    }

    #[test]
    fn names_and_bytes_round_trip() {
        for b in [&b"plain"[..], b"caf\xe9", b"\xff\xfe", b""] {
            let os = bytes_to_os(b).unwrap();
            assert_eq!(os_to_bytes(&os), b);
        }
    }
}
