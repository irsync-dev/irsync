//! Authorisation: which endpoint may access which modules.

use crate::identity::endpoint_id_of_public_key;
use iroh::EndpointId;
use ssh_key::authorized_keys::{ConfigOpts, Entry};
use ssh_key::PublicKey;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    ReadOnly,
    ReadWrite,
}

impl Access {
    fn permits(self, write: bool) -> bool {
        !write || self == Access::ReadWrite
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grants {
    pub modules: BTreeMap<String, Access>,
    /// Access granted by the `*` wildcard.
    pub all: Option<Access>,
}

impl Grants {
    /// True when the grants cover no module at all (no entries and no `*`).
    pub fn is_empty(&self) -> bool {
        self.modules.is_empty() && self.all.is_none()
    }

    pub fn allows(&self, module: &str, write: bool) -> bool {
        self.modules
            .get(module)
            .copied()
            .or(self.all)
            .is_some_and(|a| a.permits(write))
    }

    fn parse(spec: &str) -> Result<Self, AuthzError> {
        let bad = || AuthzError::BadModules(spec.to_string());
        let mut grants = Grants::default();
        for item in spec.split(',') {
            let (name, mode) = item.rsplit_once(':').ok_or_else(bad)?;
            let (name, mode) = (name.trim(), mode.trim());
            if name.is_empty() {
                return Err(bad());
            }
            let access = match mode {
                "ro" => Access::ReadOnly,
                "rw" => Access::ReadWrite,
                _ => return Err(bad()),
            };
            if name == "*" {
                grants.all = Some(access);
            } else {
                grants.modules.insert(name.to_string(), access);
            }
        }
        Ok(grants)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthzError {
    #[error("authorized_keys error: {0}")]
    Ssh(#[from] ssh_key::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid modules entry: {0:?}")]
    BadModules(String),
}

pub trait Authorizer: Send + Sync {
    fn grants(&self, id: &EndpointId) -> Option<Grants>;
}

pub struct FileAuthorizer {
    map: HashMap<EndpointId, Grants>,
}

impl FileAuthorizer {
    pub fn load(path: &Path) -> Result<Self, AuthzError> {
        let mut map = HashMap::new();
        let text = std::fs::read_to_string(path)?;
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (opts, public_key) = parse_line(line)?;
            let Some(id) = endpoint_id_of_public_key(&public_key) else {
                tracing::warn!(
                    algorithm = %public_key.algorithm(),
                    "skipping non-Ed25519 key in authorized_keys"
                );
                continue;
            };
            let grants = match modules_option(&opts) {
                Some(spec) => Grants::parse(&spec)?,
                None => Grants::default(),
            };
            if map.contains_key(&id) {
                tracing::warn!(%id, "duplicate key in authorized_keys; first entry wins");
                continue;
            }
            map.insert(id, grants);
        }
        Ok(Self { map })
    }
}

/// Parse one authorized_keys line into its option string and key.
///
/// `ssh_key::authorized_keys::Entry` mis-reads `options type base64` (no
/// comment) as a bare key, so fall back to splitting off the options.
fn parse_line(line: &str) -> Result<(ConfigOpts, PublicKey), AuthzError> {
    if let Ok(entry) = line.parse::<Entry>() {
        return Ok((entry.config_opts().clone(), entry.public_key().clone()));
    }
    let (opts, key) = line.split_once(' ').ok_or(ssh_key::Error::FormatEncoding)?;
    let opts = ConfigOpts::new(opts)?;
    Ok((opts, PublicKey::from_openssh(key)?))
}

/// Extract the `modules="..."` value from an OpenSSH option string, warning
/// about every other option because irsync does not enforce them.
fn modules_option(opts: &ConfigOpts) -> Option<String> {
    let mut modules = None;
    let mut ignored = Vec::new();
    for opt in opts.iter() {
        match opt.strip_prefix("modules=") {
            Some(v) => modules = Some(v.trim_matches('"').to_string()),
            None => ignored.push(opt),
        }
    }
    if !ignored.is_empty() {
        tracing::warn!(
            options = %ignored.join(","),
            "authorized_keys options other than `modules` are not enforced"
        );
    }
    modules
}

impl Authorizer for FileAuthorizer {
    fn grants(&self, id: &EndpointId) -> Option<Grants> {
        self.map.get(id).cloned()
    }
}

pub struct MemoryAuthorizer {
    map: HashMap<EndpointId, Grants>,
}

impl MemoryAuthorizer {
    pub fn single(id: EndpointId, grants: Grants) -> Self {
        Self {
            map: HashMap::from([(id, grants)]),
        }
    }

    /// Adds (or replaces) the grants of one more key.
    pub fn with(mut self, id: EndpointId, grants: Grants) -> Self {
        self.map.insert(id, grants);
        self
    }
}

impl Authorizer for MemoryAuthorizer {
    fn grants(&self, id: &EndpointId) -> Option<Grants> {
        self.map.get(id).cloned()
    }
}
