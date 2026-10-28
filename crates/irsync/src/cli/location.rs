use super::UsageError;
use std::path::PathBuf;

/// One side of a transfer: a local path, or `HOST:PATH` where `PATH` is
/// passed to the daemon as typed (an export name, a path under the
/// daemon's home, or an absolute path; the daemon resolves it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Local {
        path: PathBuf,
        contents_only: bool,
    },
    Remote {
        /// A client-config host name or a literal EndpointId; resolved later.
        host: String,
        /// `PATH` as typed, without its trailing slash (`contents_only`);
        /// a lone `/` is kept.
        path: Vec<u8>,
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
                if host.is_empty() {
                    return Err(UsageError(format!("empty host in {s:?}")));
                }
                let rest = &s[c + 1..];
                let path = if rest.chars().all(|ch| ch == '/') && !rest.is_empty() {
                    "/"
                } else {
                    rest.trim_end_matches('/')
                };
                Ok(Location::Remote {
                    host: host.to_string(),
                    path: path.as_bytes().to_vec(),
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
