//! Storage abstraction over the manifest database.

pub mod fjall;
pub mod sqlite;

use crate::model::Entry;
use std::path::{Path, PathBuf};

pub const SQLITE_NAME: &str = ".fileset.sqlite";
pub const FJALL_NAME: &str = ".fileset.fjall";
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Backend {
    Sqlite,
    Fjall,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpenMode {
    Create,
    ReadWrite,
    ReadOnly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    pub schema_version: u32,
    pub generation: u64,
    pub tool_version: String,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds.
    pub updated_at: i64,
    /// Being created by an `init` that has not finished; such a manifest holds nothing
    /// worth keeping. Absent in manifests written before this flag existed.
    pub unfinished: bool,
}

pub trait Store: Send {
    fn meta(&self) -> anyhow::Result<Meta>;
    fn get(&self, path: &[u8]) -> anyhow::Result<Option<Entry>>;
    /// Visits every entry in byte-wise key order.
    fn for_each(
        &self,
        f: &mut dyn FnMut(&[u8], &Entry) -> anyhow::Result<()>,
    ) -> anyhow::Result<()>;
    /// Atomic upsert of all items (xattrs replaced wholesale).
    fn put_batch(&mut self, items: &[(Vec<u8>, Entry)]) -> anyhow::Result<()>;
    /// Deletes every entry whose generation != keep. Returns count deleted.
    fn sweep(&mut self, keep: u64) -> anyhow::Result<u64>;
    fn set_meta(&mut self, meta: &Meta) -> anyhow::Result<()>;
    /// Flush, checkpoint, release. Must be called at the end of a successful update.
    /// Returns a note for the user when the release was incomplete but harmless.
    fn close(self: Box<Self>) -> anyhow::Result<Option<String>>;
}

/// Applies the schema-version policy shared by all backends.
pub(crate) fn check_schema(meta: &Meta, mode: OpenMode) -> anyhow::Result<()> {
    if meta.schema_version > SCHEMA_VERSION {
        anyhow::bail!(
            "manifest schema version {} is newer than this tool supports ({}); upgrade fileset",
            meta.schema_version,
            SCHEMA_VERSION
        );
    }
    if mode == OpenMode::ReadOnly && meta.schema_version < SCHEMA_VERSION {
        anyhow::bail!(
            "manifest schema version {} is older than {}; run 'fileset update' to migrate it",
            meta.schema_version,
            SCHEMA_VERSION
        );
    }
    Ok(())
}

pub fn open(path: &Path, backend: Backend, mode: OpenMode) -> anyhow::Result<Box<dyn Store>> {
    match backend {
        Backend::Sqlite => Ok(Box::new(sqlite::SqliteStore::open(path, mode)?)),
        Backend::Fjall => Ok(Box::new(fjall::FjallStore::open(path, mode)?)),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub backend: Backend,
    pub path: PathBuf,
    pub exists: bool,
}

/// Decides where the manifest lives and which backend it uses, without opening it.
/// `root` must already be canonical; `requested` is the `--store` value (init only);
/// `for_update` is true for init and update, which may create the manifest.
pub fn locate(
    root: &Path,
    manifest: Option<&Path>,
    requested: Option<Backend>,
    for_update: bool,
) -> anyhow::Result<Location> {
    if let Some(m) = manifest {
        // Existence is checked without following a final symlink (so a dangling link
        // still counts as present); the kind is read through it.
        if m.symlink_metadata().is_ok() {
            let md = std::fs::metadata(m)
                .map_err(|e| anyhow::anyhow!("cannot stat manifest {}: {e}", m.display()))?;
            let backend = if md.is_dir() {
                Backend::Fjall
            } else if md.is_file() {
                Backend::Sqlite
            } else {
                anyhow::bail!("manifest {} is neither a file nor a directory", m.display());
            };
            if requested.is_some_and(|r| r != backend) {
                anyhow::bail!(
                    "{} already exists, which conflicts with the requested --store backend",
                    m.display()
                );
            }
            return Ok(Location {
                backend,
                path: m.to_path_buf(),
                exists: true,
            });
        }
        if !for_update {
            anyhow::bail!("no manifest at {}", m.display());
        }
        return Ok(Location {
            backend: requested.unwrap_or(Backend::Sqlite),
            path: m.to_path_buf(),
            exists: false,
        });
    }
    let sqlite = root.join(SQLITE_NAME);
    let fjall = root.join(FJALL_NAME);
    let has_s = sqlite.symlink_metadata().is_ok();
    let has_f = fjall.symlink_metadata().is_ok();
    match (has_s, has_f) {
        (true, true) => anyhow::bail!(
            "both {SQLITE_NAME} and {FJALL_NAME} exist in {}; use --manifest to choose one",
            root.display()
        ),
        (true, false) | (false, true) => {
            let (backend, path) = if has_s {
                (Backend::Sqlite, sqlite)
            } else {
                (Backend::Fjall, fjall)
            };
            if requested.is_some_and(|r| r != backend) {
                anyhow::bail!(
                    "{} already exists, which conflicts with the requested --store backend",
                    path.display()
                );
            }
            Ok(Location {
                backend,
                path,
                exists: true,
            })
        }
        (false, false) => {
            if !for_update {
                anyhow::bail!(
                    "no manifest found in {}: neither {SQLITE_NAME} nor {FJALL_NAME} exists",
                    root.display()
                );
            }
            let backend = requested.unwrap_or(Backend::Sqlite);
            let path = match backend {
                Backend::Sqlite => sqlite,
                Backend::Fjall => fjall,
            };
            Ok(Location {
                backend,
                path,
                exists: false,
            })
        }
    }
}

/// The nearest `.fileset.sqlite` or `.fileset.fjall` in a proper ancestor of `root`, if
/// any: `root` would then be inside an existing fileset. `root` must be canonical so that
/// every ancestor is a real directory. Existence is checked without following symlinks.
pub fn enclosing_manifest(root: &Path) -> Option<PathBuf> {
    root.ancestors().skip(1).find_map(|dir| {
        [SQLITE_NAME, FJALL_NAME]
            .into_iter()
            .map(|name| dir.join(name))
            .find(|p| p.symlink_metadata().is_ok())
    })
}

/// Absolute manifest path if it lies inside `root` (so the walker can flag it), else None.
pub fn store_path_inside(root: &Path, loc: &Location) -> Option<PathBuf> {
    let abs = match (loc.path.parent(), loc.path.file_name()) {
        (Some(parent), Some(name)) => {
            let parent = if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            };
            match parent.canonicalize() {
                Ok(p) => p.join(name),
                Err(_) => loc.path.clone(),
            }
        }
        _ => loc.path.clone(),
    };
    abs.starts_with(root).then_some(abs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canon(d: &tempfile::TempDir) -> std::path::PathBuf {
        d.path().canonicalize().unwrap()
    }

    #[test]
    fn locate_defaults_to_sqlite_for_fresh_update() {
        let d = tempfile::tempdir().unwrap();
        let root = canon(&d);
        let loc = locate(&root, None, None, true).unwrap();
        assert_eq!(loc.backend, Backend::Sqlite);
        assert_eq!(loc.path, root.join(SQLITE_NAME));
        assert!(!loc.exists);
    }

    #[test]
    fn locate_check_without_manifest_errors() {
        let d = tempfile::tempdir().unwrap();
        let root = canon(&d);
        let err = locate(&root, None, None, false).unwrap_err().to_string();
        assert!(
            err.contains(".fileset.sqlite") && err.contains(".fileset.fjall"),
            "{err}"
        );
    }

    #[test]
    fn locate_errors_when_both_exist() {
        let d = tempfile::tempdir().unwrap();
        let root = canon(&d);
        std::fs::write(root.join(SQLITE_NAME), b"").unwrap();
        std::fs::create_dir(root.join(FJALL_NAME)).unwrap();
        let err = locate(&root, None, None, true).unwrap_err().to_string();
        assert!(err.contains("both"), "{err}");
    }

    #[test]
    fn locate_rejects_store_flag_mismatch() {
        let d = tempfile::tempdir().unwrap();
        let root = canon(&d);
        std::fs::write(root.join(SQLITE_NAME), b"").unwrap();
        let err = locate(&root, None, Some(Backend::Fjall), true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("already exists"), "{err}");
    }

    #[test]
    fn locate_manifest_dir_is_fjall() {
        let d = tempfile::tempdir().unwrap();
        let root = canon(&d);
        let m = root.join("m");
        std::fs::create_dir(&m).unwrap();
        let loc = locate(&root, Some(&m), None, false).unwrap();
        assert_eq!(loc.backend, Backend::Fjall);
        assert!(loc.exists);
    }

    #[test]
    fn locate_manifest_rejects_store_flag_mismatch() {
        let d = tempfile::tempdir().unwrap();
        let root = canon(&d);
        let m = root.join("m");
        std::fs::create_dir(&m).unwrap();
        let err = locate(&root, Some(&m), Some(Backend::Sqlite), true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("already exists"), "{err}");
    }

    #[test]
    fn locate_manifest_missing_check_errors() {
        let d = tempfile::tempdir().unwrap();
        let root = canon(&d);
        let m = root.join("nope");
        let err = locate(&root, Some(&m), None, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no manifest at"), "{err}");
    }

    #[test]
    fn enclosing_manifest_finds_nearest_ancestor_only() {
        let d = tempfile::tempdir().unwrap();
        let root = canon(&d);
        let deeper = root.join("sub").join("deeper");
        std::fs::create_dir_all(&deeper).unwrap();
        assert_eq!(enclosing_manifest(&deeper), None);
        std::fs::create_dir(root.join(FJALL_NAME)).unwrap();
        assert_eq!(enclosing_manifest(&deeper), Some(root.join(FJALL_NAME)));
        assert_eq!(
            enclosing_manifest(&root.join("sub")),
            Some(root.join(FJALL_NAME))
        );
        // The directory's own manifest is not "enclosing".
        assert_eq!(enclosing_manifest(&root), None);
        // The nearest one wins.
        std::fs::write(root.join("sub").join(SQLITE_NAME), b"").unwrap();
        assert_eq!(
            enclosing_manifest(&deeper),
            Some(root.join("sub").join(SQLITE_NAME))
        );
    }

    #[test]
    fn locate_manifest_file_is_sqlite() {
        let d = tempfile::tempdir().unwrap();
        let root = canon(&d);
        let m = root.join("m");
        std::fs::write(&m, b"").unwrap();
        let loc = locate(&root, Some(&m), None, false).unwrap();
        assert_eq!(
            loc,
            Location {
                backend: Backend::Sqlite,
                path: m,
                exists: true
            }
        );
    }

    #[test]
    fn locate_manifest_through_a_symlink_reads_the_kind_behind_it() {
        let d = tempfile::tempdir().unwrap();
        let root = canon(&d);
        std::fs::write(root.join("file"), b"").unwrap();
        std::fs::create_dir(root.join("dir")).unwrap();
        std::os::unix::fs::symlink(root.join("file"), root.join("to_file")).unwrap();
        std::os::unix::fs::symlink(root.join("dir"), root.join("to_dir")).unwrap();
        std::os::unix::fs::symlink(root.join("gone"), root.join("dangling")).unwrap();
        let backend = |name: &str| locate(&root, Some(&root.join(name)), None, true);
        assert_eq!(backend("to_file").unwrap().backend, Backend::Sqlite);
        assert_eq!(backend("to_dir").unwrap().backend, Backend::Fjall);
        // The link is there, so it is not "missing"; what it points at cannot be read.
        let err = backend("dangling").unwrap_err().to_string();
        assert!(err.starts_with("cannot stat manifest "), "{err}");
    }

    #[test]
    fn locate_manifest_that_is_neither_file_nor_dir_errors() {
        let d = tempfile::tempdir().unwrap();
        let root = canon(&d);
        let m = root.join("p");
        assert!(std::process::Command::new("mkfifo")
            .arg(&m)
            .status()
            .unwrap()
            .success());
        let err = locate(&root, Some(&m), None, true).unwrap_err().to_string();
        assert!(err.contains("neither a file nor a directory"), "{err}");
    }

    #[test]
    fn locate_missing_manifest_for_update_takes_the_requested_backend() {
        let d = tempfile::tempdir().unwrap();
        let root = canon(&d);
        let m = root.join("new");
        let loc = locate(&root, Some(&m), None, true).unwrap();
        assert_eq!((loc.backend, loc.exists), (Backend::Sqlite, false));
        assert_eq!(loc.path, m);
        let loc = locate(&root, Some(&m), Some(Backend::Fjall), true).unwrap();
        assert_eq!((loc.backend, loc.exists), (Backend::Fjall, false));
        assert!(!m.exists(), "locate must not create anything");
    }

    #[test]
    fn store_path_inside_finds_a_manifest_below_the_root() {
        let d = tempfile::tempdir().unwrap();
        let root = canon(&d);
        std::fs::create_dir(root.join("sub")).unwrap();
        let m = root.join("sub/m.sqlite");
        let loc = locate(&root, Some(&m), None, true).unwrap();
        assert_eq!(store_path_inside(&root, &loc), Some(m));
    }

    #[test]
    fn store_path_inside_does_not_take_a_sibling_name_prefix_for_the_root() {
        let d = tempfile::tempdir().unwrap();
        let base = canon(&d);
        let root = base.join("a");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(base.join("ab")).unwrap();
        let m = base.join("ab/m.sqlite");
        let loc = locate(&root, Some(&m), None, true).unwrap();
        assert_eq!(store_path_inside(&root, &loc), None);
    }

    #[test]
    fn store_path_inside_resolves_a_relative_manifest_against_the_cwd() {
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
        let m = Path::new("relative-manifest.sqlite");
        let loc = locate(&cwd, Some(m), None, true).unwrap();
        assert_eq!(store_path_inside(&cwd, &loc), Some(cwd.join(m)));
        let d = tempfile::tempdir().unwrap();
        let elsewhere = canon(&d);
        assert_eq!(store_path_inside(&elsewhere, &loc), None);
    }

    #[test]
    fn store_path_inside_detects_outside() {
        let d = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let root = canon(&d);
        let loc = locate(&root, None, None, true).unwrap();
        assert_eq!(store_path_inside(&root, &loc), Some(root.join(SQLITE_NAME)));
        let m = canon(&other).join("x.sqlite");
        let loc = locate(&root, Some(&m), None, true).unwrap();
        assert_eq!(store_path_inside(&root, &loc), None);
    }
}
