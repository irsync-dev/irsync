//! Shared editing layer for the config-managing commands: loading the
//! config file into its message and saving the message back (in the
//! writer's form, comments kept), and the interpretation of a command-line
//! "who" argument.

pub mod clients;
pub mod config;
pub mod exports;
pub mod grants;
pub mod servers;
pub mod show;

use crate::config::clients::Clients;
use crate::config::file::Config;
use crate::config::reader::{self, CONFIG_VERSION};
use crate::config::servers::Servers;
use crate::config::{writer, ConfigError};
use crate::identity::{endpoint_id_of_public_key, parse_endpoint_id};
use iroh::EndpointId;
use irsync_proto::config as pb;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The first irsync release that reads `config_version` 2. If stage 2 of
/// the server/client split ships in a later release, the release bump
/// changes this to that release's version.
pub const READS_CONFIG_V2: &str = "0.1.0-beta.7";

/// Whether a daemon whose lock record says `version` (its long version,
/// whose first word is its package version) reads layout 2.
fn reads_config_v2(version: &str) -> bool {
    let Some(word) = version.split_whitespace().next() else {
        return false;
    };
    if word == env!("CARGO_PKG_VERSION") {
        return true;
    }
    match (
        semver::Version::parse(word),
        semver::Version::parse(READS_CONFIG_V2),
    ) {
        (Ok(have), Ok(need)) => have >= need,
        _ => false,
    }
}

/// Refuses to write a file of layout 2 over one that does not say so
/// (a file of layout 1, one without `config_version`, or none at all)
/// while a daemon holds the lock of the state dir of `config` and cannot
/// read layout 2: it would find a file it cannot read at its next reload.
fn check_daemon(config: &Config) -> Result<(), ManageError> {
    let state_dir = &config.settings.state_dir;
    let Some(info) = crate::daemon_lock::probe(state_dir) else {
        return Ok(());
    };
    if info.pid != 0 && reads_config_v2(&info.version) {
        return Ok(());
    }
    let version = if info.version.trim().is_empty() {
        "unknown".to_string()
    } else {
        info.version
    };
    Err(ManageError::OldDaemon {
        state_dir: state_dir.clone(),
        pid: info.pid,
        version,
    })
}

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
    #[error(
        "the irsync daemon running for {} (pid {pid}, version {version}) cannot read the file this would write (config_version 2, read from irsync {READS_CONFIG_V2} on); restart the daemon with this irsync, then run the command again",
        state_dir.display()
    )]
    OldDaemon {
        state_dir: PathBuf,
        pid: u32,
        version: String,
    },
    #[error("no such entry {0:?}")]
    NotFound(String),
    #[error("server {0:?} already exists")]
    ServerExists(String),
    #[error("client {0:?} already exists")]
    ClientExists(String),
    #[error("client {name:?} is named by grants in {}: remove those grants first, or pass --force", lists.join(", "))]
    ClientInUse { name: String, lists: Vec<String> },
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
    /// Grants name clients: a name that is only a server's names nobody.
    #[error("{0:?} is a server here, not a client; to grant that machine, add it as a client: irsync client add {0} ID")]
    NotAClient(String),
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
    #[error("{}", unknown_key_text(.0))]
    UnknownKey(String),
    #[error("{0}")]
    BadValue(String),
}

/// The message for a key `config` does not know, with a pointer for the
/// keys of the old `[hosts]` table.
fn unknown_key_text(key: &str) -> String {
    match reader::dotted_key(key) {
        Some(path) if path.first().is_some_and(|p| p == "hosts") => {
            format!("unknown key {key:?}; hosts are now servers.NAME and clients.NAME")
        }
        _ => format!("unknown key {key:?}"),
    }
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
/// its line. A file of an older layout is converted (see [`reader::read`]).
pub fn load_message(path: &Path) -> Result<pb::Config, ManageError> {
    let read = read_file(path)?;
    let mut config = read.config;
    // An old file does not say `config_version`: the reader fills in the
    // new one when it converts. Report the layout the file has.
    if read.version == 1 {
        config.config_version = Some(1);
    }
    Ok(config)
}

/// The config file at `path` as [`reader::read`] reads it; see
/// [`load_message`].
fn read_file(path: &Path) -> Result<reader::Read, ManageError> {
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
    crate::platform::saved_file_permissions(path)
}

/// Save the config file, refusing a message that would not load; takes
/// and releases the file's lock (see [`lock_config`]). Like
/// [`ConfigLock::save`], it first writes peers by their aliases.
/// The file is read under the lock first, so the save makes the same
/// check of a running daemon as one after [`ConfigLock::load`].
pub fn save_config(path: &Path, msg: &mut pb::Config) -> Result<(), ManageError> {
    let mut lock = lock_config(path)?;
    lock.load()?;
    lock.save(msg)
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
/// [`grants::canonicalize`]) with the clients of `msg` itself, so a name
/// added in the same edit already counts. True when `msg` changed. A
/// message that would not load is an error naming `path`.
pub fn canonicalize_doc(msg: &mut pb::Config, path: &Path) -> Result<bool, ManageError> {
    let config = typed(msg, path)?;
    Ok(grants::canonicalize(msg, &config.clients))
}

/// As [`canonicalize_doc`], for a message that the client `name` is
/// about to leave. When another client has the same EndpointId, `name` is
/// not counted: an id written in a grant is then written as that other
/// name and so does not stand in the way of the removal. An id that only
/// `name` names is written as `name`, and is found as a grant naming it.
pub fn canonicalize_without(
    msg: &mut pb::Config,
    path: &Path,
    name: &str,
) -> Result<bool, ManageError> {
    let mut clients = typed(msg, path)?.clients;
    let shared = clients.get(name).map(|e| e.client_id).is_some_and(|id| {
        clients
            .entries
            .iter()
            .any(|(other, e)| other != name && e.client_id == id)
    });
    if shared {
        clients.entries.remove(name);
    }
    Ok(grants::canonicalize(msg, &clients))
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
    /// The layout of the file as [`ConfigLock::load`] read it; `None`
    /// before a load.
    version: Option<u32>,
    /// Whether the file itself says `config_version` 2, so any irsync
    /// that reads it reads what a save writes. False before a load.
    says_v2: bool,
    /// What converting the file left out (see [`reader::Read::dropped`]).
    dropped: Vec<String>,
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
            version: None,
            says_v2: false,
            dropped: Vec::new(),
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
    /// Records the layout it was in, and what a conversion left out, for
    /// [`ConfigLock::save`].
    pub fn load(&mut self) -> Result<pb::Config, ManageError> {
        // An edit must not pass a split file by in silence either.
        crate::config::file::warn_ignored_split_files(&self.path);
        let read = read_file(&self.path)?;
        self.version = Some(read.version);
        // A file of layout 2 without the key reads as layout 2 too; only
        // the key keeps an older irsync from reading it.
        self.says_v2 =
            read.version == CONFIG_VERSION && read.config.config_version == Some(CONFIG_VERSION);
        self.dropped = read.dropped;
        Ok(read.config)
    }

    /// Writes `msg` over the file, in the writer's form and the current
    /// layout (`config_version` [`CONFIG_VERSION`]), and releases the
    /// lock. Peers that the clients have a name for are written by it
    /// first (see [`canonicalize_doc`]), so every save leaves the file in
    /// one form. A message that would not load is refused and the file
    /// left as it was, and so is one that a running older daemon could
    /// not read when the file did not say `config_version` 2 (see
    /// [`check_daemon`]). When the file loaded was of layout 1, each thing
    /// its conversion left out is noted on stderr once the file is saved.
    pub fn save(mut self, msg: &mut pb::Config) -> Result<(), ManageError> {
        msg.config_version = Some(CONFIG_VERSION);
        if canonicalize_doc(msg, &self.path)? {
            // The rewrite only renames and drops entries; checked all the same.
            typed(msg, &self.path)?;
        }
        if !self.says_v2 {
            check_daemon(&typed(msg, &self.path)?)?;
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
        if self.version == Some(1) {
            for note in &self.dropped {
                eprintln!("irsync: note: {note}");
            }
        }
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
    /// `who` as a client name of `clients`, a public key (a line or a
    /// `.pub` file) or an EndpointId. A name that only `servers` has is
    /// refused: grants name clients.
    pub fn parse(who: &str, clients: &Clients, servers: &Servers) -> Result<PeerSpec, ManageError> {
        if who == "all" || who == "*" {
            return Err(ManageError::NoOpenAccess);
        }
        if clients.get(who).is_some() {
            return Ok(PeerSpec::Alias(who.to_string()));
        }
        if servers.get(who).is_some() {
            return Err(ManageError::NotAClient(who.to_string()));
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

    /// What a save writes for this peer: the name, also for an id that
    /// `clients` has a name for; else the id in hex.
    pub fn written(&self, clients: &Clients) -> String {
        match self {
            PeerSpec::Id { id, .. } => clients
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
