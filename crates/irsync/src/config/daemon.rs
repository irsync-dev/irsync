use super::{duration, expand_tilde, ConfigError};
use serde::{Deserialize, Deserializer};
use std::collections::BTreeMap;
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

impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig {
            identity: expand_tilde("~/.config/irsync/daemon_ed25519"),
            authorized_keys: expand_tilde("~/.config/irsync/authorized_keys"),
            state_dir: expand_tilde("~/.local/state/irsync"),
            discovery: Discovery::default(),
            relay: None,
            relay_mode: RelayMode::Default,
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
