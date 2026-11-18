//! `irsync.toml` as one file: settings, host aliases, exports and grants.
//! A `hosts.toml` or `exports.toml` beside it, once read as part of it, is
//! ignored with a warning.

use super::exports::{
    exports_from_raw, immediate_problem, AllGrants, Exports, ImmediateGrants, RawExports,
};
use super::hosts::{hosts_from_raw, Hosts, RawHosts};
use super::paths::Paths;
use super::settings::{ClientSettings, DaemonSettings, Settings, TelemetrySettings};
use super::{expand_tilde, line_of, read_optional, toml_err, ConfigError};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use toml::Spanned;

/// Everything irsync is configured with, as of one read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub settings: Settings,
    pub hosts: Hosts,
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

/// The typed shape of the merged file.
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawMerged {
    identity: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    client: ClientSettings,
    daemon: DaemonSettings,
    telemetry: TelemetrySettings,
    hosts: RawHosts,
    exports: RawExports,
    all: AllGrants,
    immediate: ImmediateGrants,
}

impl Config {
    /// Parses the text of the config file: a shape pass over the top-level
    /// keys (with the old-layout hints), then the typed pass.
    pub fn parse(text: &str, path: &Path) -> Result<Config, ConfigError> {
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
                "immediate" => match immediate_problem(value) {
                    Some(m) => m,
                    None => continue,
                },
                "hosts" | "exports" | "all" | "client" | "daemon" | "telemetry"
                    if value.is_table() =>
                {
                    continue
                }
                "hosts" | "exports" | "all" | "client" | "daemon" | "telemetry" => {
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
                    if t.contains_key("endpoint_id") || t.contains_key("client_id") {
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
            telemetry: raw.telemetry,
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
        let exports = exports_from_raw(raw.all, raw.immediate, raw.exports, text, path)?;
        Ok(Config {
            settings,
            hosts,
            exports,
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
