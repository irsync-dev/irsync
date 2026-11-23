//! The `[hosts]` table of `irsync.toml`: aliases for daemons.

use super::settings::de_opt_duration;
use super::{line_of, ConfigError};
use crate::config::settings::{Congestion, PqKeyExchange, RelayMode};
use crate::identity::parse_endpoint_id;
use crate::net::addr::HostPort;
use iroh::EndpointId;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use toml::Spanned;
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawHost {
    #[serde(default)]
    endpoint_id: Option<Spanned<String>>,
    #[serde(default)]
    client_id: Option<Spanned<String>>,
    #[serde(default)]
    relay: Option<Url>,
    #[serde(default)]
    relay_mode: Option<RelayMode>,
    #[serde(default)]
    congestion: Option<Congestion>,
    #[serde(default)]
    pq_key_exchange: Option<PqKeyExchange>,
    #[serde(default)]
    address: Vec<HostPort>,
    #[serde(default)]
    parallel: Option<u32>,
    #[serde(default, deserialize_with = "de_opt_duration")]
    reconnect: Option<Duration>,
    #[serde(default)]
    dial_retries: Option<u32>,
    #[serde(default)]
    dht: Option<bool>,
    #[serde(default)]
    mdns: Option<bool>,
    #[serde(default)]
    ssh: Option<String>,
    #[serde(default)]
    ssh_aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hosts {
    pub path: PathBuf,
    pub entries: BTreeMap<String, HostEntry>,
}

/// The `[hosts]` table as deserialized: alias -> entry, with spans.
pub(crate) type RawHosts = BTreeMap<String, Spanned<RawHost>>;

/// Validates the `[hosts]` table: alias rules and endpoint ids. `text` is
/// the document the spans point into, `path` the file named in errors.
pub(crate) fn hosts_from_raw(
    raw: RawHosts,
    text: &str,
    path: &Path,
) -> Result<BTreeMap<String, HostEntry>, ConfigError> {
    let at = |offset: usize, message: String| ConfigError::File {
        path: path.to_path_buf(),
        line: Some(line_of(text, offset)),
        message,
    };
    let mut entries = BTreeMap::new();
    for (alias, r) in raw {
        if !valid_alias(&alias) {
            return Err(at(
                r.span().start,
                format!(
                    "invalid host alias {alias:?}: must be non-empty, contain no ':' or '/', and be neither all, * nor an EndpointId"
                ),
            ));
        }
        let start = r.span().start;
        let r = r.into_inner();
        let id_of = |key: &Option<Spanned<String>>| {
            key.as_ref()
                .map(|s| {
                    parse_endpoint_id(s.get_ref())
                        .map_err(|e| at(s.span().start, format!("host {alias:?}: {e}")))
                })
                .transpose()
        };
        let endpoint_id = id_of(&r.endpoint_id)?;
        let client_id = id_of(&r.client_id)?;
        if endpoint_id.is_none() && client_id.is_none() {
            return Err(at(
                start,
                format!("host {alias:?}: needs an endpoint_id (its daemon), a client_id, or both"),
            ));
        }
        entries.insert(
            alias,
            HostEntry {
                endpoint_id,
                client_id,
                relay: r.relay,
                relay_mode: r.relay_mode,
                congestion: r.congestion,
                pq_key_exchange: r.pq_key_exchange,
                address: r.address,
                parallel: r.parallel,
                reconnect: r.reconnect,
                dial_retries: r.dial_retries,
                dht: r.dht,
                mdns: r.mdns,
                ssh: r.ssh,
                ssh_aliases: r.ssh_aliases,
            },
        );
    }
    Ok(entries)
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
