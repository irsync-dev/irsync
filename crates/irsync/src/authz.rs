//! Authorisation: which endpoint may access which exports.

use crate::config::clients::Clients;
use crate::config::exports::{Export, Exports};
use crate::config::file::Config;
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

    /// The word the file spells it with.
    pub fn as_str(self) -> &'static str {
        match self {
            Access::ReadWrite => "rw",
            Access::ReadOnly => "ro",
        }
    }

    /// `"rw"` or `"ro"`; anything else is not an access.
    pub fn parse(s: &str) -> Option<Access> {
        match s {
            "rw" => Some(Access::ReadWrite),
            "ro" => Some(Access::ReadOnly),
            _ => None,
        }
    }

    /// The access a message value of `pb::Access` stands for; `None` for
    /// unspecified or a number the message does not know.
    pub fn from_message(n: i32) -> Option<Access> {
        match irsync_proto::config::Access::try_from(n) {
            Ok(irsync_proto::config::Access::Rw) => Some(Access::ReadWrite),
            Ok(irsync_proto::config::Access::Ro) => Some(Access::ReadOnly),
            _ => None,
        }
    }
}

impl From<Access> for irsync_proto::config::Access {
    fn from(a: Access) -> Self {
        match a {
            Access::ReadWrite => irsync_proto::config::Access::Rw,
            Access::ReadOnly => irsync_proto::config::Access::Ro,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grants {
    pub exports: BTreeMap<String, Access>,
    /// The client's right over every export that is not automatic
    /// (`all_exports`).
    pub all_exports: Option<Access>,
    /// The client's right over any path (`all_paths`), when the daemon
    /// allows it (`allow_all_paths`).
    pub all_paths: Option<Access>,
    /// The client's right over the admin service (`admin`): `rw` reads and
    /// changes the configuration, `ro` reads it.
    pub admin: Option<Access>,
}

impl Grants {
    /// True when the grants cover nothing at all (no entries, no
    /// `all_exports`, no `all_paths`, no `admin`).
    pub fn is_empty(&self) -> bool {
        self.exports.is_empty()
            && self.all_exports.is_none()
            && self.all_paths.is_none()
            && self.admin.is_none()
    }

    /// The access this key has to `export`, named `name`: its own entry,
    /// else its `all_exports`, except that `all_exports` never reaches an
    /// automatic export; those belong to the peer they were made for.
    pub fn access_to(&self, name: &str, export: &Export) -> Option<Access> {
        match self.exports.get(name) {
            Some(a) => Some(*a),
            None if export.auto => None,
            None => self.all_exports,
        }
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

/// Who may use which export, as of one read of the config.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub exports: BTreeMap<String, Export>,
    grants: HashMap<EndpointId, Grants>,
}

/// The stronger of two rights: `rw` beats `ro`.
fn stronger(a: Access, b: Access) -> Access {
    if a == Access::ReadWrite {
        a
    } else {
        b
    }
}

/// Raises `slot` to `access`, as [`stronger`] says; nothing lowers it.
fn raise(slot: &mut Option<Access>, access: Option<Access>) {
    if let Some(a) = access {
        *slot = Some(slot.map_or(a, |s| stronger(s, a)));
    }
}

impl Snapshot {
    /// The rights of each client entry (`all_paths` only when
    /// `allow_all_paths`), and every grant entry resolved against
    /// `clients` (a client name first, then a raw EndpointId). Wherever two
    /// sources give one id a right, `rw` beats `ro`. An entry that
    /// resolves to nobody is skipped with a warning.
    pub fn from_files(clients: &Clients, exports: &Exports, allow_all_paths: bool) -> Snapshot {
        let mut grants: HashMap<EndpointId, Grants> = HashMap::new();
        for c in clients.entries.values() {
            let all_paths = c.all_paths.filter(|_| allow_all_paths);
            if c.all_exports.is_none() && all_paths.is_none() && c.admin.is_none() {
                continue;
            }
            let g = grants.entry(c.client_id).or_default();
            raise(&mut g.all_exports, c.all_exports);
            raise(&mut g.all_paths, all_paths);
            raise(&mut g.admin, c.admin);
        }
        for (name, e) in &exports.exports {
            let lists = [(&e.rw, Access::ReadWrite), (&e.ro, Access::ReadOnly)];
            for (list, access) in lists {
                for entry in list {
                    let Ok(id) = clients.resolve_peer(entry) else {
                        tracing::warn!(entry = %entry, export = %name, "grant entry matches nobody");
                        continue;
                    };
                    let g = grants.entry(id).or_default();
                    g.exports
                        .entry(name.clone())
                        .and_modify(|a| *a = stronger(*a, access))
                        .or_insert(access);
                }
            }
        }
        // An automatic export was made for a client with `all_paths` and
        // names it in its own list. The level written there does not count:
        // the client gets its `all_paths` level now, so lowering or raising
        // it takes effect at once, and removing it (or turning
        // `allow_all_paths` off) ends access through the export.
        for g in grants.values_mut() {
            let level = g.all_paths;
            g.exports.retain(|name, access| {
                if exports.exports.get(name).is_none_or(|e| !e.auto) {
                    return true;
                }
                match level {
                    Some(l) => {
                        *access = l;
                        true
                    }
                    None => false,
                }
            });
        }
        Snapshot {
            exports: exports.exports.clone(),
            grants,
        }
    }

    /// The admin right of `id`; none when it has none.
    pub fn admin(&self, id: &EndpointId) -> Option<Access> {
        self.grants.get(id).and_then(|g| g.admin)
    }

    /// The grants of `id`; empty when it is granted nothing.
    pub fn grants(&self, id: &EndpointId) -> Grants {
        self.grants.get(id).cloned().unwrap_or_default()
    }

    /// The access `id` has to the export `name`; none when there is no
    /// such export. An automatic export never comes from `all_exports`.
    pub fn access(&self, id: &EndpointId, name: &str) -> Option<Access> {
        self.grants(id).access_to(name, self.exports.get(name)?)
    }

    /// True when `id` may read (or, with `write`, write) the export `name`.
    pub fn allows(&self, id: &EndpointId, name: &str, write: bool) -> bool {
        self.access(id, name).is_some_and(|a| a.permits(write))
    }
}

pub trait Authorizer: Send + Sync {
    /// The current exports and grants. Errors fail closed.
    fn snapshot(&self) -> Result<Snapshot, AuthzError>;

    /// Makes sure an export `peer` may use contains `root` (a canonical
    /// absolute directory): when none does, adds an automatic export of
    /// `root` that lists `peer` with `access`, seen by the next
    /// [`Authorizer::snapshot`].
    fn add_auto_export(
        &self,
        root: &Path,
        peer: EndpointId,
        access: Access,
    ) -> Result<(), AuthzError>;
}

/// How long [`FileAuthorizer::add_auto_export`] waits for a config lock
/// someone else holds (another connection adding its export, or an
/// `irsync export` command).
pub const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// True when an export `peer` may use in `snap` already contains `root`.
fn covered(snap: &Snapshot, root: &Path, peer: &EndpointId) -> bool {
    crate::resolve::containing(root, &snap.exports, &snap.grants(peer), None).is_some()
}

/// Reads the config afresh on every
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
            &config.clients,
            &config.exports,
            config.settings.daemon.allow_all_paths,
        ))
    }

    /// Edits the config file under its lock, so that two connections (or
    /// an `irsync export` command) cannot lose each other's change; the
    /// check for an export that already covers `root` is made under the
    /// same lock. A lock held by someone else is waited for, up to
    /// [`LOCK_WAIT`]. Blocks: call it off the async runtime.
    fn add_auto_export(
        &self,
        root: &Path,
        peer: EndpointId,
        access: Access,
    ) -> Result<(), AuthzError> {
        use crate::manage::{exports, lock_config};
        let deadline = std::time::Instant::now() + LOCK_WAIT;
        let mut lock = loop {
            match lock_config(&self.paths.config) {
                Err(ManageError::Locked { .. }) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                r => break r?,
            }
        };
        if covered(&self.snapshot()?, root, &peer) {
            return Ok(());
        }
        let mut msg = lock.load()?;
        let name = exports::add_auto_export(&mut msg, root, peer, access)?;
        lock.save(&mut msg)?;
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
    fn add_auto_export(
        &self,
        root: &Path,
        peer: EndpointId,
        access: Access,
    ) -> Result<(), AuthzError> {
        let mut snap = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner);
        if covered(&snap, root, &peer) {
            return Ok(());
        }
        let name = crate::manage::exports::unique_auto_name(root, |n| snap.exports.contains_key(n));
        let listed = vec![peer.to_string()];
        let (rw, ro) = match access {
            Access::ReadWrite => (listed, Vec::new()),
            Access::ReadOnly => (Vec::new(), listed),
        };
        let export = Export {
            path: root.to_path_buf(),
            read_only: false,
            warm_index: false,
            fsync: false,
            auto: true,
            rw,
            ro,
        };
        snap.exports.insert(name.clone(), export);
        let grants = snap.grants.entry(peer).or_default();
        grants.exports.insert(name, access);
        Ok(())
    }
}
