//! `exports.toml`: what the daemon shares and who may use it.

use super::{expand_tilde, read_optional, toml_err, ConfigError};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
        let file_err = |message: String| ConfigError::File {
            path: path.to_path_buf(),
            line: None,
            message,
        };
        let mut table: toml::Table = toml::from_str(text).map_err(|e| toml_err(text, path, e))?;
        let mut list = |key: &str| -> Result<Vec<String>, ConfigError> {
            match table.remove(key) {
                None => Ok(Vec::new()),
                Some(v) => v
                    .try_into()
                    .map_err(|e: toml::de::Error| file_err(format!("{key}: {}", e.message()))),
            }
        };
        let rw = list("rw")?;
        let ro = list("ro")?;
        let mut exports = BTreeMap::new();
        for (name, value) in table {
            if RESERVED.contains(&name.as_str()) {
                return Err(ConfigError::ReservedName(name));
            }
            if !value.is_table() {
                return Err(file_err(format!(
                    "{name}: expected a table (an export), found {}",
                    value.type_str()
                )));
            }
            let mut e: Export = value
                .try_into()
                .map_err(|e: toml::de::Error| file_err(format!("[{name}]: {}", e.message())))?;
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
