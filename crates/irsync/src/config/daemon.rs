use super::settings::{de_duration, de_listen};
pub use super::settings::{listen_sockets, Discovery, RelayMode};
use super::{expand_tilde, ConfigError};
use crate::net::addr::HostPort;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Export {
    pub path: PathBuf,
    #[serde(default)]
    pub read_only: bool,
    /// Hash the export's files into its index when the daemon starts, so
    /// files placed by something other than irsync are reused from the
    /// first session on (see [`crate::warm`]).
    #[serde(default)]
    pub warm_index: bool,
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
    pub exports: BTreeMap<String, Export>,
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
            exports: BTreeMap::new(),
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
        for m in cfg.exports.values_mut() {
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
