use super::UsageError;
use crate::fs::paths::RelPath;
use std::path::PathBuf;

/// One side of a transfer, parsed with rsync's `host:export/path` rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Local {
        path: PathBuf,
        contents_only: bool,
    },
    Remote {
        /// A client-config host name or a literal EndpointId; resolved later.
        host: String,
        export: String,
        path: RelPath,
        contents_only: bool,
    },
}

impl Location {
    pub fn is_remote(&self) -> bool {
        matches!(self, Location::Remote { .. })
    }

    /// A `:` before any `/` makes the string remote (rsync's rule).
    pub fn parse(s: &str) -> Result<Location, UsageError> {
        let contents_only = s.ends_with('/');
        let colon = s.find(':');
        let slash = s.find('/');
        match colon {
            Some(c) if slash.is_none_or(|sl| c < sl) => {
                let host = &s[..c];
                let rest = s[c + 1..].trim_end_matches('/');
                let (export, path) = rest.split_once('/').unwrap_or((rest, ""));
                if host.is_empty() {
                    return Err(UsageError(format!("empty host in {s:?}")));
                }
                if export.is_empty() {
                    // Usually an rsync-over-ssh habit: `host:/abs/path`.
                    return Err(UsageError(format!(
                        "missing export in {s:?}: remote addresses are HOST:EXPORT/PATH, \
                         where EXPORT is a directory the daemon shares by name (irsync add NAME:PATH)"
                    )));
                }
                let path = if path.is_empty() {
                    RelPath::root()
                } else {
                    RelPath::new(path.as_bytes())
                        .map_err(|_| UsageError(format!("invalid path in {s:?}")))?
                };
                Ok(Location::Remote {
                    host: host.to_string(),
                    export: export.to_string(),
                    path,
                    contents_only,
                })
            }
            _ => {
                let trimmed = s.trim_end_matches('/');
                let path = if trimmed.is_empty() {
                    if s.is_empty() {
                        return Err(UsageError("empty path".into()));
                    }
                    PathBuf::from("/")
                } else {
                    PathBuf::from(trimmed)
                };
                Ok(Location::Local {
                    path,
                    contents_only,
                })
            }
        }
    }
}
