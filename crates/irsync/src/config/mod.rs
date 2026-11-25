//! Settings, servers, clients and exports (TOML) configuration.

pub mod clients;
mod comments;
pub mod duration;
pub mod exports;
pub mod file;
pub mod names;
pub mod paths;
pub mod reader;
pub mod servers;
pub mod settings;
pub mod writer;

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
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
    /// The config file is open to other users, or another user owns it,
    /// and it does not set `unsafe_config_allowed`.
    #[error("{}: {problem}; run: chmod 600 {} (or set unsafe_config_allowed = true in it)", path.display(), path.display())]
    TooOpen { path: PathBuf, problem: String },
    #[error("unknown client {name:?} (and not an EndpointId); clients are defined in {}", path.display())]
    UnknownClient { name: String, path: PathBuf },
    #[error("bad peer {0:?}: expected a client name or an EndpointId")]
    BadPeer(String),
}

/// An id of the message: 32 bytes that are a valid key.
pub(crate) fn endpoint_id_from(
    bytes: &[u8],
) -> Result<iroh::EndpointId, crate::identity::IdentityError> {
    <[u8; 32]>::try_from(bytes)
        .ok()
        .and_then(|b| iroh::EndpointId::from_bytes(&b).ok())
        .ok_or_else(|| {
            crate::identity::IdentityError::BadEndpointId(
                bytes.iter().map(|b| format!("{b:02x}")).collect(),
            )
        })
}

/// Expand a leading `~` or `~/` using the user's home directory.
pub(crate) fn expand_tilde(p: &str) -> PathBuf {
    let home = || paths::home_dir().unwrap_or_else(|| PathBuf::from("."));
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

/// An error in a config that has no line to point at, such as one a
/// command built.
pub(crate) fn file_error(path: &std::path::Path, message: String) -> ConfigError {
    ConfigError::File {
        path: path.to_path_buf(),
        line: None,
        message,
    }
}

/// True when the text of a config file sets `unsafe_config_allowed = true`
/// at its top level. Text that is not TOML sets nothing.
pub(crate) fn unsafe_allowed(text: &str) -> bool {
    toml::from_str::<toml::Table>(text)
        .ok()
        .and_then(|t| t.get("unsafe_config_allowed")?.as_bool())
        == Some(true)
}

/// Read a config file; `None` when it does not exist. A file that other
/// users can read or change, or that another user owns, is refused before
/// anything else in it counts, unless it sets `unsafe_config_allowed`.
pub(crate) fn read_optional(path: &std::path::Path) -> Result<Option<String>, ConfigError> {
    match crate::fs::private::read_with_problem(path) {
        Ok((text, Some(problem))) if !unsafe_allowed(&text) => Err(ConfigError::TooOpen {
            path: path.to_path_buf(),
            problem,
        }),
        Ok((t, _)) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}
