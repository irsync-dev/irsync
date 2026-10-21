//! Entry model and lstat/xattr capture.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Dir,
    File,
    Symlink,
    Fifo,
    Socket,
    CharDev,
    BlockDev,
    Other,
}

impl Kind {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(v: u8) -> Kind {
        match v {
            0 => Kind::Dir,
            1 => Kind::File,
            2 => Kind::Symlink,
            3 => Kind::Fifo,
            4 => Kind::Socket,
            5 => Kind::CharDev,
            6 => Kind::BlockDev,
            _ => Kind::Other,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Dir => "dir",
            Kind::File => "file",
            Kind::Symlink => "symlink",
            Kind::Fifo => "fifo",
            Kind::Socket => "socket",
            Kind::CharDev => "chardev",
            Kind::BlockDev => "blockdev",
            Kind::Other => "other",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags(pub u32);

impl Flags {
    pub const STORE: Flags = Flags(1);
    pub const UNREADABLE: Flags = Flags(2);

    pub fn contains(self, other: Flags) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn insert(&mut self, other: Flags) {
        self.0 |= other.0;
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timespec {
    pub sec: i64,
    pub nsec: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub nlink: u64,
    pub ino: u64,
    pub dev: u64,
    pub rdev: u64,
    pub mtime: Timespec,
    pub ctime: Timespec,
    pub blake3: Option<[u8; 32]>,
    pub target: Option<Vec<u8>>,
    pub xattrs: Vec<(Vec<u8>, Vec<u8>)>,
    pub flags: Flags,
    pub generation: u64,
}

/// Metadata of an entry that could not be read; the entry is recorded without it.
#[derive(Debug)]
pub enum Partial {
    /// Listing or reading extended attributes failed; the entry keeps those read before.
    Xattrs(io::Error),
    /// Reading a symlink's target failed; the entry has no target.
    Target(io::Error),
}

impl Partial {
    /// Warning text for this failure at the (escaped) relative `path`.
    pub fn warning(&self, path: &str) -> String {
        match self {
            Partial::Xattrs(e) => format!("cannot read xattrs of {path}: {e}"),
            Partial::Target(e) => format!("cannot read link target of {path}: {e}"),
        }
    }
}

impl Entry {
    /// Captures metadata of `abs` without following a final symlink. Only an lstat failure
    /// is an error; anything else that cannot be read is returned as a `Partial`.
    pub fn from_path(abs: &Path) -> io::Result<(Entry, Vec<Partial>)> {
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
        let mut partial = Vec::new();
        let target = if kind == Kind::Symlink {
            match std::fs::read_link(abs) {
                Ok(t) => Some(t.into_os_string().into_vec()),
                Err(e) => {
                    partial.push(Partial::Target(e));
                    None
                }
            }
        } else {
            None
        };
        let (xattrs, xattr_err) = read_xattrs(abs);
        if let Some(e) = xattr_err {
            partial.push(Partial::Xattrs(e));
        }
        let entry = Entry {
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
            blake3: None,
            target,
            xattrs,
            flags: Flags::default(),
            generation: 0,
        };
        Ok((entry, partial))
    }
}

fn is_unsupported(e: &io::Error) -> bool {
    // 95 = EOPNOTSUPP on Linux, 45 = ENOTSUP on macOS.
    e.kind() == io::ErrorKind::Unsupported || matches!(e.raw_os_error(), Some(95) | Some(45))
}

type Xattrs = Vec<(Vec<u8>, Vec<u8>)>;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const ALL_KINDS: [Kind; 8] = [
        Kind::Dir,
        Kind::File,
        Kind::Symlink,
        Kind::Fifo,
        Kind::Socket,
        Kind::CharDev,
        Kind::BlockDev,
        Kind::Other,
    ];

    #[test]
    fn kind_round_trips_through_u8() {
        for (i, k) in ALL_KINDS.into_iter().enumerate() {
            assert_eq!(k.as_u8() as usize, i);
            assert_eq!(Kind::from_u8(k.as_u8()), k);
        }
        assert_eq!(Kind::from_u8(200), Kind::Other);
    }

    #[test]
    fn kind_names() {
        let names: Vec<_> = ALL_KINDS.iter().map(|k| k.name()).collect();
        assert_eq!(
            names,
            ["dir", "file", "symlink", "fifo", "socket", "chardev", "blockdev", "other"]
        );
    }

    #[test]
    fn flags_contains_insert() {
        let mut f = Flags::default();
        assert!(f.is_empty());
        f.insert(Flags::STORE);
        assert!(f.contains(Flags::STORE));
        assert!(!f.contains(Flags::UNREADABLE));
        f.insert(Flags::UNREADABLE);
        assert_eq!(f.0, 3);
    }

    #[test]
    fn from_path_regular_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("f");
        std::fs::write(&p, b"abc").unwrap();
        std::fs::set_permissions(&p, PermissionsExt::from_mode(0o640)).unwrap();
        let e = Entry::from_path(&p).unwrap().0;
        assert_eq!(e.kind, Kind::File);
        assert_eq!(e.mode, 0o640);
        assert_eq!(e.size, 3);
        assert!(e.blake3.is_none());
        assert!(e.target.is_none());
        assert!(e.ino != 0);
        assert_eq!(e.generation, 0);
        assert!(e.flags.is_empty());
    }

    #[test]
    fn from_path_symlink_records_target_not_dest() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("l");
        std::os::unix::fs::symlink("missing", &p).unwrap();
        let e = Entry::from_path(&p).unwrap().0;
        assert_eq!(e.kind, Kind::Symlink);
        assert_eq!(e.target.as_deref(), Some(&b"missing"[..]));
    }

    #[test]
    fn from_path_dir_has_no_target() {
        let d = tempfile::tempdir().unwrap();
        let e = Entry::from_path(d.path()).unwrap().0;
        assert_eq!(e.kind, Kind::Dir);
        assert!(e.target.is_none());
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

    /// `/proc/1/cwd` is a symlink anyone can lstat but only root can readlink.
    #[cfg(target_os = "linux")]
    #[test]
    fn unreadable_link_target_keeps_the_entry() {
        let p = Path::new("/proc/1/cwd");
        if std::fs::read_link(p).is_ok() || std::fs::symlink_metadata(p).is_err() {
            eprintln!("skipping: /proc/1/cwd is readable here (root) or absent");
            return;
        }
        let (e, partial) = Entry::from_path(p).expect("a readlink failure must not drop the entry");
        assert_eq!(e.kind, Kind::Symlink);
        assert!(e.target.is_none());
        assert_eq!(partial.len(), 1, "{partial:?}");
        let w = partial[0].warning("l");
        assert!(w.starts_with("cannot read link target of l: "), "{w}");
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
