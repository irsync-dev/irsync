//! `hosts.toml`: aliases for daemons.

use super::settings::de_opt_duration;
use super::{read_optional, toml_err, ConfigError};
use crate::identity::parse_endpoint_id;
use crate::net::addr::HostPort;
use iroh::EndpointId;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEntry {
    pub endpoint_id: EndpointId,
    pub relay: Option<Url>,
    pub address: Vec<HostPort>,
    pub parallel: Option<u32>,
    pub reconnect: Option<Duration>,
    pub dht: Option<bool>,
    pub mdns: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHost {
    endpoint_id: String,
    #[serde(default)]
    relay: Option<Url>,
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

/// An alias is non-empty, has no `:` or `/`, and is not itself an EndpointId.
pub fn valid_alias(s: &str) -> bool {
    !s.is_empty() && !s.contains([':', '/']) && parse_endpoint_id(s).is_err()
}

impl Hosts {
    pub fn parse(text: &str, path: &Path) -> Result<Hosts, ConfigError> {
        let raw: BTreeMap<String, RawHost> =
            toml::from_str(text).map_err(|e| toml_err(text, path, e))?;
        let mut entries = BTreeMap::new();
        for (alias, r) in raw {
            let endpoint_id = parse_endpoint_id(&r.endpoint_id).map_err(|e| ConfigError::File {
                path: path.to_path_buf(),
                line: None,
                message: format!("host {alias:?}: {e}"),
            })?;
            entries.insert(
                alias,
                HostEntry {
                    endpoint_id,
                    relay: r.relay,
                    address: r.address,
                    parallel: r.parallel,
                    reconnect: r.reconnect,
                    dht: r.dht,
                    mdns: r.mdns,
                },
            );
        }
        Ok(Hosts {
            path: path.to_path_buf(),
            entries,
        })
    }

    /// A missing file yields no aliases.
    pub fn load(path: &Path) -> Result<Hosts, ConfigError> {
        match read_optional(path)? {
            Some(text) => Self::parse(&text, path),
            None => Ok(Hosts {
                path: path.to_path_buf(),
                entries: BTreeMap::new(),
            }),
        }
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
