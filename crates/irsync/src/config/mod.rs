//! Settings, host aliases and exports (TOML) configuration.

pub mod duration;
pub mod exports;
pub mod hosts;
pub mod paths;
pub mod settings;

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
    #[error("{}", match line {
        Some(l) => format!("{}:{l}: {message}", path.display()),
        None => format!("{}: {message}", path.display()),
    })]
    File {
        path: PathBuf,
        line: Option<usize>,
        message: String,
    },
    #[error("unknown host alias {alias:?} (and not an EndpointId); aliases are defined in {}", hosts.display())]
    UnknownAlias { alias: String, hosts: PathBuf },
    #[error("{0:?} is a reserved name and cannot be used as an export name")]
    ReservedName(String),
    #[error("bad peer {0:?}: expected a host alias or an EndpointId")]
    BadPeer(String),
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

/// 1-based line number of byte `offset` in `text`.
pub(crate) fn line_of(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())].matches('\n').count() + 1
}

pub(crate) fn toml_err(text: &str, path: &std::path::Path, e: toml::de::Error) -> ConfigError {
    ConfigError::File {
        path: path.to_path_buf(),
        line: e.span().map(|s| line_of(text, s.start)),
        message: e.message().to_string(),
    }
}

/// Read a config file; `None` when it does not exist.
pub(crate) fn read_optional(path: &std::path::Path) -> Result<Option<String>, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}
