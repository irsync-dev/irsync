//! Every `unsafe` block of the Windows platform code: one small function
//! per native call, each with a safe signature, and the constants and
//! plain-data structs they take. Nothing here decides anything; the rules
//! (one name, no link followed, what an error means) are in `mod.rs`.
//!
//! Handles come in as `BorrowedHandle` and go out as `OwnedHandle`; no raw
//! handle leaves this file. Variable-length results (a directory listing,
//! a reparse buffer) leave as plain bytes, parsed in safe code in `mod.rs`.
//!
//! How a native status becomes an `io::Error`: [`status_error`] turns it
//! into a Win32 error code (`RtlNtStatusToDosError`, with two exceptions
//! that code would lose), so one table in `cause` reads native errors and
//! the standard library's alike.

use std::ffi::c_void;
use std::io;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::ptr;

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FileAttributeTagInformation, FileBasicInformation, FileDispositionInformationEx,
    FileIdInformation, FileInternalInformation, FileLinkInformation, FileNamesInformation,
    FileRenameInformationEx, FileStandardInformation, NtCreateFile, NtFsControlFile,
    NtQueryDirectoryFile, NtQueryInformationFile, NtSetInformationFile, FILE_BASIC_INFORMATION,
    FILE_DISPOSITION_INFORMATION_EX, FILE_ID_INFORMATION, FILE_INFORMATION_CLASS,
    FILE_INTERNAL_INFORMATION, FILE_RENAME_INFORMATION,
};
use windows_sys::Wdk::System::SystemServices::FILE_ATTRIBUTE_TAG_INFORMATION;
use windows_sys::Win32::Foundation::{
    RtlNtStatusToDosError, ERROR_DELETE_PENDING, ERROR_DIRECTORY_NOT_SUPPORTED, ERROR_IO_PENDING,
    HANDLE, INVALID_HANDLE_VALUE, NTSTATUS, OBJ_DONT_REPARSE, STATUS_DELETE_PENDING,
    STATUS_FILE_IS_A_DIRECTORY, STATUS_NO_MORE_FILES, STATUS_NO_SUCH_FILE, STATUS_PENDING,
    UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    LockFileEx, ReOpenFile, UnlockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    MAXIMUM_REPARSE_DATA_BUFFER_SIZE,
};
use windows_sys::Win32::System::Ioctl::{FSCTL_GET_REPARSE_POINT, FSCTL_SET_REPARSE_POINT};
use windows_sys::Win32::System::Threading::{WaitForSingleObject, INFINITE};
use windows_sys::Win32::System::IO::{
    GetOverlappedResult, IO_STATUS_BLOCK, IO_STATUS_BLOCK_0, OVERLAPPED, OVERLAPPED_0,
    OVERLAPPED_0_0,
};

/// The most UTF-16 units a `UNICODE_STRING` holds: its lengths are `u16`
/// counts of bytes.
pub(super) const MAX_NAME_UNITS: usize = (u16::MAX / 2) as usize;

/// `FILE_STANDARD_INFORMATION` (wdm.h, ntifs.h), with its two `BOOLEAN`s
/// as `u8`: the windows-sys struct has them as `bool`, and a kernel byte
/// other than 0 or 1 in a `bool` would be undefined behaviour. Same layout
/// (a test compares it with windows-sys').
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub(super) struct StandardInfo {
    pub allocation_size: i64,
    pub end_of_file: i64,
    pub number_of_links: u32,
    pub delete_pending: u8,
    pub directory: u8,
}

mod sealed {
    pub trait Sealed {}
}

/// A fixed-size file information class: a plain-data struct, every bit
/// pattern of which is a valid value, and the class number the kernel
/// fills or reads it as. Sealed: implemented only here.
pub(super) trait Info: sealed::Sealed + Copy + Default {
    const CLASS: FILE_INFORMATION_CLASS;
}

macro_rules! info {
    ($t:ty, $class:expr) => {
        impl sealed::Sealed for $t {}
        impl Info for $t {
            const CLASS: FILE_INFORMATION_CLASS = $class;
        }
    };
}
// Each: integers (and a byte array) only.
info!(FILE_BASIC_INFORMATION, FileBasicInformation);
info!(StandardInfo, FileStandardInformation);
info!(FILE_INTERNAL_INFORMATION, FileInternalInformation);
info!(FILE_ATTRIBUTE_TAG_INFORMATION, FileAttributeTagInformation);
info!(FILE_ID_INFORMATION, FileIdInformation);
info!(
    FILE_DISPOSITION_INFORMATION_EX,
    FileDispositionInformationEx
);

/// The `io::Error` of a failed native status, as a Win32 error code.
/// `RtlNtStatusToDosError` gives `ERROR_ACCESS_DENIED` for both "delete
/// pending" and "is a directory", which would lose their meaning; those
/// two get their own codes, as the standard library does for the first.
pub(super) fn status_error(status: NTSTATUS) -> io::Error {
    let code = match status {
        STATUS_DELETE_PENDING => ERROR_DELETE_PENDING,
        STATUS_FILE_IS_A_DIRECTORY => ERROR_DIRECTORY_NOT_SUPPORTED,
        // SAFETY: RtlNtStatusToDosError takes an integer and returns one;
        // it touches no memory of ours (it sets the thread's last error).
        _ => unsafe { RtlNtStatusToDosError(status) },
    };
    io::Error::from_raw_os_error(code as i32)
}

/// A status block that says "pending" until the kernel writes it.
fn pending_status_block() -> IO_STATUS_BLOCK {
    IO_STATUS_BLOCK {
        Anonymous: IO_STATUS_BLOCK_0 {
            Status: STATUS_PENDING,
        },
        Information: 0,
    }
}

/// The final status of a call that was given `iosb`, and the number its
/// `Information` holds. A call on a handle opened for asynchronous I/O may
/// return before the kernel is done (`STATUS_PENDING`); then this waits
/// for the handle. A file handle is signalled when an I/O on it ends, so
/// the wait ends with this call's I/O only if no other I/O runs on the
/// handle at the same time, and only if the handle has `SYNCHRONIZE`
/// access (without it the wait fails at once). In every other case the
/// status block may still say pending after the wait, and then the
/// process aborts: the buffers the kernel would write are about to go
/// away. (The standard library does the same for its reads.) Every handle
/// irsync opens is synchronous, so the wait never happens for them.
///
/// # Safety
///
/// `iosb` points to the status block, initialised by
/// [`pending_status_block`], that the native call just made on `h` was
/// given, and that block and every buffer of that call stay alive until
/// this returns.
unsafe fn finish(
    h: BorrowedHandle<'_>,
    status: NTSTATUS,
    iosb: *mut IO_STATUS_BLOCK,
) -> (NTSTATUS, usize) {
    let mut status = status;
    if status == STATUS_PENDING {
        // SAFETY: WaitForSingleObject only waits on the handle, which the
        // borrow keeps open for the call; it touches no memory of ours.
        unsafe { WaitForSingleObject(h.as_raw_handle(), INFINITE) };
        // SAFETY: by the contract `iosb` points to a live, initialised
        // status block; `Status` is an integer, valid for any bits.
        // Volatile: the kernel writes it outside Rust's view.
        status = unsafe { ptr::read_volatile(ptr::addr_of!((*iosb).Anonymous.Status)) };
        if status == STATUS_PENDING {
            std::process::abort();
        }
    }
    // SAFETY: as above; the I/O has ended, and `Information` is a `usize`.
    let information = unsafe { ptr::read_volatile(ptr::addr_of!((*iosb).Information)) };
    (status, information)
}

/// `name` as a `UNICODE_STRING` that borrows it; refuses one that is
/// empty, longer than a `UNICODE_STRING` holds, or holds a `\` (the
/// separator of native paths), so that [`open_relative`] only ever opens
/// one component, whatever its caller checked.
fn component(name: &[u16]) -> io::Result<UNICODE_STRING> {
    if name.is_empty() || name.len() > MAX_NAME_UNITS || name.contains(&u16::from(b'\\')) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a single file name",
        ));
    }
    let bytes = (name.len() * 2) as u16;
    Ok(UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        // The kernel only reads the name (`ObjectName` is a const pointer).
        Buffer: name.as_ptr().cast_mut(),
    })
}

/// `NtCreateFile` of the one component `name` relative to the directory
/// `dir`, with `OBJ_DONT_REPARSE`: `access`, `share`, `disposition`,
/// `options` and `attributes` as `NtCreateFile` takes them. The caller
/// passes `FILE_OPEN_REPARSE_POINT` in `options` so a link at `name` is
/// opened itself.
pub(super) fn open_relative(
    dir: BorrowedHandle<'_>,
    name: &[u16],
    access: u32,
    share: u32,
    disposition: u32,
    options: u32,
    attributes: u32,
) -> io::Result<OwnedHandle> {
    let object_name = component(name)?;
    let object = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: dir.as_raw_handle(),
        ObjectName: &object_name,
        Attributes: OBJ_DONT_REPARSE,
        SecurityDescriptor: ptr::null(),
        SecurityQualityOfService: ptr::null(),
    };
    let mut handle: HANDLE = ptr::null_mut();
    let mut iosb = pending_status_block();
    // SAFETY: every pointer is to a local that outlives the call:
    // `handle` and `iosb` are written, `object` and the name it points to
    // (borrowed from `name`, its length in bytes checked by `component`)
    // are read. `dir` is a handle the borrow keeps open. No allocation
    // size and no extended attributes (null, 0). NtCreateFile completes
    // before it returns, also on an asynchronous handle.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            access,
            &object,
            &mut iosb,
            ptr::null(),
            attributes,
            share,
            disposition,
            options,
            ptr::null(),
            0,
        )
    };
    if status < 0 {
        return Err(status_error(status));
    }
    if handle.is_null() {
        return Err(io::Error::other("NtCreateFile succeeded without a handle"));
    }
    // SAFETY: on success NtCreateFile stored a new handle in `handle`,
    // owned by nothing else; it is closed once, by the OwnedHandle.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// `NtQueryInformationFile` of the class `T` names, on `h`.
pub(super) fn query<T: Info>(h: BorrowedHandle<'_>) -> io::Result<T> {
    let mut info = T::default();
    let mut iosb = pending_status_block();
    let iosb_ptr: *mut IO_STATUS_BLOCK = &mut iosb;
    // SAFETY: `info` is a local `T`, aligned for it and writable for
    // `size_of::<T>()` bytes, the length passed; the kernel writes at
    // most that many, as the class `T::CLASS`. `T` is one of the
    // plain-data structs sealed above, for which every bit pattern is a
    // valid value. `iosb` is a local; `finish` waits if the call has not
    // ended, so nothing is written after this function returns.
    let status = unsafe {
        NtQueryInformationFile(
            h.as_raw_handle(),
            iosb_ptr,
            (&mut info as *mut T).cast::<c_void>(),
            size_of::<T>() as u32,
            T::CLASS,
        )
    };
    // SAFETY: `iosb_ptr` is the status block, from `pending_status_block`,
    // that the call above was given; it and the call's buffers are
    // locals or borrows alive until this function returns.
    let (status, _) = unsafe { finish(h, status, iosb_ptr) };
    if status < 0 {
        return Err(status_error(status));
    }
    Ok(info)
}

/// `NtSetInformationFile` of `info`, as the class `T` names, on `h`.
pub(super) fn set<T: Info>(h: BorrowedHandle<'_>, info: &T) -> io::Result<()> {
    let mut iosb = pending_status_block();
    let iosb_ptr: *mut IO_STATUS_BLOCK = &mut iosb;
    // SAFETY: the kernel reads `size_of::<T>()` bytes of `info`, a
    // borrowed, aligned `T` of the class `T::CLASS`; it writes only
    // `iosb`, a local, and `finish` waits if the call has not ended.
    let status = unsafe {
        NtSetInformationFile(
            h.as_raw_handle(),
            iosb_ptr,
            (info as *const T).cast::<c_void>(),
            size_of::<T>() as u32,
            T::CLASS,
        )
    };
    // SAFETY: `iosb_ptr` is the status block, from `pending_status_block`,
    // that the call above was given; it and the call's buffers are
    // locals or borrows alive until this function returns.
    let (status, _) = unsafe { finish(h, status, iosb_ptr) };
    if status < 0 {
        return Err(status_error(status));
    }
    Ok(())
}

/// A `FILE_RENAME_INFORMATION` (or `FILE_LINK_INFORMATION`, the same
/// layout) for `name` in the directory `root`, with `flags` as its first
/// field, as 8-byte words: aligned for the struct, sized to hold the
/// struct and the name. Returns the words and the length in bytes.
fn name_info(flags: u32, root: BorrowedHandle<'_>, name: &[u16]) -> io::Result<(Vec<u64>, usize)> {
    component(name)?;
    let root_at = std::mem::offset_of!(FILE_RENAME_INFORMATION, RootDirectory);
    let length_at = std::mem::offset_of!(FILE_RENAME_INFORMATION, FileNameLength);
    let name_at = std::mem::offset_of!(FILE_RENAME_INFORMATION, FileName);
    let name_bytes = name.len() * 2;
    let len = (name_at + name_bytes).max(size_of::<FILE_RENAME_INFORMATION>());
    let mut bytes = vec![0u8; len.next_multiple_of(8)];
    bytes[..4].copy_from_slice(&flags.to_ne_bytes());
    let root = root.as_raw_handle() as usize;
    bytes[root_at..root_at + size_of::<usize>()].copy_from_slice(&root.to_ne_bytes());
    bytes[length_at..length_at + 4].copy_from_slice(&(name_bytes as u32).to_ne_bytes());
    for (i, u) in name.iter().enumerate() {
        bytes[name_at + 2 * i..name_at + 2 * i + 2].copy_from_slice(&u.to_ne_bytes());
    }
    // `bytes` is a whole number of words long.
    let words = bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| u64::from_ne_bytes(*c))
        .collect();
    Ok((words, len))
}

/// `NtSetInformationFile` with a name-carrying class: `FileRenameInformationEx`
/// or `FileLinkInformation`, the only two callers.
fn set_name_info(
    h: BorrowedHandle<'_>,
    class: FILE_INFORMATION_CLASS,
    flags: u32,
    root: BorrowedHandle<'_>,
    name: &[u16],
) -> io::Result<()> {
    let (words, len) = name_info(flags, root, name)?;
    let mut iosb = pending_status_block();
    let iosb_ptr: *mut IO_STATUS_BLOCK = &mut iosb;
    // SAFETY: `words` holds a FILE_RENAME_INFORMATION / FILE_LINK_INFORMATION
    // (same layout) and its name, 8-byte aligned, `len` bytes long, which
    // is at least the struct's size and covers the name; the kernel only
    // reads it. Its RootDirectory is `root`, which the borrow keeps open
    // for the call. `iosb` is a local and `finish` waits if the call has
    // not ended.
    let status = unsafe {
        NtSetInformationFile(
            h.as_raw_handle(),
            iosb_ptr,
            words.as_ptr().cast::<c_void>(),
            len as u32,
            class,
        )
    };
    // SAFETY: `iosb_ptr` is the status block, from `pending_status_block`,
    // that the call above was given; it and the call's buffers are
    // locals or borrows alive until this function returns.
    let (status, _) = unsafe { finish(h, status, iosb_ptr) };
    if status < 0 {
        return Err(status_error(status));
    }
    Ok(())
}

/// Renames the file `h` is open on to `name` in the directory `to`
/// (`FileRenameInformationEx`, with `flags`, the `FILE_RENAME_*` flags).
pub(super) fn rename_to(
    h: BorrowedHandle<'_>,
    to: BorrowedHandle<'_>,
    name: &[u16],
    flags: u32,
) -> io::Result<()> {
    set_name_info(h, FileRenameInformationEx, flags, to, name)
}

/// Makes `name` in the directory `to` a second name of the file `h` is
/// open on (`FileLinkInformation`); an existing `name` is not replaced.
pub(super) fn link_to(
    h: BorrowedHandle<'_>,
    to: BorrowedHandle<'_>,
    name: &[u16],
) -> io::Result<()> {
    // The first field is `ReplaceIfExists`, a BOOLEAN: 0, never replace.
    set_name_info(h, FileLinkInformation, 0, to, name)
}

/// How many bytes one [`list_some`] asks for.
const LIST_BUFFER_WORDS: usize = 64 * 1024 / 8;

/// One `NtQueryDirectoryFile` of `FileNamesInformation` entries on the
/// directory `h`, from the first entry when `restart`: the bytes the
/// kernel wrote, or `None` when no entries are left.
pub(super) fn list_some(h: BorrowedHandle<'_>, restart: bool) -> io::Result<Option<Vec<u8>>> {
    // u64 words: aligned for the entries, whose fields are at most 4 bytes.
    let mut buf = vec![0u64; LIST_BUFFER_WORDS];
    let mut iosb = pending_status_block();
    let iosb_ptr: *mut IO_STATUS_BLOCK = &mut iosb;
    // SAFETY: `buf` is writable, 8-byte aligned and `LIST_BUFFER_WORDS * 8`
    // bytes long, the length passed; the kernel writes at most that many.
    // No event, no APC, no file name filter (null). `iosb` and `buf` are
    // locals and `finish` waits if the call has not ended.
    let status = unsafe {
        NtQueryDirectoryFile(
            h.as_raw_handle(),
            ptr::null_mut(),
            None,
            ptr::null(),
            iosb_ptr,
            buf.as_mut_ptr().cast::<c_void>(),
            (LIST_BUFFER_WORDS * 8) as u32,
            FileNamesInformation,
            false,
            ptr::null(),
            restart,
        )
    };
    // SAFETY: `iosb_ptr` is the status block, from `pending_status_block`,
    // that the call above was given; it and `buf` are locals alive
    // until this function returns.
    let (status, written) = unsafe { finish(h, status, iosb_ptr) };
    if status == STATUS_NO_MORE_FILES || status == STATUS_NO_SUCH_FILE {
        return Ok(None);
    }
    if status < 0 {
        return Err(status_error(status));
    }
    Ok(Some(bytes_of(&buf, written)))
}

/// The first `n` bytes of `words` (all of them if fewer).
fn bytes_of(words: &[u64], n: usize) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_ne_bytes()).take(n).collect()
}

/// `FSCTL_GET_REPARSE_POINT` on `h`: the raw reparse buffer.
pub(super) fn get_reparse(h: BorrowedHandle<'_>) -> io::Result<Vec<u8>> {
    const WORDS: usize = MAXIMUM_REPARSE_DATA_BUFFER_SIZE as usize / 8;
    let mut buf = vec![0u64; WORDS];
    let mut iosb = pending_status_block();
    let iosb_ptr: *mut IO_STATUS_BLOCK = &mut iosb;
    // SAFETY: no input buffer (null, 0); the output buffer `buf` is
    // writable, 8-byte aligned and `WORDS * 8` bytes long, the length
    // passed; the kernel writes at most that many. No event, no APC.
    // `iosb` and `buf` are locals and `finish` waits if the call has not
    // ended.
    let status = unsafe {
        NtFsControlFile(
            h.as_raw_handle(),
            ptr::null_mut(),
            None,
            ptr::null(),
            iosb_ptr,
            FSCTL_GET_REPARSE_POINT,
            ptr::null(),
            0,
            buf.as_mut_ptr().cast::<c_void>(),
            (WORDS * 8) as u32,
        )
    };
    // SAFETY: `iosb_ptr` is the status block, from `pending_status_block`,
    // that the call above was given; it and `buf` are locals alive
    // until this function returns.
    let (status, written) = unsafe { finish(h, status, iosb_ptr) };
    if status < 0 {
        return Err(status_error(status));
    }
    Ok(bytes_of(&buf, written))
}

/// `FSCTL_SET_REPARSE_POINT` on `h` with the reparse buffer `data`.
pub(super) fn set_reparse(h: BorrowedHandle<'_>, data: &[u8]) -> io::Result<()> {
    if data.len() > MAXIMUM_REPARSE_DATA_BUFFER_SIZE as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "reparse data too long",
        ));
    }
    let mut iosb = pending_status_block();
    let iosb_ptr: *mut IO_STATUS_BLOCK = &mut iosb;
    // SAFETY: the kernel reads `data.len()` bytes of `data`, borrowed for
    // the call (the code is METHOD_BUFFERED: it copies them, so their
    // alignment does not matter); no output buffer (null, 0). No event,
    // no APC. `iosb` is a local and `finish` waits if the call has not
    // ended.
    let status = unsafe {
        NtFsControlFile(
            h.as_raw_handle(),
            ptr::null_mut(),
            None,
            ptr::null(),
            iosb_ptr,
            FSCTL_SET_REPARSE_POINT,
            data.as_ptr().cast::<c_void>(),
            data.len() as u32,
            ptr::null_mut(),
            0,
        )
    };
    // SAFETY: `iosb_ptr` is the status block, from `pending_status_block`,
    // that the call above was given; it and the call's buffers are
    // locals or borrows alive until this function returns.
    let (status, _) = unsafe { finish(h, status, iosb_ptr) };
    if status < 0 {
        return Err(status_error(status));
    }
    Ok(())
}

/// An `OVERLAPPED` that names the byte range starting at `offset`, with no
/// event: what `LockFileEx` and `UnlockFileEx` take the offset in.
fn at_offset(offset: u64) -> OVERLAPPED {
    OVERLAPPED {
        Anonymous: OVERLAPPED_0 {
            Anonymous: OVERLAPPED_0_0 {
                Offset: offset as u32,
                OffsetHigh: (offset >> 32) as u32,
            },
        },
        ..Default::default()
    }
}

/// Waits for an overlapped call on `h` that answered `ERROR_IO_PENDING`
/// (only an asynchronous handle does; irsync opens none), so `ov` is no
/// longer in use when the caller returns. Returns the call's result.
fn wait_overlapped(h: BorrowedHandle<'_>, ov: &mut OVERLAPPED) -> io::Result<()> {
    let mut transferred = 0u32;
    // SAFETY: `ov` is the OVERLAPPED the pending call was given, borrowed
    // mutably and alive until this returns; `transferred` is a local. With
    // `bWait` TRUE the call returns only once the operation has ended, so
    // the kernel no longer writes `ov` afterwards.
    let ok = unsafe { GetOverlappedResult(h.as_raw_handle(), ov, &mut transferred, 1) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `LockFileEx` of the `len` bytes at `offset` of `h`: exclusive, and
/// failing at once (`ERROR_LOCK_VIOLATION`) when another handle holds an
/// overlapping range, instead of waiting.
pub(super) fn try_lock_range(h: BorrowedHandle<'_>, offset: u64, len: u64) -> io::Result<()> {
    let mut ov = at_offset(offset);
    // SAFETY: `h` is a handle the borrow keeps open; `ov` is a local that
    // outlives the call, and if the call answers ERROR_IO_PENDING (an
    // asynchronous handle) `wait_overlapped` waits for it to end before
    // `ov` goes out of scope. The reserved argument is 0, as required.
    let ok = unsafe {
        LockFileEx(
            h.as_raw_handle(),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            len as u32,
            (len >> 32) as u32,
            &mut ov,
        )
    };
    if ok == 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() == Some(ERROR_IO_PENDING as i32) {
            return wait_overlapped(h, &mut ov);
        }
        return Err(e);
    }
    Ok(())
}

/// `UnlockFileEx` of the `len` bytes at `offset` of `h`, a range
/// [`try_lock_range`] locked through this handle.
pub(super) fn unlock_range(h: BorrowedHandle<'_>, offset: u64, len: u64) -> io::Result<()> {
    let mut ov = at_offset(offset);
    // SAFETY: as in `try_lock_range`: `h` is open for the borrow, `ov` is
    // a local that outlives the call (or the wait for it), the reserved
    // argument is 0.
    let ok = unsafe {
        UnlockFileEx(
            h.as_raw_handle(),
            0,
            len as u32,
            (len >> 32) as u32,
            &mut ov,
        )
    };
    if ok == 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() == Some(ERROR_IO_PENDING as i32) {
            return wait_overlapped(h, &mut ov);
        }
        return Err(e);
    }
    Ok(())
}

/// `ReOpenFile`: a new handle to the very file `h` has open, with `access`,
/// `share` and `flags`. No name is looked up, so nothing can have been
/// put in the file's place.
pub(super) fn reopen(
    h: BorrowedHandle<'_>,
    access: u32,
    share: u32,
    flags: u32,
) -> io::Result<OwnedHandle> {
    // SAFETY: `h` is a handle the borrow keeps open; the other arguments
    // are plain integers. ReOpenFile returns a new handle or
    // INVALID_HANDLE_VALUE.
    let handle = unsafe { ReOpenFile(h.as_raw_handle(), access, share, flags) };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: ReOpenFile succeeded, so `handle` is a new handle owned by
    // nothing else; it is closed once, by the OwnedHandle.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

#[cfg(test)]
mod tests {
    use std::mem::{offset_of, size_of};

    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_LINK_INFORMATION, FILE_RENAME_INFORMATION, FILE_STANDARD_INFORMATION,
    };

    use super::*;

    #[test]
    fn standard_info_has_the_layout_of_the_sdk_struct() {
        assert_eq!(
            size_of::<StandardInfo>(),
            size_of::<FILE_STANDARD_INFORMATION>()
        );
        assert_eq!(
            offset_of!(StandardInfo, end_of_file),
            offset_of!(FILE_STANDARD_INFORMATION, EndOfFile)
        );
        assert_eq!(
            offset_of!(StandardInfo, number_of_links),
            offset_of!(FILE_STANDARD_INFORMATION, NumberOfLinks)
        );
        assert_eq!(
            offset_of!(StandardInfo, delete_pending),
            offset_of!(FILE_STANDARD_INFORMATION, DeletePending)
        );
        assert_eq!(
            offset_of!(StandardInfo, directory),
            offset_of!(FILE_STANDARD_INFORMATION, Directory)
        );
    }

    #[test]
    fn rename_and_link_information_have_one_layout() {
        assert_eq!(
            size_of::<FILE_RENAME_INFORMATION>(),
            size_of::<FILE_LINK_INFORMATION>()
        );
        assert_eq!(
            offset_of!(FILE_RENAME_INFORMATION, RootDirectory),
            offset_of!(FILE_LINK_INFORMATION, RootDirectory)
        );
        assert_eq!(
            offset_of!(FILE_RENAME_INFORMATION, FileNameLength),
            offset_of!(FILE_LINK_INFORMATION, FileNameLength)
        );
        assert_eq!(
            offset_of!(FILE_RENAME_INFORMATION, FileName),
            offset_of!(FILE_LINK_INFORMATION, FileName)
        );
        // 64-bit: flags, padding, the handle at 8, the length at 16, the
        // name at 20; 24 bytes in all.
        assert_eq!(offset_of!(FILE_RENAME_INFORMATION, RootDirectory), 8);
        assert_eq!(offset_of!(FILE_RENAME_INFORMATION, FileName), 20);
        assert_eq!(size_of::<FILE_RENAME_INFORMATION>(), 24);
    }

    #[test]
    fn a_name_info_buffer_holds_flags_root_length_and_name() {
        use std::os::windows::io::AsHandle;
        // Any open handle will do; the buffer only records its value.
        let h = std::fs::File::open(std::env::current_exe().unwrap()).unwrap();
        let name: Vec<u16> = "ab".encode_utf16().collect();
        let (words, len) = name_info(0x43, h.as_handle(), &name).unwrap();
        assert_eq!(len, 24);
        let bytes = bytes_of(&words, usize::MAX);
        assert_eq!(&bytes[0..4], &0x43u32.to_ne_bytes());
        assert_eq!(&bytes[8..16], &(h.as_raw_handle() as usize).to_ne_bytes());
        assert_eq!(&bytes[16..20], &4u32.to_ne_bytes());
        assert_eq!(&bytes[20..24], &[b'a', 0, b'b', 0]);
        // A longer name makes the buffer longer than the struct.
        let long = vec![u16::from(b'x'); 10];
        assert_eq!(name_info(0, h.as_handle(), &long).unwrap().1, 40);
    }

    #[test]
    fn a_component_is_one_name_a_unicode_string_holds() {
        let ok: Vec<u16> = "a".encode_utf16().collect();
        assert_eq!(component(&ok).unwrap().Length, 2);
        let longest = vec![u16::from(b'x'); MAX_NAME_UNITS];
        assert_eq!(component(&longest).unwrap().Length, 65534);
        for bad in [
            vec![],
            vec![u16::from(b'x'); MAX_NAME_UNITS + 1],
            "a\\b".encode_utf16().collect(),
        ] {
            assert_eq!(
                component(&bad).err().map(|e| e.kind()),
                Some(io::ErrorKind::InvalidInput)
            );
        }
    }

    #[test]
    fn statuses_become_win32_codes() {
        use windows_sys::Win32::Foundation::*;
        let cases = [
            (STATUS_DELETE_PENDING, ERROR_DELETE_PENDING),
            (STATUS_FILE_IS_A_DIRECTORY, ERROR_DIRECTORY_NOT_SUPPORTED),
            (STATUS_OBJECT_NAME_NOT_FOUND, ERROR_FILE_NOT_FOUND),
            (STATUS_OBJECT_PATH_NOT_FOUND, ERROR_PATH_NOT_FOUND),
            (STATUS_OBJECT_NAME_COLLISION, ERROR_ALREADY_EXISTS),
            (STATUS_SHARING_VIOLATION, ERROR_SHARING_VIOLATION),
            (STATUS_DIRECTORY_NOT_EMPTY, ERROR_DIR_NOT_EMPTY),
            (STATUS_NOT_A_DIRECTORY, ERROR_DIRECTORY),
            (STATUS_NOT_SAME_DEVICE, ERROR_NOT_SAME_DEVICE),
            (STATUS_STOPPED_ON_SYMLINK, ERROR_STOPPED_ON_SYMLINK),
            (STATUS_PRIVILEGE_NOT_HELD, ERROR_PRIVILEGE_NOT_HELD),
        ];
        for (status, code) in cases {
            assert_eq!(
                status_error(status).raw_os_error(),
                Some(code as i32),
                "{status:#x}"
            );
        }
    }
}
