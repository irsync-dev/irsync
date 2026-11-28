//! Tests of the safe parts of the Windows module: what errors mean, names,
//! times, and the bytes of reparse buffers and directory listings.

use super::*;

#[test]
fn causes_from_win32_codes() {
    use windows_sys::Win32::Foundation::{
        ERROR_DISK_FULL, ERROR_INVALID_NAME, ERROR_PRIVILEGE_NOT_HELD,
    };
    let cases = [
        (ERROR_STOPPED_ON_SYMLINK, Cause::LinkRefused),
        (ERROR_CANT_RESOLVE_FILENAME, Cause::LinkRefused),
        (ERROR_REPARSE_POINT_ENCOUNTERED, Cause::LinkRefused),
        (ERROR_FILE_NOT_FOUND, Cause::NotThere),
        (ERROR_PATH_NOT_FOUND, Cause::NotThere),
        (ERROR_ALREADY_EXISTS, Cause::AlreadyThere),
        (ERROR_FILE_EXISTS, Cause::AlreadyThere),
        (ERROR_DIRECTORY, Cause::NotADirectory),
        (ERROR_DIRECTORY_NOT_SUPPORTED, Cause::IsADirectory),
        (ERROR_DIR_NOT_EMPTY, Cause::NotEmpty),
        (ERROR_NOT_SAME_DEVICE, Cause::CrossDevice),
        (ERROR_TOO_MANY_OPEN_FILES, Cause::OutOfHandles),
        (ERROR_NO_SYSTEM_RESOURCES, Cause::OutOfHandles),
        (ERROR_SHARING_VIOLATION, Cause::InUse),
        (ERROR_LOCK_VIOLATION, Cause::InUse),
        (ERROR_DELETE_PENDING, Cause::InUse),
        (ERROR_USER_MAPPED_FILE, Cause::InUse),
        (ERROR_NOT_SUPPORTED, Cause::NotSupported),
        (ERROR_INVALID_FUNCTION, Cause::NotSupported),
        // Ambiguous on Windows: never read as "in use".
        (ERROR_ACCESS_DENIED, Cause::Other),
        // A name NTFS will not hold is not "not there".
        (ERROR_INVALID_NAME, Cause::Other),
        (ERROR_PRIVILEGE_NOT_HELD, Cause::Other),
        (ERROR_NOT_A_REPARSE_POINT, Cause::Other),
        (ERROR_DISK_FULL, Cause::Other),
    ];
    for (code, want) in cases {
        assert_eq!(
            cause(&io::Error::from_raw_os_error(code as i32)),
            want,
            "{code}"
        );
    }
    assert_eq!(cause(&io::Error::other("x")), Cause::Other);
    assert_eq!(
        cause(&io::Error::new(io::ErrorKind::Unsupported, "x")),
        Cause::NotSupported
    );
}

#[test]
fn errors_this_module_raises_keep_their_meaning() {
    // Each is an io::Error that `cause` reads back.
    for (e, want) in [
        (link_refused(), Cause::LinkRefused),
        (not_a_directory(), Cause::NotADirectory),
        (is_a_directory(), Cause::IsADirectory),
        (unsupported("x"), Cause::NotSupported),
    ] {
        assert_eq!(cause(&e), want, "{e}");
        // Carried inside another error, the code is still there.
        let wrapped = io::Error::new(e.kind(), e);
        let inner = wrapped
            .get_ref()
            .unwrap()
            .downcast_ref::<io::Error>()
            .unwrap();
        assert_eq!(cause(inner), want);
    }
    // A native status reaches `cause` as a Win32 code.
    use windows_sys::Win32::Foundation::{STATUS_DELETE_PENDING, STATUS_FILE_IS_A_DIRECTORY};
    assert_eq!(
        cause(&native::status_error(STATUS_DELETE_PENDING)),
        Cause::InUse
    );
    assert_eq!(
        cause(&native::status_error(STATUS_FILE_IS_A_DIRECTORY)),
        Cause::IsADirectory
    );
}

#[test]
fn names_and_bytes_round_trip() {
    for units in [
        &[u16::from(b'a')][..],
        &[0xE9],
        &[0xD83D, 0xDE00],
        &[0xD800],
        &[],
    ] {
        let os = OsString::from_wide(units);
        let bytes = os_to_bytes(&os);
        assert_eq!(bytes, wtf8::encode(units));
        assert_eq!(bytes_to_os(&bytes).unwrap(), os);
    }
    // Not WTF-8: refused, never repaired.
    let e = bytes_to_os(b"caf\xe9").unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn one_name_refuses_all_but_one_component() {
    for bad in [
        &b""[..],
        b".",
        b"..",
        b"a/b",
        b"a\\b",
        b"\\",
        b"a:b",
        b"f::$DATA",
        b":",
        b"a\0",
        b"\xff",
        b"\xed\xa0\xbd\xed\xb8\x80", // a pair as two three-byte forms
    ] {
        let e = one_name(bad).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
    }
    assert_eq!(one_name(b"...").unwrap(), units("..."));
    assert_eq!(one_name(b"\xed\xa0\x80").unwrap(), [0xD800]);
    let longest = vec![b'x'; native::MAX_NAME_UNITS];
    assert_eq!(one_name(&longest).unwrap().len(), native::MAX_NAME_UNITS);
    let too_long = vec![b'x'; native::MAX_NAME_UNITS + 1];
    assert_eq!(
        one_name(&too_long).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}

#[test]
fn file_times_and_unix_times() {
    // The epoch, a time with 100 ns steps, and times before 1970.
    assert_eq!(unix_time(116_444_736_000_000_000), (0, 0));
    assert_eq!(
        unix_time(116_444_736_000_000_000 + 17_000_000_001_234_567),
        (1_700_000_000, 123_456_700)
    );
    assert_eq!(unix_time(116_444_736_000_000_000 - 1), (-1, 999_999_900));
    assert_eq!(unix_time(1), (-11_644_473_600, 100));
    assert_eq!(unix_time(i64::MAX).1 % 100, 0);
    for t in [
        (0, 0),
        (1_700_000_000, 123_456_700),
        (-1, 999_999_900),
        (-11_644_473_600, 100),
    ] {
        assert_eq!(unix_time(file_time(t).unwrap()), t, "{t:?}");
    }
    // Cut to 100 ns.
    assert_eq!(unix_time(file_time((5, 199)).unwrap()), (5, 100));
    // 1601 itself (0, "leave it"), before 1601, past what fits, and bad
    // nanoseconds.
    for bad in [
        (-11_644_473_600, 0),
        (-11_644_473_601, 0),
        (i64::MAX, 0),
        (0, 1_000_000_000),
    ] {
        assert_eq!(
            file_time(bad).unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
            "{bad:?}"
        );
    }
}

#[test]
fn kinds_from_attributes_and_tags() {
    const IO_REPARSE_TAG_CLOUD: u32 = 0x9000_001A;
    let reparse = FILE_ATTRIBUTE_REPARSE_POINT;
    let dir = FILE_ATTRIBUTE_DIRECTORY;
    assert_eq!(kind_of(0, 0), Kind::File);
    assert_eq!(kind_of(dir, 0), Kind::Dir);
    assert_eq!(kind_of(reparse, IO_REPARSE_TAG_SYMLINK), Kind::Symlink);
    assert_eq!(
        kind_of(reparse | dir, IO_REPARSE_TAG_SYMLINK),
        Kind::Symlink
    );
    assert_eq!(
        kind_of(reparse | dir, IO_REPARSE_TAG_MOUNT_POINT),
        Kind::Other
    );
    assert_eq!(kind_of(reparse, IO_REPARSE_TAG_CLOUD), Kind::Other);
}

/// UTF-16LE bytes of `s`.
fn le(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

#[test]
fn a_relative_symlink_buffer_byte_for_byte() {
    let b = symlink_reparse(&units("a\\b")).unwrap();
    let mut want = Vec::new();
    want.extend_from_slice(&0xA000_000Cu32.to_le_bytes()); // IO_REPARSE_TAG_SYMLINK
    want.extend_from_slice(&(12u16 + 8 + 8).to_le_bytes()); // data length
    want.extend_from_slice(&[0, 0]); // reserved
    want.extend_from_slice(&0u16.to_le_bytes()); // substitute at 0
    want.extend_from_slice(&6u16.to_le_bytes()); // 3 units
    want.extend_from_slice(&8u16.to_le_bytes()); // print after it and a NUL
    want.extend_from_slice(&6u16.to_le_bytes());
    want.extend_from_slice(&1u32.to_le_bytes()); // SYMLINK_FLAG_RELATIVE
    want.extend(le("a\\b\0a\\b\0"));
    assert_eq!(b, want);
    assert_eq!(parse_reparse(&b).unwrap(), units("a\\b"));
}

#[test]
fn absolute_symlink_targets_get_the_native_form() {
    for (target, substitute) in [
        ("C:\\x\\y", "\\??\\C:\\x\\y"),
        ("c:/x/y", "\\??\\c:\\x\\y"),
        ("\\\\?\\C:\\x", "\\??\\C:\\x"),
        ("\\\\.\\C:\\x", "\\??\\C:\\x"),
        ("\\??\\C:\\x", "\\??\\C:\\x"),
        ("\\\\server\\share\\x", "\\??\\UNC\\server\\share\\x"),
        ("\\\\?\\UNC\\server\\share", "\\??\\UNC\\server\\share"),
    ] {
        let b = symlink_reparse(&units(target)).unwrap();
        let (s, p) = (substitute.len() * 2, target.len() * 2);
        let mut want = Vec::new();
        want.extend_from_slice(&0xA000_000Cu32.to_le_bytes());
        want.extend_from_slice(&((12 + s + 2 + p + 2) as u16).to_le_bytes());
        want.extend_from_slice(&[0, 0]);
        want.extend_from_slice(&0u16.to_le_bytes());
        want.extend_from_slice(&(s as u16).to_le_bytes());
        want.extend_from_slice(&((s + 2) as u16).to_le_bytes());
        want.extend_from_slice(&(p as u16).to_le_bytes());
        want.extend_from_slice(&0u32.to_le_bytes()); // absolute
        want.extend(le(substitute));
        want.extend_from_slice(&[0, 0]);
        want.extend(le(target));
        want.extend_from_slice(&[0, 0]);
        assert_eq!(b, want, "{target}");
        assert_eq!(parse_reparse(&b).unwrap(), units(target), "{target}");
    }
    // Rooted without a drive, or drive-relative: stored as relative.
    for relative in ["\\x", "C:x", "x/y"] {
        let b = symlink_reparse(&units(relative)).unwrap();
        assert_eq!(u32_at(&b, 16), Some(SYMLINK_FLAG_RELATIVE), "{relative}");
        assert_eq!(parse_reparse(&b).unwrap(), units(relative), "{relative}");
    }
    // A relative target with `/`: followed with `\`, read back as given.
    let b = symlink_reparse(&units("../x/y")).unwrap();
    assert_eq!(&b[20..32], &le("..\\x\\y")[..]);
    assert_eq!(&b[34..46], &le("../x/y")[..]);
}

#[test]
fn a_junction_buffer_byte_for_byte() {
    let b = junction_reparse(&units("C:\\t")).unwrap();
    let mut want = Vec::new();
    want.extend_from_slice(&0xA000_0003u32.to_le_bytes()); // IO_REPARSE_TAG_MOUNT_POINT
    want.extend_from_slice(&(8u16 + 18 + 10).to_le_bytes());
    want.extend_from_slice(&[0, 0]);
    want.extend_from_slice(&0u16.to_le_bytes());
    want.extend_from_slice(&16u16.to_le_bytes()); // 8 units: \??\C:\t
    want.extend_from_slice(&18u16.to_le_bytes());
    want.extend_from_slice(&8u16.to_le_bytes()); // C:\t
    want.extend(le("\\??\\C:\\t\0C:\\t\0"));
    assert_eq!(b, want);
    assert_eq!(parse_reparse(&b).unwrap(), units("C:\\t"));
    assert_eq!(
        junction_reparse(&units("rel")).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}

/// A symlink reparse buffer with the names given as they are, for parser
/// tests: `(offset, length)` of each, then the path bytes.
fn raw_symlink(sub: (u16, u16), print: (u16, u16), path: &[u8], data_len: u16) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&IO_REPARSE_TAG_SYMLINK.to_le_bytes());
    b.extend_from_slice(&data_len.to_le_bytes());
    b.extend_from_slice(&[0, 0]);
    for v in [sub.0, sub.1, print.0, print.1] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(path);
    b
}

#[test]
fn the_reparse_parser_checks_every_length() {
    let good = symlink_reparse(&units("target")).unwrap();
    // Every truncation of a good buffer fails as malformed, never panics.
    for n in 0..good.len() {
        let e = parse_reparse(&good[..n]).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData, "cut to {n}");
    }
    // More bytes after the buffer than it says it has: ignored.
    let mut longer = good.clone();
    longer.extend_from_slice(&[0xFF; 64]);
    assert_eq!(parse_reparse(&longer).unwrap(), units("target"));
    let path = le("ab");
    let data_len = 12 + path.len() as u16;
    for (sub, print, len) in [
        ((0, 6), (0, 0), data_len),      // substitute past the end
        ((0, 4), (2, 4), data_len),      // print past the end
        ((0, 3), (0, 0), data_len),      // odd length
        ((0xFFFF, 2), (0, 0), data_len), // offset far out
        ((0, 4), (0, 0), data_len + 1),  // data longer than the buffer
        ((0, 4), (0, 0), 8),             // data too short for its fields
    ] {
        let b = raw_symlink(sub, print, &path, len);
        assert_eq!(
            parse_reparse(&b).unwrap_err().kind(),
            io::ErrorKind::InvalidData,
            "{sub:?} {print:?} {len}"
        );
    }
    // Zero-length names: no print name, so the substitute, without the
    // native prefix; an empty substitute gives an empty target.
    let sub = le("\\??\\C:\\x");
    let b = raw_symlink((0, sub.len() as u16), (0, 0), &sub, 12 + sub.len() as u16);
    assert_eq!(parse_reparse(&b).unwrap(), units("C:\\x"));
    let sub = le("\\??\\UNC\\s\\x");
    let b = raw_symlink((0, sub.len() as u16), (0, 0), &sub, 12 + sub.len() as u16);
    assert_eq!(parse_reparse(&b).unwrap(), units("\\\\s\\x"));
    assert_eq!(
        parse_reparse(&raw_symlink((0, 0), (0, 0), &[], 12)).unwrap(),
        units("")
    );
    // Another tag is not a link.
    let mut other = good.clone();
    other[..4].copy_from_slice(&0x9000_001Au32.to_le_bytes());
    assert_eq!(cause(&parse_reparse(&other).unwrap_err()), Cause::Other);
    assert_eq!(
        parse_reparse(&other).unwrap_err().raw_os_error(),
        Some(ERROR_NOT_A_REPARSE_POINT as i32)
    );
}

#[test]
fn a_long_target_fits_and_a_longer_one_is_refused() {
    // Both names and two NULs in 16 KiB: up to 4090 units.
    let fits = vec![u16::from(b'x'); 4090];
    assert_eq!(
        parse_reparse(&symlink_reparse(&fits).unwrap()).unwrap(),
        fits
    );
    let too_long = vec![u16::from(b'x'); 4091];
    assert_eq!(
        symlink_reparse(&too_long).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}

/// A `FILE_NAMES_INFORMATION` entry: `next`, a file index, the name.
fn entry(next: u32, name: &str) -> Vec<u8> {
    let n = le(name);
    let mut b = Vec::new();
    b.extend_from_slice(&next.to_le_bytes());
    b.extend_from_slice(&7u32.to_le_bytes());
    b.extend_from_slice(&(n.len() as u32).to_le_bytes());
    b.extend(n);
    b
}

#[test]
fn the_listing_parser_checks_every_length() {
    // Two entries, the first padded to 8 bytes.
    let mut first = entry(16, "ab");
    first.resize(16, 0xEE);
    let good = [first, entry(0, "c")].concat();
    assert_eq!(parse_names(&good).unwrap(), [units("ab"), units("c")]);
    // Nothing: no names.
    assert_eq!(parse_names(&[]).unwrap(), Vec::<Vec<u16>>::new());
    // Every truncation fails as malformed, never panics (cut after the
    // first entry, its `next` points past the end).
    for n in 1..good.len() {
        let e = parse_names(&good[..n]).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData, "cut to {n}");
    }
    // Bytes after the last entry are not read.
    let mut longer = good.clone();
    longer.extend_from_slice(&[0xFF; 20]);
    assert_eq!(parse_names(&longer).unwrap().len(), 2);
    for bad in [
        entry(4, "ab"),                                   // next inside its own header
        entry(0x1000, "ab"),                              // next past the end
        [entry(0, "a"), vec![0]].concat()[..13].to_vec(), // name cut
        {
            let mut b = entry(0, "ab");
            b[8] = 3; // odd name length
            b
        },
        {
            let mut b = entry(0, "ab");
            b[8..12].copy_from_slice(&u32::MAX.to_le_bytes()); // huge name
            b
        },
    ] {
        assert_eq!(
            parse_names(&bad).unwrap_err().kind(),
            io::ErrorKind::InvalidData,
            "{bad:?}"
        );
    }
}

#[test]
fn a_path_is_opened_in_its_verbatim_form() {
    let n = |p: &str| {
        native_path(Path::new(p))
            .unwrap()
            .to_string_lossy()
            .into_owned()
    };
    assert_eq!(n(r"C:\x\ro."), r"\\?\C:\x\ro.");
    assert_eq!(n(r"c:/x/ro /y"), r"\\?\C:\x\ro \y");
    assert_eq!(n(r"C:\x\NUL"), r"\\?\C:\x\NUL");
    assert_eq!(n(r"C:\x\.\y\..\z"), r"\\?\C:\x\z");
    assert_eq!(n(r"C:\.."), r"\\?\C:\");
    assert_eq!(n(r"C:\"), r"\\?\C:\");
    assert_eq!(n(r"\\server\share\x."), r"\\?\UNC\server\share\x.");
    // Already verbatim, or a device: as given.
    assert_eq!(n(r"\\?\C:\x."), r"\\?\C:\x.");
    assert_eq!(n(r"\\?\UNC\s\h\x"), r"\\?\UNC\s\h\x");
    assert_eq!(n(r"\\.\pipe\x"), r"\\.\pipe\x");
    // Relative, rooted on the current drive, and drive-relative.
    let cwd = native_path(&std::env::current_dir().unwrap())
        .unwrap()
        .into_owned();
    assert_eq!(native_path(Path::new("a/b.")).unwrap(), cwd.join(r"a\b."));
    assert_eq!(
        n(r"\x."),
        format!(r"{}x.", &cwd.to_string_lossy()[..7]),
        "the current drive's root"
    );
    let drive = &std::env::current_dir().unwrap().to_string_lossy()[..2].to_string();
    assert_eq!(
        native_path(Path::new(&format!("{drive}y."))).unwrap(),
        cwd.join("y.")
    );
    // An empty path is no path.
    assert_eq!(
        native_path(Path::new("")).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    // A verbatim current directory is taken as it is (here as the
    // directory std gives, the way `base_parts` reads it).
    let (root, names) = base_parts(Path::new(r"\\?\C:\x.\y ")).unwrap();
    assert_eq!(root, OsString::from(r"\\?\C:\"));
    assert_eq!(names, [OsString::from("x."), OsString::from("y ")]);
    let (root, names) = base_parts(Path::new(r"\\?\UNC\s\h\d")).unwrap();
    assert_eq!(root, OsString::from(r"\\?\UNC\s\h\"));
    assert_eq!(names, [OsString::from("d")]);
}

/// Win32 would open `x` for `x.` and `x `; the verbatim form opens the
/// name as written.
#[test]
fn a_root_with_a_trailing_dot_or_space_is_that_directory() {
    let t = tempfile::tempdir().unwrap();
    let base = native_path(t.path()).unwrap().into_owned();
    std::fs::create_dir(base.join("x")).unwrap();
    for name in ["x.", "x "] {
        std::fs::create_dir(base.join(name)).unwrap();
        // Given in the ordinary form, as a caller holds it.
        let d = Dir::open_root(&t.path().join(name)).unwrap();
        let (want, other) = (
            lstat(&base.join(name)).unwrap(),
            lstat(&base.join("x")).unwrap(),
        );
        let got = d.stat_self().unwrap();
        assert_eq!(got.ino, want.ino, "{name:?}");
        assert_ne!(got.ino, other.ino, "{name:?}");
        assert_eq!(lstat(&t.path().join(name)).unwrap().ino, want.ino);
        assert!(Dir::open_root_nofollow(&t.path().join(name)).is_ok());
    }
}

/// A root named like a device is an ordinary directory, and a root path
/// longer than 260 characters opens.
#[test]
fn device_names_and_long_paths_are_ordinary_paths() {
    let t = tempfile::tempdir().unwrap();
    let base = native_path(t.path()).unwrap().into_owned();
    for name in ["NUL", "CON", "aux"] {
        std::fs::create_dir(base.join(name)).unwrap();
        let d = Dir::open_root(&t.path().join(name)).unwrap();
        d.create_dir(b"in", 0).unwrap();
        assert!(base.join(name).join("in").is_dir(), "{name}");
        assert_eq!(lstat(&t.path().join(name)).unwrap().kind, Kind::Dir);
    }
    let mut long = t.path().to_path_buf();
    while long.as_os_str().len() < 300 {
        long.push("a-long-directory-name-0123456789");
    }
    create_private_dir_all(&long).unwrap();
    let d = Dir::open_root(&long).unwrap();
    d.create_dir(b"deep", 0).unwrap();
    write_private(&long.join("f"), b"x").unwrap();
    let f = open_read_nofollow(&long.join("f")).unwrap();
    assert_eq!(fstat(&f).unwrap().size, 1);
    set_mtime_path(&long.join("f"), (1_600_000_000, 0)).unwrap();
    assert_eq!(lstat(&long.join("f")).unwrap().mtime, (1_600_000_000, 0));
}

/// Only NTFS on a drive of this machine takes a tree; the decision, on
/// volume information given as data.
#[test]
fn a_root_that_is_not_ntfs_is_refused() {
    use super::{local_ntfs, DriveKind::*};
    for drive in [Fixed, Removable, RamDisk] {
        assert!(local_ntfs("NTFS", drive), "{drive:?}");
        assert!(local_ntfs("ntfs", drive), "{drive:?}");
    }
    // A share is refused whatever it says it runs (a mapped drive
    // resolves to one too); so is a drive the system cannot name.
    for drive in [Remote, CdRom, Unknown] {
        assert!(!local_ntfs("NTFS", drive), "{drive:?}");
    }
    for fs in ["ReFS", "FAT", "FAT32", "exFAT", "UDF", "CDFS", ""] {
        for drive in [Fixed, Removable, Remote] {
            assert!(!local_ntfs(fs, drive), "{fs} on {drive:?}");
        }
    }
}

/// The temporary directory is on a local NTFS volume on this machine and
/// on CI; a missing root is judged where it would be made; the error
/// names the root.
#[test]
fn check_root_reads_the_volume_of_a_real_directory() {
    let t = tempfile::tempdir().unwrap();
    super::check_root(t.path()).unwrap();
    super::check_root(&t.path().join("not").join("yet")).unwrap();
}

#[test]
fn the_volume_top_keeps_two_names_for_the_system() {
    use super::{is_builtin_exclude, is_volume_top};
    for n in [
        &b"System Volume Information"[..],
        b"system volume information",
        b"$RECYCLE.BIN",
        b"$Recycle.Bin",
    ] {
        assert!(is_builtin_exclude(n));
    }
    for n in [
        &b"System Volume"[..],
        b"RECYCLE.BIN",
        b"$RECYCLE.BIN2",
        b"x",
    ] {
        assert!(!is_builtin_exclude(n));
    }
    assert!(is_volume_top(Path::new(r"C:\")));
    assert!(is_volume_top(Path::new(r"\\?\C:\")));
    assert!(!is_volume_top(Path::new(r"C:\Users")));
    assert!(!is_volume_top(Path::new("relative")));
}

/// A volume's root reached by its GUID path is the top of a volume too,
/// as a volume mounted in a folder is; a folder on it is not.
#[test]
fn a_volume_root_by_guid_is_a_volume_top() {
    use windows_sys::Win32::Storage::FileSystem::VOLUME_NAME_GUID;
    let t = tempfile::tempdir().unwrap();
    let d = Dir::open_root(t.path()).unwrap();
    let path =
        String::from_utf16_lossy(&native::final_path(d.0.as_handle(), VOLUME_NAME_GUID).unwrap());
    let end = path.find('}').unwrap() + 2;
    let root = &path[..end];
    assert!(is_volume_guid_root(root), "{root}");
    assert!(!is_volume_guid_root(&path), "{path}");
    assert!(is_volume_top(Path::new(root)), "{root}");
    assert!(!is_volume_top(t.path()));
    assert!(!is_volume_top(Path::new(&path)), "{path}");
}
