use super::{duration, expand_tilde, ConfigError};
use crate::net::addr::HostPort;
use serde::{Deserialize, Deserializer};
use std::collections::BTreeMap;
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Module {
    pub path: PathBuf,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonConfig {
    pub identity: PathBuf,
    pub authorized_keys: PathBuf,
    pub state_dir: PathBuf,
    pub discovery: Discovery,
    pub relay: Option<Url>,
    pub relay_mode: RelayMode,
    /// Sockets to bind instead of iroh's defaults: at most one IPv4 and one
    /// IPv6 entry; port 0 lets the daemon pick. Empty keeps the defaults.
    #[serde(deserialize_with = "de_listen")]
    pub listen: Vec<HostPort>,
    /// iroh's UPnP/PCP/NAT-PMP port mapper.
    pub portmap: bool,
    #[serde(deserialize_with = "de_duration")]
    pub dormant_session_timeout: Duration,
    /// Drop a session's connection after this long without activity (wire
    /// traffic or local progress); the client resumes. Zero disables.
    #[serde(deserialize_with = "de_duration")]
    pub idle_timeout: Duration,
    pub modules: BTreeMap<String, Module>,
}

fn de_duration<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
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

fn de_listen<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<HostPort>, D::Error> {
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

impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig {
            identity: expand_tilde("~/.config/irsync/daemon_ed25519"),
            authorized_keys: expand_tilde("~/.config/irsync/authorized_keys"),
            state_dir: expand_tilde("~/.local/state/irsync"),
            discovery: Discovery::default(),
            relay: None,
            relay_mode: RelayMode::Default,
            listen: Vec::new(),
            portmap: true,
            dormant_session_timeout: Duration::from_secs(7 * 86400),
            idle_timeout: crate::session::engine::DAEMON_IDLE_TIMEOUT,
            modules: BTreeMap::new(),
        }
    }
}

impl DaemonConfig {
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let mut cfg: DaemonConfig = toml::from_str(text)?;
        for p in [
            &mut cfg.identity,
            &mut cfg.authorized_keys,
            &mut cfg.state_dir,
        ] {
            *p = expand_tilde(&p.to_string_lossy());
        }
        for m in cfg.modules.values_mut() {
            m.path = expand_tilde(&m.path.to_string_lossy());
        }
        Ok(cfg)
    }

    /// Order: explicit path (must exist), `/etc/irsync/irsyncd.toml`,
    /// `~/.config/irsync/irsyncd.toml`, else defaults.
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        let candidate = match path {
            Some(p) => Some(p.to_path_buf()),
            None => std::iter::once(PathBuf::from("/etc/irsync/irsyncd.toml"))
                .chain(dirs::home_dir().map(|h| h.join(".config/irsync/irsyncd.toml")))
                .find(|p| p.exists()),
        };
        let Some(p) = candidate else {
            return Self::parse("");
        };
        let text = std::fs::read_to_string(&p).map_err(|source| ConfigError::Io {
            path: p.clone(),
            source,
        })?;
        Self::parse(&text)
    }
}
