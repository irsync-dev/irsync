//! Authorisation: which endpoint may access which exports.

use crate::config::exports::{Export, Exports};
use crate::config::file::Config;
use crate::config::hosts::Hosts;
use crate::config::paths::Paths;
use crate::config::ConfigError;
use crate::manage::ManageError;
use iroh::EndpointId;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Mutex, PoisonError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    ReadOnly,
    ReadWrite,
}

impl Access {
    pub fn permits(self, write: bool) -> bool {
        !write || self == Access::ReadWrite
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grants {
    pub exports: BTreeMap<String, Access>,
    /// Access granted by the `[all]` table, to every export.
    pub all: Option<Access>,
    /// Read-write access to any path: in `[immediate].rw` and the daemon
    /// has `immediate` on.
    pub immediate: bool,
}

impl Grants {
    /// True when the grants cover nothing at all (no entries, no `[all]`,
    /// no immediate grant).
    pub fn is_empty(&self) -> bool {
        self.exports.is_empty() && self.all.is_none() && !self.immediate
    }

    /// The access this key has to `export`: its own entry, else what
    /// `[all]` grants it, else none.
    pub fn access(&self, export: &str) -> Option<Access> {
        self.exports.get(export).copied().or(self.all)
    }

    pub fn allows(&self, export: &str, write: bool) -> bool {
        self.access(export).is_some_and(|a| a.permits(write))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthzError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// Another irsync is editing the config file right now.
    #[error("cannot add an automatic export, the config is locked: {0}")]
    Locked(ManageError),
    #[error("cannot add an automatic export: {0}")]
    Manage(ManageError),
}

impl From<ManageError> for AuthzError {
    fn from(e: ManageError) -> Self {
        match e {
            ManageError::Locked { .. } => AuthzError::Locked(e),
            e => AuthzError::Manage(e),
        }
    }
}

/// Who may use which export, as of one read of the config and any split
/// file beside it.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub exports: BTreeMap<String, Export>,
    /// The file the exports were read from: the config, or a split file.
    pub exports_file: std::path::PathBuf,
    grants: HashMap<EndpointId, Grants>,
}

impl Snapshot {
    /// Resolves every grant entry (an alias first, then a raw EndpointId).
    /// An entry that resolves to nobody is skipped with a warning. Within one
    /// list `rw` is applied before `ro`, so `ro` never downgrades a key.
    pub fn from_files(hosts: &Hosts, exports: &Exports, immediate_enabled: bool) -> Snapshot {
        let mut grants: HashMap<EndpointId, Grants> = HashMap::new();
        let mut apply = |export: Option<&str>, rw: &[String], ro: &[String]| {
            let lists = [(rw, Access::ReadWrite), (ro, Access::ReadOnly)];
            for (list, access) in lists {
                for entry in list {
                    let Ok(id) = hosts.resolve_peer(entry) else {
                        tracing::warn!(
                            entry = %entry,
                            export = export.unwrap_or("all"),
                            "grant entry matches nobody"
                        );
                        continue;
                    };
                    let g = grants.entry(id).or_default();
                    let slot = match export {
                        Some(name) => g.exports.entry(name.to_string()).or_insert(access),
                        None => g.all.get_or_insert(access),
                    };
                    if access == Access::ReadWrite {
                        *slot = Access::ReadWrite;
                    }
                }
            }
        };
        apply(None, &exports.rw, &exports.ro);
        for (name, e) in &exports.exports {
            apply(Some(name), &e.rw, &e.ro);
        }
        if immediate_enabled {
            for entry in &exports.immediate_rw {
                match hosts.resolve_peer(entry) {
                    Ok(id) => grants.entry(id).or_default().immediate = true,
                    Err(_) => {
                        tracing::warn!(entry = %entry, "immediate grant entry matches nobody")
                    }
                }
            }
        }
        Snapshot {
            exports: exports.exports.clone(),
            exports_file: exports.path.clone(),
            grants,
        }
    }

    /// The grants of `id`; empty when it is granted nothing.
    pub fn grants(&self, id: &EndpointId) -> Grants {
        self.grants.get(id).cloned().unwrap_or_default()
    }
}

pub trait Authorizer: Send + Sync {
    /// The current exports and grants. Errors fail closed.
    fn snapshot(&self) -> Result<Snapshot, AuthzError>;

    /// Makes sure an export `peer` may use contains `root` (a canonical
    /// absolute directory): when none does, adds an automatic export of
    /// `root`, read-write for `peer`, seen by the next
    /// [`Authorizer::snapshot`].
    fn add_auto_export(&self, root: &Path, peer: EndpointId) -> Result<(), AuthzError>;
}

/// True when an export `peer` may use in `snap` already contains `root`.
fn covered(snap: &Snapshot, root: &Path, peer: &EndpointId) -> bool {
    crate::resolve::containing(root, &snap.exports, &snap.grants(peer), None).is_some()
}

/// Reads the config (and any split file beside it) afresh on every
/// [`Authorizer::snapshot`].
pub struct FileAuthorizer {
    paths: Paths,
}

impl FileAuthorizer {
    pub fn new(paths: Paths) -> Self {
        Self { paths }
    }
}

impl Authorizer for FileAuthorizer {
    fn snapshot(&self) -> Result<Snapshot, AuthzError> {
        let config = Config::load(&self.paths)?;
        Ok(Snapshot::from_files(
            &config.hosts,
            &config.exports,
            config.settings.daemon.immediate,
        ))
    }

    /// Edits the config file under its lock, so that two connections (or
    /// an `irsync export` command) cannot lose each other's change; the
    /// check for an export that already covers `root` is made under the
    /// same lock.
    fn add_auto_export(&self, root: &Path, peer: EndpointId) -> Result<(), AuthzError> {
        use crate::manage::{ensure_editable, exports, lock_config, Table};
        ensure_editable(&self.paths, Table::Exports)?;
        let lock = lock_config(&self.paths.config)?;
        if covered(&self.snapshot()?, root, &peer) {
            return Ok(());
        }
        let mut doc = lock.load()?;
        let name = exports::add_auto_export(&mut doc, root, peer)?;
        lock.save(&doc)?;
        tracing::info!(
            export = %name,
            path = %root.display(),
            peer = %peer.fmt_short(),
            "added automatic export"
        );
        Ok(())
    }
}

/// A set of exports and grants held in memory, for tests; automatic
/// exports are added to it.
pub struct MemoryAuthorizer {
    snapshot: Mutex<Snapshot>,
}

impl MemoryAuthorizer {
    pub fn single(id: EndpointId, grants: Grants) -> Self {
        Self {
            snapshot: Mutex::new(Snapshot {
                grants: HashMap::from([(id, grants)]),
                ..Snapshot::default()
            }),
        }
    }

    fn inner(&mut self) -> &mut Snapshot {
        self.snapshot
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds (or replaces) the grants of one more key.
    pub fn with(mut self, id: EndpointId, grants: Grants) -> Self {
        self.inner().grants.insert(id, grants);
        self
    }

    /// Replaces the exports served.
    pub fn with_exports(mut self, exports: BTreeMap<String, Export>) -> Self {
        self.inner().exports = exports;
        self
    }
}

impl Authorizer for MemoryAuthorizer {
    fn snapshot(&self) -> Result<Snapshot, AuthzError> {
        Ok(self
            .snapshot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone())
    }

    /// Named as the config file would name it.
    fn add_auto_export(&self, root: &Path, peer: EndpointId) -> Result<(), AuthzError> {
        let mut snap = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner);
        if covered(&snap, root, &peer) {
            return Ok(());
        }
        let base = crate::manage::exports::auto_name(root);
        let mut name = base.clone();
        let mut n = 2;
        while snap.exports.contains_key(&name) {
            name = format!("{base}-{n}");
            n += 1;
        }
        let export = Export {
            path: root.to_path_buf(),
            read_only: false,
            warm_index: false,
            fsync: false,
            auto: true,
            rw: vec![peer.to_string()],
            ro: Vec::new(),
        };
        snap.exports.insert(name.clone(), export);
        let grants = snap.grants.entry(peer).or_default();
        grants.exports.insert(name, Access::ReadWrite);
        Ok(())
    }
}
