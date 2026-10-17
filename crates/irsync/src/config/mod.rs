//! Client (ssh_config-style) and daemon (TOML) configuration.

pub mod client;
pub mod daemon;
pub mod duration;

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("line {line}: unknown keyword {keyword:?}")]
    UnknownKeyword { line: usize, keyword: String },
    #[error("line {line}: bad value for {keyword}: {message}")]
    BadValue {
        line: usize,
        keyword: String,
        message: String,
    },
    #[error("bad duration {0:?}: expected <number>[s|m|h|d]")]
    BadDuration(String),
    #[error("daemon config: {0}")]
    Toml(#[from] toml::de::Error),
}

/// Expand a leading `~` or `~/` using the user's home directory.
pub(crate) fn expand_tilde(p: &str) -> PathBuf {
    let home = || dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    if p == "~" {
        home()
    } else if let Some(rest) = p.strip_prefix("~/") {
        home().join(rest)
    } else {
        PathBuf::from(p)
    }
}
