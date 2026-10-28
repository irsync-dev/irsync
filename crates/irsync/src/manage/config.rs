//! `irsync config`: any key of the config file by its dotted path.

use super::ManageError;
use crate::config::duration;
use crate::config::settings::{RelayMode, Settings};
use toml_edit::{DocumentMut, Item, Key, TableLike, Value};

/// What a key holds, which decides how `set` reads a value and how `get`
/// prints one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Int,
    Duration,
    Str,
    Path,
    Url,
    Id,
    StrArray,
    AddrArray,
    Table,
}

/// The kind of a key path, or `None` for a key the file does not know.
pub fn kind_of(path: &[String]) -> Option<Kind> {
    let parts: Vec<&str> = path.iter().map(String::as_str).collect();
    use Kind::*;
    Some(match parts.as_slice() {
        ["identity"] | ["state_dir"] => Path,
        ["client"]
        | ["daemon"]
        | ["daemon", "discovery"]
        | ["hosts"]
        | ["hosts", _]
        | ["exports"]
        | ["exports", _]
        | ["all"] => Table,
        ["client", "parallel"] => Int,
        ["client", "reconnect"] => Duration,
        ["client", "dht"] | ["client", "mdns"] => Bool,
        ["client", "relay"] => Url,
        ["daemon", "identity"] => Path,
        ["daemon", "idle_timeout"] | ["daemon", "dormant_session_timeout"] => Duration,
        ["daemon", "listen"] => AddrArray,
        ["daemon", "portmap"] => Bool,
        ["daemon", "relay"] => Url,
        ["daemon", "relay_mode"] => Str,
        ["daemon", "discovery", "dht" | "dns" | "mdns"] => Bool,
        ["hosts", _, "endpoint_id"] => Id,
        ["hosts", _, "relay"] => Url,
        ["hosts", _, "address"] => AddrArray,
        ["hosts", _, "parallel"] => Int,
        ["hosts", _, "reconnect"] => Duration,
        ["hosts", _, "dht" | "mdns"] => Bool,
        ["exports", _, "path"] => Path,
        ["exports", _, "read_only" | "warm_index"] => Bool,
        ["exports", _, "rw" | "ro"] | ["all", "rw" | "ro"] => StrArray,
        _ => return None,
    })
}

/// `KEY` with TOML's key grammar: `exports."www.example.com".path` is three
/// segments.
pub fn parse_key(key: &str) -> Result<Vec<String>, ManageError> {
    let bad = || ManageError::BadKey(key.to_string());
    let keys = Key::parse(key).map_err(|_| bad())?;
    if keys.is_empty() || keys.iter().any(|k| k.get().is_empty()) {
        return Err(bad());
    }
    Ok(keys.into_iter().map(|k| k.get().to_string()).collect())
}

/// The default of a settings key, as `get` prints it (one element per
/// line); `None` when the key has no default or is not a settings key.
pub fn default_of(path: &[String]) -> Option<Vec<String>> {
    let parts: Vec<&str> = path.iter().map(String::as_str).collect();
    let s = Settings::default();
    let one = |v: String| Some(vec![v]);
    match parts.as_slice() {
        ["identity"] => one(s.identity.display().to_string()),
        ["state_dir"] => one(s.state_dir.display().to_string()),
        ["daemon", "identity"] => one(s.daemon.identity.display().to_string()),
        ["daemon", "idle_timeout"] => one(duration::format(s.daemon.idle_timeout)),
        ["daemon", "dormant_session_timeout"] => {
            one(duration::format(s.daemon.dormant_session_timeout))
        }
        ["daemon", "listen"] => Some(s.daemon.listen.iter().map(|a| a.to_string()).collect()),
        ["daemon", "portmap"] => one(s.daemon.portmap.to_string()),
        ["daemon", "relay_mode"] => one(match s.daemon.relay_mode {
            RelayMode::Default => "default",
            RelayMode::Disabled => "disabled",
        }
        .to_string()),
        ["daemon", "discovery", "dht"] => one(s.daemon.discovery.dht.to_string()),
        ["daemon", "discovery", "dns"] => one(s.daemon.discovery.dns.to_string()),
        ["daemon", "discovery", "mdns"] => one(s.daemon.discovery.mdns.to_string()),
        _ => None,
    }
}

/// What `get` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Got {
    /// The lines to print.
    Lines(Vec<String>),
    /// The key is known but not set.
    Unset,
}

/// A value in TOML spelling, without surrounding whitespace.
fn repr(v: &Value) -> String {
    v.to_string().trim().to_string()
}

/// A scalar as `get` prints it: strings bare, the rest in TOML spelling.
fn bare(v: &Value) -> String {
    match v.as_str() {
        Some(s) => s.to_string(),
        None => repr(v),
    }
}

/// A key segment as a dotted path prints it: quoted when not bare.
fn segment(k: &str) -> String {
    Key::new(k).display_repr().into_owned()
}

/// `prefix.key = value` for every leaf under `table`, in file order,
/// nested tables dotted.
fn walk(table: &dyn TableLike, prefix: &str, out: &mut Vec<String>) {
    for (k, item) in table.iter() {
        let name = format!("{prefix}{}", segment(k));
        match item {
            Item::Value(v) => match v.as_inline_table() {
                Some(t) => walk(t, &format!("{name}."), out),
                None => out.push(format!("{name} = {}", repr(v))),
            },
            Item::Table(t) => walk(t, &format!("{name}."), out),
            Item::ArrayOfTables(_) | Item::None => {}
        }
    }
}

fn lookup<'a>(doc: &'a DocumentMut, path: &[String]) -> Option<&'a Item> {
    let mut item: &Item = doc.as_item();
    for seg in path {
        item = item.as_table_like()?.get(seg)?;
    }
    Some(item)
}

/// The value of `path`: a bare scalar, one array element per line, or the
/// keys of a table as `key = value` lines. An unknown key is an error.
pub fn get(doc: &DocumentMut, path: &[String]) -> Result<Got, ManageError> {
    if kind_of(path).is_none() {
        return Err(ManageError::UnknownKey(path.join(".")));
    }
    let Some(item) = lookup(doc, path) else {
        return Ok(Got::Unset);
    };
    let lines = match item {
        Item::None => return Ok(Got::Unset),
        Item::Value(v) => match v.as_array() {
            Some(a) => a.iter().map(bare).collect(),
            None => match v.as_inline_table() {
                Some(t) => {
                    let mut out = Vec::new();
                    walk(t, "", &mut out);
                    out
                }
                None => vec![bare(v)],
            },
        },
        Item::Table(t) => {
            let mut out = Vec::new();
            walk(t, "", &mut out);
            out
        }
        Item::ArrayOfTables(_) => return Err(ManageError::UnknownKey(path.join("."))),
    };
    Ok(Got::Lines(lines))
}

/// Every key the document sets, as `dotted.key = value` in file order.
pub fn list(doc: &DocumentMut) -> Vec<String> {
    let mut out = Vec::new();
    walk(doc.as_table(), "", &mut out);
    out
}
