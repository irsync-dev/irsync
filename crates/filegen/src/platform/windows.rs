//! The Windows implementation: `std::os::windows`, `winapi-util`,
//! `windows-sys` and `ctrlc`.
//!
//! Nothing here follows a reparse point of any kind: metadata is read and
//! times are set through a handle opened on the reparse point itself, and a
//! symlink is removed as the link it is.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, FileTimes, OpenOptions};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{
    ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED, ERROR_PRIVILEGE_NOT_HELD, HANDLE,
};
use windows_sys::Win32::Storage::FileSystem::{
    FileAttributeTagInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_INFO_BY_HANDLE_CLASS,
    FILE_READ_ATTRIBUTES, FILE_WRITE_ATTRIBUTES,
};
use windows_sys::Win32::System::Ioctl::{
    FILE_ZERO_DATA_INFORMATION, FSCTL_SET_SPARSE, FSCTL_SET_ZERO_DATA,
};
use windows_sys::Win32::System::SystemServices::IO_REPARSE_TAG_SYMLINK;
use windows_sys::Win32::System::IO::DeviceIoControl;

use super::{FileKind, LinkTarget, Stat};
use crate::caps::Caps;
use crate::wtf8;

/// The longest path the system takes, in 16-bit units (the extended form).
pub const PATH_MAX: usize = 32_767;
/// mtimes are stored in steps of 100 ns.
pub const MTIME_STEP_NS: u32 = 100;
/// The earliest mtime stored: 1601-01-01, the start of a FILETIME.
pub const MIN_MTIME_SEC: i64 = -11_644_473_600;
/// NTFS takes at most 1023 names of one file.
pub const MAX_LINKS: u64 = 1023;
/// The separator of the components of a symlink target.
pub const TARGET_SEP: u8 = b'\\';
/// A rename cannot replace a directory. Documentation only: `rename_over`
/// renames regular files on every platform.
pub const RENAME_OVER_DIRS: bool = false;
/// A rename of one name of a file over another fails. Documentation only:
/// `rename_over` never picks two names of one file on any platform.
pub const RENAME_OVER_SAME_FILE: bool = false;
/// Names that are valid but awkward here (unpaired surrogates, a trailing
/// dot or space, reserved device names) are drawn.
pub const EDGE_NAMES: bool = true;

/// A fixed-size information class and the struct `GetFileInformationByHandleEx`
/// fills for it. Private to this file, so sealed: implemented only below, for
/// a struct of integer fields.
trait InfoClass: Copy + Default {
    const CLASS: FILE_INFO_BY_HANDLE_CLASS;
}

impl InfoClass for FILE_ATTRIBUTE_TAG_INFO {
    const CLASS: FILE_INFO_BY_HANDLE_CLASS = FileAttributeTagInfo;
}

/// Reads one fixed-size information class of an open file.
fn file_info<T: InfoClass>(file: &File) -> io::Result<T> {
    let mut buf = T::default();
    let size = u32::try_from(std::mem::size_of::<T>()).expect("an information class fits in u32");
    // SAFETY: the handle is that of `file`, which stays open for the borrow,
    // so it is valid for the whole call. The buffer is `buf`, a `T` built by
    // `Default`, and the size the call is given is `size_of::<T>()`, so it
    // writes at most that many bytes, all inside `buf`. `T` is the one type
    // `InfoClass` is implemented for (it is sealed): FILE_ATTRIBUTE_TAG_INFO
    // with FileAttributeTagInfo, the struct that class fills. It is a
    // `#[repr(C)]` struct of two `u32` fields, valid for any bit pattern, so
    // whatever the call writes leaves `buf` a valid `T`.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            T::CLASS,
            (&mut buf as *mut T).cast(),
            size,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buf)
}

/// `DeviceIoControl` with `code`, the input `input` (none when `None`) and
/// no output. Private, and called only with the two codes below.
fn ioctl_in(file: &File, code: u32, input: Option<&FILE_ZERO_DATA_INFORMATION>) -> io::Result<()> {
    let (ptr, len) = match input {
        Some(info) => (
            (info as *const FILE_ZERO_DATA_INFORMATION).cast(),
            std::mem::size_of::<FILE_ZERO_DATA_INFORMATION>() as u32,
        ),
        None => (std::ptr::null(), 0),
    };
    let mut returned = 0u32;
    // SAFETY: the handle is that of `file`, which stays open for the borrow,
    // so it is valid for the whole call. The input pointer is null with
    // length 0, or points to the live FILE_ZERO_DATA_INFORMATION `input`
    // borrows, with its exact size; the call only reads it. There is no
    // output buffer: the pointer is null and the length 0. `lpBytesReturned`
    // points to the local `returned`, which outlives the call.
    // `lpOverlapped` is null, and no handle in this crate is opened for
    // overlapped I/O (std never sets FILE_FLAG_OVERLAPPED, and no caller
    // passes it), so the call completes before it returns.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle() as HANDLE,
            code,
            ptr,
            len,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Marks `file` sparse.
fn set_sparse(file: &File) -> io::Result<()> {
    ioctl_in(file, FSCTL_SET_SPARSE, None)
}

/// Zeroes `[offset, end)` of `file`, deallocating it if the file is sparse.
fn zero_range(file: &File, offset: u64, end: u64) -> io::Result<()> {
    let to_i64 = |v: u64| {
        i64::try_from(v)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "offset out of range"))
    };
    let info = FILE_ZERO_DATA_INFORMATION {
        FileOffset: to_i64(offset)?,
        BeyondFinalZero: to_i64(end)?,
    };
    ioctl_in(file, FSCTL_SET_ZERO_DATA, Some(&info))
}

/// Whether `e` says the filesystem has no sparse files.
fn sparse_unsupported(e: &io::Error) -> bool {
    e.raw_os_error()
        .is_some_and(|c| c == ERROR_INVALID_FUNCTION as i32 || c == ERROR_NOT_SUPPORTED as i32)
}

/// The kind of an object from its attributes and, for a reparse point, its
/// tag. The reparse test comes first: a junction has the directory attribute.
fn kind_of(attributes: u32, reparse_tag: Option<u32>) -> FileKind {
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        if reparse_tag == Some(IO_REPARSE_TAG_SYMLINK) {
            FileKind::Symlink
        } else {
            FileKind::Other
        }
    } else if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        FileKind::Dir
    } else {
        FileKind::File
    }
}

/// The metadata of the open file `file`.
fn stat_of(file: &File) -> io::Result<Stat> {
    let info = winapi_util::file::information(file)?;
    // The attributes are a DWORD; winapi-util widens them.
    let attributes = info.file_attributes() as u32;
    let reparse_tag = if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        let tag: FILE_ATTRIBUTE_TAG_INFO = file_info(file)?;
        Some(tag.ReparseTag)
    } else {
        None
    };
    let kind = kind_of(attributes, reparse_tag);
    // 100 ns steps since 1601; winapi-util reads a zero time as `None`.
    let filetime = info.last_write_time().unwrap_or(0);
    Ok(Stat {
        kind,
        size: if kind == FileKind::Dir {
            0
        } else {
            info.file_size()
        },
        mode: 0,
        mtime_ns: (i128::from(filetime) + i128::from(MIN_MTIME_SEC) * 10_000_000) * 100,
        ino: info.file_index(),
        dev: info.volume_serial_number(),
        nlink: info.number_of_links(),
        uid: 0,
        attributes,
    })
}

/// Reads the metadata of `path` without following a reparse point: one
/// handle, opened on the object itself, a directory too.
pub fn lstat(path: &Path) -> io::Result<Stat> {
    let file = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    stat_of(&file)
}

/// Reads the metadata of the open file `f`.
pub fn fstat(f: &File) -> io::Result<Stat> {
    stat_of(f)
}

/// Whether filegen may pick this entry as a target: not when it is
/// read-only, since a change to it would fail, and fail again on every
/// resume. filegen never sets that attribute itself.
pub fn may_change(stat: &Stat) -> bool {
    stat.attributes & FILE_ATTRIBUTE_READONLY == 0
}

/// Never: there is no root that bypasses everything here.
pub fn is_root() -> bool {
    false
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

/// The bytes of any path, as the log writes them: WTF-8, separators as
/// they are.
pub fn path_to_bytes(path: &Path) -> Vec<u8> {
    os_to_bytes(path.as_os_str())
}

/// Whether NTFS takes `name` as a file name: none of `< > : " | ? * \` and
/// no character below U+0020.
pub fn name_ok(name: &str) -> bool {
    !name
        .chars()
        .any(|c| c < '\u{20}' || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\'))
}

/// The length of `name` as the system counts it against its name limit:
/// 16-bit units here.
pub fn name_units(name: &str) -> usize {
    name.encode_utf16().count()
}

/// Sets the mtime of `path` to `sec` seconds and `nsec` nanoseconds after
/// the epoch (stored in steps of 100 ns), never following a reparse point.
/// The access time is left as it is. The very first FILETIME, 1601-01-01
/// 00:00:00.0, cannot be set: zero means "leave unchanged" to Windows.
pub fn set_mtime(path: &Path, sec: i64, nsec: u32) -> io::Result<()> {
    let time = if sec >= 0 {
        UNIX_EPOCH + Duration::new(sec.unsigned_abs(), nsec)
    } else {
        UNIX_EPOCH - Duration::from_secs(sec.unsigned_abs()) + Duration::from_nanos(nsec.into())
    };
    let file = OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    file.set_times(FileTimes::new().set_modified(time))
}

/// Makes `link` a symlink holding `target`: a directory symlink for
/// [`LinkTarget::Dir`], a file symlink otherwise.
pub fn symlink(target: &[u8], link: &Path, kind: LinkTarget) -> io::Result<()> {
    let target = bytes_to_os(target)?;
    match kind {
        LinkTarget::Dir => std::os::windows::fs::symlink_dir(target, link),
        LinkTarget::File => std::os::windows::fs::symlink_file(target, link),
    }
}

/// Removes the symlink `link`: a directory symlink as a directory.
pub fn remove_symlink(link: &Path) -> io::Result<()> {
    if lstat(link)?.attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        fs::remove_dir(link)
    } else {
        fs::remove_file(link)
    }
}

/// The target of the symlink `path`, as the bytes it holds.
pub fn read_link(path: &Path) -> io::Result<Vec<u8>> {
    Ok(os_to_bytes(fs::read_link(path)?.as_os_str()))
}

/// Deallocates `[offset, offset + len)` of `f`, keeping its size; `false`
/// if the filesystem cannot. Marks the file sparse first.
pub fn punch_hole(f: &File, offset: u64, len: u64) -> io::Result<bool> {
    match set_sparse(f) {
        Ok(()) => {}
        Err(e) if sparse_unsupported(&e) => return Ok(false),
        Err(e) => return Err(e),
    }
    let end = offset
        .checked_add(len)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "range out of bounds"))?;
    zero_range(f, offset, end)?;
    Ok(true)
}

/// There are no permission bits on Windows.
pub fn chmod(_path: &Path, _mode: u32) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// There are no uids and gids on Windows.
pub fn chown(_path: &Path, _uid: Option<u32>, _gid: Option<u32>) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// There are no FIFOs on Windows.
pub fn mkfifo(_path: &Path, _mode: u32) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// There are no device nodes on Windows.
pub fn mknod(
    _path: &Path,
    _char_dev: bool,
    _mode: u32,
    _major: u32,
    _minor: u32,
) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// Extended attributes are not used on Windows.
pub fn get_xattr(_path: &Path, _name: &str) -> io::Result<Option<Vec<u8>>> {
    Err(io::ErrorKind::Unsupported.into())
}

/// Extended attributes are not used on Windows.
pub fn set_xattr(_path: &Path, _name: &str, _value: &[u8]) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// Extended attributes are not used on Windows.
pub fn remove_xattr(_path: &Path, _name: &str) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// The capabilities of this machine in `dir`: `symlinks` if this user may
/// create a symlink there, `sparse` if the filesystem takes sparse files.
/// Each is found by trying, with a file `.filegen-probe-{pid}` that is
/// removed again.
pub fn probe_caps(dir: &Path) -> io::Result<Caps> {
    let probe = dir.join(format!(".filegen-probe-{}", std::process::id()));
    let symlinks = match std::os::windows::fs::symlink_file("target", &probe) {
        Ok(()) => {
            fs::remove_file(&probe)?;
            true
        }
        Err(e) if e.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD as i32) => false,
        Err(e) => return Err(e),
    };
    let file = File::create_new(&probe)?;
    let marked = set_sparse(&file);
    drop(file);
    fs::remove_file(&probe)?;
    let sparse = match marked {
        Ok(()) => true,
        Err(e) if sparse_unsupported(&e) => false,
        Err(e) => return Err(e),
    };
    Ok(Caps { symlinks, sparse })
}

/// The effective user id: none on Windows.
pub fn euid() -> u32 {
    0
}

/// The effective group id: none on Windows.
pub fn egid() -> u32 {
    0
}

/// What the flag is set to on an interrupt: SIGINT's number, as on Unix.
const INTERRUPTED: i32 = 2;

/// The process-wide flag: 0, or [`INTERRUPTED`].
static FLAG: OnceLock<Arc<AtomicI32>> = OnceLock::new();

/// Registers the console event handler (once per process: `ctrlc` takes
/// one) and returns the flag it sets: 0 until an event arrives, then 2. A
/// second event while the flag is set exits the process at once with
/// status 130. Each call clears the flag. `ctrlc` receives Ctrl-C,
/// Ctrl-Break and the other console events alike and does not say which.
pub fn install_interrupt() -> Arc<AtomicI32> {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    let flag = FLAG.get_or_init(|| Arc::new(AtomicI32::new(0)));
    flag.store(0, Ordering::SeqCst);
    REGISTERED.get_or_init(|| {
        let flag = Arc::clone(flag);
        ctrlc::set_handler(move || {
            if flag
                .compare_exchange(0, INTERRUPTED, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                // A second event while shutting down: leave now.
                std::process::exit(130);
            }
        })
        .expect("installing the console event handler");
    });
    Arc::clone(flag)
}

/// The name of an interrupt for the log: every console event is `Ctrl-C`,
/// since `ctrlc` does not say which one arrived.
pub fn interrupt_name(_sig: i32) -> String {
    "Ctrl-C".to_string()
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
        match super::probe_caps(dir) {
            Ok(caps) if caps.symlinks => true,
            Ok(_) => {
                assert!(
                    std::env::var_os("CI").is_none(),
                    "CI is set, but this user may not create symlinks"
                );
                false
            }
            Err(e) => panic!("symlink probe in {}: {e}", dir.display()),
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

    /// Whether `e` says the filesystem has no user xattrs: always, here.
    pub fn xattr_unsupported(e: &io::Error) -> bool {
        e.kind() == io::ErrorKind::Unsupported
    }

    /// Creates a junction at `link` that points to the directory `target`.
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
    use std::io::{Read, Seek, SeekFrom, Write};

    use super::*;

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
    fn rel_to_path_takes_only_single_components() {
        let root = Path::new(r"C:\root");
        assert_eq!(
            rel_to_path(root, b"a/b/c").unwrap(),
            Path::new(r"C:\root\a\b\c")
        );
        for bad in [
            &b"a/../b"[..],
            b"..",
            b"C:x",
            b"a/./b",
            b"a\\b",
            b"\\a",
            b"a//b",
            b"",
            b"a\xFF",
        ] {
            assert!(rel_to_path(root, bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn path_to_bytes_keeps_separators() {
        assert_eq!(path_to_bytes(Path::new(r"a\b")), b"a\\b");
    }

    #[test]
    fn name_rules() {
        assert!(name_ok("a b.txt"));
        assert!(name_ok("CON"));
        for bad in [
            "a<b", "a>b", "a:b", "a\"b", "a|b", "a?b", "a*b", "a\\b", "a\u{1F}b",
        ] {
            assert!(!name_ok(bad), "{bad:?}");
        }
        assert_eq!(name_units("abc"), 3);
        assert_eq!(name_units("é"), 1);
        assert_eq!(name_units("😀"), 2);
    }

    #[test]
    fn lstat_kinds() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("f");
        fs::write(&f, b"abc").unwrap();
        let st = lstat(&f).unwrap();
        assert_eq!(
            (st.kind, st.size, st.mode, st.uid),
            (FileKind::File, 3, 0, 0)
        );
        let sub = d.path().join("d");
        fs::create_dir(&sub).unwrap();
        assert_eq!(lstat(&sub).unwrap().kind, FileKind::Dir);
        let j = d.path().join("j");
        testutil::junction(&sub, &j);
        assert_eq!(lstat(&j).unwrap().kind, FileKind::Other);
        if testutil::symlinks_available(d.path()) {
            let s = d.path().join("s");
            symlink(b"f", &s, LinkTarget::File).unwrap();
            assert_eq!(lstat(&s).unwrap().kind, FileKind::Symlink);
            assert_eq!(read_link(&s).unwrap(), b"f");
            let sd = d.path().join("sd");
            symlink(b"d", &sd, LinkTarget::Dir).unwrap();
            assert_eq!(lstat(&sd).unwrap().kind, FileKind::Symlink);
            remove_symlink(&s).unwrap();
            remove_symlink(&sd).unwrap();
            assert!(lstat(&s).is_err() && lstat(&sd).is_err());
            assert!(f.exists() && sub.exists());
        }
    }

    #[test]
    fn hard_links_share_an_identity() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("f");
        fs::write(&f, b"x").unwrap();
        assert_eq!(lstat(&f).unwrap().nlink, 1);
        fs::hard_link(&f, d.path().join("g")).unwrap();
        let (a, b) = (lstat(&f).unwrap(), lstat(&d.path().join("g")).unwrap());
        assert_eq!((a.nlink, b.nlink), (2, 2));
        assert_eq!((a.ino, a.dev), (b.ino, b.dev));
        assert_ne!(a.ino, 0);
        let file = File::open(&f).unwrap();
        assert_eq!(fstat(&file).unwrap().ino, a.ino);
    }

    #[test]
    fn set_mtime_round_trips_at_100_ns() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("f");
        fs::write(&f, b"x").unwrap();
        for (sec, nsec) in [
            (1_700_000_000, 123_456_700),
            (0, 0),
            (-1, 100),
            // The first step after the start of a FILETIME: its zero means
            // "leave unchanged", and std refuses it.
            (MIN_MTIME_SEC, 100),
        ] {
            set_mtime(&f, sec, nsec).unwrap();
            let want = i128::from(sec) * 1_000_000_000 + i128::from(nsec);
            assert_eq!(lstat(&f).unwrap().mtime_ns, want, "{sec} {nsec}");
        }
        // A directory too, through the same handle flags.
        set_mtime(d.path(), 1_000_000_000, 500).unwrap();
        assert_eq!(lstat(d.path()).unwrap().mtime_ns, 1_000_000_000_000_000_500);
    }

    #[test]
    fn set_mtime_does_not_follow_a_symlink() {
        let d = tempfile::tempdir().unwrap();
        if !testutil::symlinks_available(d.path()) {
            return;
        }
        let f = d.path().join("f");
        fs::write(&f, b"x").unwrap();
        let before = lstat(&f).unwrap().mtime_ns;
        let s = d.path().join("s");
        symlink(b"f", &s, LinkTarget::File).unwrap();
        set_mtime(&s, 1_000_000_000, 0).unwrap();
        assert_eq!(lstat(&s).unwrap().mtime_ns, 1_000_000_000_000_000_000);
        assert_eq!(lstat(&f).unwrap().mtime_ns, before);
    }

    #[test]
    fn punch_hole_zeroes_the_range() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("f");
        let len = 1 << 20;
        fs::write(&p, vec![0xAB; len]).unwrap();
        let mut f = OpenOptions::new().read(true).write(true).open(&p).unwrap();
        assert!(punch_hole(&f, 65_536, 131_072).unwrap());
        assert_eq!(f.metadata().unwrap().len(), len as u64);
        let mut back = Vec::new();
        f.seek(SeekFrom::Start(0)).unwrap();
        f.read_to_end(&mut back).unwrap();
        assert!(back[..65_536].iter().all(|&b| b == 0xAB));
        assert!(back[65_536..196_608].iter().all(|&b| b == 0));
        assert!(back[196_608..].iter().all(|&b| b == 0xAB));
        f.write_all(b"still writable").unwrap();
    }

    #[test]
    fn probe_caps_finds_ntfs_and_leaves_nothing() {
        let d = tempfile::tempdir().unwrap();
        let caps = probe_caps(d.path()).unwrap();
        assert!(caps.sparse);
        assert_eq!(caps.symlinks, testutil::symlinks_available(d.path()));
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_read_only_file_may_not_change() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("f");
        fs::write(&f, b"x").unwrap();
        assert!(may_change(&lstat(&f).unwrap()));
        let mut perms = fs::metadata(&f).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&f, perms.clone()).unwrap();
        assert!(!may_change(&lstat(&f).unwrap()));
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        fs::set_permissions(&f, perms).unwrap();
    }

    #[test]
    fn posix_only_calls_are_unsupported() {
        let p = Path::new("x");
        let unsupported = |r: io::Result<()>| r.unwrap_err().kind() == io::ErrorKind::Unsupported;
        assert!(unsupported(chmod(p, 0o644)));
        assert!(unsupported(chown(p, Some(0), None)));
        assert!(unsupported(mkfifo(p, 0o644)));
        assert!(unsupported(mknod(p, true, 0o644, 1, 3)));
        assert!(unsupported(set_xattr(p, "user.a", b"1")));
        assert!(unsupported(remove_xattr(p, "user.a")));
        assert!(unsupported(get_xattr(p, "user.a").map(drop)));
    }
}
