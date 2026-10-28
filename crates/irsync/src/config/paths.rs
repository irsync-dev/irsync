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

/// The config file, and the two split files that may sit beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config: PathBuf,
    /// `hosts.toml` in the config file's directory; read when it exists.
    pub hosts: PathBuf,
    /// `exports.toml` in the config file's directory; read when it exists.
    pub exports: PathBuf,
}

impl Paths {
    /// `config`, or `irsync.toml` under [`config_dir`]; the split files are
    /// its siblings.
    pub fn resolve(config: Option<&Path>) -> Paths {
        let config = config
            .map(Path::to_path_buf)
            .unwrap_or_else(|| config_dir().join("irsync.toml"));
        let dir = config.parent().map(Path::to_path_buf).unwrap_or_default();
        Paths {
            hosts: dir.join("hosts.toml"),
            exports: dir.join("exports.toml"),
            config,
        }
    }
}
