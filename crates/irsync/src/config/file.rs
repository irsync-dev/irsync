//! `irsync.toml` as one file: settings, servers, clients, exports and
//! grants.

use super::clients::Clients;
use super::exports::Exports;
use super::paths::Paths;
use super::reader::read_values;
use super::servers::Servers;
use super::settings::Settings;
use super::{read_optional, ConfigError};
use irsync_proto::config as pb;
use std::path::Path;

/// Everything irsync is configured with, as of one read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub settings: Settings,
    pub servers: Servers,
    pub clients: Clients,
    pub exports: Exports,
}

impl Config {
    /// Parses the text of the config file. Without its comments, which
    /// the typed view has no use for: the daemon parses the file for
    /// every connection.
    pub fn parse(text: &str, path: &Path) -> Result<Config, ConfigError> {
        Self::from_message(&read_values(text, path)?, path)
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

    /// `paths.config`, or the defaults when it is missing.
    pub fn load(paths: &Paths) -> Result<Config, ConfigError> {
        let text = read_optional(&paths.config)?.unwrap_or_default();
        Self::parse(&text, &paths.config)
    }
}
