//! The operands of a sync: a local path or `HOST:PATH`.

use std::path::PathBuf;

/// An operand or a set of operands that cannot be used.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct UsageError(pub String);

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

    /// A `:` before any `/` makes the string remote (rsync's rule). An
    /// IPv6 host is written in brackets, `[fe80::1]:PATH` or
    /// `user@[fe80::1]:PATH`; the host is kept without them.
    pub fn parse(s: &str) -> Result<Location, UsageError> {
        let contents_only = s.ends_with('/');
        let remote = match bracketed(s)? {
            Bracketed::Remote(host, rest) => Some((host, rest)),
            Bracketed::Local => None,
            Bracketed::No => match (s.find(':'), s.find('/')) {
                (Some(c), slash) if slash.is_none_or(|sl| c < sl) => {
                    Some((s[..c].to_string(), &s[c + 1..]))
                }
                _ => None,
            },
        };
        match remote {
            Some((host, rest)) => {
                if host.is_empty() {
                    return Err(UsageError(format!("empty host in {s:?}")));
                }
                let path = if rest.chars().all(|ch| ch == '/') && !rest.is_empty() {
                    "/"
                } else {
                    rest.trim_end_matches('/')
                };
                Ok(Location::Remote {
                    host,
                    path: path.as_bytes().to_vec(),
                    contents_only,
                })
            }
            None => {
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

/// What a leading `[` (after an optional `user@`) makes of an operand.
enum Bracketed<'a> {
    /// `[ipv6]:REST`: the host without brackets, and `REST`.
    Remote(String, &'a str),
    /// `[x]/...` or `[x]`: a local path.
    Local,
    /// No leading bracket, or one the usual rule decides.
    No,
}

fn bracketed(s: &str) -> Result<Bracketed<'_>, UsageError> {
    let (user, rest) = match s.split_once('@') {
        Some((u, r)) if !u.contains(['/', ':']) => (Some(u), r),
        _ => (None, s),
    };
    if !rest.starts_with('[') {
        return Ok(Bracketed::No);
    }
    let name = &rest[..rest.find('/').unwrap_or(rest.len())];
    let Some(close) = name.find(']') else {
        if name.contains(':') {
            return Err(UsageError(format!("no closing `]` in {s:?}")));
        }
        return Ok(Bracketed::No);
    };
    let after = &rest[close + 1..];
    let Some(path) = after.strip_prefix(':') else {
        return Ok(if after.is_empty() || after.starts_with('/') {
            Bracketed::Local
        } else {
            Bracketed::No
        });
    };
    let addr = &rest[1..close];
    // A link-local address may carry a zone (`fe80::1%eth0`), as for ssh.
    let bare = addr.split_once('%').map_or(addr, |(a, _)| a);
    if bare.parse::<std::net::Ipv6Addr>().is_err() {
        return Err(UsageError(format!(
            "{addr:?} in {s:?} is not an IPv6 address"
        )));
    }
    let host = match user {
        Some(u) => format!("{u}@{addr}"),
        None => addr.to_string(),
    };
    Ok(Bracketed::Remote(host, path))
}
