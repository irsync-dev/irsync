//! `exports.toml`: what the daemon shares and who may use it.

use super::{expand_tilde, line_of, read_optional, toml_err, ConfigError};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use toml::Spanned;

/// Names that cannot be export names: `all` is the word `grant add all WHO`
/// uses for every export.
pub const RESERVED: [&str; 1] = ["all"];

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Export {
    pub path: PathBuf,
    #[serde(default)]
    pub read_only: bool,
    /// Hash the export's files into its index when the daemon starts.
    #[serde(default)]
    pub warm_index: bool,
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
    pub exports: BTreeMap<String, Export>,
}

/// The message for a reserved export name.
pub fn reserved(name: &str) -> String {
    format!("{name:?} is reserved: it is the word `grant add all WHO` uses for every export")
}

/// The `[all]` table: grants that apply to every export.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct AllGrants {
    rw: Vec<String>,
    ro: Vec<String>,
}

/// The typed shape of the file, read once the top-level keys are known
/// to be `all` and `exports`.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawFile {
    all: AllGrants,
    exports: BTreeMap<String, Spanned<toml::Value>>,
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
        for (name, value) in &shape {
            let start = value.span().start;
            let value = value.get_ref();
            let message = match name.as_str() {
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
        let mut exports = BTreeMap::new();
        for (name, value) in raw.exports {
            let start = value.span().start;
            if RESERVED.contains(&name.as_str()) {
                return Err(at(start, reserved(&name)));
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
            rw: raw.all.rw,
            ro: raw.all.ro,
            exports,
        })
    }

    /// A missing file yields no exports.
    pub fn load(path: &Path) -> Result<Exports, ConfigError> {
        match read_optional(path)? {
            Some(text) => Self::parse(&text, path),
            None => Self::parse("", path),
        }
    }
}
