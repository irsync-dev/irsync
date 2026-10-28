//! Authorisation: which endpoint may access which exports.

use crate::config::exports::{Export, Exports};
use crate::config::hosts::Hosts;
use crate::config::ConfigError;
use iroh::EndpointId;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

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
}

impl Grants {
    /// True when the grants cover no export at all (no entries and no `[all]`).
    pub fn is_empty(&self) -> bool {
        self.exports.is_empty() && self.all.is_none()
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
}

/// Who may use which export, as of one read of `hosts.toml` and `exports.toml`.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub exports: BTreeMap<String, Export>,
    grants: HashMap<EndpointId, Grants>,
}

impl Snapshot {
    /// Resolves every grant entry (an alias first, then a raw EndpointId).
    /// An entry that resolves to nobody is skipped with a warning. Within one
    /// list `rw` is applied before `ro`, so `ro` never downgrades a key.
    pub fn from_files(hosts: &Hosts, exports: &Exports) -> Snapshot {
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
        Snapshot {
            exports: exports.exports.clone(),
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
}

/// Reads `hosts.toml` and `exports.toml` afresh on every [`Authorizer::snapshot`].
pub struct FileAuthorizer {
    hosts: PathBuf,
    exports: PathBuf,
}

impl FileAuthorizer {
    pub fn new(hosts: PathBuf, exports: PathBuf) -> Self {
        Self { hosts, exports }
    }
}

impl Authorizer for FileAuthorizer {
    fn snapshot(&self) -> Result<Snapshot, AuthzError> {
        let hosts = Hosts::load(&self.hosts)?;
        let exports = Exports::load(&self.exports)?;
        Ok(Snapshot::from_files(&hosts, &exports))
    }
}

/// A fixed set of exports and grants, for tests.
pub struct MemoryAuthorizer {
    snapshot: Snapshot,
}

impl MemoryAuthorizer {
    pub fn single(id: EndpointId, grants: Grants) -> Self {
        Self {
            snapshot: Snapshot {
                exports: BTreeMap::new(),
                grants: HashMap::from([(id, grants)]),
            },
        }
    }

    /// Adds (or replaces) the grants of one more key.
    pub fn with(mut self, id: EndpointId, grants: Grants) -> Self {
        self.snapshot.grants.insert(id, grants);
        self
    }

    /// Replaces the exports served.
    pub fn with_exports(mut self, exports: BTreeMap<String, Export>) -> Self {
        self.snapshot.exports = exports;
        self
    }
}

impl Authorizer for MemoryAuthorizer {
    fn snapshot(&self) -> Result<Snapshot, AuthzError> {
        Ok(self.snapshot.clone())
    }
}
