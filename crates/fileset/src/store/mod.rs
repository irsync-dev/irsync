//! Storage abstraction over the manifest database.

pub mod sqlite;

use crate::model::Entry;
use std::path::Path;

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
        Backend::Fjall => anyhow::bail!("fjall backend not built yet"),
    }
}
