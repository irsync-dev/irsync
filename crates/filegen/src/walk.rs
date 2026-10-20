//! Start-up walk: build a model from what is on disk.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

use crate::model::{Model, NodeId, NodeKind};

/// Prefix of names filegen never touches or descends (fileset's own files).
const OFF_LIMITS_PREFIX: &[u8] = b".fileset";

/// Whether an entry with this name is off limits at any depth.
pub fn is_off_limits_name(name: &[u8]) -> bool {
    name.starts_with(OFF_LIMITS_PREFIX)
}

/// Walk the tree under `root` (never following symlinks) into a model.
///
/// Iterative with an explicit stack. Listings are sorted bytewise so the
/// resulting ids are deterministic. `.fileset*` names, entries on another
/// device, sockets and unknown types become `OffLimits` and are not
/// descended. A subdirectory that cannot be listed (permission denied, or
/// vanished) is kept as an empty `Dir`; an entry that is listed but cannot
/// be stat'ed for the same reasons becomes `OffLimits`.
pub fn from_disk(root: &Path) -> io::Result<Model> {
    let root_meta = fs::symlink_metadata(root)?;
    if !root_meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    let root_dev = root_meta.dev();
    let mut model = Model::new();
    let mut stack: Vec<(PathBuf, NodeId)> = vec![(root.to_path_buf(), model.root())];
    while let Some((dir_path, dir_id)) = stack.pop() {
        let names = match list_sorted(&dir_path) {
            Ok(names) => names,
            // An unlistable (or vanished) subdirectory stays an empty Dir.
            Err(e) if tolerated(&e) && dir_id != model.root() => continue,
            Err(e) => return Err(e),
        };
        for name in names {
            let path = dir_path.join(&name);
            let name = name.into_vec();
            if is_off_limits_name(&name) {
                model.insert(dir_id, name, NodeKind::OffLimits, 0, 0);
                continue;
            }
            let meta = match fs::symlink_metadata(&path) {
                Ok(meta) => meta,
                // Listed but not statable (dir lacks search permission, or
                // the entry vanished): keep the name, never touch it.
                Err(e) if tolerated(&e) => {
                    model.insert(dir_id, name, NodeKind::OffLimits, 0, 0);
                    continue;
                }
                Err(e) => return Err(e),
            };
            let ft = meta.file_type();
            let kind = if meta.dev() != root_dev {
                NodeKind::OffLimits
            } else if ft.is_dir() {
                NodeKind::Dir
            } else if ft.is_file() {
                NodeKind::File
            } else if ft.is_symlink() {
                NodeKind::Symlink
            } else if ft.is_fifo() {
                NodeKind::Fifo
            } else if ft.is_char_device() || ft.is_block_device() {
                NodeKind::Device
            } else {
                NodeKind::OffLimits
            };
            let size = if kind == NodeKind::File {
                meta.len()
            } else {
                0
            };
            let id = model.insert(dir_id, name, kind, size, meta.ino());
            if kind == NodeKind::Dir {
                stack.push((path, id));
            }
        }
    }
    Ok(model)
}

/// Names in `dir`, sorted bytewise. Any failure, opening the directory or
/// mid-iteration, is reported the same way.
fn list_sorted(dir: &Path) -> io::Result<Vec<OsString>> {
    let mut names: Vec<OsString> = fs::read_dir(dir)?
        .map(|entry| entry.map(|e| e.file_name()))
        .collect::<io::Result<_>>()?;
    names.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    Ok(names)
}

/// Errors on a single entry or subdirectory that the walk absorbs.
fn tolerated(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::NotFound
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NodeKind;
    use std::fs;

    fn child_names(m: &Model, dir: crate::model::NodeId) -> Vec<&[u8]> {
        m.node(dir)
            .children
            .iter()
            .map(|&c| m.node(c).name.as_ref())
            .collect()
    }

    #[test]
    fn walk_builds_sorted_model() {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir(t.path().join("b")).unwrap();
        fs::write(t.path().join("a.txt"), b"1234567").unwrap();
        std::os::unix::fs::symlink("a.txt", t.path().join("b/sym")).unwrap();
        nix::unistd::mkfifo(&t.path().join("b/fifo"), nix::sys::stat::Mode::S_IRWXU).unwrap();
        let m = from_disk(t.path()).unwrap();
        let r = m.root();
        assert_eq!(m.entries(), 4);
        assert_eq!(child_names(&m, r), [&b"a.txt"[..], b"b"]);
        let a = m.resolve(b"a.txt").unwrap();
        assert_eq!(m.node(a).kind, NodeKind::File);
        assert_eq!(m.node(a).size, 7);
        assert_ne!(m.node(a).ino, 0);
        let b = m.resolve(b"b").unwrap();
        assert_eq!(m.node(b).kind, NodeKind::Dir);
        assert_eq!(child_names(&m, b), [&b"fifo"[..], b"sym"]);
        let sym = m.resolve(b"b/sym").unwrap();
        assert_eq!(m.node(sym).kind, NodeKind::Symlink);
        let fifo = m.resolve(b"b/fifo").unwrap();
        assert_eq!(m.node(fifo).kind, NodeKind::Fifo);
        assert_eq!(m.total_bytes(), 7);
    }

    #[cfg(unix)]
    #[test]
    fn walk_marks_off_limits() {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join(".fileset.sqlite"), b"x").unwrap();
        fs::create_dir_all(t.path().join("sub/.fileset.fjall")).unwrap();
        fs::write(t.path().join("sub/.fileset.fjall/inner"), b"y").unwrap();
        let _sock = std::os::unix::net::UnixListener::bind(t.path().join("sock")).unwrap();
        let m = from_disk(t.path()).unwrap();
        let r = m.root();
        for p in [&b".fileset.sqlite"[..], b"sub/.fileset.fjall", b"sock"] {
            let id = m.resolve(p).unwrap();
            assert_eq!(m.node(id).kind, NodeKind::OffLimits, "{p:?}");
        }
        let fjall = m.resolve(b"sub/.fileset.fjall").unwrap();
        assert!(m.node(fjall).children.is_empty());
        assert_eq!(m.resolve(b"sub/.fileset.fjall/inner"), None);
        let sub = m.resolve(b"sub").unwrap();
        assert_eq!(m.node(sub).kind, NodeKind::Dir);
        assert!(m.has_off_limits_below(r));
        assert!(m.has_off_limits_below(sub));
        assert_eq!(m.total_bytes(), 0);
        assert!(is_off_limits_name(b".fileset.sqlite-wal"));
        assert!(is_off_limits_name(b".fileset"));
        assert!(!is_off_limits_name(b"fileset"));
        assert!(!is_off_limits_name(b"x.fileset"));
    }

    #[test]
    fn walk_keeps_unreadable_dir_as_empty_dir() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let locked = t.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("hidden"), b"z").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let readable = fs::read_dir(&locked).is_ok(); // true when running as root
        let m = from_disk(t.path());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let m = m.unwrap();
        let id = m.resolve(b"locked").unwrap();
        assert_eq!(m.node(id).kind, NodeKind::Dir);
        assert_eq!(m.node(id).children.is_empty(), !readable);
    }

    #[test]
    fn walk_marks_unstatable_entry_off_limits() {
        use std::os::unix::fs::PermissionsExt;
        if nix::unistd::geteuid().is_root() {
            eprintln!("skipping walk_marks_unstatable_entry_off_limits: running as root");
            return;
        }
        let t = tempfile::tempdir().unwrap();
        let listable = t.path().join("listable");
        fs::create_dir(&listable).unwrap();
        fs::write(listable.join("f"), b"z").unwrap();
        // Readable but not searchable: listing works, lstat of entries fails.
        fs::set_permissions(&listable, fs::Permissions::from_mode(0o644)).unwrap();
        let m = from_disk(t.path());
        fs::set_permissions(&listable, fs::Permissions::from_mode(0o755)).unwrap();
        let m = m.unwrap();
        let dir = m.resolve(b"listable").unwrap();
        assert_eq!(m.node(dir).kind, NodeKind::Dir);
        let f = m.resolve(b"listable/f").unwrap();
        assert_eq!(m.node(f).kind, NodeKind::OffLimits);
        assert!(m.has_off_limits_below(dir));
        assert_eq!(m.total_bytes(), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn walk_non_utf8_name() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join(OsStr::from_bytes(b"\xff\xfe")), b"ab").unwrap();
        let m = from_disk(t.path()).unwrap();
        let id = m.resolve(b"\xff\xfe").unwrap();
        assert_eq!(m.node(id).kind, NodeKind::File);
        assert_eq!(m.node(id).size, 2);
        let key = crate::escape::escape(b"\xff\xfe");
        assert!(m.has_folded_name(m.root(), &key));
    }
}
