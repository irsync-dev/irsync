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
    // `lpOverlapped` is null, which is sound only for a handle not opened for
    // overlapped I/O. That holds for every handle that reaches here:
    // `ioctl_in` is private, called only by `set_sparse` and `zero_range`,
    // which are private too. Their only callers are `punch_hole`, which is
    // `pub(crate)` and whose callers (the `punch_hole` op in `ops/file.rs`
    // and this file's tests) open the file with std's `OpenOptions` and no
    // custom flags, and `probe_caps`, which creates its own file with
    // `File::create_new`. std sets FILE_FLAG_OVERLAPPED only when a caller
    // passes it as a custom flag, so none of these handles has it, and the
    // call completes before it returns.
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

/// The range `[offset, offset + len)` as `FSCTL_SET_ZERO_DATA` takes it; an
/// error if its end does not fit in an `i64`.
fn zero_data(offset: u64, len: u64) -> io::Result<FILE_ZERO_DATA_INFORMATION> {
    let out_of_range = || io::Error::new(io::ErrorKind::InvalidInput, "range out of bounds");
    let end = offset.checked_add(len).ok_or_else(out_of_range)?;
    Ok(FILE_ZERO_DATA_INFORMATION {
        FileOffset: i64::try_from(offset).map_err(|_| out_of_range())?,
        BeyondFinalZero: i64::try_from(end).map_err(|_| out_of_range())?,
    })
}

/// Zeroes the range `info` names in `file`, deallocating it if the file is
/// sparse.
fn zero_range(file: &File, info: &FILE_ZERO_DATA_INFORMATION) -> io::Result<()> {
    ioctl_in(file, FSCTL_SET_ZERO_DATA, Some(info))
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
/// error, and so is one NTFS cannot hold (see [`ntfs_refuses`]). So is a
/// name that only the extended form (`\\?\`) keeps as it is (see
/// [`needs_extended`]) below a root not in that form: the path would name
/// another entry, or a device. filegen's root is canonical, and so in the
/// extended form.
pub fn rel_to_path(root: &Path, rel: &[u8]) -> io::Result<PathBuf> {
    let extended = is_extended(root);
    let mut path = root.to_path_buf();
    for part in rel.split(|&b| b == b'/') {
        if ntfs_refuses(part) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a stored path component",
            ));
        }
        let name = bytes_to_os(part)?;
        let mut components = Path::new(&name).components();
        match (components.next(), components.next()) {
            (Some(Component::Normal(n)), None) if n == name.as_os_str() => {
                if !extended && needs_extended(&name) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "{name:?} is named as it is only below a root in the \
                             extended form (\\\\?\\), not below {}",
                            root.display()
                        ),
                    ));
                }
                path.push(&name);
            }
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

/// Whether the stored name `part` holds a character NTFS refuses in a
/// name: one of `< > : " | ? *` or one below U+0020. (`:` would name an
/// alternate data stream.) In WTF-8 every byte below 0x80 is such a
/// character itself, never part of a longer one.
fn ntfs_refuses(part: &[u8]) -> bool {
    part.iter()
        .any(|&b| b < 0x20 || matches!(b, b'<' | b'>' | b':' | b'"' | b'|' | b'?' | b'*'))
}

/// Whether `path` is in the extended form (`\\?\C:\...`, `\\?\UNC\...`),
/// which the system takes as it is.
fn is_extended(path: &Path) -> bool {
    matches!(path.components().next(), Some(Component::Prefix(p)) if p.kind().is_verbatim())
}

/// Whether a path not in the extended form would change the name `name`:
/// such a path drops a trailing dot or space, and may take a reserved
/// device name (`CON`, `NUL.txt`, `COM1`, ...) for the device.
fn needs_extended(name: &OsStr) -> bool {
    let text = name.to_string_lossy();
    if text.ends_with(['.', ' ']) {
        return true;
    }
    let stem = text.split('.').next().unwrap_or("").trim_end_matches(' ');
    let stem = stem.to_ascii_uppercase();
    let numbered = |prefix: &str| {
        stem.strip_prefix(prefix).is_some_and(|n| {
            let mut c = n.chars();
            matches!(
                (c.next(), c.next()),
                (Some('0'..='9' | '\u{B9}' | '\u{B2}' | '\u{B3}'), None)
            )
        })
    };
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || numbered("COM")
        || numbered("LPT")
}

/// The bytes of an absolute symlink target naming `path` (a path below the
/// canonical root, so in the extended form), in the form [`read_link`]
/// gives back: std's `read_link` drops the `\\?\` prefix where the path
/// means the same without it and is short enough, and keeps it otherwise
/// (for a trailing dot or space, or a long path). In that form the target
/// has the same bytes when it is written, logged and read back
/// (`absolute_targets_read_back_as_written` holds std to that). A long
/// target must keep the prefix anyway: the system refuses to make a
/// symlink to a long path without it.
pub fn target_bytes(path: &Path) -> Vec<u8> {
    os_to_bytes(user_form(path).as_os_str())
}

/// `path` without its extended prefix (`\\?\C:\` to `C:\`, `\\?\UNC\` to
/// `\\`) when, as std's `read_link` decides it, the extended form is
/// shorter than `MAX_PATH` units and the system reads the shorter form as
/// the very same path (`GetFullPathNameW`, which `path::absolute` calls,
/// leaves it as it is); `path` itself otherwise.
fn user_form(path: &Path) -> PathBuf {
    /// The classic limit of a path, in 16-bit units.
    const MAX_PATH: usize = 260;
    let units: Vec<u16> = path.as_os_str().encode_wide().collect();
    if units.len() >= MAX_PATH {
        return path.to_path_buf();
    }
    let wide = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    let shorter: Vec<u16> = if units.starts_with(&wide(r"\\?\UNC\")) {
        [wide(r"\\"), units[8..].to_vec()].concat()
    } else if units.starts_with(&wide(r"\\?\")) && units.get(5) == Some(&u16::from(b':')) {
        units[4..].to_vec()
    } else {
        return path.to_path_buf();
    };
    let shorter = PathBuf::from(OsString::from_wide(&shorter));
    match std::path::absolute(&shorter) {
        Ok(full)
            if full
                .as_os_str()
                .encode_wide()
                .eq(shorter.as_os_str().encode_wide()) =>
        {
            shorter
        }
        _ => path.to_path_buf(),
    }
}

/// The bytes of any path, as the log writes them: WTF-8, separators as
/// they are.
pub fn path_to_bytes(path: &Path) -> Vec<u8> {
    os_to_bytes(path.as_os_str())
}

/// The stored path of `path` below `root`, as the log writes it: the names
/// after the root, joined with `/` as in every stored path. A path not
/// below `root` is written as it is.
pub fn rel_bytes(root: &Path, path: &Path) -> Vec<u8> {
    let Ok(rel) = path.strip_prefix(root) else {
        return path_to_bytes(path);
    };
    let mut out = Vec::new();
    for (i, c) in rel.components().enumerate() {
        if i > 0 {
            out.push(b'/');
        }
        out.extend(os_to_bytes(c.as_os_str()));
    }
    out
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
/// if the filesystem cannot. Marks the file sparse first, once the range is
/// known to be valid. Crate-private: `f` must not be opened for overlapped
/// I/O (see `ioctl_in`), which every caller here ensures.
pub(crate) fn punch_hole(f: &File, offset: u64, len: u64) -> io::Result<bool> {
    let info = zero_data(offset, len)?;
    match set_sparse(f) {
        Ok(()) => {}
        Err(e) if sparse_unsupported(&e) => return Ok(false),
        Err(e) => return Err(e),
    }
    zero_range(f, &info)?;
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

    /// False, saying so on stderr, when this user may not create symlinks
    /// (that needs Developer Mode, an elevated process or the symlink
    /// privilege); panics instead when the environment variable `CI` is
    /// set, so CI cannot pass by skipping.
    pub fn symlinks_available(dir: &Path) -> bool {
        match super::probe_caps(dir) {
            Ok(caps) if caps.symlinks => true,
            Ok(_) => {
                assert!(
                    std::env::var_os("CI").is_none(),
                    "CI is set, but this user may not create symlinks"
                );
                eprintln!(
                    "skipping: this user may not create symlinks (that needs \
                     Developer Mode, an elevated process or the symlink privilege)"
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
        // Names NTFS cannot hold: `ab:c` would name a stream of `ab`.
        for bad in [
            &b"ab:c"[..],
            b"d/ab:c",
            b"a<b",
            b"a>b",
            b"a\"b",
            b"a|b",
            b"a?b",
            b"a*b",
            b"a\x01b",
            b"a\x1Fb",
        ] {
            let err = rel_to_path(root, bad).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{bad:?}");
        }
        assert!(rel_to_path(root, b"a b/c.d").is_ok());
    }

    #[test]
    fn names_only_the_extended_form_keeps_need_an_extended_root() {
        let plain = Path::new(r"C:\root");
        let extended = Path::new(r"\\?\C:\root");
        let surrogate = [b'a', 0xED, 0xA0, 0x80];
        for name in [
            &b"x."[..],
            b"x ",
            b"CON",
            b"nul.txt",
            b"Com1",
            b"LPT9.log",
            b"AUX .md",
        ] {
            let rel = [b"d/", name].concat();
            let err = rel_to_path(plain, &rel).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{rel:?}");
            assert_eq!(
                path_to_bytes(&rel_to_path(extended, &rel).unwrap()),
                [&br"\\?\C:\root\d\"[..], name].concat()
            );
        }
        for name in [
            &b"x.y"[..],
            b"CONSOLE",
            b"COM10",
            b"a.CON",
            b"x",
            &surrogate,
        ] {
            assert!(rel_to_path(plain, name).is_ok(), "{name:?}");
        }
    }

    #[test]
    fn absolute_targets_read_back_as_written() {
        let d = tempfile::tempdir().unwrap();
        if !testutil::symlinks_available(d.path()) {
            return;
        }
        let root = fs::canonicalize(d.path()).unwrap();
        let surrogate = OsString::from_wide(&[0x61, 0xD800]);
        let mut names: Vec<OsString> = ["f", "x.", "x ", "CON", "NUL.txt"]
            .into_iter()
            .map(OsString::from)
            .chain([surrogate])
            .collect();
        // Around MAX_PATH: the extended form shorter, as long, and longer.
        let room = 260 - root.as_os_str().encode_wide().count() - 1;
        for len in [room - 2, room - 1, room, room + 1, room + 40] {
            names.push("n".repeat(len).into());
        }
        for (i, name) in names.iter().enumerate() {
            let target = target_bytes(&root.join(name));
            let link = root.join(format!("link{i}"));
            symlink(&target, &link, LinkTarget::File).unwrap();
            assert_eq!(
                read_link(&link).unwrap(),
                target,
                "{}",
                String::from_utf8_lossy(&target)
            );
        }
        // A plain name is written without the prefix, a trailing dot with it.
        assert!(!target_bytes(&root.join("f")).starts_with(br"\\?\"));
        assert!(target_bytes(&root.join("x.")).starts_with(br"\\?\"));
        // Not a path in the extended form: as it is.
        assert_eq!(target_bytes(Path::new(r"C:\a\b.")), br"C:\a\b.");
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

    /// `fsutil sparse <command> <path>`: its output, after checking it
    /// succeeded.
    fn fsutil_sparse(command: &str, path: &Path) -> String {
        let out = std::process::Command::new("fsutil")
            .args(["sparse", command])
            .arg(path)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "fsutil sparse {command}: {text}");
        text
    }

    #[test]
    fn punch_hole_makes_a_real_hole() {
        // Checked by the system's own tool rather than by more calls of
        // ours: the file is sparse and the range is deallocated.
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("f");
        let len = 1 << 20;
        fs::write(&p, vec![0x5A; len]).unwrap();
        let dense = fsutil_sparse("queryflag", &p);
        let mut f = OpenOptions::new().read(true).write(true).open(&p).unwrap();
        assert!(punch_hole(&f, 262_144, 524_288).unwrap());
        let mut back = Vec::new();
        f.read_to_end(&mut back).unwrap();
        assert!(back[..262_144].iter().all(|&b| b == 0x5A));
        assert!(back[262_144..786_432].iter().all(|&b| b == 0));
        assert!(back[786_432..].iter().all(|&b| b == 0x5A));
        drop(f);
        assert_eq!(lstat(&p).unwrap().size, len as u64);
        // The flag's text is in the system's language: it differs from the
        // one for the file before, whatever the language.
        assert_ne!(fsutil_sparse("queryflag", &p), dense);
        let ranges = fsutil_sparse("queryrange", &p);
        assert_eq!(
            ranges.lines().filter(|l| l.contains("0x")).count(),
            2,
            "{ranges}"
        );
    }

    #[test]
    fn a_bad_range_is_refused_before_the_file_is_marked_sparse() {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_SPARSE_FILE;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("f");
        fs::write(&p, b"abc").unwrap();
        let f = OpenOptions::new().write(true).open(&p).unwrap();
        for (offset, len) in [(u64::MAX, 2), (1 << 63, 0), (0, 1 << 63)] {
            let err = punch_hole(&f, offset, len).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{offset} {len}");
        }
        let sparse =
            |p: &Path| fs::metadata(p).unwrap().file_attributes() & FILE_ATTRIBUTE_SPARSE_FILE;
        assert_eq!(sparse(&p), 0);
        assert!(punch_hole(&f, 0, 1).unwrap());
        assert_ne!(sparse(&p), 0);
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
