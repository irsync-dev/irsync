//! `irsync.toml`: client and daemon settings.

use super::{duration, paths, read_optional, ConfigError};
use crate::net::addr::HostPort;
use serde::{Deserialize, Deserializer};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RelayMode {
    #[default]
    Default,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Discovery {
    pub dht: bool,
    pub dns: bool,
    pub mdns: bool,
}

impl Default for Discovery {
    fn default() -> Self {
        Discovery {
            dht: true,
            dns: true,
            mdns: true,
        }
    }
}

pub(super) fn de_duration<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Secs(u64),
        Text(String),
    }
    match Raw::deserialize(d)? {
        Raw::Secs(n) => Ok(Duration::from_secs(n)),
        Raw::Text(s) => duration::parse(&s).map_err(serde::de::Error::custom),
    }
}

pub(super) fn de_listen<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<HostPort>, D::Error> {
    let listen = Vec::<HostPort>::deserialize(d)?;
    let (mut v4, mut v6) = (0, 0);
    for entry in &listen {
        match entry {
            HostPort::Ip(a) if a.is_ipv4() => v4 += 1,
            HostPort::Ip(_) => v6 += 1,
            HostPort::Name { .. } => {}
        }
    }
    if v4 > 1 || v6 > 1 {
        return Err(serde::de::Error::custom(
            "at most one IPv4 and one IPv6 listen address",
        ));
    }
    Ok(listen)
}

/// Resolves `listen` to the sockets to bind: each entry contributes the
/// first IPv4 and the first IPv6 address it yields; two entries landing in
/// one family is an error naming both.
pub async fn listen_sockets(listen: &[HostPort]) -> anyhow::Result<Vec<SocketAddr>> {
    use anyhow::{anyhow, Context};
    let mut out: Vec<(SocketAddr, &HostPort)> = Vec::new();
    for entry in listen {
        let resolved = entry
            .resolve()
            .await
            .with_context(|| format!("resolving listen address {entry}"))?;
        if resolved.is_empty() {
            return Err(anyhow!("listen address {entry} resolved to no addresses"));
        }
        let mut taken = [false, false];
        for addr in resolved {
            let fam = usize::from(addr.is_ipv6());
            if taken[fam] {
                continue;
            }
            taken[fam] = true;
            if let Some((_, prev)) = out.iter().find(|(a, _)| a.is_ipv6() == addr.is_ipv6()) {
                let family = if addr.is_ipv6() { "IPv6" } else { "IPv4" };
                return Err(anyhow!(
                    "listen: {entry} and {prev} are both {family}; at most one per family"
                ));
            }
            out.push((addr, entry));
        }
    }
    Ok(out.into_iter().map(|(a, _)| a).collect())
}

pub(super) fn de_opt_duration<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Duration>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Secs(u64),
        Text(String),
    }
    match Option::<Raw>::deserialize(d)? {
        None => Ok(None),
        Some(Raw::Secs(n)) => Ok(Some(Duration::from_secs(n))),
        Some(Raw::Text(s)) => duration::parse(&s)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClientSettings {
    pub parallel: Option<u32>,
    #[serde(deserialize_with = "de_opt_duration")]
    pub reconnect: Option<Duration>,
    pub dht: Option<bool>,
    pub mdns: Option<bool>,
    pub relay: Option<Url>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonSettings {
    pub identity: PathBuf,
    #[serde(deserialize_with = "de_duration")]
    pub idle_timeout: Duration,
    #[serde(deserialize_with = "de_duration")]
    pub dormant_session_timeout: Duration,
    #[serde(deserialize_with = "de_listen")]
    pub listen: Vec<HostPort>,
    pub portmap: bool,
    pub relay: Option<Url>,
    pub relay_mode: RelayMode,
    pub discovery: Discovery,
}

impl Default for DaemonSettings {
    fn default() -> Self {
        DaemonSettings {
            identity: paths::config_dir().join("daemon_ed25519"),
            idle_timeout: crate::session::engine::DAEMON_IDLE_TIMEOUT,
            dormant_session_timeout: Duration::from_secs(7 * 86400),
            listen: Vec::new(),
            portmap: true,
            relay: None,
            relay_mode: RelayMode::Default,
            discovery: Discovery::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub identity: PathBuf,
    pub state_dir: PathBuf,
    pub client: ClientSettings,
    pub daemon: DaemonSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            identity: paths::config_dir().join("id_ed25519"),
            state_dir: paths::state_dir(),
            client: ClientSettings::default(),
            daemon: DaemonSettings::default(),
        }
    }
}

impl Settings {
    /// The settings of a merged document (see [`super::file::Config`]).
    pub fn parse(text: &str, path: &Path) -> Result<Settings, ConfigError> {
        super::file::Config::parse(text, path).map(|c| c.settings)
    }

    /// A missing file yields the defaults.
    pub fn load(path: &Path) -> Result<Settings, ConfigError> {
        match read_optional(path)? {
            Some(text) => Self::parse(&text, path),
            None => Ok(Settings::default()),
        }
    }
}
