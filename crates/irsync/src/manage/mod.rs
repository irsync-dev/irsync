//! Shared editing layer for the config-managing commands: loading the
//! config file into its message and saving the message back (in the
//! writer's form, comments kept), and the interpretation of a command-line
//! "who" argument.

pub mod config;
pub mod exports;
pub mod grants;
pub mod hosts;
pub mod show;

use crate::config::file::Config;
use crate::config::hosts::Hosts;
use crate::config::{reader, writer, ConfigError};
use crate::identity::{endpoint_id_of_public_key, parse_endpoint_id};
use iroh::EndpointId;
use irsync_proto::config as pb;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ManageError {
    #[error("no home directory to expand ~")]
    NoHome,
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("export {0:?} already exists")]
    Exists(String),
    #[error("no such entry {0:?}")]
    NotFound(String),
    #[error("host {0:?} already exists")]
    HostExists(String),
    #[error("host {alias:?} is named by grants in {}: remove those grants first, or pass --force", lists.join(", "))]
    HostInUse { alias: String, lists: Vec<String> },
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
    /// `reason` names the file and line itself; it is no error `source`, so
    /// a printed chain (`{:#}`) does not repeat it.
    #[error("refusing to save, the edited file would not load: {reason}")]
    Invalid { path: PathBuf, reason: ConfigError },
    #[error("{} is being edited by another irsync ({} exists); retry, or delete that file if no irsync is running", path.display(), lock.display())]
    Locked { path: PathBuf, lock: PathBuf },
    #[error("invalid key {0:?}")]
    BadKey(String),
    #[error("unknown key {0:?}")]
    UnknownKey(String),
    #[error("{0}")]
    BadValue(String),
}

/// A comment kept in a config file must be one line of printable text.
pub fn check_comment(c: &str) -> Result<(), ManageError> {
    if c.chars().any(char::is_control) {
        return Err(ManageError::BadComment(c.to_string()));
    }
    Ok(())
}

/// An endpoint id as the message holds it: its 32 bytes.
pub fn id_bytes(id: EndpointId) -> Vec<u8> {
    id.as_bytes().to_vec()
}

/// The id of 32 bytes; `None` for bytes that are no id.
pub fn id_of(bytes: &[u8]) -> Option<EndpointId> {
    let b = <[u8; 32]>::try_from(bytes).ok()?;
    EndpointId::from_bytes(&b).ok()
}

/// Load the config file for editing; a missing file is the empty message.
/// A file open to other users is refused as it is for use (see
/// [`Config::load`]), and a file the reader refuses is that error, with
/// its line.
pub fn load_message(path: &Path) -> Result<pb::Config, ManageError> {
    let text = match crate::config::read_optional(path) {
        Ok(t) => t.unwrap_or_default(),
        Err(ConfigError::Io { path, source }) => return Err(ManageError::Io { path, source }),
        Err(e) => return Err(e.into()),
    };
    Ok(reader::read(&text, path)?)
}

/// The mode a saved config gets: the mode the file at `path` has, so a
/// save never opens up a file its owner closed; 0600 for a new file, since
/// it names exports, peers and grants.
fn saved_mode(path: &Path) -> std::fs::Permissions {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path).map_or(0o600, |m| m.permissions().mode() & 0o777);
    std::fs::Permissions::from_mode(mode)
}

/// Save the config file, refusing a message that would not load; takes
/// and releases the file's lock (see [`lock_config`]). Like
/// [`ConfigLock::save`], it first writes peers by their aliases.
pub fn save_config(path: &Path, msg: &mut pb::Config) -> Result<(), ManageError> {
    lock_config(path)?.save(msg)
}

/// The typed config of `msg`; one that would not load is an error naming
/// `path`.
fn typed(msg: &pb::Config, path: &Path) -> Result<Config, ManageError> {
    Config::from_message(msg, path).map_err(|reason| ManageError::Invalid {
        path: path.to_path_buf(),
        reason,
    })
}

/// Puts the grants of `msg` into their canonical form (see
/// [`grants::canonicalize`]) with the hosts of `msg` itself, so an alias
/// added in the same edit already counts. True when `msg` changed. A
/// message that would not load is an error naming `path`.
pub fn canonicalize_doc(msg: &mut pb::Config, path: &Path) -> Result<bool, ManageError> {
    let config = typed(msg, path)?;
    Ok(grants::canonicalize(msg, &config.hosts))
}

/// As [`canonicalize_doc`], for a message that the host `alias` is about
/// to leave. When another alias has the same EndpointId, `alias` is not
/// counted: an id written in a grant is then written as that other alias
/// and so does not stand in the way of the removal. An id that only
/// `alias` names is written as `alias`, and is found as a grant naming it.
pub fn canonicalize_without(
    msg: &mut pb::Config,
    path: &Path,
    alias: &str,
) -> Result<bool, ManageError> {
    let mut hosts = typed(msg, path)?.hosts;
    let shared = hosts.get(alias).map(|e| e.peer_id()).is_some_and(|id| {
        hosts
            .entries
            .iter()
            .any(|(other, e)| other != alias && e.peer_id() == id)
    });
    if shared {
        hosts.entries.remove(alias);
    }
    Ok(grants::canonicalize(msg, &hosts))
}

/// Exclusive right to edit a config file, held from [`ConfigLock::load`] to
/// [`ConfigLock::save`] so that two edits cannot lose each other's change.
/// The lock is `<file>.lock` beside it, created exclusively, like git's: a
/// second editor is refused while it exists, and a crash leaves it behind
/// for the user to remove. The new content is written into it and renamed
/// over the file, so a save is atomic and releases the lock in one step.
#[derive(Debug)]
pub struct ConfigLock {
    path: PathBuf,
    lock: PathBuf,
    file: Option<std::fs::File>,
}

/// Takes the lock of `path`, creating its directory if needed.
pub fn lock_config(path: &Path) -> Result<ConfigLock, ManageError> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(parent).map_err(|source| ManageError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    let lock = parent.join(name);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
    {
        Ok(file) => Ok(ConfigLock {
            path: path.to_path_buf(),
            lock,
            file: Some(file),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(ManageError::Locked {
            path: path.to_path_buf(),
            lock,
        }),
        Err(source) => Err(ManageError::Io { path: lock, source }),
    }
}

impl ConfigLock {
    /// The file as it is now (empty when missing); see [`load_message`].
    pub fn load(&self) -> Result<pb::Config, ManageError> {
        // An edit must not pass a split file by in silence either.
        crate::config::file::warn_ignored_split_files(&self.path);
        load_message(&self.path)
    }

    /// Writes `msg` over the file, in the writer's form, and releases the
    /// lock. Peers that the hosts have an alias for are written by it
    /// first (see [`canonicalize_doc`]), so every save leaves the file in
    /// one form. A message that would not load is refused and the file
    /// left as it was.
    pub fn save(mut self, msg: &mut pb::Config) -> Result<(), ManageError> {
        if canonicalize_doc(msg, &self.path)? {
            // The rewrite only renames and drops entries; checked all the same.
            typed(msg, &self.path)?;
        }
        let io = |source| ManageError::Io {
            path: self.lock.clone(),
            source,
        };
        let file = self.file.as_mut().expect("held until saved");
        file.write_all(writer::write(msg).as_bytes()).map_err(io)?;
        file.sync_all().map_err(io)?;
        std::fs::set_permissions(&self.lock, saved_mode(&self.path)).map_err(io)?;
        std::fs::rename(&self.lock, &self.path).map_err(io)?;
        // Renamed away: nothing to remove on drop.
        self.file = None;
        Ok(())
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        if self.file.take().is_some() {
            let _ = std::fs::remove_file(&self.lock);
        }
    }
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
        // A comment is bytes. One from a pasted line is always UTF-8, since
        // the line was text; the error is there in case a key ever reaches
        // this from somewhere else.
        let comment = pk
            .comment()
            .as_str()
            .map_err(|_| ManageError::BadComment(pk.comment().to_string()))?;
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

    /// What a save writes for this peer: the alias, also for an id that
    /// `hosts` has an alias for; else the id in hex.
    pub fn written(&self, hosts: &Hosts) -> String {
        match self {
            PeerSpec::Id { id, .. } => hosts
                .alias_of(id)
                .map_or_else(|| id.to_string(), str::to_string),
            PeerSpec::Alias(a) => a.clone(),
        }
    }

    /// The text stored in a config entry: the alias, or the id in hex.
    pub fn entry(&self) -> String {
        match self {
            PeerSpec::Alias(a) => a.clone(),
            PeerSpec::Id { id, .. } => id.to_string(),
        }
    }
}
