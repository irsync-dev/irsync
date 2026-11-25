//! `irsync config get/set/unset/list`: any key of the config file by its
//! dotted path, on the config message. Pure on a message; loading and
//! saving belong to the caller.

use super::{id_bytes, ManageError};
use crate::authz::Access;
use crate::config::clients::RIGHTS;
use crate::config::file::Config;
use crate::config::keys;
use crate::config::paths::Paths;
use crate::config::settings::{Congestion, OtlpProtocol, PqKeyExchange, Relay, Spelled};
use crate::config::{duration, reader, writer};
use crate::net::addr::HostPort;
use anyhow::Context;
use irsync_proto::config as pb;
use std::collections::BTreeMap;

pub use crate::config::keys::Kind;

/// The kind of a key path, or `None` for a key the file does not know.
pub fn kind_of(path: &[String]) -> Option<Kind> {
    keys::find(path).map(|k| k.kind)
}

/// The error for a key the file does not know, `key` as typed.
pub(crate) fn unknown_key(key: &str) -> ManageError {
    ManageError::UnknownKey(key.to_string())
}

/// Refuses a key that only irsync sets: `config_version`.
pub fn writable(path: &[String]) -> Result<(), ManageError> {
    if path == ["config_version"] {
        return Err(ManageError::BadValue(
            "config_version is set by irsync".into(),
        ));
    }
    Ok(())
}

/// `KEY` with TOML's key grammar: `exports."www.example.com".path` is three
/// segments.
pub fn parse_key(key: &str) -> Result<Vec<String>, ManageError> {
    let bad = || ManageError::BadKey(key.to_string());
    let keys = reader::dotted_key(key).ok_or_else(bad)?;
    if keys.is_empty() || keys.iter().any(String::is_empty) {
        return Err(bad());
    }
    Ok(keys)
}

/// `KEY` as `config get` takes it: parsed (see [`parse_key`]) and known
/// (see [`kind_of`]).
pub fn key_path(key: &str) -> Result<Vec<String>, ManageError> {
    let path = parse_key(key)?;
    if kind_of(&path).is_none() {
        return Err(unknown_key(key));
    }
    Ok(path)
}

/// The lines `config get` prints for `path`: its value, else its
/// default; `None` for a key unset and without a default. A key of a
/// server, client or export that is not there has no default either.
pub fn get_or_default(
    msg: &pb::Config,
    path: &[String],
) -> Result<Option<Vec<String>>, ManageError> {
    Ok(match get(msg, path)? {
        Got::Lines(lines) => Some(lines),
        Got::Unset => {
            let entry = matches!(
                path.first().map(String::as_str),
                Some("servers" | "clients" | "exports")
            );
            if entry && path.len() > 2 && !has_table(msg, &path[..2]) {
                None
            } else {
                default_of(path)
            }
        }
    })
}

/// The default of a key, as `get` prints it (one element per line);
/// `None` when the key has no default or is not a key. A server's keys
/// have none of their own: they fall back to `[defaults]`'s.
pub fn default_of(path: &[String]) -> Option<Vec<String>> {
    keys::find(path)?.default.map(|default| default())
}

/// What `get` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Got {
    /// The lines to print.
    Lines(Vec<String>),
    /// The key is known but not set.
    Unset,
}

/// A value for `set`, read by [`parse_value`] in the key's kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Text(String),
    List(Vec<String>),
}

/// `VALUE...` read in the key's kind: `true`/`false`, an integer, a
/// duration as the file spells it (bare seconds as an integer), a path, a
/// URL, an EndpointId (kept as typed), or, for an array kind, one element
/// per value (none: an empty array). A scalar kind takes exactly one
/// value; a table cannot be set.
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
        for v in values {
            check(v).map_err(bad)?;
        }
        Ok(Value::List(values.to_vec()))
    };
    Ok(match kind {
        Kind::Bool => match one()? {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            v => return Err(bad(format!("expected true or false, got {v:?}"))),
        },
        Kind::Int => {
            let v = one()?;
            Value::Int(
                v.parse::<i64>()
                    .map_err(|_| bad(format!("expected an integer, got {v:?}")))?,
            )
        }
        Kind::Duration => {
            let v = one()?;
            duration::parse(v).map_err(|e| bad(e.to_string()))?;
            // Bare seconds are a number in the file, as the parser reads them.
            match v.parse::<i64>() {
                Ok(secs) => Value::Int(secs),
                Err(_) => Value::Text(v.to_string()),
            }
        }
        Kind::Str | Kind::Path => Value::Text(one()?.to_string()),
        Kind::Relay => {
            let v = one()?;
            v.parse::<Relay>().map_err(bad)?;
            Value::Text(v.to_string())
        }
        Kind::Url => {
            let v = one()?;
            url::Url::parse(v).map_err(|e| bad(format!("expected a URL: {e}")))?;
            Value::Text(v.to_string())
        }
        Kind::Id => {
            let v = one()?;
            crate::identity::parse_endpoint_id(v).map_err(|e| bad(e.to_string()))?;
            Value::Text(v.to_string())
        }
        Kind::StrArray => strings(&|_| Ok(()))?,
        Kind::AddrArray => strings(&|v| {
            v.parse::<HostPort>()
                .map(|_| ())
                .map_err(|e| format!("expected host:port, got {v:?}: {e}"))
        })?,
        Kind::Table => return Err(bad("a table cannot be set; set its keys".into())),
    })
}

/// The value of `path`: a bare scalar, one array element per line, or the
/// keys of a table as `key = value` lines. An unknown key is an error.
pub fn get(msg: &pb::Config, path: &[String]) -> Result<Got, ManageError> {
    let kind = kind_of(path).ok_or_else(|| unknown(path))?;
    if kind == Kind::Table {
        if !has_table(msg, path) {
            return Ok(Got::Unset);
        }
        return Ok(Got::Lines(key_lines(msg, path)));
    }
    let mut m = msg.clone();
    Ok(
        match slot(&mut m, path, false).and_then(|s| shown(&s.field)) {
            Some(Shown { lines, .. }) => Got::Lines(lines),
            None => Got::Unset,
        },
    )
}

/// Every key the message sets, as `dotted.key = value` in the order the
/// writer writes them.
pub fn list(msg: &pb::Config) -> Vec<String> {
    key_lines(msg, &[])
}

/// Sets `path` to `value`, keeping the key's comments and the rest of the
/// message; a missing table or host or export is created, a new one last.
/// A table key, an unknown key, or a value the key cannot hold is an
/// error, and leaves the message as it was.
pub fn set(msg: &mut pb::Config, path: &[String], value: Value) -> Result<(), ManageError> {
    writable(path)?;
    match kind_of(path) {
        None => return Err(unknown(path)),
        Some(Kind::Table) => {
            return Err(ManageError::BadValue(format!(
                "{} is a table; set its keys",
                path.join(".")
            )))
        }
        Some(_) => {}
    }
    let mut m = msg.clone();
    let slot = slot(&mut m, path, true).ok_or_else(|| unknown(path))?;
    assign(slot.field, value).map_err(ManageError::BadValue)?;
    *msg = m;
    Ok(())
}

/// Removes `path`: a key with its comments, or a whole table with its
/// comments. A table the removal leaves without keys goes too, as an
/// empty `[servers.home]` would not load and an empty `[defaults]` is only
/// noise. Absent: `NothingToRemove`.
pub fn unset(msg: &mut pb::Config, path: &[String]) -> Result<(), ManageError> {
    let kind = kind_of(path).ok_or_else(|| unknown(path))?;
    writable(path)?;
    let nothing = || ManageError::NothingToRemove(path.join("."));
    if kind == Kind::Table {
        if !remove_table(msg, path) {
            return Err(nothing());
        }
    } else {
        let slot = slot(msg, path, false).ok_or_else(nothing)?;
        if !clear(slot.field) {
            return Err(nothing());
        }
        slot.comments
            .remove(path.last().expect("parse_key refuses an empty key"));
    }
    prune(msg, path);
    Ok(())
}

fn unknown(path: &[String]) -> ManageError {
    let key: Vec<String> = path.iter().map(|s| writer::key(s)).collect();
    unknown_key(&key.join("."))
}

/// One key's field in the message, by how it holds the value.
enum Field<'a> {
    Bool(&'a mut Option<bool>),
    Int(&'a mut Option<u32>),
    Duration(&'a mut Option<prost_types::Duration>),
    /// A string, a path or a debug address, as written.
    Text(&'a mut Option<String>),
    Url(&'a mut Option<String>),
    /// A relay as written: `default`, `disabled` or a URL.
    Relay(&'a mut Option<String>),
    /// An id's 32 bytes; empty is unset.
    Id(&'a mut Vec<u8>),
    /// An enum key, with its spellings.
    Spelled(&'a mut Option<i32>, Spelling),
    /// A client's right: `rw` or `ro`.
    Right(&'a mut Option<i32>),
    /// A string array and whether its key is in the file.
    List(&'a mut Vec<pb::Entry>, &'a mut bool),
    Addresses(&'a mut Vec<pb::Entry>, &'a mut bool),
    /// An export's path; "" is unset.
    ExportPath(&'a mut String),
}

/// How an enum key is spelled: [`Spelled`] for one type, as plain
/// functions.
#[derive(Clone, Copy)]
struct Spelling {
    show: fn(i32) -> Option<&'static str>,
    parse: fn(&str) -> Result<i32, String>,
}

fn spelling<T: Spelled>() -> Spelling {
    Spelling {
        show: writer::spelled::<T>,
        parse: |s| s.parse::<T>().map(T::to_message),
    }
}

/// A key's field with the comments of its table.
struct Slot<'a> {
    field: Field<'a>,
    comments: &'a mut BTreeMap<String, pb::Comment>,
}

/// The table in `t`, created when `create`.
fn table<T: Default>(t: &mut Option<T>, create: bool) -> Option<&mut T> {
    if create {
        Some(t.get_or_insert_with(T::default))
    } else {
        t.as_mut()
    }
}

fn server<'a>(
    msg: &'a mut pb::Config,
    name: &str,
    create: bool,
) -> Option<&'a mut pb::ServerEntry> {
    let i = match msg.servers.iter().position(|s| s.name == name) {
        Some(i) => i,
        None if create => {
            msg.servers.push(pb::ServerEntry {
                name: name.to_string(),
                ..Default::default()
            });
            msg.servers.len() - 1
        }
        None => return None,
    };
    Some(&mut msg.servers[i])
}

fn client<'a>(
    msg: &'a mut pb::Config,
    name: &str,
    create: bool,
) -> Option<&'a mut pb::ClientEntry> {
    let i = match msg.clients.iter().position(|c| c.name == name) {
        Some(i) => i,
        None if create => {
            msg.clients.push(pb::ClientEntry {
                name: name.to_string(),
                ..Default::default()
            });
            msg.clients.len() - 1
        }
        None => return None,
    };
    Some(&mut msg.clients[i])
}

fn export<'a>(msg: &'a mut pb::Config, name: &str, create: bool) -> Option<&'a mut pb::Export> {
    let i = match msg.exports.iter().position(|e| e.name == name) {
        Some(i) => i,
        None if create => {
            msg.exports.push(pb::Export {
                name: name.to_string(),
                ..Default::default()
            });
            msg.exports.len() - 1
        }
        None => return None,
    };
    Some(&mut msg.exports[i])
}

/// The field of the key `path` (not a table), its tables created on the
/// way when `create`. `None` when a table is missing and not created,
/// or for a key the message does not have.
fn slot<'a>(msg: &'a mut pb::Config, path: &[String], create: bool) -> Option<Slot<'a>> {
    use Field::*;
    let parts: Vec<&str> = path.iter().map(String::as_str).collect();
    let (field, comments) = match parts.as_slice() {
        [key] => {
            let pb::Config {
                config_version,
                unsafe_config_allowed,
                state_dir,
                key_comments,
                ..
            } = msg;
            let f = match *key {
                "config_version" => Int(config_version),
                "unsafe_config_allowed" => Bool(unsafe_config_allowed),
                "state_dir" => Text(state_dir),
                _ => return None,
            };
            (f, key_comments)
        }
        ["defaults", key] => {
            let c = table(&mut msg.defaults, create)?;
            let f = match *key {
                "identity" => Text(&mut c.identity),
                "parallel" => Int(&mut c.parallel),
                "reconnect" => Duration(&mut c.reconnect),
                "dial_retries" => Int(&mut c.dial_retries),
                "dht" => Bool(&mut c.dht),
                "dns" => Bool(&mut c.dns),
                "ssh_install" => Bool(&mut c.ssh_install),
                "mdns" => Bool(&mut c.mdns),
                "relay" => Relay(&mut c.relay),
                "congestion" => Spelled(&mut c.congestion, spelling::<Congestion>()),
                "pq_key_exchange" => Spelled(&mut c.pq_key_exchange, spelling::<PqKeyExchange>()),
                "idle_timeout" => Duration(&mut c.idle_timeout),
                _ => return None,
            };
            (f, &mut c.key_comments)
        }
        ["daemon", key] => {
            let d = table(&mut msg.daemon, create)?;
            let f = match *key {
                "identity" => Text(&mut d.identity),
                "idle_timeout" => Duration(&mut d.idle_timeout),
                "dormant_session_timeout" => Duration(&mut d.dormant_session_timeout),
                "max_dormant_sessions_per_key" => Int(&mut d.max_dormant_sessions_per_key),
                "listen" => Addresses(&mut d.listen, &mut d.listen_set),
                "portmap" => Bool(&mut d.portmap),
                "relay" => Relay(&mut d.relay),
                "dht" => Bool(&mut d.dht),
                "dns" => Bool(&mut d.dns),
                "mdns" => Bool(&mut d.mdns),
                "congestion" => Spelled(&mut d.congestion, spelling::<Congestion>()),
                "pq_key_exchange" => Spelled(&mut d.pq_key_exchange, spelling::<PqKeyExchange>()),
                "debug_addr" => Text(&mut d.debug_addr),
                "allow_all_paths" => Bool(&mut d.allow_all_paths),
                "allow_root" => Bool(&mut d.allow_root),
                _ => return None,
            };
            (f, &mut d.key_comments)
        }
        ["telemetry", key] => {
            let t = table(&mut msg.telemetry, create)?;
            let f = match *key {
                "otlp_endpoint" => Url(&mut t.otlp_endpoint),
                "otlp_protocol" => Spelled(&mut t.otlp_protocol, spelling::<OtlpProtocol>()),
                _ => return None,
            };
            (f, &mut t.key_comments)
        }
        ["servers", name, key] => {
            let h = server(msg, name, create)?;
            let f = match *key {
                "endpoint_id" => Id(&mut h.endpoint_id),
                "relay" => Relay(&mut h.relay),
                "congestion" => Spelled(&mut h.congestion, spelling::<Congestion>()),
                "pq_key_exchange" => Spelled(&mut h.pq_key_exchange, spelling::<PqKeyExchange>()),
                "address" => Addresses(&mut h.address, &mut h.address_set),
                "parallel" => Int(&mut h.parallel),
                "reconnect" => Duration(&mut h.reconnect),
                "dial_retries" => Int(&mut h.dial_retries),
                "dht" => Bool(&mut h.dht),
                "dns" => Bool(&mut h.dns),
                "mdns" => Bool(&mut h.mdns),
                "ssh" => Text(&mut h.ssh),
                "ssh_aliases" => List(&mut h.ssh_aliases, &mut h.ssh_aliases_set),
                _ => return None,
            };
            (f, &mut h.key_comments)
        }
        ["clients", name, "client_id"] => {
            let c = client(msg, name, create)?;
            (Id(&mut c.client_id), &mut c.key_comments)
        }
        ["clients", name, key] => {
            let right = RIGHTS.into_iter().find(|r| r.key() == *key)?;
            let c = client(msg, name, create)?;
            let (field, comments) = right.field_mut(c);
            (Right(field), comments)
        }
        ["exports", name, key] => {
            let e = export(msg, name, create)?;
            let f = match *key {
                "path" => ExportPath(&mut e.path),
                "read_only" => Bool(&mut e.read_only),
                "index_at_start" => Bool(&mut e.index_at_start),
                "fsync" => Bool(&mut e.fsync),
                "auto" => Bool(&mut e.auto),
                "rw" => List(&mut e.rw, &mut e.rw_set),
                "ro" => List(&mut e.ro, &mut e.ro_set),
                _ => return None,
            };
            (f, &mut e.key_comments)
        }
        _ => return None,
    };
    Some(Slot { field, comments })
}

/// The path of every key the message could hold, in the writer's order:
/// [`keys::KEYS`], with each `*` table once for every entry of its group.
fn key_paths(msg: &pb::Config) -> Vec<Vec<String>> {
    let names = |group: &str| -> Vec<String> {
        match group {
            "servers" => msg.servers.iter().map(|s| s.name.clone()).collect(),
            "clients" => msg.clients.iter().map(|c| c.name.clone()).collect(),
            "exports" => msg.exports.iter().map(|e| e.name.clone()).collect(),
            _ => Vec::new(),
        }
    };
    let leaves: Vec<&keys::Key> = keys::KEYS
        .iter()
        .filter(|k| k.kind != Kind::Table)
        .collect();
    let mut out = Vec::new();
    // The keys of one table are next to each other in KEYS.
    for run in leaves.chunk_by(|a, b| a.table() == b.table()) {
        match run[0].table().strip_suffix(".*") {
            Some(group) => {
                for name in names(group) {
                    for k in run {
                        out.push(vec![group.to_string(), name.clone(), k.name().to_string()]);
                    }
                }
            }
            None => out.extend(
                run.iter()
                    .map(|k| k.path.split('.').map(String::from).collect()),
            ),
        }
    }
    out
}

/// `key = value` for every key set under `prefix`, the key dotted from
/// below `prefix`, segments quoted when not bare.
fn key_lines(msg: &pb::Config, prefix: &[String]) -> Vec<String> {
    let mut m = msg.clone();
    key_paths(msg)
        .into_iter()
        .filter(|p| p.starts_with(prefix))
        .filter_map(|p| {
            let toml = shown(&slot(&mut m, &p, false)?.field)?.toml;
            let name: Vec<String> = p[prefix.len()..].iter().map(|s| writer::key(s)).collect();
            Some(format!("{} = {toml}", name.join(".")))
        })
        .collect()
}

/// Whether the table `path` is in the message; `servers`, `clients`
/// and `exports` are when they have an entry.
fn has_table(msg: &pb::Config, path: &[String]) -> bool {
    let parts: Vec<&str> = path.iter().map(String::as_str).collect();
    match parts.as_slice() {
        ["defaults"] => msg.defaults.is_some(),
        ["daemon"] => msg.daemon.is_some(),
        ["telemetry"] => msg.telemetry.is_some(),
        ["servers"] => !msg.servers.is_empty(),
        ["servers", name] => msg.servers.iter().any(|s| s.name == *name),
        ["clients"] => !msg.clients.is_empty(),
        ["clients", name] => msg.clients.iter().any(|c| c.name == *name),
        ["exports"] => !msg.exports.is_empty(),
        ["exports", name] => msg.exports.iter().any(|e| e.name == *name),
        _ => false,
    }
}

/// Removes the table `path` with its comments; false when it is not there.
fn remove_table(msg: &mut pb::Config, path: &[String]) -> bool {
    let parts: Vec<&str> = path.iter().map(String::as_str).collect();
    match parts.as_slice() {
        ["defaults"] => msg.defaults.take().is_some(),
        ["daemon"] => msg.daemon.take().is_some(),
        ["telemetry"] => msg.telemetry.take().is_some(),
        ["servers"] => !std::mem::take(&mut msg.servers).is_empty(),
        ["servers", name] => {
            let before = msg.servers.len();
            msg.servers.retain(|s| s.name != *name);
            msg.servers.len() < before
        }
        ["clients"] => !std::mem::take(&mut msg.clients).is_empty(),
        ["clients", name] => {
            let before = msg.clients.len();
            msg.clients.retain(|c| c.name != *name);
            msg.clients.len() < before
        }
        ["exports"] => !std::mem::take(&mut msg.exports).is_empty(),
        ["exports", name] => {
            let before = msg.exports.len();
            msg.exports.retain(|e| e.name != *name);
            msg.exports.len() < before
        }
        _ => false,
    }
}

/// True when `$v`, a `pb::$T`, sets no key: only its comments, and the
/// named fields that are no key, are left.
macro_rules! keyless {
    ($v:expr, $T:ident $(, $f:ident)*) => {
        *$v == pb::$T {
            comment: $v.comment.clone(),
            key_comments: $v.key_comments.clone(),
            tail: $v.tail.clone(),
            $($f: $v.$f.clone(),)*
            ..Default::default()
        }
    };
}

/// Drops `t` when it is there and `empty` says it sets no key.
fn drop_if<T>(t: &mut Option<T>, empty: impl Fn(&T) -> bool) {
    if t.as_ref().is_some_and(empty) {
        *t = None;
    }
}

/// After a removal at `path`: drops the tables above it that are left
/// without keys, innermost first.
fn prune(msg: &mut pb::Config, path: &[String]) {
    let parts: Vec<&str> = path.iter().map(String::as_str).collect();
    match parts.as_slice() {
        ["defaults", _] => drop_if(&mut msg.defaults, |c| keyless!(c, Defaults)),
        ["daemon", _] => drop_if(&mut msg.daemon, |d| keyless!(d, Daemon)),
        ["telemetry", _] => drop_if(&mut msg.telemetry, |t| keyless!(t, Telemetry)),
        ["servers", name, _] => msg
            .servers
            .retain(|s| s.name != *name || !keyless!(s, ServerEntry, name)),
        ["clients", name, _] => msg
            .clients
            .retain(|c| c.name != *name || !keyless!(c, ClientEntry, name)),
        ["exports", name, _] => msg
            .exports
            .retain(|e| e.name != *name || !keyless!(e, Export, name)),
        _ => {}
    }
}

/// A set value: as `get` prints it (strings bare, one array element per
/// line), and as the writer writes it.
struct Shown {
    lines: Vec<String>,
    toml: String,
}

/// The value of `field`; `None` when it is not set.
fn shown(field: &Field) -> Option<Shown> {
    let plain = |s: String| Shown {
        lines: vec![s.clone()],
        toml: s,
    };
    let text = |s: &str| Shown {
        lines: vec![s.to_string()],
        toml: writer::quoted(s),
    };
    let list = |entries: &[pb::Entry], set: bool| {
        (set || !entries.is_empty()).then(|| {
            let values: Vec<String> = entries.iter().map(|e| e.value.clone()).collect();
            let quoted: Vec<String> = values.iter().map(|v| writer::quoted(v)).collect();
            Shown {
                lines: values,
                toml: format!("[{}]", quoted.join(", ")),
            }
        })
    };
    match field {
        Field::Bool(v) => v.map(|b| plain(b.to_string())),
        Field::Int(v) => v.map(|n| plain(n.to_string())),
        Field::Duration(v) => v.as_ref().map(|d| text(&writer::duration_text(d))),
        Field::Text(v) | Field::Url(v) | Field::Relay(v) => v.as_deref().map(text),
        Field::Id(v) => (!v.is_empty()).then(|| text(&writer::id(v))),
        Field::Spelled(v, s) => v.and_then(s.show).map(text),
        Field::Right(v) => v.and_then(Access::from_message).map(|a| text(a.as_str())),
        Field::List(v, set) | Field::Addresses(v, set) => list(v, **set),
        Field::ExportPath(p) => (!p.is_empty()).then(|| text(p)),
    }
}

/// Clears `field`; false when it was not set.
fn clear(field: Field) -> bool {
    match field {
        Field::Bool(v) => v.take().is_some(),
        Field::Int(v) => v.take().is_some(),
        Field::Duration(v) => v.take().is_some(),
        Field::Text(v) | Field::Url(v) | Field::Relay(v) => v.take().is_some(),
        Field::Id(v) => !std::mem::take(v).is_empty(),
        Field::Spelled(v, _) | Field::Right(v) => v.take().is_some(),
        Field::List(v, set) | Field::Addresses(v, set) => {
            let was = *set || !v.is_empty();
            v.clear();
            *set = false;
            was
        }
        Field::ExportPath(p) => !std::mem::take(p).is_empty(),
    }
}

fn entries(values: Vec<String>) -> Vec<pb::Entry> {
    values
        .into_iter()
        .map(|value| pb::Entry {
            value,
            comment: None,
        })
        .collect()
}

/// An integer key holds a `u32`; the message for one out of range.
fn uint(n: i64) -> Result<u32, String> {
    u32::try_from(n).map_err(|_| format!("invalid value: integer `{n}`, expected u32"))
}

/// Stores `value` in `field`, as the reader would have stored it.
fn assign(field: Field, value: Value) -> Result<(), String> {
    match (field, value) {
        (Field::Bool(f), Value::Bool(b)) => *f = Some(b),
        (Field::Int(f), Value::Int(n)) => *f = Some(uint(n)?),
        (Field::Duration(f), v @ (Value::Int(_) | Value::Text(_))) => {
            let d = match v {
                Value::Int(n) => std::time::Duration::from_secs(
                    u64::try_from(n).map_err(|_| format!("bad duration {n:?}"))?,
                ),
                Value::Text(s) => duration::parse(&s).map_err(|e| e.to_string())?,
                _ => unreachable!("matched above"),
            };
            *f = Some(duration::to_proto(d));
        }
        (Field::Text(f), Value::Text(s)) => *f = Some(s),
        (Field::Relay(f), Value::Text(s)) => {
            s.parse::<Relay>()?;
            *f = Some(s);
        }
        (Field::Url(f), Value::Text(s)) => {
            url::Url::parse(&s).map_err(|e| format!("expected a URL: {e}"))?;
            *f = Some(s);
        }
        (Field::Id(f), Value::Text(s)) => {
            let id = crate::identity::parse_endpoint_id(&s).map_err(|e| e.to_string())?;
            *f = id_bytes(id);
        }
        (Field::Spelled(f, spelling), Value::Text(s)) => *f = Some((spelling.parse)(&s)?),
        (Field::Right(f), Value::Text(s)) => {
            let access = Access::parse(&s).ok_or("expected \"rw\" or \"ro\"")?;
            *f = Some(pb::Access::from(access) as i32);
        }
        (Field::List(v, set), Value::List(items)) => {
            *v = entries(items);
            *set = true;
        }
        (Field::Addresses(v, set), Value::List(items)) => {
            let addrs = items
                .iter()
                .map(|a| {
                    a.parse::<HostPort>()
                        .map(|hp| hp.to_string())
                        .map_err(|e| format!("expected host:port, got {a:?}: {e}"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            *v = entries(addrs);
            *set = true;
        }
        (Field::ExportPath(p), Value::Text(s)) => *p = s,
        (field, value) => {
            return Err(format!("expected {}, got {value:?}", expected(&field)));
        }
    }
    Ok(())
}

/// What a field takes, for the message of a value of the wrong kind.
fn expected(field: &Field) -> &'static str {
    match field {
        Field::Bool(_) => "true or false",
        Field::Int(_) => "an integer",
        Field::Duration(_) => "a duration",
        Field::List(..) | Field::Addresses(..) => "a list",
        _ => "a string",
    }
}

// The commands that change the file, from its lock to the notes they say.

/// `config set KEY VALUE...`; notes on grants that now apply to another
/// id (see [`super::clients::moved_grant_notes`]).
pub fn set_in_file(paths: &Paths, key: &str, values: &[String]) -> anyhow::Result<Vec<String>> {
    let path = parse_key(key)?;
    let kind = kind_of(&path).ok_or_else(|| unknown_key(key))?;
    writable(&path)?;
    let value = parse_value(kind, values).with_context(|| key.to_string())?;
    let mut lock = super::lock_config(&paths.config)?;
    let mut msg = lock.load()?;
    let before = super::clients::client_ids(&msg, &paths.config);
    set(&mut msg, &path, value).with_context(|| key.to_string())?;
    lock.save(&mut msg).with_context(|| key.to_string())?;
    Ok(super::clients::moved_grant_notes(
        "config set",
        &before,
        &msg,
        &paths.config,
    ))
}

/// `config unset KEY`; notes as for [`set_in_file`].
pub fn unset_in_file(paths: &Paths, key: &str) -> anyhow::Result<Vec<String>> {
    let path = parse_key(key)?;
    if kind_of(&path).is_none() {
        return Err(unknown_key(key).into());
    }
    writable(&path)?;
    let mut lock = super::lock_config(&paths.config)?;
    let mut msg = lock.load()?;
    let before = super::clients::client_ids(&msg, &paths.config);
    // Unsetting a client's table or its id removes the client:
    // like `client remove`, not while grants name it. A server
    // goes whatever the grants say: they name clients only.
    let clients = if path[0] == "clients" {
        // A file that does not load is that error, not a
        // refused save.
        Config::load(paths)?;
        match path.get(1) {
            Some(name) => super::canonicalize_without(&mut msg, &paths.config, name)?,
            None => super::canonicalize_doc(&mut msg, &paths.config)?,
        };
        super::clients::client_names(&msg)
    } else {
        Vec::new()
    };
    unset(&mut msg, &path).with_context(|| key.to_string())?;
    let left = super::clients::client_names(&msg);
    for name in clients.iter().filter(|a| !left.contains(a)) {
        let lists = super::grants::lists_naming(&msg, name);
        if !lists.is_empty() {
            anyhow::bail!(
                "client {name:?} is named by grants in {}: remove those grants \
                 first, or use `irsync client remove {name} --force`",
                lists.join(", ")
            );
        }
    }
    lock.save(&mut msg).with_context(|| key.to_string())?;
    Ok(super::clients::moved_grant_notes(
        "config unset",
        &before,
        &msg,
        &paths.config,
    ))
}
