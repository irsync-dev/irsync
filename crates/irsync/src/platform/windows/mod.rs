//! The Windows implementation: `std::os::windows`, `windows-sys` and the
//! native calls of `native.rs`, the only file with `unsafe` in it.
//!
//! A [`Dir`] is a directory handle. Every operation on a name inside it
//! opens that one name relative to the handle (`NtCreateFile` with
//! `RootDirectory`), never following a reparse point
//! (`FILE_OPEN_REPARSE_POINT`, `OBJ_DONT_REPARSE`), then checks what it
//! opened on the handle it holds and acts on that handle: rename, link and
//! delete by handle. Between the check and the act there is no name to
//! swap.
//!
//! One race the check on the handle cannot cover: a directory that a
//! `Dir` already holds can be turned into a reparse point (a junction, a
//! directory symlink) through another handle, if it is empty. The held
//! handle still names that directory, not the link's target. What then
//! happens to every relative open, rename and link through it was
//! observed on Windows 11, not found specified: the kernel refuses each
//! with `STATUS_REPARSE_POINT_NOT_RESOLVED` (`ERROR_CANT_RESOLVE_FILENAME`,
//! read here as [`Cause::LinkRefused`]), with or without
//! `FILE_OPEN_REPARSE_POINT`, and listing gives the directory's own (no)
//! names. So an attacker who can write the directory can make operations
//! in an empty directory fail, which they could do anyway by removing it;
//! they cannot redirect one. `dir_tests` holds the kernel to this.
//!
//! Names are WTF-8 bytes ([`crate::wtf8`]) on the irsync side and UTF-16
//! units here; bytes that are not WTF-8 are refused, never repaired.

// Only `native` holds unsafe code; the rest of the Windows module is safe.
#![deny(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::os::windows::io::{AsHandle, BorrowedHandle, OwnedHandle};
use std::path::Path;

use windows_sys::Wdk::Storage::FileSystem::{
    FILE_BASIC_INFORMATION, FILE_CREATE, FILE_DIRECTORY_FILE, FILE_DISPOSITION_DELETE,
    FILE_DISPOSITION_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_INFORMATION_EX,
    FILE_DISPOSITION_POSIX_SEMANTICS, FILE_ID_INFORMATION, FILE_INTERNAL_INFORMATION,
    FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_IF, FILE_OPEN_REPARSE_POINT,
    FILE_RENAME_IGNORE_READONLY_ATTRIBUTE, FILE_RENAME_POSIX_SEMANTICS,
    FILE_RENAME_REPLACE_IF_EXISTS, FILE_SYNCHRONOUS_IO_NONALERT, SYMLINK_FLAG_RELATIVE,
};
use windows_sys::Wdk::System::SystemServices::FILE_ATTRIBUTE_TAG_INFORMATION;
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_CANT_RESOLVE_FILENAME, ERROR_DELETE_PENDING,
    ERROR_DIRECTORY, ERROR_DIRECTORY_NOT_SUPPORTED, ERROR_DIR_NOT_EMPTY, ERROR_DISK_FULL,
    ERROR_FILE_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_INVALID_FUNCTION, ERROR_LOCK_VIOLATION,
    ERROR_NOT_A_REPARSE_POINT, ERROR_NOT_SAME_DEVICE, ERROR_NOT_SUPPORTED,
    ERROR_NO_SYSTEM_RESOURCES, ERROR_PATH_NOT_FOUND, ERROR_REPARSE_POINT_ENCOUNTERED,
    ERROR_SHARING_VIOLATION, ERROR_STOPPED_ON_SYMLINK, ERROR_TOO_MANY_OPEN_FILES,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
    FILE_GENERIC_WRITE, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA,
    MAXIMUM_REPARSE_DATA_BUFFER_SIZE, SYNCHRONIZE,
};
use windows_sys::Win32::System::SystemServices::{
    IO_REPARSE_TAG_MOUNT_POINT, IO_REPARSE_TAG_SYMLINK,
};

use super::{Cause, Create, Kind, LinkKind, Open, Stat};
use crate::wtf8;

#[allow(unsafe_code)]
mod native;

// ---------------------------------------------------------------------------
// Errors: what a Win32 error code means.

/// What `e` means, from its Win32 error code. Native statuses reach here
/// as Win32 codes too (`native::status_error`). An error without a code is
/// [`Cause::NotSupported`] if its kind says unsupported (the operations
/// Windows does not have), else [`Cause::Other`].
///
/// `ERROR_ACCESS_DENIED` is [`Cause::Other`]: Windows gives it for a
/// read-only file, an ACL, a directory where a file was expected and a
/// file being deleted alike, so it cannot be read as "in use". This module
/// turns the two of those it can tell apart into their own codes.
pub fn cause(e: &io::Error) -> Cause {
    let Some(n) = e.raw_os_error() else {
        return if e.kind() == io::ErrorKind::Unsupported {
            Cause::NotSupported
        } else {
            Cause::Other
        };
    };
    match n as u32 {
        // A relative open through a held directory that has become a
        // reparse point (see the module doc). `ERROR_REPARSE_POINT_ENCOUNTERED`
        // is what `OBJ_DONT_REPARSE` gives for a reparse point met on the
        // way to a name; with one-component names it is not expected, but
        // if it comes it means exactly a link where none may be followed.
        ERROR_STOPPED_ON_SYMLINK
        | ERROR_CANT_RESOLVE_FILENAME
        | ERROR_REPARSE_POINT_ENCOUNTERED => Cause::LinkRefused,
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Cause::NotThere,
        ERROR_ALREADY_EXISTS | ERROR_FILE_EXISTS => Cause::AlreadyThere,
        ERROR_DIRECTORY => Cause::NotADirectory,
        ERROR_DIRECTORY_NOT_SUPPORTED => Cause::IsADirectory,
        ERROR_DIR_NOT_EMPTY => Cause::NotEmpty,
        ERROR_NOT_SAME_DEVICE => Cause::CrossDevice,
        ERROR_TOO_MANY_OPEN_FILES | ERROR_NO_SYSTEM_RESOURCES => Cause::OutOfHandles,
        ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION | ERROR_DELETE_PENDING => Cause::InUse,
        ERROR_NOT_SUPPORTED | ERROR_INVALID_FUNCTION => Cause::NotSupported,
        _ => Cause::Other,
    }
}

/// A link where none may be followed: what `cause` reads as
/// [`Cause::LinkRefused`] (`ERROR_STOPPED_ON_SYMLINK`, "the create
/// operation stopped after reaching a symbolic link").
fn link_refused() -> io::Error {
    io::Error::from_raw_os_error(ERROR_STOPPED_ON_SYMLINK as i32)
}

/// [`Cause::NotADirectory`]: `ERROR_DIRECTORY`, which is also what Windows
/// itself gives for a directory open of a file.
fn not_a_directory() -> io::Error {
    io::Error::from_raw_os_error(ERROR_DIRECTORY as i32)
}

/// [`Cause::IsADirectory`]: `ERROR_DIRECTORY_NOT_SUPPORTED`, "an operation
/// is not supported on a directory".
fn is_a_directory() -> io::Error {
    io::Error::from_raw_os_error(ERROR_DIRECTORY_NOT_SUPPORTED as i32)
}

/// What Windows does not have: modes, owners, FIFOs, devices.
fn unsupported(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!("{what} is not supported on Windows"),
    )
}

// ---------------------------------------------------------------------------
// Names: stored bytes (WTF-8) and the names the system takes (UTF-16).

/// The stored bytes of a name: its WTF-8 form.
pub fn os_to_bytes(name: &OsStr) -> Vec<u8> {
    wtf8::encode(&name.encode_wide().collect::<Vec<_>>())
}

/// The name that stored bytes stand for; bytes that are not WTF-8 are an
/// `InvalidInput` error, as a bad name given to [`Dir`] is.
pub fn bytes_to_os(bytes: &[u8]) -> io::Result<OsString> {
    units_of(bytes).map(|u| OsString::from_wide(&u))
}

/// The UTF-16 units of WTF-8 `bytes`; `InvalidInput` if they are not WTF-8.
fn units_of(bytes: &[u8]) -> io::Result<Vec<u16>> {
    wtf8::decode(bytes).map_err(|bad| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "not a valid name (not WTF-8 at byte {}): {:?}",
                bad.at,
                String::from_utf8_lossy(bytes)
            ),
        )
    })
}

/// The units of `name` if it is one component: not empty, `.` or `..`,
/// without `/`, `\`, `:` or NUL, valid WTF-8, and no longer than a native
/// name can be. Checked before any call, so no operation of [`Dir`] can
/// reach past the directory it holds. Every refusal is `InvalidInput`.
///
/// A `:` is refused because a native open reads it as a stream: `f:s` is
/// the stream `s` of `f`, and `f::$DATA` or `d::$INDEX_ALLOCATION` are `f`
/// and `d` themselves, so two different names would reach one file past
/// any rule made on names above this layer. That is the least this layer
/// needs; the full NTFS name rules, with their own messages, come later,
/// above it.
fn one_name(name: &[u8]) -> io::Result<Vec<u16>> {
    if name.is_empty()
        || name == b"."
        || name == b".."
        || name.contains(&b'/')
        || name.contains(&b'\\')
        || name.contains(&b':')
        || name.contains(&0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "not a single file name: {:?}",
                String::from_utf8_lossy(name)
            ),
        ));
    }
    let units = units_of(name)?;
    if units.len() > native::MAX_NAME_UNITS {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "name too long"));
    }
    Ok(units)
}

// ---------------------------------------------------------------------------
// `Dir`: a directory held open, and files opened by path.

/// A directory held open. Every operation takes one name (stored bytes,
/// one component) and never follows a link: a symlink, a junction or any
/// other reparse point is opened itself, checked on its handle, and acted
/// on only as what it is.
#[derive(Debug)]
pub struct Dir(OwnedHandle);

/// Every open shares read, write and delete: irsync never blocks another
/// program, and a file it has open can still be replaced or removed.
const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

/// Every relative open: synchronous I/O (so `std::fs::File` works on the
/// handle, and nothing completes after a call returns), and no reparse
/// point followed.
const NOFOLLOW: u32 = FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT;

/// A directory is opened to list it and open names in it.
const DIR_ACCESS: u32 = FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;

/// `FILE_RENAME_*`: replace, POSIX semantics (a destination open elsewhere
/// with delete sharing is replaced), and ignore the read-only attribute.
const RENAME_FLAGS: u32 = FILE_RENAME_REPLACE_IF_EXISTS
    | FILE_RENAME_POSIX_SEMANTICS
    | FILE_RENAME_IGNORE_READONLY_ATTRIBUTE;

/// The attributes and reparse tag of the open `h`.
fn attribute_tag(h: BorrowedHandle<'_>) -> io::Result<(u32, u32)> {
    let t: FILE_ATTRIBUTE_TAG_INFORMATION = native::query(h)?;
    Ok((t.FileAttributes, t.ReparseTag))
}

fn is_reparse(attributes: u32) -> bool {
    attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

fn is_dir(attributes: u32) -> bool {
    attributes & FILE_ATTRIBUTE_DIRECTORY != 0
}

/// Deletes the file `h` is open on, by handle: POSIX semantics (the name
/// goes when `h` is closed, even while others have the file open) and the
/// read-only attribute ignored. A directory with entries is
/// [`Cause::NotEmpty`].
fn delete(h: BorrowedHandle<'_>) -> io::Result<()> {
    native::set(
        h,
        &FILE_DISPOSITION_INFORMATION_EX {
            Flags: FILE_DISPOSITION_DELETE
                | FILE_DISPOSITION_POSIX_SEMANTICS
                | FILE_DISPOSITION_IGNORE_READONLY_ATTRIBUTE,
        },
    )
}

impl Dir {
    /// Opens `name` (one component, checked) relative to this directory,
    /// never following a reparse point: `access` plus reading attributes,
    /// shared fully, synchronous.
    fn open(
        &self,
        name: &[u16],
        access: u32,
        disposition: u32,
        options: u32,
        attributes: u32,
    ) -> io::Result<OwnedHandle> {
        self.open_shared(name, access, SHARE_ALL, disposition, options, attributes)
    }

    /// [`Dir::open`] with the share mode `share`.
    fn open_shared(
        &self,
        name: &[u16],
        access: u32,
        share: u32,
        disposition: u32,
        options: u32,
        attributes: u32,
    ) -> io::Result<OwnedHandle> {
        native::open_relative(
            self.0.as_handle(),
            name,
            access | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            share,
            disposition,
            options | NOFOLLOW,
            attributes,
        )
    }

    /// Opens the existing `name` for `access` (plus attributes).
    fn open_existing(&self, name: &[u16], access: u32) -> io::Result<OwnedHandle> {
        self.open(name, access, FILE_OPEN, 0, 0)
    }

    /// After an open of `name` failed with `otherwise`: [`link_refused`]
    /// if `name` is a reparse point, else `otherwise`. Only says which
    /// error it was; nothing is done with what is there.
    fn link_or(&self, name: &[u16], otherwise: io::Error) -> io::Error {
        match self
            .open_existing(name, 0)
            .and_then(|h| attribute_tag(h.as_handle()))
        {
            Ok((a, _)) if is_reparse(a) => link_refused(),
            _ => otherwise,
        }
    }

    /// Opens `path` by name. Links in `path` are followed: the caller
    /// trusts it.
    pub fn open_root(path: &Path) -> io::Result<Dir> {
        Self::open_root_with(path, 0)
    }

    /// Opens `path` by name, like [`Dir::open_root`], but a link (any
    /// reparse point) as its final name is refused ([`Cause::LinkRefused`]).
    /// Links before it are followed: the caller trusts them.
    pub fn open_root_nofollow(path: &Path) -> io::Result<Dir> {
        Self::open_root_with(path, FILE_FLAG_OPEN_REPARSE_POINT)
    }

    fn open_root_with(path: &Path, flags: u32) -> io::Result<Dir> {
        let h = OwnedHandle::from(
            std::fs::OpenOptions::new()
                .access_mode(DIR_ACCESS)
                .share_mode(SHARE_ALL)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | flags)
                .open(path)?,
        );
        let (a, _) = attribute_tag(h.as_handle())?;
        if flags & FILE_FLAG_OPEN_REPARSE_POINT != 0 && is_reparse(a) {
            return Err(link_refused());
        }
        if !is_dir(a) {
            return Err(not_a_directory());
        }
        Ok(Dir(h))
    }

    /// Opens the directory `name`. A link (any reparse point) there is
    /// [`Cause::LinkRefused`], anything else that is not a directory
    /// [`Cause::NotADirectory`].
    pub fn open_dir(&self, name: &[u8]) -> io::Result<Dir> {
        let name = one_name(name)?;
        let h = match self.open(&name, DIR_ACCESS, FILE_OPEN, FILE_DIRECTORY_FILE, 0) {
            Ok(h) => h,
            // A file, or a file symlink: say which.
            Err(e) if e.raw_os_error() == Some(ERROR_DIRECTORY as i32) => {
                return Err(self.link_or(&name, e))
            }
            Err(e) => return Err(e),
        };
        let (a, _) = attribute_tag(h.as_handle())?;
        if is_reparse(a) {
            return Err(link_refused());
        }
        if !is_dir(a) {
            return Err(not_a_directory());
        }
        Ok(Dir(h))
    }

    /// Makes the directory `name`. Windows has no mode: `mode` is ignored.
    pub fn create_dir(&self, name: &[u8], _mode: u32) -> io::Result<()> {
        let name = one_name(name)?;
        self.open(
            &name,
            0,
            FILE_CREATE,
            FILE_DIRECTORY_FILE,
            FILE_ATTRIBUTE_NORMAL,
        )
        .map(drop)
    }

    /// Opens the file `name` as `how` says. A link (any reparse point) is
    /// [`Cause::LinkRefused`] and never changed (a truncate happens only
    /// after the check), a directory [`Cause::IsADirectory`]. `nonblock`
    /// and `mode` mean nothing on Windows. The file is shared for read,
    /// write and delete. `truncate` without `write` truncates all the same,
    /// as Linux does for `O_RDONLY | O_TRUNC`: the handle gets the data
    /// write access that needs.
    pub fn open_file(&self, name: &[u8], how: Open) -> io::Result<File> {
        let name = one_name(name)?;
        let mut access = 0;
        if how.read || !how.write {
            access |= FILE_GENERIC_READ;
        }
        if how.write {
            access |= FILE_GENERIC_WRITE;
        }
        if how.truncate {
            access |= FILE_WRITE_DATA;
        }
        let disposition = match how.create {
            Create::No => FILE_OPEN,
            Create::IfMissing => FILE_OPEN_IF,
            Create::New => FILE_CREATE,
        };
        let h = match self.open(
            &name,
            access,
            disposition,
            FILE_NON_DIRECTORY_FILE,
            FILE_ATTRIBUTE_NORMAL,
        ) {
            Ok(h) => h,
            // Something is there: for a new file that is all that counts,
            // as for Unix `O_EXCL`, which NTFS checks after the kind.
            Err(e)
                if e.raw_os_error() == Some(ERROR_DIRECTORY_NOT_SUPPORTED as i32)
                    && how.create == Create::New =>
            {
                return Err(io::Error::from_raw_os_error(ERROR_ALREADY_EXISTS as i32))
            }
            // A directory, or a directory link: say which.
            Err(e) if e.raw_os_error() == Some(ERROR_DIRECTORY_NOT_SUPPORTED as i32) => {
                return Err(self.link_or(&name, e))
            }
            Err(e) => return Err(e),
        };
        let (a, _) = attribute_tag(h.as_handle())?;
        if is_reparse(a) {
            return Err(link_refused());
        }
        if is_dir(a) {
            return Err(is_a_directory());
        }
        let file = File::from(h);
        if how.truncate {
            file.set_len(0)?;
        }
        Ok(file)
    }

    /// What is at `name`; a link is described, not followed.
    pub fn stat(&self, name: &[u8]) -> io::Result<Stat> {
        let name = one_name(name)?;
        let h = self.open_existing(&name, 0)?;
        stat_of(h.as_handle())
    }

    /// The directory itself.
    pub fn stat_self(&self) -> io::Result<Stat> {
        stat_of(self.0.as_handle())
    }

    /// Whether a name made in this directory is looked up ignoring case;
    /// `None`: not asked here (NTFS ignores case unless a directory is set
    /// otherwise).
    pub fn ignores_case(&self) -> Option<bool> {
        None
    }

    /// The target of the link `name`, as stored: a symlink's, or a
    /// junction's. Anything else is an error (`ERROR_NOT_A_REPARSE_POINT`
    /// for a plain file, which [`cause`] reads as [`Cause::Other`], as
    /// Unix's `EINVAL`).
    pub fn read_link(&self, name: &[u8]) -> io::Result<Vec<u8>> {
        let name = one_name(name)?;
        let h = self.open_existing(&name, 0)?;
        let data = native::get_reparse(h.as_handle())?;
        Ok(wtf8::encode(&parse_reparse(&data)?))
    }

    /// The names in the directory, without `.` and `..`, in no order.
    pub fn list(&self) -> io::Result<Vec<Vec<u8>>> {
        let mut names = Vec::new();
        self.list_while(|n| {
            names.push(n.to_vec());
            true
        })?;
        Ok(names)
    }

    /// Calls `f` with each name in the directory (without `.` and `..`),
    /// in the order the directory gives them, and stops reading as soon as
    /// `f` returns false. Every listing starts from the first name. A
    /// failed read ends the listing with its error.
    pub fn list_while(&self, mut f: impl FnMut(&[u8]) -> bool) -> io::Result<()> {
        let mut restart = true;
        while let Some(bytes) = native::list_some(self.0.as_handle(), restart)? {
            restart = false;
            for name in parse_names(&bytes)? {
                if name == [u16::from(b'.')] || name == [u16::from(b'.'); 2] {
                    continue;
                }
                if !f(&wtf8::encode(&name)) {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Removes `name`, which is not a directory, as Unix `unlink` does:
    /// a file, or a link of any kind (a file symlink, a directory symlink,
    /// a junction, any other reparse point), never what it leads to. A
    /// plain directory is [`Cause::IsADirectory`].
    pub fn remove_file(&self, name: &[u8]) -> io::Result<()> {
        let name = one_name(name)?;
        let h = self.open_existing(&name, DELETE)?;
        let (a, _) = attribute_tag(h.as_handle())?;
        if is_dir(a) && !is_reparse(a) {
            return Err(is_a_directory());
        }
        delete(h.as_handle())
    }

    /// Removes the empty directory `name`. Anything else, a link to a
    /// directory or a junction included, is [`Cause::NotADirectory`], as
    /// Unix `rmdir` on a symlink: [`Dir::remove_file`] removes links.
    pub fn remove_dir(&self, name: &[u8]) -> io::Result<()> {
        let name = one_name(name)?;
        let h = self.open_existing(&name, DELETE)?;
        let (a, _) = attribute_tag(h.as_handle())?;
        if is_reparse(a) || !is_dir(a) {
            return Err(not_a_directory());
        }
        delete(h.as_handle())
    }

    /// Moves `name` (a link itself, never followed) to `to_name` in `to`,
    /// replacing what is there as Unix `rename` does: a file even if it is
    /// read-only or open elsewhere with delete sharing, an empty directory
    /// with a directory. A directory with entries there is
    /// [`Cause::NotEmpty`], a directory for a file [`Cause::IsADirectory`],
    /// a file for a directory [`Cause::NotADirectory`], and a destination
    /// open elsewhere without delete sharing [`Cause::InUse`].
    pub fn rename(&self, name: &[u8], to: &Dir, to_name: &[u8]) -> io::Result<()> {
        let (name, to_name) = (one_name(name)?, one_name(to_name)?);
        let h = self.open_existing(&name, DELETE)?;
        native::rename_to(h.as_handle(), to.0.as_handle(), &to_name, RENAME_FLAGS)
    }

    /// Makes `name` a second name of the file `from_name` in `from`. A
    /// link at `from_name` is linked itself, never followed, as Unix
    /// `linkat` without `AT_SYMLINK_FOLLOW` does (NTFS links a file
    /// symlink; a directory, a directory symlink or a junction it refuses,
    /// [`Cause::IsADirectory`]). An existing `name` is not replaced.
    pub fn hard_link(&self, name: &[u8], from: &Dir, from_name: &[u8]) -> io::Result<()> {
        let (name, from_name) = (one_name(name)?, one_name(from_name)?);
        let h = from.open_existing(&from_name, 0)?;
        native::link_to(h.as_handle(), self.0.as_handle(), &name)
    }

    /// Makes `name` a symlink to `target`: a file symlink or a directory
    /// symlink as `kind` says. A target that is not absolute is stored as
    /// relative; an absolute one (`C:\…`, `\\?\…`, `\\server\share\…`)
    /// gets the native form Windows follows, and the target as given as
    /// the name [`Dir::read_link`] returns. Needs the symlink privilege or
    /// Developer Mode; without either the error is
    /// `ERROR_PRIVILEGE_NOT_HELD` and nothing is left at `name`.
    ///
    /// Until the reparse point is set, the new file or directory is held
    /// with no sharing, so no other handle can open it (a `stat` of the
    /// name still works: reading attributes is not shared access, and it
    /// sees a plain empty file or directory for that moment). Sharing does
    /// not stop another program from making an entry inside a new
    /// directory, though; then the link cannot be set, the directory
    /// cannot be removed, and the error says that a directory was left.
    pub fn symlink(&self, name: &[u8], target: &[u8], kind: LinkKind) -> io::Result<()> {
        let name = one_name(name)?;
        let target = units_of(target)?;
        if target.is_empty() || target.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a symlink target is not empty and has no NUL",
            ));
        }
        let data = symlink_reparse(&target)?;
        let options = match kind {
            LinkKind::File => FILE_NON_DIRECTORY_FILE,
            LinkKind::Dir => FILE_DIRECTORY_FILE,
        };
        let h = self.open_shared(
            &name,
            FILE_WRITE_ATTRIBUTES | FILE_WRITE_DATA | DELETE,
            0,
            FILE_CREATE,
            options,
            FILE_ATTRIBUTE_NORMAL,
        )?;
        if let Err(e) = native::set_reparse(h.as_handle(), &data) {
            // Not a link after all: what was made goes again.
            return match delete(h.as_handle()) {
                Ok(()) => Err(e),
                Err(left) => Err(io::Error::new(
                    e.kind(),
                    format!(
                        "making the symlink {:?} failed ({e}), and what was made in its \
                         place could not be removed ({left})",
                        String::from_utf8_lossy(&wtf8::encode(&name))
                    ),
                )),
            };
        }
        Ok(())
    }

    /// Sets the modification time of `name`, a link's own, never its
    /// target's, in NTFS's steps of 100 ns (the rest is cut off). The
    /// other times stay: in `FILE_BASIC_INFORMATION` a time of 0 means
    /// "leave it". A time before 1601 cannot be stored (`InvalidInput`).
    pub fn set_mtime(&self, name: &[u8], mtime: (i64, u32)) -> io::Result<()> {
        let name = one_name(name)?;
        let t = file_time(mtime)?;
        let h = self.open_existing(&name, FILE_WRITE_ATTRIBUTES)?;
        native::set(
            h.as_handle(),
            &FILE_BASIC_INFORMATION {
                LastWriteTime: t,
                ..Default::default()
            },
        )
    }

    /// Not on Windows ([`Cause::NotSupported`]); the name is checked first.
    pub fn set_mode(&self, name: &[u8], _mode: u32) -> io::Result<()> {
        one_name(name)?;
        Err(unsupported("a file mode"))
    }

    /// Not on Windows ([`Cause::NotSupported`]); the name is checked first.
    pub fn set_owner(&self, name: &[u8], _uid: Option<u32>, _gid: Option<u32>) -> io::Result<()> {
        one_name(name)?;
        Err(unsupported("a Unix owner"))
    }

    /// Not on Windows ([`Cause::NotSupported`]); the name is checked first.
    pub fn make_fifo(&self, name: &[u8], _mode: u32) -> io::Result<()> {
        one_name(name)?;
        Err(unsupported("a FIFO"))
    }

    /// Not on Windows ([`Cause::NotSupported`]); the name is checked first.
    pub fn make_device(&self, name: &[u8], _kind: Kind, _mode: u32, _rdev: u64) -> io::Result<()> {
        one_name(name)?;
        Err(unsupported("a device node"))
    }
}

/// Opens the file at `path` for reading, shared for read, write and
/// delete. A link (any reparse point) as the final name is refused
/// ([`Cause::LinkRefused`]); links before it are followed: the caller
/// trusts them. A directory cannot be opened this way (Windows refuses
/// it, "access denied"; that is said as [`Cause::IsADirectory`], or as
/// [`Cause::LinkRefused`] for a directory link), so what opens is a
/// regular file.
pub fn open_read_nofollow(path: &Path) -> io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    open_nofollow(options, path)
}

/// Opens `path` as `options` say (std's default sharing: read, write and
/// delete), as [`open_read_nofollow`] does: a final link is opened itself
/// and refused, a directory refused, links before it followed. `options`
/// must not truncate: a truncating open of a link would change the link.
fn open_nofollow(mut options: std::fs::OpenOptions, path: &Path) -> io::Result<File> {
    let file = match options
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
    {
        Ok(file) => file,
        // Only says which error it was: what is there is not used.
        Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
            return Err(match lstat(path) {
                Ok(st) if matches!(st.kind, Kind::Symlink | Kind::Other) => link_refused(),
                Ok(st) if st.kind == Kind::Dir => is_a_directory(),
                _ => e,
            })
        }
        Err(e) => return Err(e),
    };
    let (a, _) = attribute_tag(file.as_handle())?;
    if is_reparse(a) {
        return Err(link_refused());
    }
    if is_dir(a) {
        return Err(is_a_directory());
    }
    Ok(file)
}

// ---------------------------------------------------------------------------
// Stat and positional I/O.

/// 100 ns steps from 1601-01-01 to 1970-01-01.
const UNIX_EPOCH_IN_FILE_TIME: i128 = 116_444_736_000_000_000;
const STEPS_PER_SECOND: i128 = 10_000_000;

/// A file time (100 ns steps since 1601) as `(seconds, nanoseconds)` since
/// the Unix epoch; the nanoseconds are never negative, also before 1970.
fn unix_time(t: i64) -> (i64, u32) {
    let since = i128::from(t) - UNIX_EPOCH_IN_FILE_TIME;
    (
        // |t| / 10^7 fits an i64.
        since.div_euclid(STEPS_PER_SECOND) as i64,
        (since.rem_euclid(STEPS_PER_SECOND) * 100) as u32,
    )
}

/// `(seconds, nanoseconds)` since the Unix epoch as a file time, cut to
/// 100 ns. Times NTFS cannot store (before 1601, or 0, 1601 itself, which
/// `FILE_BASIC_INFORMATION` reads as "leave it") are `InvalidInput`.
fn file_time(t: (i64, u32)) -> io::Result<i64> {
    let bad = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("time {t:?} out of range"),
        )
    };
    if t.1 >= 1_000_000_000 {
        return Err(bad());
    }
    let steps =
        i128::from(t.0) * STEPS_PER_SECOND + i128::from(t.1 / 100) + UNIX_EPOCH_IN_FILE_TIME;
    match i64::try_from(steps) {
        Ok(s) if s > 0 => Ok(s),
        _ => Err(bad()),
    }
}

/// The kind of a file from its attributes and reparse tag: only a symlink
/// is [`Kind::Symlink`]; a junction or any other reparse point is
/// [`Kind::Other`].
fn kind_of(attributes: u32, tag: u32) -> Kind {
    if is_reparse(attributes) {
        if tag == IO_REPARSE_TAG_SYMLINK {
            Kind::Symlink
        } else {
            Kind::Other
        }
    } else if is_dir(attributes) {
        Kind::Dir
    } else {
        Kind::File
    }
}

/// What the open `h` is. `ino` is the 64-bit file index, `dev` the volume
/// serial number; Windows has no mode, owner or device number (all 0), and
/// sparse files are not reported.
fn stat_of(h: BorrowedHandle<'_>) -> io::Result<Stat> {
    let basic: FILE_BASIC_INFORMATION = native::query(h)?;
    let standard: native::StandardInfo = native::query(h)?;
    let (attributes, tag) = attribute_tag(h)?;
    let index: FILE_INTERNAL_INFORMATION = native::query(h)?;
    let id: FILE_ID_INFORMATION = native::query(h)?;
    let kind = kind_of(attributes, tag);
    Ok(Stat {
        kind,
        mode: 0,
        uid: 0,
        gid: 0,
        size: if kind == Kind::Dir {
            0
        } else {
            standard.end_of_file.max(0) as u64
        },
        nlink: u64::from(standard.number_of_links),
        ino: index.IndexNumber as u64,
        dev: id.VolumeSerialNumber,
        rdev: 0,
        mtime: unix_time(basic.LastWriteTime),
        ctime: unix_time(basic.ChangeTime),
        sparse: false,
    })
}

/// What the open `file` is.
pub fn fstat(file: &File) -> io::Result<Stat> {
    stat_of(file.as_handle())
}

/// What is at `path`; a final link is described, not followed. Links
/// before it are followed: the caller trusts them.
pub fn lstat(path: &Path) -> io::Result<Stat> {
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(SHARE_ALL)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    stat_of(file.as_handle())
}

/// What the open `file` is: [`fstat`], read by handle (`std::fs::Metadata`
/// has no file index, volume serial, change time or link count on Windows).
pub fn file_metadata(file: &File) -> io::Result<Stat> {
    fstat(file)
}

/// What is at `path`, a final link described, not followed: [`lstat`].
pub fn symlink_metadata(path: &Path) -> io::Result<Stat> {
    lstat(path)
}

/// The modification time of the file `m` describes, `(seconds,
/// nanoseconds)`, as [`Stat::mtime`] gives it: the same `LastWriteTime`.
pub fn mtime_of(m: &std::fs::Metadata) -> (i64, u32) {
    use std::os::windows::fs::MetadataExt;
    // A file time is below `i64::MAX` steps (year 30828).
    unix_time(m.last_write_time() as i64)
}

/// The mode of the file `m` describes: always 0, Windows has none.
pub fn mode_of(_m: &std::fs::Metadata) -> u32 {
    0
}

/// Reads at `offset`. Never relies on the file's position, but on Windows
/// it moves it.
pub fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    file.seek_read(buf, offset)
}

/// Writes at `offset`. Never relies on the file's position, but on Windows
/// it moves it.
pub fn write_at(file: &File, buf: &[u8], offset: u64) -> io::Result<usize> {
    file.seek_write(buf, offset)
}

/// Fills `buf` from `offset`; a file that ends first is an
/// `UnexpectedEof` error. Moves the file's position, as [`read_at`].
pub fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        match file.seek_read(buf, offset) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "failed to fill whole buffer",
                ))
            }
            Ok(n) => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Writes all of `buf` at `offset`. Moves the file's position, as
/// [`write_at`].
pub fn write_all_at(file: &File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        match file.seek_write(buf, offset) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write whole buffer",
                ))
            }
            Ok(n) => {
                buf = &buf[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Bytes from the kernel: directory entries and reparse buffers, parsed with
// every length checked.

fn malformed(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("malformed {what}"))
}

/// The `u16` at `at` in `b`, little-endian.
fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        b.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

/// The `u32` at `at` in `b`, little-endian.
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

/// `len` bytes at `at` in `b` as UTF-16 units; `len` must be even.
fn units_at(b: &[u8], at: usize, len: usize) -> Option<Vec<u16>> {
    if !len.is_multiple_of(2) {
        return None;
    }
    let bytes = b.get(at..at.checked_add(len)?)?;
    Some(
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect(),
    )
}

/// The names in a buffer of `FILE_NAMES_INFORMATION` entries
/// (`NextEntryOffset` u32, `FileIndex` u32, `FileNameLength` u32 in bytes,
/// then the name), as `NtQueryDirectoryFile` writes them.
fn parse_names(b: &[u8]) -> io::Result<Vec<Vec<u16>>> {
    const HEADER: usize = 12;
    let bad = || malformed("directory listing");
    let mut names = Vec::new();
    let mut at = 0;
    while at < b.len() {
        let next = u32_at(b, at).ok_or_else(bad)? as usize;
        let len = u32_at(b, at + 8).ok_or_else(bad)? as usize;
        names.push(units_at(b, at + HEADER, len).ok_or_else(bad)?);
        if next == 0 {
            break;
        }
        if next < HEADER + len {
            return Err(bad());
        }
        at = at
            .checked_add(next)
            .filter(|&n| n < b.len())
            .ok_or_else(bad)?;
    }
    Ok(names)
}

/// The size of a reparse buffer's header: tag, data length, reserved.
const REPARSE_HEADER: usize = 8;

/// `\??\`, the prefix of a native absolute path.
const NT_PREFIX: &[u16] = &[b'\\' as u16, b'?' as u16, b'?' as u16, b'\\' as u16];

fn units(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// `target` with every `/` as `\`, the only separator Windows follows in
/// a link.
fn backslashed(target: &[u16]) -> Vec<u16> {
    target
        .iter()
        .map(|&u| {
            if u == u16::from(b'/') {
                u16::from(b'\\')
            } else {
                u
            }
        })
        .collect()
}

/// The substitute name Windows follows for the absolute `target` as given
/// (`C:\x`, `\\?\C:\x`, `\\.\C:\x`, `\??\C:\x` or `\\server\share\x`), with
/// `/` as `\`; `None` for a relative target.
fn native_target(target: &[u16]) -> Option<Vec<u16>> {
    let t = backslashed(target);
    let starts = |p: &str| t.starts_with(&units(p));
    let with = |prefix: &str, rest: &[u16]| [units(prefix).as_slice(), rest].concat();
    if starts("\\??\\") {
        Some(t)
    } else if starts("\\\\?\\") || starts("\\\\.\\") {
        Some(with("\\??\\", &t[4..]))
    } else if starts("\\\\") {
        Some(with("\\??\\UNC\\", &t[2..]))
    } else if t.len() >= 3
        && t[0] < 0x80
        && (t[0] as u8).is_ascii_alphabetic()
        && t[1] == u16::from(b':')
        && t[2] == u16::from(b'\\')
    {
        Some(with("\\??\\", &t))
    } else {
        None
    }
}

/// A reparse buffer of `tag` with a substitute and a print name, laid out
/// as `REPARSE_DATA_BUFFER` (ntifs.h) holds them: the header, then for a
/// symlink the four name offsets and lengths and `flags`, for a mount point
/// the four only; then the substitute name, then the print name, each
/// followed by a NUL that the lengths leave out. Too long for a reparse
/// buffer is `InvalidInput`.
fn reparse_buffer(
    tag: u32,
    substitute: &[u16],
    print: &[u16],
    flags: Option<u32>,
) -> io::Result<Vec<u8>> {
    let sub_len = substitute.len() * 2;
    let print_len = print.len() * 2;
    let fields = if flags.is_some() { 12 } else { 8 };
    let data_len = fields + sub_len + 2 + print_len + 2;
    if REPARSE_HEADER + data_len > MAXIMUM_REPARSE_DATA_BUFFER_SIZE as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "link target too long",
        ));
    }
    let mut b = Vec::with_capacity(REPARSE_HEADER + data_len);
    b.extend_from_slice(&tag.to_le_bytes());
    b.extend_from_slice(&(data_len as u16).to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes()); // substitute name offset
    b.extend_from_slice(&(sub_len as u16).to_le_bytes());
    b.extend_from_slice(&((sub_len + 2) as u16).to_le_bytes()); // print name offset
    b.extend_from_slice(&(print_len as u16).to_le_bytes());
    if let Some(f) = flags {
        b.extend_from_slice(&f.to_le_bytes());
    }
    for u in substitute.iter().chain(&[0]).chain(print).chain(&[0]) {
        b.extend_from_slice(&u.to_le_bytes());
    }
    Ok(b)
}

/// The reparse buffer of a symlink to `target`: a relative one with
/// `SYMLINK_FLAG_RELATIVE` (and `/` as `\` in the substitute name, the
/// one Windows follows), an absolute one with its native form as the
/// substitute name; the print name is always `target` as given, so
/// [`parse_reparse`] gives back exactly `target`.
fn symlink_reparse(target: &[u16]) -> io::Result<Vec<u8>> {
    match native_target(target) {
        Some(substitute) => reparse_buffer(IO_REPARSE_TAG_SYMLINK, &substitute, target, Some(0)),
        None => reparse_buffer(
            IO_REPARSE_TAG_SYMLINK,
            &backslashed(target),
            target,
            Some(SYMLINK_FLAG_RELATIVE),
        ),
    }
}

/// The reparse buffer of a junction to the absolute `target`.
fn junction_reparse(target: &[u16]) -> io::Result<Vec<u8>> {
    let substitute = native_target(target).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "a junction needs an absolute target",
        )
    })?;
    reparse_buffer(IO_REPARSE_TAG_MOUNT_POINT, &substitute, target, None)
}

/// The target of a symlink's or a junction's reparse buffer: the print
/// name if there is one, else the substitute name without `\??\` (and
/// `\??\UNC\` as `\\`). Any other tag is `ERROR_NOT_A_REPARSE_POINT`; a
/// buffer whose lengths do not fit is `InvalidData`.
fn parse_reparse(b: &[u8]) -> io::Result<Vec<u16>> {
    let bad = || malformed("reparse buffer");
    let tag = u32_at(b, 0).ok_or_else(bad)?;
    let data_len = u16_at(b, 4).ok_or_else(bad)? as usize;
    let data = b
        .get(REPARSE_HEADER..REPARSE_HEADER + data_len)
        .ok_or_else(bad)?;
    let names_at = match tag {
        IO_REPARSE_TAG_SYMLINK => 12,
        IO_REPARSE_TAG_MOUNT_POINT => 8,
        _ => {
            return Err(io::Error::from_raw_os_error(
                ERROR_NOT_A_REPARSE_POINT as i32,
            ))
        }
    };
    let paths = data.get(names_at..).ok_or_else(bad)?;
    let name = |field: usize| -> io::Result<Vec<u16>> {
        let off = u16_at(data, field).ok_or_else(bad)? as usize;
        let len = u16_at(data, field + 2).ok_or_else(bad)? as usize;
        units_at(paths, off, len).ok_or_else(bad)
    };
    let (substitute, print) = (name(0)?, name(4)?);
    if !print.is_empty() {
        return Ok(print);
    }
    let unc = units("\\??\\UNC\\");
    Ok(if substitute.starts_with(&unc) {
        [units("\\\\").as_slice(), &substitute[unc.len()..]].concat()
    } else if substitute.starts_with(NT_PREFIX) {
        substitute[NT_PREFIX.len()..].to_vec()
    } else {
        substitute
    })
}

// ---------------------------------------------------------------------------
// Errors that no `Cause` names, and links by path.

/// Whether `e` is a momentary shortage worth waiting out that no [`Cause`]
/// names. None is known on Windows: running out of handles is
/// [`Cause::OutOfHandles`] already.
pub fn is_transient_extra(_e: &io::Error) -> bool {
    false
}

/// The error a write gets on a full disk (`ERROR_DISK_FULL`), for a test
/// that makes one fail as if it had.
pub fn no_space_error() -> io::Error {
    io::Error::from_raw_os_error(ERROR_DISK_FULL as i32)
}

/// Makes `link` a symlink to `target`, both paths the caller trusts: a
/// directory symlink if `target` (from `link`'s directory) is a directory
/// now, else a file symlink. Needs the symlink privilege or Developer Mode.
pub fn symlink_path(target: impl AsRef<Path>, link: impl AsRef<Path>) -> io::Result<()> {
    let (target, link) = (target.as_ref(), link.as_ref());
    let base = link.parent().unwrap_or(Path::new("."));
    if base.join(target).is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

// ---------------------------------------------------------------------------
// Metadata setters by open file and by path. Windows keeps only the
// modification time: a mode or an owner cannot be set (`Unsupported`, as
// [`Dir::set_mode`]), except where a caller only means "as a new file has
// it" and must not fail (said at each).

/// Sets `LastWriteTime` of the open `h` (which needs
/// `FILE_WRITE_ATTRIBUTES`), as [`Dir::set_mtime`] does.
fn set_mtime_handle(h: BorrowedHandle<'_>, mtime: (i64, u32)) -> io::Result<()> {
    native::set(
        h,
        &FILE_BASIC_INFORMATION {
            LastWriteTime: file_time(mtime)?,
            ..Default::default()
        },
    )
}

/// Sets the modification time of the open `file`, as [`Dir::set_mtime`]
/// does.
pub fn set_mtime_file(file: &File, mtime: (i64, u32)) -> io::Result<()> {
    set_mtime_handle(file.as_handle(), mtime)
}

/// [`Dir::set_mtime`] of a path the caller trusts; a final link's own
/// time is set.
pub fn set_mtime_path(path: &Path, mtime: (i64, u32)) -> io::Result<()> {
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
        .share_mode(SHARE_ALL)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    set_mtime_handle(file.as_handle(), mtime)
}

/// Not on Windows ([`Cause::NotSupported`]).
pub fn set_mode_file(_file: &File, _mode: u32) -> io::Result<()> {
    Err(unsupported("a file mode"))
}

/// Not on Windows ([`Cause::NotSupported`]).
pub fn set_owner_file(_file: &File, _uid: Option<u32>, _gid: Option<u32>) -> io::Result<()> {
    Err(unsupported("a Unix owner"))
}

/// Not on Windows ([`Cause::NotSupported`]).
pub fn set_mode_path(_path: &Path, _mode: u32) -> io::Result<()> {
    Err(unsupported("a file mode"))
}

/// Not on Windows ([`Cause::NotSupported`]).
pub fn set_owner_path(_path: &Path, _uid: Option<u32>, _gid: Option<u32>) -> io::Result<()> {
    Err(unsupported("a Unix owner"))
}

/// Not on Windows ([`Cause::NotSupported`]).
pub fn make_device_path(_path: &Path, _kind: Kind, _mode: u32, _rdev: u64) -> io::Result<()> {
    Err(unsupported("a device node"))
}

/// Not on Windows ([`Cause::NotSupported`]); the one caller, which makes
/// its own `.part` writable again after a failed finalize, takes that as
/// "could not".
pub fn make_owner_writable(_file: &File) -> io::Result<()> {
    Err(unsupported("a file mode"))
}

/// Does nothing on Windows: a file keeps the access its directory gives
/// it. The callers give a file the mode a new file has, or keep it private,
/// and must not fail for it.
pub fn set_permissions_file(_file: &File, _mode: u32) -> io::Result<()> {
    Ok(())
}

/// Does nothing on Windows: a file keeps the access its directory gives
/// it. The caller makes a directory of irsync's own state private, inside
/// the user's profile, and must not fail for it.
pub fn set_mode_path_following(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Process identity, user and group names: Windows has no Unix ids. The
// process is never root, its ids are 0 and no name has an id.

/// The process's umask: none on Windows.
pub fn umask() -> u32 {
    0
}

/// No Unix user id on Windows: 0.
pub fn euid() -> u32 {
    0
}

/// No Unix group id on Windows: 0.
pub fn egid() -> u32 {
    0
}

/// Never on Windows.
pub fn is_root() -> bool {
    false
}

/// No Unix groups on Windows.
pub fn supplementary_groups() -> Vec<u32> {
    Vec::new()
}

/// No Unix groups on Windows.
pub fn groups() -> Vec<u32> {
    Vec::new()
}

/// No Unix user names on Windows.
pub fn user_name(_uid: u32) -> Option<String> {
    None
}

/// No Unix group names on Windows.
pub fn group_name(_gid: u32) -> Option<String> {
    None
}

/// No Unix user ids on Windows.
pub fn user_id(_name: &str) -> Option<u32> {
    None
}

/// No Unix group ids on Windows.
pub fn group_id(_name: &str) -> Option<u32> {
    None
}

// ---------------------------------------------------------------------------
// Default locations: each from its environment variable when that is set
// and not empty, so a process started with them redirected is redirected
// wholly; else from the system (the Known Folder API, through `dirs`), as
// on Unix a missing `HOME` falls back to the password database. Never the
// current directory, unless the system has no answer either.

/// `name` from `env` as a path, when set and not empty.
fn env_path(env: &dyn Fn(&str) -> Option<OsString>, name: &str) -> Option<std::path::PathBuf> {
    env(name)
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
}

/// `name` from `env`, else what `system` says.
fn env_or_system(
    env: &dyn Fn(&str) -> Option<OsString>,
    name: &str,
    system: fn() -> Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    env_path(env, name).or_else(system)
}

/// The home directory: `%USERPROFILE%`, else the profile folder.
pub fn home_dir(env: &dyn Fn(&str) -> Option<OsString>) -> Option<std::path::PathBuf> {
    env_or_system(env, "USERPROFILE", dirs::home_dir)
}

/// `%APPDATA%\irsync\irsync.toml`, `%LOCALAPPDATA%\irsync` and
/// `%USERPROFILE%\.irsync\id_ed25519` (`.ssh` is never looked in); a
/// variable that is unset or empty is replaced by the system's folder
/// (roaming app data, local app data, the profile). `None` only when the
/// system has no config or state folder either; no key without a home.
pub fn default_locations(env: &dyn Fn(&str) -> Option<OsString>) -> Option<super::Locations> {
    Some(super::Locations {
        config: env_or_system(env, "APPDATA", dirs::config_dir)?
            .join("irsync")
            .join("irsync.toml"),
        state: env_or_system(env, "LOCALAPPDATA", dirs::data_local_dir)?.join("irsync"),
        key: home_dir(env).map(|h| h.join(".irsync").join("id_ed25519")),
    })
}

// ---------------------------------------------------------------------------
// Time.

/// `secs` since the epoch as local time, `YYYY-MM-DD HH:MM:SS`, as on
/// Unix; `@secs` when it cannot be converted. The time zone is the
/// system's, as `jiff` reads it.
pub fn local_time(secs: i64) -> String {
    let Ok(t) = jiff::Timestamp::from_second(secs) else {
        return format!("@{secs}");
    };
    let z = t.to_zoned_datetime(jiff::tz::TimeZone::system());
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        z.year(),
        z.month(),
        z.day(),
        z.hour(),
        z.minute(),
        z.second()
    )
}

// ---------------------------------------------------------------------------
// Private files (`private.rs`); limits, process usage, the host name and
// signals (`process.rs`); reflink clones, which Windows does not make
// (`clone.rs`); a daemon started in the background, not yet on Windows
// (`daemonize.rs`).

mod private;
pub use private::*;

mod process;
pub use process::*;

pub mod clone;

pub mod daemonize;

// ---------------------------------------------------------------------------
// Test helpers.

/// Helpers for tests, in this crate and in `tests/`: what a test needs
/// from the system goes through here.
#[doc(hidden)]
pub mod testutil;

#[cfg(test)]
mod tests;
