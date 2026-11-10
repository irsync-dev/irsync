//! `irsync config`: any key of the config file by its dotted path.

use super::ManageError;
use crate::config::duration;
use crate::config::settings::{RelayMode, Settings};
use toml_edit::{Array, DocumentMut, Item, Key, Table, TableLike, Value};

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
        | ["telemetry"]
        | ["hosts"]
        | ["hosts", _]
        | ["exports"]
        | ["exports", _]
        | ["all"] => Table,
        ["client", "parallel"] => Int,
        ["client", "reconnect"] => Duration,
        ["client", "dial_retries"] => Int,
        ["client", "dht"] | ["client", "mdns"] => Bool,
        ["client", "relay"] => Url,
        ["client", "relay_mode"] | ["client", "congestion"] => Str,
        ["daemon", "identity"] => Path,
        ["daemon", "idle_timeout"] | ["daemon", "dormant_session_timeout"] => Duration,
        ["daemon", "listen"] => AddrArray,
        ["daemon", "portmap"] => Bool,
        ["daemon", "relay"] => Url,
        ["daemon", "relay_mode"] | ["daemon", "congestion"] => Str,
        ["daemon", "debug_addr"] => Str,
        ["telemetry", "otlp_endpoint"] => Url,
        ["telemetry", "otlp_protocol"] => Str,
        ["daemon", "discovery", "dht" | "dns" | "mdns"] => Bool,
        ["hosts", _, "endpoint_id"] => Id,
        ["hosts", _, "relay"] => Url,
        ["hosts", _, "relay_mode" | "congestion"] => Str,
        ["hosts", _, "address"] => AddrArray,
        ["hosts", _, "parallel"] => Int,
        ["hosts", _, "reconnect"] => Duration,
        ["hosts", _, "dial_retries"] => Int,
        ["hosts", _, "dht" | "mdns"] => Bool,
        ["exports", _, "path"] => Path,
        ["exports", _, "read_only" | "warm_index" | "fsync"] => Bool,
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
/// A default path as the file would hold it: the home directory as `~`.
fn as_written(p: &std::path::Path) -> String {
    match dirs::home_dir().and_then(|h| p.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => p.display().to_string(),
    }
}

pub fn default_of(path: &[String]) -> Option<Vec<String>> {
    let parts: Vec<&str> = path.iter().map(String::as_str).collect();
    let s = Settings::default();
    let one = |v: String| Some(vec![v]);
    match parts.as_slice() {
        ["identity"] => one(as_written(&s.identity)),
        // The effective [client] defaults, as book/src/reference/configuration.md lists them.
        ["client", "parallel"] => one(crate::send::DEFAULT_PARALLEL.to_string()),
        ["client", "reconnect"] => one(duration::format(
            crate::session::reconnect::ReconnectOpts::default().max,
        )),
        ["client", "dial_retries"] => one(crate::session::reconnect::ReconnectOpts::default()
            .dial_retries
            .to_string()),
        ["client", "dht"] | ["client", "mdns"] => one("true".to_string()),
        ["client", "relay_mode"] => one("default".to_string()),
        ["client", "congestion"] => one(crate::config::settings::Congestion::default()
            .as_str()
            .into()),
        ["state_dir"] => one(as_written(&s.state_dir)),
        ["daemon", "identity"] => one(as_written(&s.daemon.identity)),
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
        ["daemon", "congestion"] => one(s.daemon.congestion.as_str().to_string()),
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

/// Drops comments and whitespace decor from a value and its elements, and
/// re-lays out arrays and inline tables on one line.
fn strip(v: &mut Value) {
    *v.decor_mut() = toml_edit::Decor::default();
    match v {
        Value::Array(a) => {
            for e in a.iter_mut() {
                strip(e);
            }
            a.fmt();
        }
        Value::InlineTable(t) => {
            for (_, e) in t.iter_mut() {
                strip(e);
            }
            t.fmt();
        }
        _ => {}
    }
}

/// A value in TOML spelling: no comments, no layout, one line.
fn repr(v: &Value) -> String {
    let mut v = v.clone();
    strip(&mut v);
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

/// `VALUE...` read in the key's kind: `true`/`false`, an integer, a
/// duration as the file spells it, a path, a URL, an EndpointId (stored as
/// typed), or, for an array kind, one element per value (none: an empty
/// array). A scalar kind takes exactly one value; a table cannot be set.
pub fn parse_value(kind: Kind, values: &[String]) -> Result<Value, ManageError> {
    let bad = |m: String| ManageError::BadValue(m);
    let one = || -> Result<&str, ManageError> {
        match values {
            [v] => Ok(v.as_str()),
            [] => Err(bad("a value is required".into())),
            _ => Err(bad(
                "one value, not several (only an array key takes several)".into(),
            )),
        }
    };
    let strings = |check: &dyn Fn(&str) -> Result<(), String>| -> Result<Value, ManageError> {
        let mut a = Array::new();
        for v in values {
            check(v).map_err(bad)?;
            a.push(v.as_str());
        }
        Ok(Value::Array(a))
    };
    Ok(match kind {
        Kind::Bool => match one()? {
            "true" => Value::from(true),
            "false" => Value::from(false),
            v => return Err(bad(format!("expected true or false, got {v:?}"))),
        },
        Kind::Int => {
            let v = one()?;
            Value::from(
                v.parse::<i64>()
                    .map_err(|_| bad(format!("expected an integer, got {v:?}")))?,
            )
        }
        Kind::Duration => {
            let v = one()?;
            duration::parse(v).map_err(|e| bad(e.to_string()))?;
            // Bare seconds are a number in the file, as the parser reads them.
            match v.parse::<i64>() {
                Ok(secs) => Value::from(secs),
                Err(_) => Value::from(v),
            }
        }
        Kind::Str | Kind::Path => Value::from(one()?),
        Kind::Url => {
            let v = one()?;
            url::Url::parse(v).map_err(|e| bad(format!("expected a URL: {e}")))?;
            Value::from(v)
        }
        Kind::Id => {
            let v = one()?;
            crate::identity::parse_endpoint_id(v).map_err(|e| bad(e.to_string()))?;
            Value::from(v)
        }
        Kind::StrArray => strings(&|_| Ok(()))?,
        Kind::AddrArray => strings(&|v| {
            v.parse::<crate::net::addr::HostPort>()
                .map(|_| ())
                .map_err(|e| format!("expected host:port, got {v:?}: {e}"))
        })?,
        Kind::Table => return Err(bad("a table cannot be set; set its keys".into())),
    })
}

/// The table at `path`, created on demand: tables created on the way are
/// implicit (no header of their own), the last one is a real table.
fn table_at<'a>(
    doc: &'a mut DocumentMut,
    path: &[String],
) -> Result<&'a mut dyn TableLike, ManageError> {
    let mut table: &mut dyn TableLike = doc.as_table_mut();
    for (i, seg) in path.iter().enumerate() {
        let implicit = i + 1 < path.len();
        let item = table.entry(seg).or_insert_with(|| {
            let mut t = Table::new();
            t.set_implicit(implicit);
            Item::Table(t)
        });
        if !item.is_table_like() {
            return Err(ManageError::NotATable(path[..=i].join(".")));
        }
        table = item.as_table_like_mut().expect("checked");
    }
    Ok(table)
}

/// Sets `path` to `value`, keeping the key's comments and the rest of the
/// document. A table key or an unknown key is an error.
pub fn set(doc: &mut DocumentMut, path: &[String], value: Value) -> Result<(), ManageError> {
    match kind_of(path) {
        None => return Err(ManageError::UnknownKey(path.join("."))),
        Some(Kind::Table) => {
            return Err(ManageError::BadValue(format!(
                "{} is a table; set its keys",
                path.join(".")
            )))
        }
        Some(_) => {}
    }
    let (leaf, parents) = path.split_last().expect("parse_key refuses an empty key");
    let table = table_at(doc, parents)?;
    match table.get_mut(leaf).and_then(Item::as_value_mut) {
        Some(old) => {
            let decor = old.decor().clone();
            *old = value;
            *old.decor_mut() = decor;
        }
        None => {
            table.insert(leaf, Item::Value(value));
        }
    }
    Ok(())
}

/// Removes `path`: a key, or a whole table. Absent: `NothingToRemove`.
pub fn unset(doc: &mut DocumentMut, path: &[String]) -> Result<(), ManageError> {
    if kind_of(path).is_none() {
        return Err(ManageError::UnknownKey(path.join(".")));
    }
    let (leaf, parents) = path.split_last().expect("parse_key refuses an empty key");
    let mut table: &mut dyn TableLike = doc.as_table_mut();
    for seg in parents {
        table = match table.get_mut(seg).and_then(Item::as_table_like_mut) {
            Some(t) => t,
            None => return Err(ManageError::NothingToRemove(path.join("."))),
        };
    }
    if table.remove(leaf).is_none() {
        return Err(ManageError::NothingToRemove(path.join(".")));
    }
    // A table left with nothing in it goes too, up to the root: an empty
    // `[hosts.home]` would not load, and an empty `[client]` is only noise.
    for depth in (1..=parents.len()).rev() {
        let mut t: &mut dyn TableLike = doc.as_table_mut();
        for seg in &parents[..depth - 1] {
            t = match t.get_mut(seg).and_then(Item::as_table_like_mut) {
                Some(t) => t,
                None => return Ok(()),
            };
        }
        let name = &parents[depth - 1];
        let empty = t
            .get(name)
            .and_then(Item::as_table_like)
            .is_some_and(TableLike::is_empty);
        if !empty {
            break;
        }
        t.remove(name);
    }
    Ok(())
}
