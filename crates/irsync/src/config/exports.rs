//! `exports.toml`: what the daemon shares and who may use it.

use super::{expand_tilde, line_of, read_optional, toml_err, ConfigError};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use toml::Spanned;

/// Names that cannot be export names.
pub const RESERVED: [&str; 3] = ["rw", "ro", "all"];

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

impl Exports {
    pub fn parse(text: &str, path: &Path) -> Result<Exports, ConfigError> {
        let at = |offset: usize, message: String| ConfigError::File {
            path: path.to_path_buf(),
            line: Some(line_of(text, offset)),
            message,
        };
        let mut table: BTreeMap<String, Spanned<toml::Value>> =
            toml::from_str(text).map_err(|e| toml_err(text, path, e))?;
        let mut list = |key: &str| -> Result<Vec<String>, ConfigError> {
            match table.remove(key) {
                None => Ok(Vec::new()),
                Some(v) if v.get_ref().is_table() => {
                    Err(ConfigError::ReservedName(key.to_string()))
                }
                Some(v) => {
                    let start = v.span().start;
                    v.into_inner()
                        .try_into()
                        .map_err(|e: toml::de::Error| at(start, format!("{key}: {}", e.message())))
                }
            }
        };
        let rw = list("rw")?;
        let ro = list("ro")?;
        let mut exports = BTreeMap::new();
        for (name, value) in table {
            let start = value.span().start;
            if RESERVED.contains(&name.as_str()) {
                return Err(ConfigError::ReservedName(name));
            }
            let value = value.into_inner();
            if !value.is_table() {
                return Err(at(
                    start,
                    format!(
                        "{name}: expected a table (an export), found {}",
                        value.type_str()
                    ),
                ));
            }
            let mut e: Export = value
                .try_into()
                .map_err(|e: toml::de::Error| at(start, format!("[{name}]: {}", e.message())))?;
            e.path = expand_tilde(&e.path.to_string_lossy());
            exports.insert(name, e);
        }
        Ok(Exports {
            path: path.to_path_buf(),
            rw,
            ro,
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
