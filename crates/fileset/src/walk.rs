//! Sorted depth-first filesystem walker with store and mount-point rules.

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use crate::escape::escape;
use crate::model::{Entry, Flags};
use crate::store::{FJALL_NAME, SQLITE_NAME};

#[derive(Clone, Debug)]
pub struct WalkConfig {
    /// Canonical absolute root.
    pub root: PathBuf,
    pub cross_mount_points: bool,
    pub sort: bool,
    /// Absolute path of the store inside the tree, if any.
    pub store_path: Option<PathBuf>,
    /// The store is a SQLite file: its `-wal`, `-shm` and `-journal` sidecars, whatever
    /// the store is named, are skipped silently.
    pub sqlite_store: bool,
}

#[derive(Debug)]
pub struct Observed {
    /// Relative path bytes; `b"."` for the root.
    pub path: Vec<u8>,
    pub abs: PathBuf,
    pub entry: Entry,
    pub hash_error: Option<String>,
}

#[derive(Debug)]
// The enum shape is fixed by the design.
#[allow(clippy::large_enum_variant)]
pub enum WalkEvent {
    Entry(Observed),
    Warning(String),
    /// Relative path of a `.fileset.sqlite` or `.fileset.fjall` below the root. The
    /// object itself follows as an ordinary `Entry`; the engine decides whether this is a
    /// warning or a reason to stop.
    NestedManifest(Vec<u8>),
    /// Relative path of a directory the walker did not descend, or of an object it could
    /// not stat; emitted right after the corresponding `Warning`. Nothing stored at or
    /// below it was observed.
    NotDescended(Vec<u8>),
}

type Sink<'a> = dyn FnMut(WalkEvent) -> anyhow::Result<()> + 'a;

/// The warning text for a `WalkEvent::NestedManifest`.
pub fn nested_manifest_warning(rel: &[u8]) -> String {
    format!("nested manifest at {}", escape(rel))
}

pub fn walk(cfg: &WalkConfig, sink: &mut Sink<'_>) -> anyhow::Result<()> {
    let (entry, partial) = Entry::from_path(&cfg.root)?;
    for p in &partial {
        sink(WalkEvent::Warning(p.warning(".")))?;
    }
    let root_dev = entry.dev;
    let is_dir = entry.kind == crate::model::Kind::Dir;
    sink(WalkEvent::Entry(Observed {
        path: b".".to_vec(),
        abs: cfg.root.clone(),
        entry,
        hash_error: None,
    }))?;
    if is_dir {
        walk_dir(cfg, root_dev, &cfg.root, b".", sink)?;
    }
    Ok(())
}

fn is_sidecar(cfg: &WalkConfig, abs: &Path) -> bool {
    if !cfg.sqlite_store {
        return false;
    }
    let Some(store) = cfg.store_path.as_deref() else {
        return false;
    };
    let (Some(store_name), Some(name)) = (store.file_name(), abs.file_name()) else {
        return false;
    };
    let Some(suffix) = name.as_bytes().strip_prefix(store_name.as_bytes()) else {
        return false;
    };
    abs.parent() == store.parent() && [&b"-wal"[..], b"-shm", b"-journal"].contains(&suffix)
}

// Recursion depth equals tree depth; the producer thread's stack is sized for it (pipeline).
fn walk_dir(
    cfg: &WalkConfig,
    root_dev: u64,
    dir_abs: &Path,
    dir_rel: &[u8],
    sink: &mut Sink<'_>,
) -> anyhow::Result<()> {
    let mut names: Vec<OsString> = match std::fs::read_dir(dir_abs) {
        Ok(rd) => {
            let mut names = Vec::new();
            let mut warned = false;
            for item in rd {
                match item {
                    Ok(e) => names.push(e.file_name()),
                    Err(err) if !warned => {
                        warned = true;
                        sink(WalkEvent::Warning(format!(
                            "cannot read directory {}: {}",
                            escape(dir_rel),
                            err
                        )))?;
                    }
                    Err(_) => {}
                }
            }
            names
        }
        Err(err) => {
            sink(WalkEvent::Warning(format!(
                "cannot read directory {}: {}",
                escape(dir_rel),
                err
            )))?;
            return sink(WalkEvent::NotDescended(dir_rel.to_vec()));
        }
    };
    if cfg.sort {
        names.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    }
    for name in names {
        let abs = dir_abs.join(&name);
        if is_sidecar(cfg, &abs) {
            continue;
        }
        let name_bytes = name.into_vec();
        let rel = if dir_rel == b"." {
            name_bytes.clone()
        } else {
            let mut r = dir_rel.to_vec();
            r.push(b'/');
            r.extend_from_slice(&name_bytes);
            r
        };
        let mut entry = match Entry::from_path(&abs) {
            Ok((e, partial)) => {
                for p in &partial {
                    sink(WalkEvent::Warning(p.warning(&escape(&rel))))?;
                }
                e
            }
            Err(err) => {
                sink(WalkEvent::Warning(format!(
                    "cannot stat {}: {}",
                    escape(&rel),
                    err
                )))?;
                sink(WalkEvent::NotDescended(rel))?;
                continue;
            }
        };
        let is_store = cfg.store_path.as_deref() == Some(abs.as_path());
        if is_store {
            entry.flags.insert(Flags::STORE);
        } else if dir_rel != b"."
            && (name_bytes == SQLITE_NAME.as_bytes() || name_bytes == FJALL_NAME.as_bytes())
        {
            // Below the root only: a root-level manifest is the tree's own, merely not the
            // one in use (--manifest points elsewhere).
            sink(WalkEvent::NestedManifest(rel.clone()))?;
        }
        let is_dir = entry.kind == crate::model::Kind::Dir;
        let other_mount = is_dir && entry.dev != root_dev && !cfg.cross_mount_points;
        sink(WalkEvent::Entry(Observed {
            path: rel.clone(),
            abs: abs.clone(),
            entry,
            hash_error: None,
        }))?;
        if is_store || !is_dir {
            continue;
        }
        if other_mount {
            sink(WalkEvent::Warning(format!(
                "not crossing mount point at {}",
                escape(&rel)
            )))?;
            sink(WalkEvent::NotDescended(rel))?;
            continue;
        }
        walk_dir(cfg, root_dev, &abs, &rel, sink)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Flags, Kind};
    use std::path::Path;

    fn run(cfg: &WalkConfig) -> (Vec<Observed>, Vec<String>) {
        let (entries, warnings, _) = run_all(cfg);
        (entries, warnings)
    }

    /// Entries, warnings and not-descended paths.
    fn run_all(cfg: &WalkConfig) -> (Vec<Observed>, Vec<String>, Vec<Vec<u8>>) {
        let mut entries = Vec::new();
        let mut warnings = Vec::new();
        let mut skipped = Vec::new();
        walk(cfg, &mut |ev| {
            match ev {
                WalkEvent::Entry(o) => entries.push(o),
                WalkEvent::Warning(w) => warnings.push(w),
                WalkEvent::NestedManifest(p) => warnings.push(nested_manifest_warning(&p)),
                WalkEvent::NotDescended(p) => skipped.push(p),
            }
            Ok(())
        })
        .unwrap();
        (entries, warnings, skipped)
    }

    fn cfg(root: &Path, store: Option<PathBuf>) -> WalkConfig {
        WalkConfig {
            root: root.to_path_buf(),
            cross_mount_points: false,
            sort: true,
            sqlite_store: store
                .as_deref()
                .is_some_and(|p| !p.to_string_lossy().ends_with(".fjall")),
            store_path: store,
        }
    }

    fn paths(entries: &[Observed]) -> Vec<Vec<u8>> {
        entries.iter().map(|o| o.path.clone()).collect()
    }

    fn strs(v: &[&str]) -> Vec<Vec<u8>> {
        v.iter().map(|s| s.as_bytes().to_vec()).collect()
    }

    #[test]
    fn walk_order_is_sorted_depth_first() {
        let t = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(t.path()).unwrap();
        std::fs::create_dir(r.join("b")).unwrap();
        std::fs::create_dir(r.join("a")).unwrap();
        std::fs::write(r.join("a/z"), b"").unwrap();
        std::fs::write(r.join("a/y"), b"").unwrap();
        std::fs::write(r.join("c"), b"").unwrap();
        let (e, w) = run(&cfg(&r, None));
        assert_eq!(paths(&e), strs(&[".", "a", "a/y", "a/z", "b", "c"]));
        assert!(w.is_empty());
    }

    #[test]
    fn walk_flags_store_and_skips_sidecars() {
        let t = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(t.path()).unwrap();
        std::fs::write(r.join(".fileset.sqlite"), b"").unwrap();
        std::fs::write(r.join(".fileset.sqlite-wal"), b"").unwrap();
        std::fs::write(r.join("f"), b"").unwrap();
        let (e, w) = run(&cfg(&r, Some(r.join(".fileset.sqlite"))));
        assert_eq!(paths(&e), strs(&[".", ".fileset.sqlite", "f"]));
        assert!(e[1].entry.flags.contains(Flags::STORE));
        assert!(!e[2].entry.flags.contains(Flags::STORE));
        assert!(w.is_empty());
    }

    #[test]
    fn walk_warns_on_nested_manifest() {
        let t = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(t.path()).unwrap();
        std::fs::create_dir(r.join("sub")).unwrap();
        std::fs::write(r.join("sub/.fileset.sqlite"), b"").unwrap();
        // Directly below the root is not nested: a store elsewhere leaves it unflagged.
        std::fs::write(r.join(".fileset.fjall"), b"").unwrap();
        let (e, w) = run(&cfg(&r, Some(r.join(".fileset.sqlite"))));
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("nested manifest at sub/.fileset.sqlite"));
        let n = e.iter().find(|o| o.path == b"sub/.fileset.sqlite").unwrap();
        assert!(!n.entry.flags.contains(Flags::STORE));
    }

    #[test]
    fn walk_skips_sidecars_of_custom_named_sqlite_store() {
        let t = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(t.path()).unwrap();
        std::fs::create_dir(r.join("sub")).unwrap();
        for n in [
            "m.sqlite",
            "m.sqlite-wal",
            "m.sqlite-shm",
            "m.sqlite-journal",
            "x-wal",
        ] {
            std::fs::write(r.join("sub").join(n), b"").unwrap();
        }
        let (e, w) = run(&cfg(&r, Some(r.join("sub/m.sqlite"))));
        assert_eq!(paths(&e), strs(&[".", "sub", "sub/m.sqlite", "sub/x-wal"]));
        assert!(w.is_empty());
    }

    #[test]
    fn walk_does_not_descend_fjall_store_dir() {
        let t = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(t.path()).unwrap();
        std::fs::create_dir(r.join(".fileset.fjall")).unwrap();
        std::fs::write(r.join(".fileset.fjall/x"), b"").unwrap();
        let (e, _) = run(&cfg(&r, Some(r.join(".fileset.fjall"))));
        assert_eq!(paths(&e), strs(&[".", ".fileset.fjall"]));
        assert!(e[1].entry.flags.contains(Flags::STORE));
    }

    #[test]
    fn walk_root_symlink_is_followed_once() {
        let t = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(t.path()).unwrap();
        std::fs::create_dir(base.join("real")).unwrap();
        std::fs::write(base.join("real/f"), b"").unwrap();
        std::os::unix::fs::symlink(base.join("real"), base.join("link")).unwrap();
        let root = std::fs::canonicalize(base.join("link")).unwrap();
        let (e, _) = run(&cfg(&root, None));
        assert_eq!(e[0].path, b".");
        assert_eq!(e[0].entry.kind, Kind::Dir);
        assert!(paths(&e).contains(&b"f".to_vec()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn walk_non_utf8_name() {
        let t = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(t.path()).unwrap();
        std::fs::write(r.join(std::ffi::OsStr::from_bytes(b"bad\xff")), b"").unwrap();
        let (e, _) = run(&cfg(&r, None));
        assert!(paths(&e).contains(&b"bad\xff".to_vec()));
    }

    #[test]
    fn walk_unreadable_dir_warns_and_reports_not_descended() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(t.path()).unwrap();
        std::fs::create_dir(r.join("d")).unwrap();
        std::fs::write(r.join("d/f"), b"").unwrap();
        std::fs::write(r.join("e"), b"").unwrap();
        let mode = |m| std::fs::set_permissions(r.join("d"), std::fs::Permissions::from_mode(m));
        mode(0o000).unwrap();
        if std::fs::read_dir(r.join("d")).is_ok() {
            mode(0o755).unwrap();
            eprintln!("skipping: running with privileges that bypass directory permissions");
            return;
        }
        let (e, mut w, skipped) = run_all(&cfg(&r, None));
        mode(0o755).unwrap();
        // macOS also refuses to read the xattrs of an unreadable directory.
        if cfg!(target_os = "macos") {
            w.retain(|m| !m.starts_with("cannot read xattrs of d: "));
        }
        assert_eq!(paths(&e), strs(&[".", "d", "e"]));
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].starts_with("cannot read directory d: "), "{}", w[0]);
        assert_eq!(skipped, strs(&["d"]));
    }

    #[test]
    fn walk_symlink_not_followed() {
        let t = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(t.path()).unwrap();
        std::fs::create_dir(r.join("d")).unwrap();
        std::fs::write(r.join("d/file"), b"").unwrap();
        std::os::unix::fs::symlink(r.join("d"), r.join("s")).unwrap();
        let (e, _) = run(&cfg(&r, None));
        let p = paths(&e);
        let s = e.iter().find(|o| o.path == b"s").unwrap();
        assert_eq!(s.entry.kind, Kind::Symlink);
        assert!(p.contains(&b"d/file".to_vec()));
        assert!(!p.contains(&b"s/file".to_vec()));
    }
}
