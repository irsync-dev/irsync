//! `irsync.toml` as one file: settings, host aliases, exports and grants,
//! with `hosts.toml` and `exports.toml` still read when they sit beside it.

use super::exports::{exports_from_raw, AllGrants, Exports, RawExports};
use super::hosts::{hosts_from_raw, Hosts, RawHosts};
use super::paths::Paths;
use super::settings::{ClientSettings, DaemonSettings, Settings};
use super::{expand_tilde, line_of, read_optional, toml_err, ConfigError};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use toml::Spanned;

/// Everything irsync is configured with, as of one read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub settings: Settings,
    /// `Hosts::path` is the file that holds `[hosts]`.
    pub hosts: Hosts,
    /// `Exports::path` is the file that holds `[exports]` and `[all]`.
    pub exports: Exports,
    /// `hosts.toml`, when it was read.
    pub split_hosts: Option<PathBuf>,
    /// `exports.toml`, when it was read.
    pub split_exports: Option<PathBuf>,
}

/// The typed shape of the merged file.
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawMerged {
    identity: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    client: ClientSettings,
    daemon: DaemonSettings,
    hosts: RawHosts,
    exports: RawExports,
    all: AllGrants,
}

/// Which top-level tables the merged text has, for the one-home rule.
struct Shape {
    hosts: Option<usize>,
    exports: Option<usize>,
}

impl Config {
    /// Parses the merged text alone: a shape pass over the top-level keys
    /// (with the old-layout hints), then the typed pass. Split files are
    /// not consulted; see [`Config::load`].
    pub fn parse(text: &str, path: &Path) -> Result<Config, ConfigError> {
        Self::parse_merged(text, path).map(|(c, _)| c)
    }

    fn parse_merged(text: &str, path: &Path) -> Result<(Config, Shape), ConfigError> {
        let at = |offset: usize, message: String| ConfigError::File {
            path: path.to_path_buf(),
            line: Some(line_of(text, offset)),
            message,
        };
        let shape: BTreeMap<String, Spanned<toml::Value>> =
            toml::from_str(text).map_err(|e| toml_err(text, path, e))?;
        let mut keys: Vec<_> = shape.iter().collect();
        keys.sort_by_key(|(_, v)| v.span().start);
        for (name, value) in keys {
            let start = value.span().start;
            let value = value.get_ref();
            let message = match name.as_str() {
                "hosts" | "exports" | "all" | "client" | "daemon" if value.is_table() => continue,
                "hosts" | "exports" | "all" | "client" | "daemon" => {
                    format!("{name}: expected a table, found {}", value.type_str())
                }
                "identity" | "state_dir" if value.is_str() => continue,
                "identity" | "state_dir" => {
                    format!("{name}: expected a string, found {}", value.type_str())
                }
                // A bare table is an old-layout host or export: say which
                // by its keys, both when it has neither.
                _ if value.is_table() => {
                    let t = value.as_table().expect("checked");
                    if t.contains_key("endpoint_id") {
                        format!("unknown table {name:?}; hosts live under [hosts.{name}]")
                    } else if t.contains_key("path") {
                        format!("unknown table {name:?}; exports live under [exports.{name}]")
                    } else {
                        format!(
                            "unknown table {name:?}; hosts live under [hosts.{name}], exports under [exports.{name}]"
                        )
                    }
                }
                "rw" | "ro" => {
                    format!("unknown key {name:?}; grants for every export live under [all]")
                }
                _ => format!("unknown key {name:?}"),
            };
            return Err(at(start, message));
        }
        // Typed pass over the same text, so the spans match.
        let raw: RawMerged = toml::from_str(text).map_err(|e| toml_err(text, path, e))?;
        let defaults = Settings::default();
        let mut settings = Settings {
            // A named identity is the whole answer; the ssh key is only
            // consulted for the built-in default.
            ssh_identity: raw
                .identity
                .is_none()
                .then_some(defaults.ssh_identity)
                .flatten(),
            identity: raw.identity.unwrap_or(defaults.identity),
            state_dir: raw.state_dir.unwrap_or(defaults.state_dir),
            client: raw.client,
            daemon: raw.daemon,
        };
        for p in [
            &mut settings.identity,
            &mut settings.state_dir,
            &mut settings.daemon.identity,
        ] {
            *p = expand_tilde(&p.to_string_lossy());
        }
        let hosts = Hosts {
            path: path.to_path_buf(),
            entries: hosts_from_raw(raw.hosts, text, path)?,
        };
        let exports = exports_from_raw(raw.all, raw.exports, text, path)?;
        let line = |k: &str| shape.get(k).map(|v| line_of(text, v.span().start));
        let shape = Shape {
            hosts: line("hosts"),
            exports: line("exports").or_else(|| line("all")),
        };
        Ok((
            Config {
                settings,
                hosts,
                exports,
                split_hosts: None,
                split_exports: None,
            },
            shape,
        ))
    }

    /// `paths.config` (defaults when missing), then `hosts.toml` and
    /// `exports.toml` beside it when they exist. Each table has one home:
    /// a split file present means its tables must not be in the config
    /// file.
    pub fn load(paths: &Paths) -> Result<Config, ConfigError> {
        let (mut config, shape) = match read_optional(&paths.config)? {
            Some(text) => Self::parse_merged(&text, &paths.config)?,
            None => Self::parse_merged("", &paths.config)?,
        };
        let conflict = |line: Option<usize>, what: &str, table: &str, split: &Path| {
            line.map(|line| ConfigError::File {
                path: paths.config.clone(),
                line: Some(line),
                message: format!(
                    "{what} are in {}; remove [{table}] from {}, or delete {0}",
                    split.display(),
                    paths.config.display()
                ),
            })
        };
        if let Some(text) = read_optional(&paths.hosts)? {
            if let Some(e) = conflict(shape.hosts, "hosts", "hosts", &paths.hosts) {
                return Err(e);
            }
            config.hosts = Hosts::parse(&text, &paths.hosts)?;
            config.split_hosts = Some(paths.hosts.clone());
        }
        if let Some(text) = read_optional(&paths.exports)? {
            if let Some(e) = conflict(
                shape.exports,
                "exports and grants",
                "exports] and [all",
                &paths.exports,
            ) {
                return Err(e);
            }
            config.exports = Exports::parse(&text, &paths.exports)?;
            config.split_exports = Some(paths.exports.clone());
        }
        Ok(config)
    }
}
