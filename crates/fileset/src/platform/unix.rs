//! The Unix implementation: `std::os::unix`, `libc` and `xattr`.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{Stat, Xattrs};
use crate::model::{Kind, Timespec};

/// Reads the metadata of `abs` without following a final symlink.
pub fn lstat(abs: &Path) -> io::Result<Stat> {
    let md = std::fs::symlink_metadata(abs)?;
    let ft = md.file_type();
    let kind = if ft.is_dir() {
        Kind::Dir
    } else if ft.is_file() {
        Kind::File
    } else if ft.is_symlink() {
        Kind::Symlink
    } else if ft.is_fifo() {
        Kind::Fifo
    } else if ft.is_socket() {
        Kind::Socket
    } else if ft.is_char_device() {
        Kind::CharDev
    } else if ft.is_block_device() {
        Kind::BlockDev
    } else {
        Kind::Other
    };
    Ok(Stat {
        kind,
        mode: md.mode() & 0o7777,
        uid: md.uid(),
        gid: md.gid(),
        size: md.size(),
        nlink: md.nlink(),
        ino: md.ino(),
        dev: md.dev(),
        rdev: md.rdev(),
        mtime: Timespec {
            sec: md.mtime(),
            nsec: md.mtime_nsec() as u32,
        },
        ctime: Timespec {
            sec: md.ctime(),
            nsec: md.ctime_nsec() as u32,
        },
    })
}

/// The target of the symlink `abs`, as the bytes it holds.
pub fn read_link(abs: &Path) -> io::Result<Vec<u8>> {
    Ok(std::fs::read_link(abs)?.into_os_string().into_vec())
}

fn is_unsupported(e: &io::Error) -> bool {
    // 95 = EOPNOTSUPP on Linux, 45 = ENOTSUP on macOS.
    e.kind() == io::ErrorKind::Unsupported || matches!(e.raw_os_error(), Some(95) | Some(45))
}

/// Reads the extended attributes of `abs` (not following symlinks), sorted by name, with
/// the first failure other than "not supported", if any.
pub fn read_xattrs(abs: &Path) -> (Xattrs, Option<io::Error>) {
    collect_xattrs(xattr::list(abs), |name| xattr::get(abs, name))
}

/// Gathers xattrs from a listing and a getter. "Not supported" ends the gathering silently;
/// any other failure ends it and is returned. Either way the attributes read so far are kept.
fn collect_xattrs<I>(
    names: io::Result<I>,
    mut get: impl FnMut(&OsStr) -> io::Result<Option<Vec<u8>>>,
) -> (Xattrs, Option<io::Error>)
where
    I: IntoIterator<Item = OsString>,
{
    let names = match names {
        Ok(n) => n,
        Err(e) if is_unsupported(&e) => return (Vec::new(), None),
        Err(e) => return (Vec::new(), Some(e)),
    };
    let mut out = Vec::new();
    let mut err = None;
    for name in names {
        match get(&name) {
            Ok(Some(v)) => out.push((name.into_vec(), v)),
            Ok(None) => {}
            Err(e) if is_unsupported(&e) => break,
            Err(e) => {
                err = Some(e);
                break;
            }
        }
    }
    out.sort();
    (out, err)
}

/// Whether `md`, read without following a final link, is that of a link.
pub fn is_link(md: &std::fs::Metadata) -> bool {
    md.is_symlink()
}

/// Opens `abs` read-only. A symlink there is not followed and a FIFO does not wait for a
/// writer.
pub fn open_nofollow(abs: &Path) -> io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(abs)
}

/// The stored bytes of a name.
pub fn os_to_bytes(name: &OsStr) -> Vec<u8> {
    name.as_bytes().to_vec()
}

/// The name that stored bytes stand for.
pub fn bytes_to_os(bytes: &[u8]) -> io::Result<OsString> {
    Ok(OsString::from_vec(bytes.to_vec()))
}

/// The path below `root` of the stored, `/`-separated relative path `rel`.
pub fn rel_to_path(root: &Path, rel: &[u8]) -> io::Result<PathBuf> {
    Ok(root.join(OsStr::from_bytes(rel)))
}

/// The stored bytes of the relative path `rel`.
pub fn path_to_rel(rel: &Path) -> Vec<u8> {
    rel.as_os_str().as_bytes().to_vec()
}

/// Whether `e` is a read refused because another handle holds a byte-range
/// lock on what it reads. Never: such locks are advisory on Unix.
pub fn is_lock_violation(_e: &io::Error) -> bool {
    false
}

/// CPU time the calling thread has used so far (user and system), or `None`
/// if the clock cannot be read.
pub fn thread_cpu_time() -> Option<Duration> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `&mut ts` points to a local, initialised timespec that
    // outlives the call; clock_gettime writes only that one struct.
    let ret = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    if ret != 0 {
        return None;
    }
    Some(Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32))
}

/// Apple silicon: `hw.perflevel0` is the performance cores, the levels after
/// it are efficiency cores.
#[cfg(target_os = "macos")]
pub fn core_split() -> Option<(usize, usize)> {
    fn sysctl_u32(name: &std::ffi::CStr) -> Option<u32> {
        let mut value: u32 = 0;
        let mut size = std::mem::size_of::<u32>();
        // SAFETY: `name` is a CStr, so NUL-terminated. `value` is a local
        // u32 and `size` holds its length, so sysctlbyname writes at most
        // 4 bytes there (and the size it wrote into `size`). No new value
        // is set: null and 0.
        let ret = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                (&mut value as *mut u32).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        (ret == 0 && size == std::mem::size_of::<u32>()).then_some(value)
    }
    if sysctl_u32(c"hw.nperflevels")? < 2 {
        return None;
    }
    let p = sysctl_u32(c"hw.perflevel0.logicalcpu")? as usize;
    let e = sysctl_u32(c"hw.perflevel1.logicalcpu")? as usize;
    (p > 0 && e > 0).then_some((p, e))
}

/// Intel hybrid CPUs list their performance and efficiency cores as two PMU
/// devices.
#[cfg(target_os = "linux")]
pub fn core_split() -> Option<(usize, usize)> {
    let count = |path: &str| {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| crate::cpu::cpu_list_len(&s))
    };
    let p = count("/sys/devices/cpu_core/cpus")?;
    let e = count("/sys/devices/cpu_atom/cpus")?;
    (p > 0 && e > 0).then_some((p, e))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn core_split() -> Option<(usize, usize)> {
    None
}

/// Helpers for the tests, here and in `tests/`: what they need of the system
/// beyond the interface above.
#[doc(hidden)]
pub mod testutil {
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    pub fn symlink(target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    /// False when this user may not create symlinks; a platform where that
    /// can happen panics instead when the environment variable `CI` is set.
    /// Any user may, on Unix.
    pub fn symlinks_available(_dir: &Path) -> bool {
        true
    }

    /// A file made unreadable; its permissions come back when this is dropped.
    pub struct Unreadable {
        path: PathBuf,
        permissions: std::fs::Permissions,
    }

    impl Drop for Unreadable {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.path, self.permissions.clone());
        }
    }

    /// Takes every permission from `path` until the result is dropped.
    /// `None` when the file cannot be made unreadable here: privileges that
    /// bypass file permissions (root) still open it.
    pub fn make_unreadable(path: &Path) -> Option<Unreadable> {
        let permissions = std::fs::metadata(path).unwrap().permissions();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = Unreadable {
            path: path.to_path_buf(),
            permissions,
        };
        if std::fs::File::open(path).is_ok() {
            return None;
        }
        Some(unreadable)
    }

    /// Creates a FIFO at `path`; false where there are none.
    pub fn mkfifo(path: &Path) -> bool {
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c` is a valid NUL-terminated path for the call.
        let ret = unsafe { libc::mkfifo(c.as_ptr(), 0o600) };
        assert_eq!(
            ret,
            0,
            "mkfifo {}: {}",
            path.display(),
            io::Error::last_os_error()
        );
        true
    }

    /// Sets the mtime of a file or directory the caller owns, without
    /// needing write access to it.
    pub fn set_mtime(path: &Path, t: std::time::SystemTime) {
        std::fs::File::open(path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(t))
            .unwrap();
    }

    #[cfg(unix)]
    pub fn set_mode(path: &Path, mode: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Sets the extended attribute `name` of `path`.
    #[cfg(unix)]
    pub fn set_xattr(path: &Path, name: &str, value: &[u8]) -> io::Result<()> {
        xattr::set(path, name, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Entry;

    #[test]
    fn from_path_fifo_socket_and_char_device_kinds() {
        let d = tempfile::tempdir().unwrap();
        let fifo = d.path().join("p");
        assert!(std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        assert_eq!(Entry::from_path(&fifo).unwrap().0.kind, Kind::Fifo);
        let sock = d.path().join("s");
        let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        assert_eq!(Entry::from_path(&sock).unwrap().0.kind, Kind::Socket);
        let (null, _) = Entry::from_path(Path::new("/dev/null")).unwrap();
        assert_eq!(null.kind, Kind::CharDev);
        assert_ne!(null.rdev, 0, "a device records its device number");
        assert!(null.target.is_none() && null.blake3.is_none());
    }

    #[test]
    fn xattrs_sorted_by_name() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("f");
        std::fs::write(&p, b"x").unwrap();
        if let Err(err) =
            xattr::set(&p, "user.b", b"2").and_then(|_| xattr::set(&p, "user.a", b"1"))
        {
            eprintln!("skipped xattrs_sorted_by_name: xattr set failed: {err}");
            return;
        }
        let (mut got, err) = read_xattrs(&p);
        assert!(err.is_none(), "{err:?}");
        // macOS adds its own attributes (com.apple.provenance) to new files.
        got.retain(|(k, _)| k.starts_with(b"user."));
        assert_eq!(
            got,
            vec![
                (b"user.a".to_vec(), b"1".to_vec()),
                (b"user.b".to_vec(), b"2".to_vec())
            ]
        );
        let mut from_entry = Entry::from_path(&p).unwrap().0.xattrs;
        from_entry.retain(|(k, _)| k.starts_with(b"user."));
        assert_eq!(from_entry, got);
    }

    fn names(v: &[&str]) -> io::Result<Vec<OsString>> {
        Ok(v.iter().map(OsString::from).collect())
    }

    fn val(name: &OsStr) -> io::Result<Option<Vec<u8>>> {
        Ok(Some(name.as_encoded_bytes().to_vec()))
    }

    #[test]
    fn xattr_list_failure_is_reported() {
        let (got, err) = collect_xattrs(
            Err::<Vec<OsString>, _>(io::Error::from_raw_os_error(5)),
            val,
        );
        assert!(got.is_empty());
        assert_eq!(err.unwrap().raw_os_error(), Some(5));
    }

    #[test]
    fn xattr_get_failure_keeps_those_read_before() {
        let (got, err) = collect_xattrs(names(&["user.b", "user.a", "user.c"]), |n| {
            if n == "user.c" {
                Err(io::Error::from_raw_os_error(13))
            } else {
                val(n)
            }
        });
        assert_eq!(
            got,
            vec![
                (b"user.a".to_vec(), b"user.a".to_vec()),
                (b"user.b".to_vec(), b"user.b".to_vec())
            ]
        );
        assert_eq!(err.unwrap().raw_os_error(), Some(13));
    }

    #[test]
    fn xattr_unsupported_midway_keeps_those_read_before() {
        let (got, err) = collect_xattrs(names(&["user.a", "user.b"]), |n| {
            if n == "user.b" {
                Err(io::Error::from_raw_os_error(95))
            } else {
                val(n)
            }
        });
        assert_eq!(got, vec![(b"user.a".to_vec(), b"user.a".to_vec())]);
        assert!(err.is_none());
    }
}
