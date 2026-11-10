//! The `[exports]` and `[all]` tables: what the daemon shares and who may
//! use it, in `irsync.toml` or a split `exports.toml` beside it.

use super::{expand_tilde, line_of, toml_err, ConfigError};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use toml::Spanned;

/// Names that cannot be export names: `all` is the word `grant add all WHO`
/// uses for every export.
pub const RESERVED: [&str; 2] = ["all", "immediate"];

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Export {
    pub path: PathBuf,
    #[serde(default)]
    pub read_only: bool,
    /// Hash the export's files into its index when the daemon starts.
    #[serde(default)]
    pub warm_index: bool,
    /// Flush every file a push writes into the export before placing it,
    /// as if the client had passed `--fsync`.
    #[serde(default)]
    pub fsync: bool,
    /// Created by the daemon for a path an `[immediate]` client reached,
    /// not written by hand.
    #[serde(default)]
    pub auto: bool,
    #[serde(default)]
    pub rw: Vec<String>,
    #[serde(default)]
    pub ro: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exports {
    pub path: PathBuf,
    pub rw: Vec<String>,
    pub ro: Vec<String>,
    /// The `[immediate]` table's `rw`: peers that may reach any path the
    /// daemon's user can.
    pub immediate_rw: Vec<String>,
    pub exports: BTreeMap<String, Export>,
}

/// The message for a reserved export name.
pub fn reserved(name: &str) -> String {
    let word = if name == "immediate" {
        "`grant add immediate WHO` uses for a grant to any path"
    } else {
        "`grant add all WHO` uses for every export"
    };
    format!("{name:?} is reserved: it is the word {word}")
}

/// The `[all]` table: grants that apply to every export.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct AllGrants {
    pub(crate) rw: Vec<String>,
    pub(crate) ro: Vec<String>,
}

/// The `[immediate]` table: peers that may reach any path (read-write only).
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ImmediateGrants {
    pub(crate) rw: Vec<String>,
}

/// The message for a bad `[immediate]` table, or `None` when it is fine.
pub(crate) fn immediate_problem(value: &toml::Value) -> Option<String> {
    let Some(t) = value.as_table() else {
        return Some(format!(
            "immediate: expected a table, found {}",
            value.type_str()
        ));
    };
    t.keys().find(|k| *k != "rw").map(|k| {
        format!("[immediate]: unknown key {k:?}; only `rw` is allowed (immediate grants are read-write)")
    })
}

/// The `[exports]` table as deserialized: name -> table, with spans.
pub(crate) type RawExports = BTreeMap<String, Spanned<toml::Value>>;

/// Validates `[all]` and every `[exports.NAME]`: reserved names, the
/// export schema, absolute paths. `text` is the document the spans point
/// into, `path` the file named in errors (and recorded as `Exports.path`).
pub(crate) fn exports_from_raw(
    all: AllGrants,
    immediate: ImmediateGrants,
    raw: RawExports,
    text: &str,
    path: &Path,
) -> Result<Exports, ConfigError> {
    let at = |offset: usize, message: String| ConfigError::File {
        path: path.to_path_buf(),
        line: Some(line_of(text, offset)),
        message,
    };
    let mut exports = BTreeMap::new();
    for (name, value) in raw {
        let start = value.span().start;
        if RESERVED.contains(&name.as_str()) {
            return Err(at(start, reserved(&name)));
        }
        // The name is the first segment of a HOST:PATH.
        if name.is_empty() || name.contains('/') {
            return Err(at(
                start,
                format!("{name:?} is not a valid export name: it must not be empty or contain '/'"),
            ));
        }
        let value = value.into_inner();
        if !value.is_table() {
            return Err(at(
                start,
                format!(
                    "exports.{name}: expected a table (an export), found {}",
                    value.type_str()
                ),
            ));
        }
        let mut e: Export = value.try_into().map_err(|e: toml::de::Error| {
            at(start, format!("[exports.{name}]: {}", e.message()))
        })?;
        e.path = expand_tilde(&e.path.to_string_lossy());
        if !e.path.is_absolute() {
            return Err(at(
                start,
                format!(
                    "[exports.{name}]: path {:?} must be absolute",
                    e.path.to_string_lossy()
                ),
            ));
        }
        exports.insert(name, e);
    }
    Ok(Exports {
        path: path.to_path_buf(),
        rw: all.rw,
        ro: all.ro,
        immediate_rw: immediate.rw,
        exports,
    })
}

/// The typed shape of the file, read once the top-level keys are known
/// to be `all` and `exports`.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawFile {
    all: AllGrants,
    immediate: ImmediateGrants,
    exports: RawExports,
}

impl Exports {
    pub fn parse(text: &str, path: &Path) -> Result<Exports, ConfigError> {
        let at = |offset: usize, message: String| ConfigError::File {
            path: path.to_path_buf(),
            line: Some(line_of(text, offset)),
            message,
        };
        // Shape pass: every top-level key is `all` or `exports`, both tables.
        let shape: BTreeMap<String, Spanned<toml::Value>> =
            toml::from_str(text).map_err(|e| toml_err(text, path, e))?;
        // In file order, so the first offender is the one reported.
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
                "all" | "exports" if value.is_table() => continue,
                "all" | "exports" => {
                    format!("{name}: expected a table, found {}", value.type_str())
                }
                _ if value.is_table() => {
                    format!("unknown table {name:?}; exports live under [exports.{name}]")
                }
                "rw" | "ro" => {
                    format!("unknown key {name:?}; grants for every export live under [all]")
                }
                _ => format!("unknown key {name:?}"),
            };
            return Err(at(start, message));
        }
        // Typed pass over the same text, so the spans match.
        let raw: RawFile = toml::from_str(text).map_err(|e| {
            let line = e
                .span()
                .map(|s| s.start)
                .or_else(|| shape.get("all").map(|v| v.span().start))
                .unwrap_or(0);
            at(line, format!("[all]: {}", e.message()))
        })?;
        exports_from_raw(raw.all, raw.immediate, raw.exports, text, path)
    }
}
