//! Storage abstraction over the manifest database.

pub mod fjall;
pub mod sqlite;

use crate::model::Entry;
use std::path::{Path, PathBuf};

pub const SQLITE_NAME: &str = ".fileset.sqlite";
pub const FJALL_NAME: &str = ".fileset.fjall";
pub const SQLITE_SIDECARS: [&str; 3] =
    [".fileset.sqlite-wal", ".fileset.sqlite-shm", ".fileset.sqlite-journal"];
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
}

pub trait Store: Send {
    fn meta(&self) -> anyhow::Result<Meta>;
    fn get(&self, path: &[u8]) -> anyhow::Result<Option<Entry>>;
    /// Visits every entry in byte-wise key order.
    fn for_each(&self, f: &mut dyn FnMut(&[u8], &Entry) -> anyhow::Result<()>) -> anyhow::Result<()>;
    /// Atomic upsert of all items (xattrs replaced wholesale).
    fn put_batch(&mut self, items: &[(Vec<u8>, Entry)]) -> anyhow::Result<()>;
    /// Deletes every entry whose generation != keep. Returns count deleted.
    fn sweep(&mut self, keep: u64) -> anyhow::Result<u64>;
    fn set_meta(&mut self, meta: &Meta) -> anyhow::Result<()>;
    /// Flush, checkpoint, release. Must be called at the end of a successful update.
    fn close(self: Box<Self>) -> anyhow::Result<()>;
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
/// `root` must already be canonical; `requested` is the `--store` value (update only).
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
            return Ok(Location { backend, path: m.to_path_buf(), exists: true });
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
            let (backend, path) =
                if has_s { (Backend::Sqlite, sqlite) } else { (Backend::Fjall, fjall) };
            if requested.is_some_and(|r| r != backend) {
                anyhow::bail!(
                    "{} already exists, which conflicts with the requested --store backend",
                    path.display()
                );
            }
            Ok(Location { backend, path, exists: true })
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
            Ok(Location { backend, path, exists: false })
        }
    }
}

/// Absolute manifest path if it lies inside `root` (so the walker can flag it), else None.
pub fn store_path_inside(root: &Path, loc: &Location) -> Option<PathBuf> {
    let abs = match (loc.path.parent(), loc.path.file_name()) {
        (Some(parent), Some(name)) => {
            let parent = if parent.as_os_str().is_empty() { Path::new(".") } else { parent };
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
        assert!(err.contains(".fileset.sqlite") && err.contains(".fileset.fjall"), "{err}");
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
        let err = locate(&root, None, Some(Backend::Fjall), true).unwrap_err().to_string();
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
    fn locate_manifest_missing_check_errors() {
        let d = tempfile::tempdir().unwrap();
        let root = canon(&d);
        let m = root.join("nope");
        let err = locate(&root, Some(&m), None, false).unwrap_err().to_string();
        assert!(err.contains("no manifest at"), "{err}");
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
