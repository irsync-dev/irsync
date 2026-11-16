//! The `[hosts]` table of `irsync.toml`: aliases for daemons.

use super::settings::de_opt_duration;
use super::{line_of, ConfigError};
use crate::config::settings::{Congestion, RelayMode};
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
    pub endpoint_id: EndpointId,
    pub relay: Option<Url>,
    /// `"disabled"`: reach this host directly only, never through a relay.
    pub relay_mode: Option<RelayMode>,
    /// For pushes to this host; unset: the client settings'.
    pub congestion: Option<Congestion>,
    pub address: Vec<HostPort>,
    pub parallel: Option<u32>,
    pub reconnect: Option<Duration>,
    pub dial_retries: Option<u32>,
    pub dht: Option<bool>,
    pub mdns: Option<bool>,
    /// The `[user@]host` that `ssh` reaches this daemon's machine by.
    pub ssh: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawHost {
    endpoint_id: Spanned<String>,
    #[serde(default)]
    relay: Option<Url>,
    #[serde(default)]
    relay_mode: Option<RelayMode>,
    #[serde(default)]
    congestion: Option<Congestion>,
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
        let r = r.into_inner();
        let endpoint_id = parse_endpoint_id(r.endpoint_id.get_ref())
            .map_err(|e| at(r.endpoint_id.span().start, format!("host {alias:?}: {e}")))?;
        entries.insert(
            alias,
            HostEntry {
                endpoint_id,
                relay: r.relay,
                relay_mode: r.relay_mode,
                congestion: r.congestion,
                address: r.address,
                parallel: r.parallel,
                reconnect: r.reconnect,
                dial_retries: r.dial_retries,
                dht: r.dht,
                mdns: r.mdns,
                ssh: r.ssh,
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

impl Hosts {
    pub fn get(&self, alias: &str) -> Option<&HostEntry> {
        self.entries.get(alias)
    }

    /// The alias that stands for `id`: the first in lexicographic order
    /// when several entries have it.
    pub fn alias_of(&self, id: &EndpointId) -> Option<&str> {
        self.entries
            .iter()
            .find(|(_, e)| e.endpoint_id == *id)
            .map(|(alias, _)| alias.as_str())
    }

    /// An alias first, then a literal EndpointId.
    pub fn resolve_peer(&self, s: &str) -> Result<EndpointId, ConfigError> {
        if let Some(e) = self.get(s) {
            return Ok(e.endpoint_id);
        }
        parse_endpoint_id(s).map_err(|_| ConfigError::UnknownAlias {
            alias: s.to_string(),
            hosts: self.path.clone(),
        })
    }
}
