//! The Windows implementation: `std::os::windows`, `winapi-util`,
//! `windows-sys` and `cpu-time`.
//!
//! Nothing here follows a reparse point of any kind: metadata is read from a
//! handle opened on the reparse point itself, and a file opened for hashing
//! that turns out to be one is refused.

use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::{
    FileAttributeTagInfo, FileBasicInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_BASIC_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_INFO_BY_HANDLE_CLASS,
    FILE_READ_ATTRIBUTES,
};

use super::{Stat, Xattrs};
use crate::model::{Kind, Timespec};
use crate::wtf8;

/// The reparse tag of a symlink. `windows-sys` has it as
/// `Win32::System::SystemServices::IO_REPARSE_TAG_SYMLINK`, behind a feature
/// this crate needs for nothing else.
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;

/// Reads one fixed-size information class of an open file.
///
/// Called only with `(FILE_BASIC_INFO, FileBasicInfo)` and
/// `(FILE_ATTRIBUTE_TAG_INFO, FileAttributeTagInfo)`.
fn file_info<T: Copy + Default>(file: &File, class: FILE_INFO_BY_HANDLE_CLASS) -> io::Result<T> {
    // Both `T` derive `Default`, which zeroes every field.
    let mut buf = T::default();
    let size = u32::try_from(std::mem::size_of::<T>()).expect("an information class fits in u32");
    // SAFETY: the handle is that of `file`, which stays open for the borrow,
    // so it is valid for the whole call. The buffer is `buf`, a zeroed `T`
    // of exactly `size_of::<T>()` bytes, and that size is what the call is
    // given, so it writes at most that many bytes, all inside `buf`. Both
    // `T` it is called with, FILE_BASIC_INFO and FILE_ATTRIBUTE_TAG_INFO,
    // are `#[repr(C)]` structs of plain integers, valid for any bit pattern,
    // so whatever the call writes leaves `buf` a valid `T`.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            class,
            (&mut buf as *mut T).cast(),
            size,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buf)
}

/// A FILETIME (100 ns steps since 1601) as a Unix time.
fn filetime_to_timespec(ft: i64) -> Timespec {
    Timespec {
        sec: ft.div_euclid(10_000_000) - 11_644_473_600,
        nsec: (ft.rem_euclid(10_000_000) * 100) as u32,
    }
}

/// The kind of an object from its attributes and, for a reparse point, its
/// tag. The reparse test comes first: a junction has the directory attribute.
fn kind_of(attributes: u32, reparse_tag: Option<u32>) -> Kind {
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        if reparse_tag == Some(IO_REPARSE_TAG_SYMLINK) {
            Kind::Symlink
        } else {
            Kind::Other
        }
    } else if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        Kind::Dir
    } else {
        Kind::File
    }
}

/// Opens `abs` to read its metadata: the object itself, a reparse point
/// included, and a directory too.
fn open_for_metadata(abs: &Path) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(abs)
}

/// Reads the metadata of `abs` without following a reparse point.
pub fn lstat(abs: &Path) -> io::Result<Stat> {
    let file = open_for_metadata(abs)?;
    let info = winapi_util::file::information(&file)?;
    let basic: FILE_BASIC_INFO = file_info(&file, FileBasicInfo)?;
    // The attributes are a DWORD; winapi-util widens them.
    let attributes = info.file_attributes() as u32;
    let reparse_tag = if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        let tag: FILE_ATTRIBUTE_TAG_INFO = file_info(&file, FileAttributeTagInfo)?;
        Some(tag.ReparseTag)
    } else {
        None
    };
    let kind = kind_of(attributes, reparse_tag);
    Ok(Stat {
        kind,
        mode: 0,
        uid: 0,
        gid: 0,
        size: if kind == Kind::Dir {
            0
        } else {
            info.file_size()
        },
        nlink: info.number_of_links(),
        ino: info.file_index(),
        dev: info.volume_serial_number(),
        rdev: 0,
        mtime: filetime_to_timespec(basic.LastWriteTime),
        ctime: filetime_to_timespec(basic.ChangeTime),
    })
}

/// The target of the symlink `abs`, as the bytes it holds.
pub fn read_link(abs: &Path) -> io::Result<Vec<u8>> {
    Ok(os_to_bytes(std::fs::read_link(abs)?.as_os_str()))
}

/// Extended attributes are not recorded on Windows.
pub fn read_xattrs(_abs: &Path) -> (Xattrs, Option<io::Error>) {
    (Vec::new(), None)
}

/// Whether `md`, read without following a final link, is that of a reparse
/// point of any kind.
pub fn is_link(md: &std::fs::Metadata) -> bool {
    md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// Opens `abs` read-only. A reparse point there is not followed: it is an
/// error.
pub fn open_nofollow(abs: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(abs)?;
    if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(file)
}

/// The stored bytes of a name: WTF-8.
pub fn os_to_bytes(name: &OsStr) -> Vec<u8> {
    wtf8::encode(&name.encode_wide().collect::<Vec<u16>>())
}

/// The name that stored bytes stand for. Bytes that are not WTF-8 are an
/// error.
pub fn bytes_to_os(bytes: &[u8]) -> io::Result<OsString> {
    let units = wtf8::decode(bytes).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("not a stored name: bad bytes at offset {}", e.at),
        )
    })?;
    Ok(OsString::from_wide(&units))
}

/// The path below `root` of the stored, `/`-separated relative path `rel`.
/// Each component must name exactly one entry: a stored name that Windows
/// would read as a drive, a root, `.`, `..` or more than one component is an
/// error.
pub fn rel_to_path(root: &Path, rel: &[u8]) -> io::Result<PathBuf> {
    let mut path = root.to_path_buf();
    for part in rel.split(|&b| b == b'/') {
        let name = bytes_to_os(part)?;
        let mut components = Path::new(&name).components();
        match (components.next(), components.next()) {
            (Some(Component::Normal(n)), None) if n == name.as_os_str() => path.push(&name),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "not a stored path component",
                ))
            }
        }
    }
    Ok(path)
}

/// The stored bytes of the relative path `rel`: its components, joined with
/// `/`.
pub fn path_to_rel(rel: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, c) in rel.components().enumerate() {
        if i > 0 {
            out.push(b'/');
        }
        out.extend(os_to_bytes(c.as_os_str()));
    }
    out
}

/// CPU time the calling thread has used so far (user and system), or `None`
/// if the clock cannot be read.
pub fn thread_cpu_time() -> Option<Duration> {
    cpu_time::ThreadTime::try_now()
        .ok()
        .map(|t| t.as_duration())
}

/// Not known on Windows.
pub fn core_split() -> Option<(usize, usize)> {
    None
}

/// Helpers for the tests, here and in `tests/`: what they need of the system
/// beyond the interface above.
#[doc(hidden)]
pub mod testutil {
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::Path;

    /// A directory symlink when `target`, resolved against the link's
    /// directory, is a directory; a file symlink otherwise, a dangling
    /// target included.
    pub fn symlink(target: &Path, link: &Path) -> io::Result<()> {
        let resolved = link.parent().unwrap_or(Path::new(".")).join(target);
        if resolved.is_dir() {
            std::os::windows::fs::symlink_dir(target, link)
        } else {
            std::os::windows::fs::symlink_file(target, link)
        }
    }

    /// False when this user may not create symlinks (that needs Developer
    /// Mode, an elevated process or the symlink privilege); panics instead
    /// when the environment variable `CI` is set.
    pub fn symlinks_available(dir: &Path) -> bool {
        // ERROR_PRIVILEGE_NOT_HELD: the one failure that means "not allowed".
        const PRIVILEGE_NOT_HELD: i32 = 1314;
        let probe = dir.join(".fileset-symlink-probe");
        match std::os::windows::fs::symlink_file("target", &probe) {
            Ok(()) => {
                std::fs::remove_file(&probe).unwrap();
                true
            }
            Err(e) if e.raw_os_error() != Some(PRIVILEGE_NOT_HELD) => {
                panic!("symlink probe {}: {e}", probe.display())
            }
            Err(e) => {
                assert!(
                    std::env::var_os("CI").is_none(),
                    "CI is set, but this user may not create symlinks: {e}"
                );
                false
            }
        }
    }

    /// A file held open with no sharing, so nobody else can open it to read,
    /// until this is dropped.
    pub struct Unreadable {
        _file: File,
    }

    /// Holds `path` open with no sharing until the result is dropped. `None`
    /// when it can still be opened anyway.
    pub fn make_unreadable(path: &Path) -> Option<Unreadable> {
        let file = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(path)
            .unwrap();
        if File::open(path).is_ok() {
            return None;
        }
        Some(Unreadable { _file: file })
    }

    /// There are no FIFOs on Windows.
    pub fn mkfifo(_path: &Path) -> bool {
        false
    }

    /// Creates a junction at `link` that points to the directory `target`.
    #[cfg(windows)]
    pub fn junction(target: &Path, link: &Path) {
        let status = std::process::Command::new("cmd")
            .arg("/c")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(
            status.success(),
            "mklink /J {} {} failed",
            link.display(),
            target.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_conversion() {
        assert_eq!(
            filetime_to_timespec(116_444_736_000_000_000),
            Timespec { sec: 0, nsec: 0 }
        );
        assert_eq!(
            filetime_to_timespec(116_444_736_000_000_001),
            Timespec { sec: 0, nsec: 100 }
        );
        assert_eq!(
            filetime_to_timespec(0),
            Timespec {
                sec: -11_644_473_600,
                nsec: 0
            }
        );
    }

    #[test]
    fn names_round_trip_through_the_os() {
        let cases: [&[u16]; 3] = [&[0x61], &[0xD800], &[0x61, 0xDFFF, 0x62]];
        for units in cases {
            let bytes = os_to_bytes(&OsString::from_wide(units));
            let back: Vec<u16> = bytes_to_os(&bytes).unwrap().encode_wide().collect();
            assert_eq!(back, units, "{bytes:02X?}");
        }
    }

    #[test]
    fn invalid_stored_bytes_are_an_error() {
        let err = bytes_to_os(b"a\xFF").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_junction_is_other_and_a_symlink_is_a_symlink() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("t");
        std::fs::create_dir(&target).unwrap();
        let j = d.path().join("j");
        testutil::junction(&target, &j);
        assert_eq!(lstat(&j).unwrap().kind, Kind::Other);
        assert!(is_link(&std::fs::symlink_metadata(&j).unwrap()));
        assert_eq!(lstat(&target).unwrap().kind, Kind::Dir);
        assert_eq!(lstat(&target).unwrap().size, 0);
        if testutil::symlinks_available(d.path()) {
            let s = d.path().join("s");
            testutil::symlink(Path::new("t"), &s).unwrap();
            assert_eq!(lstat(&s).unwrap().kind, Kind::Symlink);
            assert_eq!(read_link(&s).unwrap(), b"t");
        }
    }

    #[test]
    fn open_nofollow_refuses_a_reparse_point() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("t");
        std::fs::create_dir(&target).unwrap();
        let j = d.path().join("j");
        testutil::junction(&target, &j);
        assert!(open_nofollow(&j).is_err());
        let f = d.path().join("f");
        std::fs::write(&f, b"x").unwrap();
        assert!(open_nofollow(&f).is_ok());
    }

    #[test]
    fn rel_paths_round_trip() {
        let root = Path::new(r"C:\root");
        let p = rel_to_path(root, b"a/b/c").unwrap();
        assert_eq!(p, Path::new(r"C:\root\a\b\c"));
        assert_eq!(path_to_rel(p.strip_prefix(root).unwrap()), b"a/b/c");
        for bad in [&b"a/../b"[..], b"C:x", b"a/./b", b"a\\b", b"a//b", b"a\xFF"] {
            assert!(rel_to_path(root, bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn file_facts() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("f");
        std::fs::write(&f, b"abc").unwrap();
        let st = lstat(&f).unwrap();
        assert_eq!(st.kind, Kind::File);
        assert_eq!(st.size, 3);
        assert_eq!(st.nlink, 1);
        assert_ne!(st.ino, 0);
        assert!(st.mtime.sec > 1_600_000_000);
        assert!(st.ctime.sec > 1_600_000_000);
        std::fs::hard_link(&f, d.path().join("g")).unwrap();
        let st2 = lstat(&f).unwrap();
        assert_eq!(st2.nlink, 2);
        assert_eq!(st2.ino, lstat(&d.path().join("g")).unwrap().ino);
        assert_eq!(st2.dev, lstat(&d.path().join("g")).unwrap().dev);
    }
}
