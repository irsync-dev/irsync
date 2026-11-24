//! The `[servers]` table of `irsync.toml`: aliases for daemons to dial.

use super::names::{invalid_name, valid_alias};
use super::settings::{addresses_from, duration_from, url_from};
use super::{endpoint_id_from, file_error, ConfigError};
use crate::config::settings::{Congestion, PqKeyExchange, RelayMode};
use crate::net::addr::HostPort;
use iroh::EndpointId;
use irsync_proto::config as pb;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerEntry {
    /// The daemon's id: the id to dial.
    pub endpoint_id: EndpointId,
    pub relay: Option<Url>,
    /// `"disabled"`: reach this server directly only, never through a relay.
    pub relay_mode: Option<RelayMode>,
    /// For pushes to this server; unset: the client settings'.
    pub congestion: Option<Congestion>,
    /// For connections to this server; unset: the client settings'.
    pub pq_key_exchange: Option<PqKeyExchange>,
    pub address: Vec<HostPort>,
    pub parallel: Option<u32>,
    pub reconnect: Option<Duration>,
    pub dial_retries: Option<u32>,
    pub dht: Option<bool>,
    pub mdns: Option<bool>,
    /// The `[user@]host` that `ssh` reaches this daemon's machine by.
    pub ssh: Option<String>,
    /// Other ssh destinations that were typed for this server, such as its
    /// address: they find this entry as `ssh` does.
    pub ssh_aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Servers {
    pub path: PathBuf,
    pub entries: BTreeMap<String, ServerEntry>,
}

impl ServerEntry {
    fn from_message(s: &pb::ServerEntry) -> Result<ServerEntry, String> {
        let name = &s.name;
        if s.endpoint_id.is_empty() {
            return Err(format!("server {name:?}: needs an endpoint_id"));
        }
        let field = |e: String| format!("server {name:?}: {e}");
        Ok(ServerEntry {
            endpoint_id: endpoint_id_from(&s.endpoint_id).map_err(|e| field(e.to_string()))?,
            relay: url_from(s.relay.as_deref()).map_err(field)?,
            relay_mode: RelayMode::from_optional(s.relay_mode).map_err(field)?,
            congestion: Congestion::from_optional(s.congestion).map_err(field)?,
            pq_key_exchange: PqKeyExchange::from_optional(s.pq_key_exchange).map_err(field)?,
            address: addresses_from(&s.address).map_err(field)?,
            parallel: s.parallel,
            reconnect: duration_from(s.reconnect.as_ref()).map_err(field)?,
            dial_retries: s.dial_retries,
            dht: s.dht,
            mdns: s.mdns,
            ssh: s.ssh.clone(),
            ssh_aliases: s.ssh_aliases.iter().map(|e| e.value.clone()).collect(),
        })
    }
}

impl Servers {
    /// The servers of the message, checked as the reader checks the file:
    /// name rules, ids, and no name twice. `path` is the file named in
    /// errors (and recorded as `Servers.path`).
    pub fn from_message(servers: &[pb::ServerEntry], path: &Path) -> Result<Servers, ConfigError> {
        let mut entries = BTreeMap::new();
        for s in servers {
            if !valid_alias(&s.name) {
                return Err(file_error(path, invalid_name(&s.name)));
            }
            let entry = ServerEntry::from_message(s).map_err(|m| file_error(path, m))?;
            if entries.insert(s.name.clone(), entry).is_some() {
                return Err(file_error(
                    path,
                    format!("duplicate server name {:?}", s.name),
                ));
            }
        }
        Ok(Servers {
            path: path.to_path_buf(),
            entries,
        })
    }

    pub fn get(&self, name: &str) -> Option<&ServerEntry> {
        self.entries.get(name)
    }

    /// The name whose `ssh` value is exactly `dest`, or that lists it in
    /// `ssh_aliases`: the first in lexicographic order when several entries
    /// of one daemon have it. `Err`: the names, when entries of different
    /// daemons have it.
    pub fn by_ssh(&self, dest: &str) -> Result<Option<&str>, Vec<String>> {
        let mut found = self.entries.iter().filter(|(_, e)| {
            e.ssh.as_deref() == Some(dest) || e.ssh_aliases.iter().any(|a| a == dest)
        });
        let Some((first, entry)) = found.next() else {
            return Ok(None);
        };
        let rest: Vec<_> = found.collect();
        if rest.iter().all(|(_, e)| e.endpoint_id == entry.endpoint_id) {
            return Ok(Some(first));
        }
        let others = rest.into_iter().map(|(a, _)| a.clone());
        Err(std::iter::once(first.clone()).chain(others).collect())
    }
}
