//! The `[clients]` table of `irsync.toml`: names for the peers that
//! grants are given to.

use super::access::Access;
use super::names::{invalid_name, valid_alias};
use super::{endpoint_id_from, file_error, ConfigError};
use crate::identity::parse_endpoint_id;
use iroh::EndpointId;
use irsync_proto::config as pb;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientEntry {
    /// The id the grants naming this client are for.
    pub client_id: EndpointId,
    /// The right over every export that is not automatic, when the client has it.
    pub all_exports: Option<Access>,
    /// The right over any path no export names, when the client has it.
    pub all_paths: Option<Access>,
    /// The right to read and, with `rw`, change the server's configuration
    /// remotely, when the client has it.
    pub admin: Option<Access>,
}

/// A right a `[clients.NAME]` entry can hold. The one place that names the
/// rights: the reader, the writer, `client set` and the listings go through
/// [`RIGHTS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Right {
    AllExports,
    AllPaths,
    Admin,
}

pub const RIGHTS: [Right; 3] = [Right::AllExports, Right::AllPaths, Right::Admin];

impl Right {
    /// The key of the right in the file.
    pub fn key(self) -> &'static str {
        match self {
            Right::AllExports => "all_exports",
            Right::AllPaths => "all_paths",
            Right::Admin => "admin",
        }
    }

    /// The right of the entry; none when unset (or not a known value).
    pub fn get(self, c: &pb::ClientEntry) -> Option<Access> {
        self.field(c).and_then(Access::from_message)
    }

    /// The right of a typed entry; none when unset.
    pub fn of(self, c: &ClientEntry) -> Option<Access> {
        match self {
            Right::AllExports => c.all_exports,
            Right::AllPaths => c.all_paths,
            Right::Admin => c.admin,
        }
    }

    /// Sets or, with `None`, removes the right of the entry.
    pub fn set(self, c: &mut pb::ClientEntry, a: Option<Access>) {
        *self.field_mut(c).0 = a.map(|a| pb::Access::from(a) as i32);
    }

    /// The message's own value, whatever number it holds.
    fn field(self, c: &pb::ClientEntry) -> Option<i32> {
        match self {
            Right::AllExports => c.all_exports,
            Right::AllPaths => c.all_paths,
            Right::Admin => c.admin,
        }
    }

    /// The message's field of the right, with the entry's key comments.
    pub(crate) fn field_mut(
        self,
        c: &mut pb::ClientEntry,
    ) -> (&mut Option<i32>, &mut BTreeMap<String, pb::Comment>) {
        match self {
            Right::AllExports => (&mut c.all_exports, &mut c.key_comments),
            Right::AllPaths => (&mut c.all_paths, &mut c.key_comments),
            Right::Admin => (&mut c.admin, &mut c.key_comments),
        }
    }

    /// The message of a value that is not `"rw"` or `"ro"`.
    pub fn not_an_access(self) -> String {
        format!("{} must be \"rw\" or \"ro\"", self.key())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clients {
    pub path: PathBuf,
    pub entries: BTreeMap<String, ClientEntry>,
}

impl ClientEntry {
    fn from_message(c: &pb::ClientEntry) -> Result<ClientEntry, String> {
        let name = &c.name;
        if c.client_id.is_empty() {
            return Err(format!("client {name:?}: needs a client_id"));
        }
        let client_id =
            endpoint_id_from(&c.client_id).map_err(|e| format!("client {name:?}: {e}"))?;
        let mut entry = ClientEntry {
            client_id,
            all_exports: None,
            all_paths: None,
            admin: None,
        };
        for right in RIGHTS {
            let access = match right.field(c) {
                None => None,
                Some(n) => Some(
                    Access::from_message(n)
                        .ok_or_else(|| format!("client {name:?}: {}", right.not_an_access()))?,
                ),
            };
            match right {
                Right::AllExports => entry.all_exports = access,
                Right::AllPaths => entry.all_paths = access,
                Right::Admin => entry.admin = access,
            }
        }
        Ok(entry)
    }
}

impl Clients {
    /// The clients of the message, checked as the reader checks the file:
    /// name rules, ids, and no name twice. `path` is the file named in
    /// errors (and recorded as `Clients.path`).
    pub fn from_message(clients: &[pb::ClientEntry], path: &Path) -> Result<Clients, ConfigError> {
        let mut entries = BTreeMap::new();
        for c in clients {
            if !valid_alias(&c.name) {
                return Err(file_error(path, invalid_name(&c.name)));
            }
            let entry = ClientEntry::from_message(c).map_err(|m| file_error(path, m))?;
            if entries.insert(c.name.clone(), entry).is_some() {
                return Err(file_error(
                    path,
                    format!("duplicate client name {:?}", c.name),
                ));
            }
        }
        Ok(Clients {
            path: path.to_path_buf(),
            entries,
        })
    }

    pub fn get(&self, name: &str) -> Option<&ClientEntry> {
        self.entries.get(name)
    }

    /// The name that stands for the client `id`: the first in
    /// lexicographic order when several entries have it.
    pub fn alias_of(&self, id: &EndpointId) -> Option<&str> {
        self.entries
            .iter()
            .find(|(_, e)| e.client_id == *id)
            .map(|(name, _)| name.as_str())
    }

    /// A name first, then a literal EndpointId.
    pub fn resolve_peer(&self, s: &str) -> Result<EndpointId, ConfigError> {
        if let Some(e) = self.get(s) {
            return Ok(e.client_id);
        }
        parse_endpoint_id(s).map_err(|_| ConfigError::UnknownClient {
            name: s.to_string(),
            path: self.path.clone(),
        })
    }
}
