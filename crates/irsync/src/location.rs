//! The operands of a sync: a local path or `HOST:PATH`.

use crate::platform::Platform;
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
    /// `user@[fe80::1]:PATH`; the host is kept without them. On Windows a
    /// drive path (`C:\x`, `c:/x`, `C:`) is local (see [`is_drive_path`]).
    pub fn parse(s: &str) -> Result<Location, UsageError> {
        Self::parse_on(s, Platform::CURRENT)
    }

    /// The operand of a listing (`irsync list HOST`, `irsync HOST:`): as
    /// [`Location::parse`], except that a letter and `:` alone is the host
    /// of that letter, also on Windows. A listing has no local side, so
    /// `h:` cannot mean the drive there, and `irsync list h` (which becomes
    /// `h:`) lists a one-letter host. Elsewhere `C:` is the drive (see
    /// [`is_drive_path`]). On Unix this is `parse`.
    pub fn parse_listed(s: &str) -> Result<Location, UsageError> {
        match s.as_bytes() {
            [l, b':'] if l.is_ascii_alphabetic() => Ok(Location::Remote {
                host: s[..1].to_string(),
                path: Vec::new(),
                contents_only: false,
            }),
            _ => Self::parse(s),
        }
    }

    /// [`Location::parse`] as it reads `s` on `platform`.
    pub fn parse_on(s: &str, platform: Platform) -> Result<Location, UsageError> {
        let contents_only = s.ends_with('/');
        if is_drive_path(s, platform) {
            return Ok(local(s, contents_only, platform));
        }
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
                if s.is_empty() {
                    return Err(UsageError("empty path".into()));
                }
                Ok(local(s, contents_only, platform))
            }
        }
    }
}

/// The local path `s` (not empty), without its trailing slashes; a lone
/// run of slashes is `/`, and a drive's root (`C:/`) keeps its first one,
/// as `C:` alone is another directory (the drive's current one).
fn local(s: &str, contents_only: bool, platform: Platform) -> Location {
    let trimmed = s.trim_end_matches('/');
    let path = if trimmed.is_empty() {
        PathBuf::from("/")
    } else if trimmed.len() == 2 && trimmed.len() < s.len() && is_drive_path(trimmed, platform) {
        PathBuf::from(&s[..3])
    } else {
        PathBuf::from(trimmed)
    };
    Location::Local {
        path,
        contents_only,
    }
}

/// Whether `arg` is a local drive path on `platform`, never `HOST:PATH` or
/// `NAME:PATH`: on Windows one ASCII letter and `:`, then `\` or `/` or
/// nothing more (`C:\x`, `c:/x`, `C:`). This is the convention of
/// OpenSSH's scp on Windows. A one-letter host needs a path without a
/// separator right after the colon (`h:photos`), or a name for the host.
/// A drive-relative path (`C:x`, the current directory of drive `C:`) is
/// not supported: it stays `HOST:PATH`. On Unix nothing is a drive path,
/// so `C:\x` and `c:/x` are remote there, as they always were.
pub fn is_drive_path(arg: &str, platform: Platform) -> bool {
    match (platform, arg.as_bytes()) {
        (Platform::Unix, _) => false,
        (Platform::Windows, [l, b':', rest @ ..]) => {
            l.is_ascii_alphabetic() && matches!(rest.first(), None | Some(b'\\' | b'/'))
        }
        (Platform::Windows, _) => false,
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
