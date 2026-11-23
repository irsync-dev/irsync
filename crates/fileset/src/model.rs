//! Entry model and lstat/xattr capture.

use std::io;
use std::path::Path;

use crate::platform;

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
    /// Reading the xattrs failed partway; the entry holds those read before. Such an
    /// entry's xattrs are read again on every update.
    pub const XATTRS_PARTIAL: Flags = Flags(4);

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
        let (mut entry, mut partial) = Entry::stat(abs)?;
        entry.load_xattrs(abs, &mut partial);
        Ok((entry, partial))
    }

    /// [`Entry::from_path`] without the xattrs, which are left empty.
    pub fn stat(abs: &Path) -> io::Result<(Entry, Vec<Partial>)> {
        let st = platform::lstat(abs)?;
        let mut partial = Vec::new();
        let target = if st.kind == Kind::Symlink {
            match platform::read_link(abs) {
                Ok(t) => Some(t),
                Err(e) => {
                    partial.push(Partial::Target(e));
                    None
                }
            }
        } else {
            None
        };
        let entry = Entry {
            kind: st.kind,
            mode: st.mode,
            uid: st.uid,
            gid: st.gid,
            size: st.size,
            nlink: st.nlink,
            ino: st.ino,
            dev: st.dev,
            rdev: st.rdev,
            mtime: st.mtime,
            ctime: st.ctime,
            blake3: None,
            target,
            xattrs: Vec::new(),
            flags: Flags::default(),
            generation: 0,
        };
        Ok((entry, partial))
    }

    /// Reads the xattrs of `abs` into the entry; a failure partway is added to `partial`
    /// and flags the entry `XATTRS_PARTIAL`.
    pub fn load_xattrs(&mut self, abs: &Path, partial: &mut Vec<Partial>) {
        let (xattrs, err) = platform::read_xattrs(abs);
        self.xattrs = xattrs;
        if let Some(e) = err {
            self.flags.insert(Flags::XATTRS_PARTIAL);
            partial.push(Partial::Xattrs(e));
        }
    }

    /// Whether `stored`, the manifest's record of this path, still holds its xattrs: the
    /// same object, unchanged since. Changing an xattr updates the ctime.
    pub fn xattrs_current_in(&self, stored: &Entry) -> bool {
        stored.kind == self.kind
            && stored.ino == self.ino
            && stored.size == self.size
            && stored.mtime == self.mtime
            && stored.ctime == self.ctime
            && !stored.flags.contains(Flags::XATTRS_PARTIAL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::testutil;

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
    fn a_failed_xattr_read_flags_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f");
        std::fs::write(&f, b"x").unwrap();
        let (e, partial) = Entry::from_path(&f).unwrap();
        assert!(partial.is_empty());
        assert!(!e.flags.contains(Flags::XATTRS_PARTIAL));
        let Some(_unreadable) = testutil::make_unreadable(&f) else {
            eprintln!("skipping: running with privileges that bypass file permissions");
            return;
        };
        let (e, partial) = Entry::from_path(&f).unwrap();
        // Only some systems refuse the xattrs of an unreadable file (macOS does).
        if !partial.iter().any(|p| matches!(p, Partial::Xattrs(_))) {
            eprintln!("skipping: xattrs of an unreadable file are readable here");
            return;
        }
        assert!(e.flags.contains(Flags::XATTRS_PARTIAL));
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
        testutil::set_mode(&p, 0o640);
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
    fn from_path_keeps_setuid_setgid_and_sticky_bits() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("f");
        std::fs::write(&f, b"").unwrap();
        testutil::set_mode(&f, 0o4755);
        let sub = d.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        testutil::set_mode(&sub, 0o3755);
        for (p, want) in [(f, 0o4755), (sub, 0o3755)] {
            if platform::lstat(&p).unwrap().mode != want {
                eprintln!("skipping: this filesystem does not keep mode {want:o}");
                return;
            }
            assert_eq!(Entry::from_path(&p).unwrap().0.mode, want);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn from_path_non_utf8_name() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join(platform::bytes_to_os(b"bad\xff").unwrap());
        if let Err(e) = std::fs::write(&p, b"abc") {
            // ZFS with utf8only=on refuses such names (EILSEQ, 84 on Linux).
            // CI's filesystem takes them, so there a refusal must fail.
            assert_eq!(e.raw_os_error(), Some(84), "{e}");
            assert!(std::env::var_os("CI").is_none(), "CI is set: {e}");
            eprintln!("skipping: the filesystem refuses non-UTF-8 names");
            return;
        }
        let e = Entry::from_path(&p).unwrap().0;
        assert_eq!(e.kind, Kind::File);
        assert_eq!(e.size, 3);
    }

    #[test]
    fn from_path_symlink_records_target_not_dest() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("l");
        testutil::symlink(Path::new("missing"), &p).unwrap();
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
}
