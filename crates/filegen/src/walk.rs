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
/// descended. An entry that is listed but cannot be stat'ed (permission
/// denied, or vanished) becomes `OffLimits` too.
///
/// Unless running as root, so are the entries filegen could not safely
/// change (see [`Guard`]): a subdirectory it cannot list, any entry not
/// owned by the effective uid, a regular file without owner read and
/// write, and a directory without owner read, write and search. As root, a
/// subdirectory that cannot be listed (it vanished) stays an empty `Dir`.
pub fn from_disk(root: &Path) -> io::Result<Model> {
    let root_meta = fs::symlink_metadata(root)?;
    if !root_meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    let root_dev = root_meta.dev();
    let guard = Guard::current();
    let mut model = Model::new();
    let mut stack: Vec<(PathBuf, NodeId)> = vec![(root.to_path_buf(), model.root())];
    while let Some((dir_path, dir_id)) = stack.pop() {
        let names = match list_sorted(&dir_path) {
            Ok(names) => names,
            Err(e) if tolerated(&e) && dir_id != model.root() => {
                if guard.is_some() {
                    mark_off_limits(&mut model, dir_id);
                }
                continue;
            }
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
            let kind = match guard {
                Some(g) if !g.may_change(&meta, kind) => NodeKind::OffLimits,
                _ => kind,
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

/// Turns the (still empty) directory `id` into an off-limits entry of the
/// same name. The id just freed is the next one handed out and children
/// stay sorted by name, so every id is as if the entry had been classified
/// off limits when it was listed.
fn mark_off_limits(model: &mut Model, id: NodeId) {
    let node = model.node(id);
    let parent = node.parent.expect("a subdirectory has a parent");
    let (name, ino) = (node.name.to_vec(), node.ino);
    model.remove(id);
    let again = model.insert(parent, name, NodeKind::OffLimits, 0, ino);
    debug_assert_eq!(again, id);
}

/// What an unprivileged filegen may change; `None` when running as root,
/// which may change anything.
///
/// The rules look only at ownership and the owner's permission bits, which
/// filegen's own operations never take away (`chmod` always keeps owner
/// rw, rwx on directories; giving an entry to another uid needs root), so a
/// later walk never puts an entry filegen made off limits.
#[derive(Clone, Copy)]
struct Guard {
    euid: u32,
}

impl Guard {
    fn current() -> Option<Guard> {
        let euid = nix::unistd::geteuid();
        (!euid.is_root()).then(|| Guard {
            euid: euid.as_raw(),
        })
    }

    /// Whether an entry of `kind` may be a target: owned by the effective
    /// uid, with owner read and write on a file and read, write and search
    /// on a directory. Without them an operation fails, and since a failed
    /// step rewinds the generator, fails again on every resume; and a
    /// rename into a directory filegen cannot list could replace entries
    /// the walk never saw.
    fn may_change(self, meta: &fs::Metadata, kind: NodeKind) -> bool {
        let need = match kind {
            NodeKind::File => 0o600,
            NodeKind::Dir => 0o700,
            _ => 0,
        };
        meta.uid() == self.euid && meta.mode() & need == need
    }
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

    #[test]
    fn walk_links_hard_linked_files() {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir(t.path().join("d")).unwrap();
        fs::write(t.path().join("a"), b"12345").unwrap();
        fs::hard_link(t.path().join("a"), t.path().join("d/b")).unwrap();
        fs::write(t.path().join("c"), b"1").unwrap();
        let m = from_disk(t.path()).unwrap();
        let a = m.resolve(b"a").unwrap();
        let b = m.resolve(b"d/b").unwrap();
        let c = m.resolve(b"c").unwrap();
        let mut both = vec![a, b];
        both.sort();
        assert_eq!(m.linked(a), both);
        assert_eq!(m.linked(b), both);
        assert_eq!(m.linked(c), [c]);
        assert_eq!(m.total_bytes(), 11);
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
    fn walk_marks_unlistable_dir_off_limits() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let locked = t.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("hidden"), b"z").unwrap();
        fs::write(t.path().join("z"), b"after").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let m = from_disk(t.path());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let m = m.unwrap();
        let id = m.resolve(b"locked").unwrap();
        if nix::unistd::geteuid().is_root() {
            // Root lists it and may change anything: nothing is guarded.
            assert_eq!(m.node(id).kind, NodeKind::Dir);
            assert!(m.resolve(b"locked/hidden").is_some());
        } else {
            assert_eq!(m.node(id).kind, NodeKind::OffLimits);
            assert!(m.node(id).children.is_empty());
            assert!(m.has_off_limits_below(m.root()));
            assert_eq!(child_names(&m, m.root()), [&b"locked"[..], b"z"]);
            assert_eq!(m.total_bytes(), 5);
        }
    }

    #[test]
    fn walk_marks_entries_it_cannot_safely_change_off_limits() {
        use std::os::unix::fs::PermissionsExt;
        if nix::unistd::geteuid().is_root() {
            eprintln!("skipping walk_marks_entries_it_cannot_safely_change_off_limits: root");
            return;
        }
        let t = tempfile::tempdir().unwrap();
        let mode = |rel: &str, m: u32| {
            fs::set_permissions(t.path().join(rel), fs::Permissions::from_mode(m)).unwrap()
        };
        let dir = |rel: &str| {
            fs::create_dir(t.path().join(rel)).unwrap();
            fs::write(t.path().join(rel).join("f"), b"f").unwrap();
        };
        // Files without owner read and write, as in a git checkout.
        fs::write(t.path().join("ro"), b"read only").unwrap();
        mode("ro", 0o444);
        fs::write(t.path().join("wo"), b"w").unwrap();
        mode("wo", 0o200);
        fs::write(t.path().join("rw"), b"rw").unwrap();
        mode("rw", 0o600);
        // Search but no write: entries could not be created or removed.
        dir("rx");
        mode("rx", 0o555);
        // Write and search but no read: a rename into it could replace
        // entries the walk cannot see.
        dir("wx");
        mode("wx", 0o300);
        // Read and write but no search: listable, its entries not statable.
        dir("rw_dir");
        mode("rw_dir", 0o600);
        dir("ok");
        mode("ok", 0o700);
        let m = from_disk(t.path());
        for d in ["rx", "wx", "rw_dir"] {
            mode(d, 0o755);
        }
        let m = m.unwrap();
        for p in [&b"ro"[..], b"wo", b"rx", b"wx", b"rw_dir"] {
            let id = m.resolve(p).unwrap();
            assert_eq!(m.node(id).kind, NodeKind::OffLimits, "{p:?}");
            assert!(m.node(id).children.is_empty(), "{p:?}");
        }
        assert_eq!(m.node(m.resolve(b"rw").unwrap()).kind, NodeKind::File);
        let ok = m.resolve(b"ok").unwrap();
        assert_eq!(m.node(ok).kind, NodeKind::Dir);
        assert_eq!(m.node(m.resolve(b"ok/f").unwrap()).kind, NodeKind::File);
        // Off-limits files carry no bytes in the model.
        assert_eq!(m.total_bytes(), 3);
        assert_eq!(m.entries(), 8);
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
