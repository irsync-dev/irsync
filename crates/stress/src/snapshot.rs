//! A tree snapshot for the replay: `walk_meta` records every entry's
//! metadata, `copy_tree` makes a cheap copy that keeps everything filegen's
//! walk and ops read (names, types, content, modes, mtimes, symlink targets,
//! fifos, hard-link groups, `user.*` xattrs), and owners when run as root.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{symlink, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use nix::fcntl::AT_FDCWD;
use nix::sys::stat::{mknod, utimensat, Mode, SFlag, UtimensatFlags};
use nix::sys::time::TimeSpec;
use nix::unistd::mkfifo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Fifo,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    pub kind: EntryKind,
    /// Byte size; 0 for directories, whose size is filesystem noise.
    pub size: u64,
    /// Permission bits (`st_mode & 0o7777`).
    pub mode: u32,
    pub mtime_ns: i128,
    /// A symlink's target, byte for byte.
    pub target: Option<Vec<u8>>,
    pub ino: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyMode {
    Reflink,
    Plain,
}

fn kind_of(ft: fs::FileType) -> EntryKind {
    if ft.is_file() {
        EntryKind::File
    } else if ft.is_dir() {
        EntryKind::Dir
    } else if ft.is_symlink() {
        EntryKind::Symlink
    } else if ft.is_fifo() {
        EntryKind::Fifo
    } else {
        EntryKind::Other
    }
}

fn mtime_ns(m: &fs::Metadata) -> i128 {
    i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec())
}

fn meta_of(path: &Path) -> Result<Meta> {
    let m = fs::symlink_metadata(path).with_context(|| format!("stat {}", path.display()))?;
    let kind = kind_of(m.file_type());
    let target = if kind == EntryKind::Symlink {
        Some(
            fs::read_link(path)
                .with_context(|| format!("readlink {}", path.display()))?
                .into_os_string()
                .into_vec(),
        )
    } else {
        None
    };
    Ok(Meta {
        kind,
        size: if kind == EntryKind::Dir { 0 } else { m.len() },
        mode: m.mode() & 0o7777,
        mtime_ns: mtime_ns(&m),
        target,
        ino: m.ino(),
    })
}

/// Every entry under `root` (not `root` itself), keyed by its path relative
/// to `root` as bytes. Symlinks are not followed.
pub fn walk_meta(root: &Path) -> Result<BTreeMap<Vec<u8>, Meta>> {
    fn rec(dir: &Path, rel: &[u8], out: &mut BTreeMap<Vec<u8>, Meta>) -> Result<()> {
        for e in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let e = e.with_context(|| format!("reading {}", dir.display()))?;
            let mut key = rel.to_vec();
            if !key.is_empty() {
                key.push(b'/');
            }
            key.extend_from_slice(e.file_name().as_bytes());
            let meta = meta_of(&e.path())?;
            let is_dir = meta.kind == EntryKind::Dir;
            out.insert(key.clone(), meta);
            if is_dir {
                rec(&e.path(), &key, out)?;
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    rec(root, b"", &mut out)?;
    Ok(out)
}

/// Makes `dst` share `src`'s extents. Fails where the filesystem cannot.
#[cfg(target_os = "linux")]
fn reflink(src: &File, dst: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: FICLONE takes the source descriptor as its integer argument
    // and touches no memory of ours.
    let r = unsafe { libc::ioctl(dst.as_raw_fd(), libc::FICLONE, src.as_raw_fd()) };
    if r == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// macOS: `clonefile` needs `dst` not to exist.
#[cfg(target_os = "macos")]
fn clonefile(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    let f = CString::new(from.as_os_str().as_bytes())?;
    let t = CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: both are valid NUL-terminated strings that outlive the call.
    let r = unsafe { libc::clonefile(f.as_ptr(), t.as_ptr(), 0) };
    if r == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Copies `from` to `to` (replaced): a clone where the
/// filesystem allows, else a plain copy. True means cloned. Mode and times
/// are not carried over. A missing source is an error that leaves `to` as
/// it was.
pub fn clone_or_copy_file(from: &Path, to: &Path) -> Result<bool> {
    // Open the source before touching `to`, so a failure leaves it alone.
    let mut src = File::open(from).with_context(|| format!("opening {}", from.display()))?;
    #[cfg(target_os = "macos")]
    {
        let _ = fs::remove_file(to);
        if clonefile(from, to).is_ok() {
            return Ok(true);
        }
    }
    // A fresh file, never the old one truncated. On ZFS, writing into a
    // file again after an earlier clone or copy filled it (the replay
    // reuses one `pre-step` file for every step) fails with EIO under load,
    // on the write. Seen as a harness error in `--concurrent` runs with
    // eight at once, in about 3 of 8 parallel runs; after unlinking first,
    // 0 of 48 parallel runs hit it.
    match fs::remove_file(to) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            return Err(e).with_context(|| format!("removing {}", to.display()));
        }
        _ => {}
    }
    let mut dst = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(to)
        .with_context(|| format!("creating {}", to.display()))?;
    #[cfg(target_os = "linux")]
    if reflink(&src, &dst).is_ok() {
        return Ok(true);
    }
    io::copy(&mut src, &mut dst)
        .with_context(|| format!("copying {} to {}", from.display(), to.display()))?;
    Ok(false)
}

fn set_times(path: &Path, m: &fs::Metadata) -> Result<()> {
    utimensat(
        AT_FDCWD,
        path,
        &TimeSpec::new(m.atime(), m.atime_nsec()),
        &TimeSpec::new(m.mtime(), m.mtime_nsec()),
        UtimensatFlags::NoFollowSymlink,
    )
    .with_context(|| format!("setting times of {}", path.display()))
}

/// As root, gives `to` `m`'s owner; before any chmod, since a chown
/// clears set-id bits. Others cannot, and keep their own.
fn copy_owner(to: &Path, m: &fs::Metadata) -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        return Ok(());
    }
    std::os::unix::fs::lchown(to, Some(m.uid()), Some(m.gid()))
        .with_context(|| format!("chown {}", to.display()))
}

fn copy_xattrs(from: &Path, to: &Path) -> Result<()> {
    let names = match xattr::list(from) {
        Ok(n) => n,
        // The filesystem has no xattrs, so there is nothing to carry.
        Err(e) if e.raw_os_error() == Some(libc::ENOTSUP) => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("listing xattrs of {}", from.display())),
    };
    for name in names {
        if !name.as_bytes().starts_with(b"user.") {
            continue;
        }
        if let Some(v) = xattr::get(from, &name)
            .with_context(|| format!("reading xattr of {}", from.display()))?
        {
            xattr::set(to, &name, &v)
                .with_context(|| format!("setting xattr on {}", to.display()))?;
        }
    }
    Ok(())
}

struct CopyState {
    all_cloned: bool,
    /// (dev, ino) of a multiply-linked source entry to its copy.
    links: HashMap<(u64, u64), PathBuf>,
    /// Directories whose mode and times are set once their contents are in.
    dirs: Vec<(PathBuf, fs::Metadata)>,
}

fn copy_entry(from: &Path, to: &Path, st: &mut CopyState) -> Result<()> {
    let m = fs::symlink_metadata(from).with_context(|| format!("stat {}", from.display()))?;
    let ft = m.file_type();
    if ft.is_dir() {
        match fs::create_dir(to) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && to.is_dir() => {}
            Err(e) => return Err(e).with_context(|| format!("creating {}", to.display())),
        }
        copy_owner(to, &m)?;
        // Writable for now, whatever the source's mode.
        fs::set_permissions(to, fs::Permissions::from_mode(0o700))?;
        copy_xattrs(from, to)?;
        let mut names: Vec<_> = fs::read_dir(from)
            .with_context(|| format!("reading {}", from.display()))?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<io::Result<_>>()?;
        names.sort();
        for n in names {
            copy_entry(&from.join(&n), &to.join(&n), st)?;
        }
        st.dirs.push((to.to_path_buf(), m));
        return Ok(());
    }
    if m.nlink() > 1 {
        if let Some(first) = st.links.get(&(m.dev(), m.ino())) {
            fs::hard_link(first, to)
                .with_context(|| format!("linking {} to {}", to.display(), first.display()))?;
            return Ok(());
        }
        st.links.insert((m.dev(), m.ino()), to.to_path_buf());
    }
    if ft.is_file() {
        if !clone_or_copy_file(from, to)? {
            st.all_cloned = false;
        }
        copy_xattrs(from, to)?;
        copy_owner(to, &m)?;
        fs::set_permissions(to, fs::Permissions::from_mode(m.mode() & 0o7777))?;
    } else if ft.is_symlink() {
        let target = fs::read_link(from)?;
        symlink(&target, to).with_context(|| format!("symlink {}", to.display()))?;
        copy_owner(to, &m)?;
    } else if ft.is_fifo() {
        mkfifo(to, Mode::from_bits_truncate(0o600))
            .with_context(|| format!("mkfifo {}", to.display()))?;
        copy_owner(to, &m)?;
        fs::set_permissions(to, fs::Permissions::from_mode(m.mode() & 0o7777))?;
    } else if (ft.is_block_device() || ft.is_char_device()) && unsafe { libc::geteuid() } == 0 {
        let sflag = if ft.is_block_device() {
            SFlag::S_IFBLK
        } else {
            SFlag::S_IFCHR
        };
        mknod(
            to,
            sflag,
            Mode::from_bits_truncate((m.mode() & 0o7777) as _),
            m.rdev() as _,
        )
        .with_context(|| format!("mknod {}", to.display()))?;
        copy_owner(to, &m)?;
    } else {
        bail!(
            "cannot copy {}: a device node or socket (device nodes need root)",
            from.display()
        );
    }
    set_times(to, &m)
}

/// Copies the tree at `from` to `to` (created if missing), keeping types,
/// content, modes, mtimes to the nanosecond, symlink targets, fifos,
/// hard-link groups and `user.*` xattrs. Each file is cloned where the
/// filesystem allows and copied otherwise; the result is `Reflink` only if
/// every file was cloned.
pub fn copy_tree(from: &Path, to: &Path) -> Result<CopyMode> {
    let mut st = CopyState {
        all_cloned: true,
        links: HashMap::new(),
        dirs: Vec::new(),
    };
    copy_entry(from, to, &mut st)?;
    // Deepest last-pushed is the root; setting in push order puts children
    // first, so a child's change never moves a parent's mtime afterwards.
    for (p, m) in st.dirs {
        fs::set_permissions(&p, fs::Permissions::from_mode(m.mode() & 0o7777))?;
        set_times(&p, &m)?;
    }
    Ok(if st.all_cloned {
        CopyMode::Reflink
    } else {
        CopyMode::Plain
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::os::unix::ffi::OsStrExt;

    use super::*;
    use crate::filegen::{self as fg, FilegenOpts};

    /// The workspace's `filegen`, in the nearest folder above this test
    /// binary that holds one: the profile folder (`target/debug`), however
    /// deep Cargo puts test binaries below it (`deps/`, or `build/<pkg>/<hash>/`
    /// since Rust 1.100).
    fn filegen_bin() -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        exe.ancestors()
            .skip(1)
            .map(|d| d.join("filegen"))
            .find(|p| p.is_file())
            .unwrap_or_else(|| {
                panic!(
                    "no filegen above {}: run `cargo build --workspace` first",
                    exe.display()
                )
            })
    }

    fn opts(steps: u64) -> FilegenOpts<'static> {
        FilegenOpts {
            steps,
            max_total_size: Some("4M"),
            max_file_size: Some("256K"),
            sets: &[],
        }
    }

    /// A filegen tree of `steps` steps under `base/tree`; returns the tree
    /// and the out dir holding the state file.
    fn make_tree(base: &Path, steps: u64) -> (PathBuf, PathBuf) {
        let tree = base.join("tree");
        let out = base.join("out");
        fs::create_dir_all(&tree).unwrap();
        let args = fg::fill_args(7, &out, &tree, &opts(steps));
        fg::run(&filegen_bin(), &args, &base.join("fg.err")).unwrap();
        (tree, out)
    }

    fn without_ino(m: &BTreeMap<Vec<u8>, Meta>) -> BTreeMap<Vec<u8>, Meta> {
        m.iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    Meta {
                        ino: 0,
                        ..v.clone()
                    },
                )
            })
            .collect()
    }

    fn link_groups(m: &BTreeMap<Vec<u8>, Meta>) -> BTreeSet<BTreeSet<Vec<u8>>> {
        let mut by: BTreeMap<u64, BTreeSet<Vec<u8>>> = BTreeMap::new();
        for (k, v) in m {
            if v.kind != EntryKind::Dir {
                by.entry(v.ino).or_default().insert(k.clone());
            }
        }
        by.into_values().collect()
    }

    #[test]
    fn copy_tree_preserves_what_filegen_reads() {
        let t = tempfile::tempdir().unwrap();
        let (tree, _) = make_tree(t.path(), 200);
        let copy = t.path().join("copy");
        copy_tree(&tree, &copy).unwrap();
        let a = walk_meta(&tree).unwrap();
        let b = walk_meta(&copy).unwrap();
        assert!(a.len() > 10, "only {} entries", a.len());
        assert_eq!(without_ino(&a), without_ino(&b));
        assert_eq!(link_groups(&a), link_groups(&b));
        // Content too.
        for (k, v) in &a {
            if v.kind == EntryKind::File {
                let p = PathBuf::from(std::ffi::OsStr::from_bytes(k));
                assert_eq!(
                    blake3::hash(&fs::read(tree.join(&p)).unwrap()),
                    blake3::hash(&fs::read(copy.join(&p)).unwrap()),
                    "{}",
                    p.display()
                );
            }
        }
    }

    type Shape = BTreeMap<Vec<u8>, (EntryKind, u64, u32, Option<Vec<u8>>, Option<[u8; 32]>)>;

    fn shape(root: &Path) -> Shape {
        walk_meta(root)
            .unwrap()
            .into_iter()
            .map(|(k, m)| {
                let hash = (m.kind == EntryKind::File).then(|| {
                    let p = root.join(std::ffi::OsStr::from_bytes(&k));
                    *blake3::hash(&fs::read(p).unwrap()).as_bytes()
                });
                (k, (m.kind, m.size, m.mode, m.target, hash))
            })
            .collect()
    }

    type Step = (String, Vec<u8>, Option<Vec<u8>>, u64);

    fn steps_in(log: &Path) -> Vec<Step> {
        fs::read_to_string(log)
            .unwrap()
            .lines()
            .filter_map(filegen::log::parse_step_line)
            .map(|s| (s.op.name().to_string(), s.path, s.to, s.bytes))
            .collect()
    }

    #[test]
    fn copy_tree_then_filegen_resume_is_identical() {
        let t = tempfile::tempdir().unwrap();
        let (tree, out) = make_tree(t.path(), 200);
        let copy = t.path().join("copy");
        copy_tree(&tree, &copy).unwrap();
        let state_name = "filegen-7.state.yaml";
        let mut logs = Vec::new();
        for (name, target) in [("ra", &tree), ("rb", &copy)] {
            let dir = t.path().join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::copy(out.join(state_name), dir.join(state_name)).unwrap();
            let args = fg::mutate_args(&dir.join(state_name), target, &opts(50));
            fg::run(&filegen_bin(), &args, &dir.join("err")).unwrap();
            logs.push(dir.join("filegen-7.log"));
        }
        let (a, b) = (steps_in(&logs[0]), steps_in(&logs[1]));
        assert!(!a.is_empty(), "no steps replayed");
        assert_eq!(a, b);
        assert_eq!(shape(&tree), shape(&copy));
    }

    #[test]
    fn walk_meta_sees_awkward_names() {
        let t = tempfile::tempdir().unwrap();
        let name = std::ffi::OsStr::from_bytes(b"x\xff");
        if let Err(e) = fs::write(t.path().join(name), b"hi") {
            // A utf8only filesystem (ZFS) refuses such names (EILSEQ).
            assert_eq!(e.raw_os_error(), Some(libc::EILSEQ), "{e}");
            eprintln!("skipped: filesystem refuses non-UTF-8 names");
            return;
        }
        fs::create_dir(t.path().join("d")).unwrap();
        fs::write(t.path().join("d").join("y"), b"").unwrap();
        let m = walk_meta(t.path()).unwrap();
        let keys: Vec<&[u8]> = m.keys().map(|k| k.as_slice()).collect();
        assert_eq!(keys, vec![&b"d"[..], b"d/y", b"x\xff"]);
        assert_eq!(m[&b"x\xff".to_vec()].size, 2);
    }

    #[test]
    fn copy_keeps_links_fifos_xattrs_and_restrictive_dirs() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        fs::create_dir_all(src.join("ro/sub")).unwrap();
        fs::write(src.join("ro/sub/f"), b"data").unwrap();
        fs::hard_link(src.join("ro/sub/f"), src.join("h")).unwrap();
        symlink("../nowhere\u{e9}", src.join("l")).unwrap();
        mkfifo(&src.join("p"), Mode::from_bits_truncate(0o640)).unwrap();
        let has_xattr = xattr::set(src.join("h"), "user.k", b"v").is_ok();
        fs::set_permissions(src.join("ro"), fs::Permissions::from_mode(0o500)).unwrap();
        let dst = t.path().join("dst");
        let r = copy_tree(&src, &dst);
        fs::set_permissions(src.join("ro"), fs::Permissions::from_mode(0o700)).unwrap();
        if dst.join("ro").exists() {
            fs::set_permissions(dst.join("ro"), fs::Permissions::from_mode(0o700)).unwrap();
        }
        r.unwrap();
        let a = walk_meta(&src).unwrap();
        let b = walk_meta(&dst).unwrap();
        assert_eq!(a.len(), 6);
        assert_eq!(b[&b"p".to_vec()].kind, EntryKind::Fifo);
        assert_eq!(b[&b"p".to_vec()].mode, 0o640);
        assert_eq!(b[&b"l".to_vec()].target, a[&b"l".to_vec()].target);
        assert_eq!(b[&b"h".to_vec()].ino, b[&b"ro/sub/f".to_vec()].ino);
        assert_eq!(
            b[&b"ro/sub/f".to_vec()].mtime_ns,
            a[&b"ro/sub/f".to_vec()].mtime_ns
        );
        if has_xattr {
            assert_eq!(xattr::get(dst.join("h"), "user.k").unwrap().unwrap(), b"v");
        }
    }

    #[test]
    fn clone_or_copy_file_replaces_the_old_file() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        fs::write(d.join("a"), b"new").unwrap();
        fs::write(d.join("b"), b"a much longer old content").unwrap();
        fs::hard_link(d.join("b"), d.join("b2")).unwrap();
        clone_or_copy_file(&d.join("a"), &d.join("b")).unwrap();
        assert_eq!(fs::read(d.join("b")).unwrap(), b"new");
        // A new file, not the old one rewritten: the other link is untouched.
        assert_eq!(
            fs::read(d.join("b2")).unwrap(),
            b"a much longer old content"
        );
        // A missing destination is fine too, and a missing source is an error.
        clone_or_copy_file(&d.join("a"), &d.join("c")).unwrap();
        assert!(clone_or_copy_file(&d.join("nope"), &d.join("b")).is_err());
        assert_eq!(fs::read(d.join("b")).unwrap(), b"new");
    }

    #[test]
    fn clone_or_copy_file_copies_content() {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join("a"), b"hello").unwrap();
        fs::write(t.path().join("b"), b"old and longer").unwrap();
        clone_or_copy_file(&t.path().join("a"), &t.path().join("b")).unwrap();
        assert_eq!(fs::read(t.path().join("b")).unwrap(), b"hello");
    }
}
