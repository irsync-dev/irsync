//! `irsync.toml` as one file: settings, servers, clients, exports and
//! grants.
//! A `hosts.toml` or `exports.toml` beside it, once read as part of it, is
//! ignored with a warning.

use super::clients::Clients;
use super::exports::Exports;
use super::paths::Paths;
use super::reader::read;
use super::servers::Servers;
use super::settings::Settings;
use super::{read_optional, ConfigError};
use irsync_proto::config as pb;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Everything irsync is configured with, as of one read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub settings: Settings,
    pub servers: Servers,
    pub clients: Clients,
    pub exports: Exports,
}

/// Those of `hosts.toml` and `exports.toml` that exist beside `config`, in
/// that order. They were split files once; nothing reads them now.
pub fn ignored_split_files(config: &Path) -> Vec<PathBuf> {
    let dir = config.parent().unwrap_or(Path::new(""));
    ["hosts.toml", "exports.toml"]
        .into_iter()
        .map(|name| dir.join(name))
        .filter(|p| p.exists())
        .collect()
}

/// Says on stderr that a `hosts.toml` or `exports.toml` beside `config` is
/// ignored, the first time this process finds it. Every read of the config
/// calls this, for use and for editing alike. Printed, not logged, so that
/// neither `-q` nor a command that does not log hides it: the file may
/// hold the grants a server ran on.
pub fn warn_ignored_split_files(config: &Path) {
    for split in ignored_split_files(config) {
        if first_sighting(&split) {
            eprintln!(
                "irsync: warning: ignoring {}: hosts.toml and exports.toml are no longer read; move its tables into {}",
                split.display(),
                config.display()
            );
        }
    }
}

/// True the first time this process asks about `path`, false after: a
/// daemon loads the config for every connection and warns only once.
pub fn first_sighting(path: &Path) -> bool {
    static SEEN: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());
    SEEN.lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(path.to_path_buf())
}

impl Config {
    /// Parses the text of the config file, of any layout (see [`read`]).
    pub fn parse(text: &str, path: &Path) -> Result<Config, ConfigError> {
        Self::from_message(&read(text, path)?.config, path)
    }

    /// The typed view of the message: the defaults for what it leaves
    /// unset, `~` expanded, and the checks of what an edit can break
    /// (the reader has made the others). `path` is the file named in
    /// errors, which have no line.
    pub fn from_message(msg: &pb::Config, path: &Path) -> Result<Config, ConfigError> {
        Ok(Config {
            settings: Settings::from_message(msg, path)?,
            servers: Servers::from_message(&msg.servers, path)?,
            clients: Clients::from_message(&msg.clients, path)?,
            exports: Exports::from_message(msg, path)?,
        })
    }

    /// `paths.config`, or the defaults when it is missing. A `hosts.toml`
    /// or `exports.toml` beside it is not read; see
    /// [`warn_ignored_split_files`].
    pub fn load(paths: &Paths) -> Result<Config, ConfigError> {
        warn_ignored_split_files(&paths.config);
        let text = read_optional(&paths.config)?.unwrap_or_default();
        Self::parse(&text, &paths.config)
    }
}
