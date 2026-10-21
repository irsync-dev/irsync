use super::{duration, expand_tilde, ConfigError};
use crate::net::addr::HostPort;
use iroh::EndpointId;
use std::path::{Path, PathBuf};
use std::time::Duration;
use url::Url;

/// Settings resolved for one destination; unset options are `None`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HostConfig {
    pub endpoint_id: Option<EndpointId>,
    pub identity_file: Option<PathBuf>,
    pub relay: Option<Url>,
    pub parallel: Option<u32>,
    pub reconnect: Option<Duration>,
    pub dht: Option<bool>,
    pub mdns: Option<bool>,
    /// Direct addresses to dial (all `Address` lines, in file order). Any
    /// address puts the client in isolated mode: no relay, no lookups.
    pub addresses: Vec<HostPort>,
}

impl HostConfig {
    /// Fill every still-unset field from `other` (first value wins);
    /// `addresses` accumulate instead.
    fn fill_from(&mut self, other: &HostConfig) {
        macro_rules! fill {
            ($($f:ident),*) => { $( if self.$f.is_none() { self.$f = other.$f.clone(); } )* };
        }
        fill!(
            endpoint_id,
            identity_file,
            relay,
            parallel,
            reconnect,
            dht,
            mdns
        );
        self.addresses.extend(other.addresses.iter().cloned());
    }
}

#[derive(Debug)]
struct Block {
    /// `None` for options before the first `Host` line (matches all hosts).
    patterns: Option<Vec<String>>,
    opts: HostConfig,
}

impl Block {
    fn applies(&self, host: &str) -> bool {
        let Some(patterns) = &self.patterns else {
            return true;
        };
        let mut positive = false;
        for p in patterns {
            if let Some(neg) = p.strip_prefix('!') {
                if glob(neg.as_bytes(), host.as_bytes()) {
                    return false;
                }
            } else if glob(p.as_bytes(), host.as_bytes()) {
                positive = true;
            }
        }
        positive
    }
}

/// `*` matches any run of characters, `?` exactly one byte; case-sensitive.
fn glob(pat: &[u8], text: &[u8]) -> bool {
    match pat.split_first() {
        None => text.is_empty(),
        Some((b'*', rest)) => (0..=text.len()).any(|i| glob(rest, &text[i..])),
        Some((b'?', rest)) => !text.is_empty() && glob(rest, &text[1..]),
        Some((c, rest)) => text.first() == Some(c) && glob(rest, &text[1..]),
    }
}

#[derive(Debug, Default)]
pub struct ClientConfig {
    blocks: Vec<Block>,
}

fn parse_bool(v: &str) -> Result<bool, String> {
    match v.to_ascii_lowercase().as_str() {
        "yes" | "true" => Ok(true),
        "no" | "false" => Ok(false),
        _ => Err("expected yes or no".into()),
    }
}

/// Returns `Ok(false)` for an unknown keyword. The first value per option wins.
fn set_option(opts: &mut HostConfig, keyword: &str, value: &str) -> Result<bool, String> {
    macro_rules! first {
        ($f:ident, $v:expr) => {{
            let v = $v?;
            if opts.$f.is_none() {
                opts.$f = Some(v);
            }
        }};
    }
    match keyword.to_ascii_lowercase().as_str() {
        "endpointid" => first!(
            endpoint_id,
            crate::identity::parse_endpoint_id(value).map_err(|e| e.to_string())
        ),
        "identityfile" => first!(identity_file, Ok::<_, String>(expand_tilde(value))),
        "relay" => first!(relay, Url::parse(value).map_err(|e| e.to_string())),
        "parallel" => first!(parallel, value.parse::<u32>().map_err(|e| e.to_string())),
        "reconnect" => first!(reconnect, duration::parse(value).map_err(|e| e.to_string())),
        "dht" => first!(dht, parse_bool(value)),
        "mdns" => first!(mdns, parse_bool(value)),
        "address" => opts
            .addresses
            .push(value.parse::<HostPort>().map_err(|e| e.to_string())?),
        _ => return Ok(false),
    }
    Ok(true)
}

/// Split `Keyword value` / `Keyword=value`, dropping a trailing `# comment`.
fn split_line(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    let line = match line
        .char_indices()
        .find(|&(i, c)| c == '#' && (i == 0 || line[..i].ends_with(char::is_whitespace)))
    {
        Some((i, _)) => line[..i].trim_end(),
        None => line,
    };
    if line.is_empty() {
        return None;
    }
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let value = line[end..].trim_start_matches(|c: char| c.is_whitespace() || c == '=');
    Some((&line[..end], value.trim()))
}

impl ClientConfig {
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let mut blocks = vec![Block {
            patterns: None,
            opts: HostConfig::default(),
        }];
        for (idx, raw) in text.lines().enumerate() {
            let line = idx + 1;
            let Some((keyword, value)) = split_line(raw) else {
                continue;
            };
            if keyword.eq_ignore_ascii_case("host") {
                blocks.push(Block {
                    patterns: Some(value.split_whitespace().map(str::to_string).collect()),
                    opts: HostConfig::default(),
                });
                continue;
            }
            let block = blocks.last_mut().expect("global block always present");
            match set_option(&mut block.opts, keyword, value) {
                Ok(true) => {}
                Ok(false) => {
                    return Err(ConfigError::UnknownKeyword {
                        line,
                        keyword: keyword.to_string(),
                    })
                }
                Err(message) => {
                    return Err(ConfigError::BadValue {
                        line,
                        keyword: keyword.to_string(),
                        message,
                    })
                }
            }
        }
        Ok(ClientConfig { blocks })
    }

    /// Order: explicit path, `$IRSYNC_CONFIG`, `~/.config/irsync/config`, else empty.
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        let candidate = match path {
            Some(p) => Some(p.to_path_buf()),
            None => match std::env::var_os("IRSYNC_CONFIG") {
                Some(p) => Some(PathBuf::from(p)),
                None => dirs::home_dir()
                    .map(|h| h.join(".config/irsync/config"))
                    .filter(|p| p.exists()),
            },
        };
        let Some(p) = candidate else {
            return Ok(Self::default());
        };
        let text = std::fs::read_to_string(&p).map_err(|source| ConfigError::Io {
            path: p.clone(),
            source,
        })?;
        Self::parse(&text)
    }

    /// Merge all matching blocks in file order; the first value per option wins.
    pub fn resolve(&self, host: &str) -> HostConfig {
        let mut out = HostConfig::default();
        for b in self.blocks.iter().filter(|b| b.applies(host)) {
            out.fill_from(&b.opts);
        }
        out
    }
}
