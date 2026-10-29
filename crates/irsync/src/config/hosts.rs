//! The `[hosts]` table: aliases for daemons, in `irsync.toml` or a split
//! `hosts.toml` beside it.

use super::settings::de_opt_duration;
use super::{line_of, toml_err, ConfigError};
use crate::config::settings::RelayMode;
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
    pub address: Vec<HostPort>,
    pub parallel: Option<u32>,
    pub reconnect: Option<Duration>,
    pub dht: Option<bool>,
    pub mdns: Option<bool>,
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
    address: Vec<HostPort>,
    #[serde(default)]
    parallel: Option<u32>,
    #[serde(default, deserialize_with = "de_opt_duration")]
    reconnect: Option<Duration>,
    #[serde(default)]
    dht: Option<bool>,
    #[serde(default)]
    mdns: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hosts {
    pub path: PathBuf,
    pub entries: BTreeMap<String, HostEntry>,
}

/// The `[hosts]` table as deserialized: alias -> entry, with spans.
pub(crate) type RawHosts = BTreeMap<String, Spanned<RawHost>>;

/// The typed shape of the split file: every alias lives under `[hosts]`.
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawFile {
    hosts: RawHosts,
}

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
                address: r.address,
                parallel: r.parallel,
                reconnect: r.reconnect,
                dht: r.dht,
                mdns: r.mdns,
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
    pub fn parse(text: &str, path: &Path) -> Result<Hosts, ConfigError> {
        let at = |offset: usize, message: String| ConfigError::File {
            path: path.to_path_buf(),
            line: Some(line_of(text, offset)),
            message,
        };
        // Shape pass: the only top-level key is `hosts`, a table.
        let shape: BTreeMap<String, Spanned<toml::Value>> =
            toml::from_str(text).map_err(|e| toml_err(text, path, e))?;
        // In file order, so the first offender is the one reported.
        let mut keys: Vec<_> = shape.iter().collect();
        keys.sort_by_key(|(_, v)| v.span().start);
        for (name, value) in keys {
            let start = value.span().start;
            let value = value.get_ref();
            let message = match name.as_str() {
                "hosts" if value.is_table() => continue,
                "hosts" => format!("hosts: expected a table, found {}", value.type_str()),
                _ if value.is_table() => {
                    format!("unknown table {name:?}; hosts live under [hosts.{name}]")
                }
                _ => format!("unknown key {name:?}"),
            };
            return Err(at(start, message));
        }
        // Typed pass over the same text, so the spans match.
        let raw: RawFile = toml::from_str(text).map_err(|e| toml_err(text, path, e))?;
        let entries = hosts_from_raw(raw.hosts, text, path)?;
        Ok(Hosts {
            path: path.to_path_buf(),
            entries,
        })
    }

    pub fn get(&self, alias: &str) -> Option<&HostEntry> {
        self.entries.get(alias)
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
