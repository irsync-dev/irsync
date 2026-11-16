//! Default locations of the config and state files.

use std::path::{Path, PathBuf};

/// `~/.config/irsync` (home from `dirs::home_dir()`, `.` if unknown).
pub fn config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/irsync")
}

/// `~/.ssh/id_ed25519`, the key ssh itself tries first; `None` without a home.
pub fn ssh_identity() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".ssh/id_ed25519"))
}

/// `$XDG_STATE_HOME/irsync` when that is an absolute path, else
/// `~/.local/state/irsync`, on every platform (`dirs::state_dir()` honours
/// the variable only on Linux).
pub fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("irsync")
}

/// The config file: the one file irsync is configured with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config: PathBuf,
}

impl Paths {
    /// `config`, or `irsync.toml` under [`config_dir`].
    pub fn resolve(config: Option<&Path>) -> Paths {
        let config = config
            .map(Path::to_path_buf)
            .unwrap_or_else(|| config_dir().join("irsync.toml"));
        Paths { config }
    }
}
