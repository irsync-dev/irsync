//! Shared editing layer for the config-managing commands: format-preserving
//! TOML load/save and the interpretation of a command-line "who" argument.

pub mod exports;
pub mod grants;
pub mod hosts;
pub mod show;

use crate::config::exports::Exports;
use crate::config::hosts::Hosts;
use crate::config::ConfigError;
use crate::identity::{endpoint_id_of_public_key, parse_endpoint_id};
use iroh::EndpointId;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ManageError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("{}: {source}", path.display())]
    Toml {
        path: PathBuf,
        source: toml_edit::TomlError,
    },
    #[error("export {0:?} already exists")]
    Exists(String),
    #[error("no such entry {0:?}")]
    NotFound(String),
    #[error("{0:?} is not a table; fix the file by hand")]
    NotATable(String),
    #[error("{0:?} is not a valid export name")]
    BadName(String),
    #[error("{}", crate::config::exports::reserved(.0))]
    Reserved(String),
    #[error(
        "{0:?} is not a valid alias: must not contain ':' or '/' and must be neither all, * nor an EndpointId"
    )]
    BadAlias(String),
    #[error("no open access: a peer must be named (an alias, an EndpointId, a public key line or a .pub file)")]
    NoOpenAccess,
    #[error("{}: not a directory", .0.display())]
    NotADirectory(PathBuf),
    #[error("nothing to remove for {0:?}")]
    NothingToRemove(String),
    #[error("comment {0:?} contains control characters (such as a line break); retype the key line on one line")]
    BadComment(String),
    #[error("{}: refusing to save, the edited file would not load: {source}", path.display())]
    Invalid { path: PathBuf, source: ConfigError },
}

/// A comment kept in a config file must be one line of printable text.
pub fn check_comment(c: &str) -> Result<(), ManageError> {
    if c.chars().any(char::is_control) {
        return Err(ManageError::BadComment(c.to_string()));
    }
    Ok(())
}

/// Load a TOML file for editing; a missing file yields an empty document.
pub fn load_doc(path: &Path) -> Result<toml_edit::DocumentMut, ManageError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(ManageError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    text.parse::<toml_edit::DocumentMut>()
        .map_err(|source| ManageError::Toml {
            path: path.to_path_buf(),
            source,
        })
}

/// Atomically write `doc` to `path` (temp file in the same directory, then
/// rename), creating the parent directory and using mode 0644.
pub fn save_doc(path: &Path, doc: &toml_edit::DocumentMut) -> Result<(), ManageError> {
    let io = |source| ManageError::Io {
        path: path.to_path_buf(),
        source,
    };
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(parent).map_err(io)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(io)?;
    tmp.write_all(doc.to_string().as_bytes()).map_err(io)?;
    tmp.as_file().sync_all().map_err(io)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o644)).map_err(io)?;
    }
    tmp.persist(path).map_err(|e| ManageError::Io {
        path: path.to_path_buf(),
        source: e.error,
    })?;
    Ok(())
}

/// Save `exports.toml`, refusing a document that would not load.
pub fn save_exports(path: &Path, doc: &toml_edit::DocumentMut) -> Result<(), ManageError> {
    Exports::parse(&doc.to_string(), path).map_err(|source| ManageError::Invalid {
        path: path.to_path_buf(),
        source,
    })?;
    save_doc(path, doc)
}

/// Save `hosts.toml`, refusing a document that would not load.
pub fn save_hosts(path: &Path, doc: &toml_edit::DocumentMut) -> Result<(), ManageError> {
    Hosts::parse(&doc.to_string(), path).map_err(|source| ManageError::Invalid {
        path: path.to_path_buf(),
        source,
    })?;
    save_doc(path, doc)
}

/// How a command-line "who" argument is understood.
#[derive(Debug, Clone)]
pub enum PeerSpec {
    Alias(String),
    Id {
        id: EndpointId,
        comment: Option<String>,
    },
}

impl PeerSpec {
    pub fn parse(who: &str, hosts: &Hosts) -> Result<PeerSpec, ManageError> {
        if who == "all" || who == "*" {
            return Err(ManageError::NoOpenAccess);
        }
        if hosts.get(who).is_some() {
            return Ok(PeerSpec::Alias(who.to_string()));
        }
        let bad = || ManageError::Config(ConfigError::BadPeer(who.to_string()));
        if let Ok(text) = std::fs::read_to_string(who) {
            if let Some(line) = text.lines().map(str::trim).find(|l| !l.is_empty()) {
                if let Ok(pk) = ssh_key::PublicKey::from_openssh(line) {
                    return Self::from_key(&pk)?.ok_or_else(bad);
                }
            }
        }
        if who.starts_with("ssh-") {
            let pk = ssh_key::PublicKey::from_openssh(who).map_err(|_| bad())?;
            return Self::from_key(&pk)?.ok_or_else(bad);
        }
        if let Ok(id) = parse_endpoint_id(who) {
            return Ok(PeerSpec::Id { id, comment: None });
        }
        Err(bad())
    }

    /// `None` when the key is not Ed25519; an error when its comment is
    /// not one line of printable text.
    fn from_key(pk: &ssh_key::PublicKey) -> Result<Option<PeerSpec>, ManageError> {
        let comment = pk.comment();
        check_comment(comment)?;
        let Some(id) = endpoint_id_of_public_key(pk) else {
            return Ok(None);
        };
        let comment = if comment.is_empty() {
            None
        } else {
            Some(comment.to_string())
        };
        Ok(Some(PeerSpec::Id { id, comment }))
    }

    /// The text stored in a config entry: the alias, or the id in hex.
    pub fn entry(&self) -> String {
        match self {
            PeerSpec::Alias(a) => a.clone(),
            PeerSpec::Id { id, .. } => id.to_string(),
        }
    }
}
