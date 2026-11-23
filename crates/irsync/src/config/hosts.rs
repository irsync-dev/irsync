//! The `[hosts]` table of `irsync.toml`: aliases for daemons.

use super::settings::{addresses_from, duration_from, url_from};
use super::{file_error, ConfigError};
use crate::config::settings::{Congestion, PqKeyExchange, RelayMode};
use crate::identity::{parse_endpoint_id, IdentityError};
use crate::net::addr::HostPort;
use iroh::EndpointId;
use irsync_proto::config as pb;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEntry {
    /// The machine's daemon: the id to dial. `None`: the machine is known
    /// only as a client (`client_id`).
    pub endpoint_id: Option<EndpointId>,
    /// The machine as a client: the id grants naming this host are for.
    /// An entry has this, `endpoint_id`, or both.
    pub client_id: Option<EndpointId>,
    pub relay: Option<Url>,
    /// `"disabled"`: reach this host directly only, never through a relay.
    pub relay_mode: Option<RelayMode>,
    /// For pushes to this host; unset: the client settings'.
    pub congestion: Option<Congestion>,
    /// For connections to this host; unset: the client settings'.
    pub pq_key_exchange: Option<PqKeyExchange>,
    pub address: Vec<HostPort>,
    pub parallel: Option<u32>,
    pub reconnect: Option<Duration>,
    pub dial_retries: Option<u32>,
    pub dht: Option<bool>,
    pub mdns: Option<bool>,
    /// The `[user@]host` that `ssh` reaches this daemon's machine by.
    pub ssh: Option<String>,
    /// Other ssh destinations that were typed for this host, such as its
    /// address: they find this entry as `ssh` does.
    pub ssh_aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hosts {
    pub path: PathBuf,
    pub entries: BTreeMap<String, HostEntry>,
}

/// The message for an alias that [`valid_alias`] refuses.
pub(crate) fn invalid_alias(alias: &str) -> String {
    format!(
        "invalid host alias {alias:?}: must be non-empty, contain no ':' or '/', and be neither all, * nor an EndpointId"
    )
}

/// The message for a host with neither id.
pub(crate) fn needs_an_id(alias: &str) -> String {
    format!("host {alias:?}: needs an endpoint_id (its daemon), a client_id, or both")
}

/// An id of the message: 32 bytes that are a valid key.
fn endpoint_id_from(bytes: &[u8]) -> Result<EndpointId, IdentityError> {
    <[u8; 32]>::try_from(bytes)
        .ok()
        .and_then(|b| EndpointId::from_bytes(&b).ok())
        .ok_or_else(|| {
            IdentityError::BadEndpointId(bytes.iter().map(|b| format!("{b:02x}")).collect())
        })
}

impl HostEntry {
    fn from_message(h: &pb::Host) -> Result<HostEntry, String> {
        let alias = &h.name;
        let id_of = |id: &Option<Vec<u8>>| {
            id.as_deref()
                .map(|b| endpoint_id_from(b).map_err(|e| format!("host {alias:?}: {e}")))
                .transpose()
        };
        let endpoint_id = id_of(&h.endpoint_id)?;
        let client_id = id_of(&h.client_id)?;
        if endpoint_id.is_none() && client_id.is_none() {
            return Err(needs_an_id(alias));
        }
        let field = |e: String| format!("host {alias:?}: {e}");
        Ok(HostEntry {
            endpoint_id,
            client_id,
            relay: url_from(h.relay.as_deref()).map_err(field)?,
            relay_mode: RelayMode::from_optional(h.relay_mode).map_err(field)?,
            congestion: Congestion::from_optional(h.congestion).map_err(field)?,
            pq_key_exchange: PqKeyExchange::from_optional(h.pq_key_exchange).map_err(field)?,
            address: addresses_from(&h.address).map_err(field)?,
            parallel: h.parallel,
            reconnect: duration_from(h.reconnect.as_ref()).map_err(field)?,
            dial_retries: h.dial_retries,
            dht: h.dht,
            mdns: h.mdns,
            ssh: h.ssh.clone(),
            ssh_aliases: h.ssh_aliases.iter().map(|e| e.value.clone()).collect(),
        })
    }
}

/// An alias is non-empty, has no `:` or `/`, is not itself an EndpointId,
/// and is neither `all` nor `*` (the words that mean every peer).
pub fn valid_alias(s: &str) -> bool {
    !s.is_empty()
        && !s.contains([':', '/'])
        && s != "all"
        && s != "*"
        && parse_endpoint_id(s).is_err()
}

/// A machine's host name as an alias: `raw` up to its first `.`, when
/// that says something about the machine. `None` for a name that is not
/// a valid alias, for `localhost`, for one made of digits only (the start
/// of an IPv4 address), and for one with control characters.
pub fn short_host_name(raw: &str) -> Option<String> {
    let name = raw.trim();
    let name = name.split_once('.').map_or(name, |(first, _)| first).trim();
    let usable = valid_alias(name)
        && !name.eq_ignore_ascii_case("localhost")
        && !name.bytes().all(|b| b.is_ascii_digit())
        && !name.chars().any(char::is_control);
    usable.then(|| name.to_string())
}

impl HostEntry {
    /// The id this host has as a peer in a grant: its client id, else the
    /// one id it has.
    pub fn peer_id(&self) -> EndpointId {
        self.client_id
            .or(self.endpoint_id)
            .expect("a host entry has an endpoint_id or a client_id")
    }
}

impl Hosts {
    /// The hosts of the message, checked as the reader checks the file:
    /// alias rules, ids, and no alias twice. `path` is the file named in
    /// errors (and recorded as `Hosts.path`).
    pub fn from_message(hosts: &[pb::Host], path: &Path) -> Result<Hosts, ConfigError> {
        let mut entries = BTreeMap::new();
        for h in hosts {
            if !valid_alias(&h.name) {
                return Err(file_error(path, invalid_alias(&h.name)));
            }
            let entry = HostEntry::from_message(h).map_err(|m| file_error(path, m))?;
            if entries.insert(h.name.clone(), entry).is_some() {
                return Err(file_error(
                    path,
                    format!("duplicate host alias {:?}", h.name),
                ));
            }
        }
        Ok(Hosts {
            path: path.to_path_buf(),
            entries,
        })
    }

    pub fn get(&self, alias: &str) -> Option<&HostEntry> {
        self.entries.get(alias)
    }

    /// The alias that stands for the peer `id` (see
    /// [`HostEntry::peer_id`]): the first in lexicographic order when
    /// several entries have it.
    pub fn alias_of(&self, id: &EndpointId) -> Option<&str> {
        self.entries
            .iter()
            .find(|(_, e)| e.peer_id() == *id)
            .map(|(alias, _)| alias.as_str())
    }

    /// The alias whose `ssh` value is exactly `dest`, or that lists it in
    /// `ssh_aliases`: the first in lexicographic order when several entries
    /// of one daemon have it. `Err`: the aliases, when entries of
    /// different daemons have it.
    pub fn by_ssh(&self, dest: &str) -> Result<Option<&str>, Vec<String>> {
        let mut found = self.entries.iter().filter(|(_, e)| {
            e.ssh.as_deref() == Some(dest) || e.ssh_aliases.iter().any(|a| a == dest)
        });
        let Some((first, entry)) = found.next() else {
            return Ok(None);
        };
        let rest: Vec<_> = found.collect();
        // Entries without a daemon id (clients only) are never the same
        // daemon: nothing says so.
        let same = |e: &HostEntry| e.endpoint_id.is_some() && e.endpoint_id == entry.endpoint_id;
        if rest.iter().all(|(_, e)| same(e)) {
            return Ok(Some(first));
        }
        let others = rest.into_iter().map(|(a, _)| a.clone());
        Err(std::iter::once(first.clone()).chain(others).collect())
    }

    /// An alias first, then a literal EndpointId.
    pub fn resolve_peer(&self, s: &str) -> Result<EndpointId, ConfigError> {
        if let Some(e) = self.get(s) {
            return Ok(e.peer_id());
        }
        parse_endpoint_id(s).map_err(|_| ConfigError::UnknownAlias {
            alias: s.to_string(),
            hosts: self.path.clone(),
        })
    }
}
