//! Start-up walk: build a model from what is on disk.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::model::{Model, NodeId, NodeKind};
use crate::platform::{self, FileKind};

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
/// change (see [`platform::may_change`]): a subdirectory it cannot list,
/// any entry not owned by the effective uid, a regular file without owner
/// read and write, and a directory without owner read, write and search.
/// As root, a subdirectory that cannot be listed (it vanished) stays an
/// empty `Dir`.
pub fn from_disk(root: &Path) -> io::Result<Model> {
    let root_meta = platform::lstat(root)?;
    if root_meta.kind != FileKind::Dir {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    let root_dev = root_meta.dev;
    let guarded = !platform::is_root();
    let mut model = Model::new();
    let mut stack: Vec<(PathBuf, NodeId)> = vec![(root.to_path_buf(), model.root())];
    while let Some((dir_path, dir_id)) = stack.pop() {
        let names = match list_sorted(&dir_path) {
            Ok(names) => names,
            Err(e) if tolerated(&e) && dir_id != model.root() => {
                if guarded {
                    mark_off_limits(&mut model, dir_id);
                }
                continue;
            }
            Err(e) => return Err(e),
        };
        for (name, os_name) in names {
            let path = dir_path.join(&os_name);
            if is_off_limits_name(&name) {
                model.insert(dir_id, name, NodeKind::OffLimits, 0, 0);
                continue;
            }
            let meta = match platform::lstat(&path) {
                Ok(meta) => meta,
                // Listed but not statable (dir lacks search permission, or
                // the entry vanished): keep the name, never touch it.
                Err(e) if tolerated(&e) => {
                    model.insert(dir_id, name, NodeKind::OffLimits, 0, 0);
                    continue;
                }
                Err(e) => return Err(e),
            };
            let kind = if meta.dev != root_dev {
                NodeKind::OffLimits
            } else {
                match meta.kind {
                    FileKind::Dir => NodeKind::Dir,
                    FileKind::File => NodeKind::File,
                    FileKind::Symlink => NodeKind::Symlink,
                    FileKind::Fifo => NodeKind::Fifo,
                    FileKind::CharDev | FileKind::BlockDev => NodeKind::Device,
                    FileKind::Socket | FileKind::Other => NodeKind::OffLimits,
                }
            };
            // Off limits already (another device, a socket) stays so
            // whatever the guard says.
            let kind = if platform::may_change(&meta) {
                kind
            } else {
                NodeKind::OffLimits
            };
            let size = if kind == NodeKind::File { meta.size } else { 0 };
            let id = model.insert(dir_id, name, kind, size, meta.ino);
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

/// Names in `dir`, each as its stored bytes and as the OS name, sorted by
/// the bytes. Any failure, opening the directory or mid-iteration, is
/// reported the same way.
fn list_sorted(dir: &Path) -> io::Result<Vec<(Vec<u8>, OsString)>> {
    let mut names: Vec<(Vec<u8>, OsString)> = fs::read_dir(dir)?
        .map(|entry| {
            entry.map(|e| {
                let name = e.file_name();
                (platform::os_to_bytes(&name), name)
            })
        })
        .collect::<io::Result<_>>()?;
    // By the bytes alone: no two names of a directory have the same.
    names.sort_by(|a, b| a.0.cmp(&b.0));
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
    use crate::platform::testutil;
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
        if !testutil::symlinks_available(t.path()) {
            return;
        }
        fs::create_dir(t.path().join("b")).unwrap();
        fs::write(t.path().join("a.txt"), b"1234567").unwrap();
        testutil::symlink(Path::new("a.txt"), &t.path().join("b").join("sym")).unwrap();
        // FIFOs are POSIX-only: the tree has one where the platform has them.
        #[cfg(unix)]
        platform::mkfifo(&t.path().join("b/fifo"), 0o700).unwrap();
        let fifo = cfg!(unix);
        let m = from_disk(t.path()).unwrap();
        let r = m.root();
        assert_eq!(m.entries(), 3 + usize::from(fifo));
        assert_eq!(child_names(&m, r), [&b"a.txt"[..], b"b"]);
        let a = m.resolve(b"a.txt").unwrap();
        assert_eq!(m.node(a).kind, NodeKind::File);
        assert_eq!(m.node(a).size, 7);
        assert_ne!(m.node(a).ino, 0);
        let b = m.resolve(b"b").unwrap();
        assert_eq!(m.node(b).kind, NodeKind::Dir);
        if fifo {
            assert_eq!(child_names(&m, b), [&b"fifo"[..], b"sym"]);
        } else {
            assert_eq!(child_names(&m, b), [&b"sym"[..]]);
        }
        let sym = m.resolve(b"b/sym").unwrap();
        assert_eq!(m.node(sym).kind, NodeKind::Symlink);
        if fifo {
            let fifo = m.resolve(b"b/fifo").unwrap();
            assert_eq!(m.node(fifo).kind, NodeKind::Fifo);
        }
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
        let _sock = testutil::bind_socket(&t.path().join("sock"));
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

    #[cfg(unix)]
    #[test]
    fn walk_marks_unlistable_dir_off_limits() {
        let t = tempfile::tempdir().unwrap();
        let locked = t.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("hidden"), b"z").unwrap();
        fs::write(t.path().join("z"), b"after").unwrap();
        testutil::set_mode(&locked, 0o000);
        let m = from_disk(t.path());
        testutil::set_mode(&locked, 0o755);
        let m = m.unwrap();
        let id = m.resolve(b"locked").unwrap();
        if platform::is_root() {
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

    #[cfg(unix)]
    #[test]
    fn walk_marks_entries_it_cannot_safely_change_off_limits() {
        if platform::is_root() {
            eprintln!("skipping walk_marks_entries_it_cannot_safely_change_off_limits: root");
            return;
        }
        let t = tempfile::tempdir().unwrap();
        let mode = |rel: &str, m: u32| testutil::set_mode(&t.path().join(rel), m);
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

    /// Sets or clears the read-only attribute of `path`.
    #[cfg(windows)]
    fn set_readonly(path: &Path, on: bool) {
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_readonly(on);
        fs::set_permissions(path, perms).unwrap();
    }

    /// The Windows counterpart of the test above: what filegen could not
    /// safely change is what carries the read-only attribute.
    #[cfg(windows)]
    #[test]
    fn walk_marks_read_only_entries_off_limits() {
        let t = tempfile::tempdir().unwrap();
        let p = |rel: &str| t.path().join(rel);
        // A read-only file, as in some checkouts and copies from media.
        fs::write(p("ro"), b"read only").unwrap();
        set_readonly(&p("ro"), true);
        fs::write(p("rw"), b"rw").unwrap();
        // A read-only directory is not entered.
        fs::create_dir(p("ro_dir")).unwrap();
        fs::write(p("ro_dir").join("f"), b"f").unwrap();
        set_readonly(&p("ro_dir"), true);
        fs::create_dir(p("ok")).unwrap();
        fs::write(p("ok").join("f"), b"f").unwrap();
        let m = from_disk(t.path());
        set_readonly(&p("ro"), false);
        set_readonly(&p("ro_dir"), false);
        let m = m.unwrap();
        for rel in [&b"ro"[..], b"ro_dir"] {
            let id = m.resolve(rel).unwrap();
            assert_eq!(m.node(id).kind, NodeKind::OffLimits, "{rel:?}");
            assert!(m.node(id).children.is_empty(), "{rel:?}");
        }
        assert_eq!(m.node(m.resolve(b"rw").unwrap()).kind, NodeKind::File);
        let ok = m.resolve(b"ok").unwrap();
        assert_eq!(m.node(ok).kind, NodeKind::Dir);
        assert_eq!(m.node(m.resolve(b"ok/f").unwrap()).kind, NodeKind::File);
        // Off-limits files carry no bytes in the model.
        assert_eq!(m.total_bytes(), 3);
        assert_eq!(m.entries(), 5);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn walk_non_utf8_name() {
        let t = tempfile::tempdir().unwrap();
        let name = platform::bytes_to_os(b"\xff\xfe").unwrap();
        if let Err(e) = fs::write(t.path().join(name), b"ab") {
            // ZFS with utf8only=on refuses such names (EILSEQ, 84 on Linux).
            // CI's filesystem takes them, so there a refusal must fail.
            assert_eq!(e.raw_os_error(), Some(84), "{e}");
            assert!(std::env::var_os("CI").is_none(), "CI is set: {e}");
            eprintln!("skipping: the filesystem refuses non-UTF-8 names");
            return;
        }
        let m = from_disk(t.path()).unwrap();
        let id = m.resolve(b"\xff\xfe").unwrap();
        assert_eq!(m.node(id).kind, NodeKind::File);
        assert_eq!(m.node(id).size, 2);
        let key = crate::escape::log_escape(b"\xff\xfe");
        assert!(m.has_folded_name(m.root(), &key));
    }
}
