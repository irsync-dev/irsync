//! The `[clients]` table of `irsync.toml`: names for the peers that
//! grants are given to.

use super::hosts::endpoint_id_from;
use super::names::{invalid_name, valid_alias};
use super::{file_error, ConfigError};
use crate::identity::parse_endpoint_id;
use iroh::EndpointId;
use irsync_proto::config as pb;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientEntry {
    /// The id the grants naming this client are for.
    pub client_id: EndpointId,
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
        Ok(ClientEntry { client_id })
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
