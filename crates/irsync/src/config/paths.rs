//! Default locations of the config and state files.

use std::path::{Path, PathBuf};

/// `~/.config/irsync` (home from `dirs::home_dir()`, `.` if unknown).
pub fn config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/irsync")
}

pub fn state_dir() -> PathBuf {
    dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("irsync")
}

/// The three config file locations; each defaults under [`config_dir`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config: PathBuf,
    pub hosts: PathBuf,
    pub exports: PathBuf,
}

impl Paths {
    pub fn resolve(config: Option<&Path>, hosts: Option<&Path>, exports: Option<&Path>) -> Paths {
        let dir = config_dir();
        let pick = |o: Option<&Path>, name: &str| {
            o.map(Path::to_path_buf).unwrap_or_else(|| dir.join(name))
        };
        Paths {
            config: pick(config, "irsync.toml"),
            hosts: pick(hosts, "hosts.toml"),
            exports: pick(exports, "exports.toml"),
        }
    }
}
