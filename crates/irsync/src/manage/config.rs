//! `irsync config get/set/unset/list`: any key of the config file by its
//! dotted path, on the config message. Pure on a message; loading and
//! saving belong to the caller.

use super::{id_bytes, ManageError};
use crate::config::settings::{RelayMode, Settings};
use crate::config::{duration, reader, writer};
use crate::net::addr::HostPort;
use irsync_proto::config as pb;
use std::collections::BTreeMap;

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
        // Read-only: see [`writable`].
        ["config_version"] => Int,
        ["unsafe_config_allowed"] => Bool,
        ["identity"] | ["state_dir"] => Path,
        ["client"]
        | ["daemon"]
        | ["daemon", "discovery"]
        | ["telemetry"]
        | ["servers"]
        | ["servers", _]
        | ["clients"]
        | ["clients", _]
        | ["exports"]
        | ["exports", _]
        | ["all"]
        | ["immediate"] => Table,
        ["client", "parallel"] => Int,
        ["client", "reconnect"] => Duration,
        ["client", "dial_retries"] => Int,
        ["client", "dht" | "mdns" | "ssh_install"] => Bool,
        ["client", "relay"] => Url,
        ["client", "relay_mode" | "congestion" | "pq_key_exchange"] => Str,
        ["daemon", "identity"] => Path,
        ["daemon", "idle_timeout"] | ["daemon", "dormant_session_timeout"] => Duration,
        ["daemon", "max_dormant_sessions_per_key"] => Int,
        ["daemon", "listen"] => AddrArray,
        ["daemon", "portmap"] | ["daemon", "immediate"] | ["daemon", "allow_root"] => Bool,
        ["daemon", "relay"] => Url,
        ["daemon", "relay_mode" | "congestion" | "pq_key_exchange"] => Str,
        ["daemon", "debug_addr"] => Str,
        ["telemetry", "otlp_endpoint"] => Url,
        ["telemetry", "otlp_protocol"] => Str,
        ["daemon", "discovery", "dht" | "dns" | "mdns"] => Bool,
        ["servers", _, "endpoint_id"] | ["clients", _, "client_id"] => Id,
        ["servers", _, "relay"] => Url,
        ["servers", _, "relay_mode" | "congestion" | "pq_key_exchange" | "ssh"] => Str,
        ["servers", _, "address"] => AddrArray,
        ["servers", _, "ssh_aliases"] => StrArray,
        ["servers", _, "parallel"] => Int,
        ["servers", _, "reconnect"] => Duration,
        ["servers", _, "dial_retries"] => Int,
        ["servers", _, "dht" | "mdns"] => Bool,
        ["exports", _, "path"] => Path,
        ["exports", _, "read_only" | "warm_index" | "fsync" | "auto"] => Bool,
        ["exports", _, "rw" | "ro"] | ["all", "rw" | "ro"] | ["immediate", "rw"] => StrArray,
        _ => return None,
    })
}

/// The error for a key the file does not know, `key` as typed. The keys
/// of the old `[hosts]` table get a pointer to their new places.
pub fn unknown_key(key: &str) -> ManageError {
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

/// A default path as the file would hold it: the home directory as `~`.
fn as_written(p: &std::path::Path) -> String {
    match dirs::home_dir().and_then(|h| p.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => p.display().to_string(),
    }
}

/// The default of a settings key, as `get` prints it (one element per
/// line); `None` when the key has no default or is not a settings key.
pub fn default_of(path: &[String]) -> Option<Vec<String>> {
    let parts: Vec<&str> = path.iter().map(String::as_str).collect();
    let s = Settings::default();
    let one = |v: String| Some(vec![v]);
    match parts.as_slice() {
        ["unsafe_config_allowed"] => one(s.unsafe_config_allowed.to_string()),
        ["identity"] => one(as_written(&s.identity)),
        // The effective [client] defaults, as book/src/reference/configuration.md lists them.
        ["client", "parallel"] => one(crate::send::DEFAULT_PARALLEL.to_string()),
        ["client", "reconnect"] => one(duration::format(
            crate::session::reconnect::ReconnectOpts::default().max,
        )),
        ["client", "dial_retries"] => one(crate::session::reconnect::ReconnectOpts::default()
            .dial_retries
            .to_string()),
        ["client", "dht"] | ["client", "mdns"] | ["client", "ssh_install"] => {
            one("true".to_string())
        }
        ["client", "relay_mode"] => one("default".to_string()),
        ["client", "congestion"] => one(crate::config::settings::Congestion::default()
            .as_str()
            .into()),
        ["client", "pq_key_exchange"] => one(crate::config::settings::PqKeyExchange::default()
            .as_str()
            .into()),
        ["state_dir"] => one(as_written(&s.state_dir)),
        ["daemon", "identity"] => one(as_written(&s.daemon.identity)),
        ["daemon", "idle_timeout"] => one(duration::format(s.daemon.idle_timeout)),
        ["daemon", "dormant_session_timeout"] => {
            one(duration::format(s.daemon.dormant_session_timeout))
        }
        ["daemon", "max_dormant_sessions_per_key"] => {
            one(s.daemon.max_dormant_sessions_per_key.to_string())
        }
        ["daemon", "listen"] => Some(s.daemon.listen.iter().map(|a| a.to_string()).collect()),
        ["daemon", "portmap"] => one(s.daemon.portmap.to_string()),
        ["daemon", "relay_mode"] => one(match s.daemon.relay_mode {
            RelayMode::Default => "default",
            RelayMode::Disabled => "disabled",
        }
        .to_string()),
        ["daemon", "congestion"] => one(s.daemon.congestion.as_str().to_string()),
        ["daemon", "pq_key_exchange"] => one(s.daemon.pq_key_exchange.as_str().to_string()),
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
/// empty `[servers.home]` would not load and an empty `[client]` is only
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
    /// An id's 32 bytes; empty is unset.
    Id(&'a mut Vec<u8>),
    RelayMode(&'a mut Option<i32>),
    Congestion(&'a mut Option<i32>),
    PqKeyExchange(&'a mut Option<i32>),
    OtlpProtocol(&'a mut Option<i32>),
    /// A string array and whether its key is in the file.
    List(&'a mut Vec<pb::Entry>, &'a mut bool),
    Addresses(&'a mut Vec<pb::Entry>, &'a mut bool),
    /// An export's path; "" is unset.
    ExportPath(&'a mut String),
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
                identity,
                state_dir,
                key_comments,
                ..
            } = msg;
            let f = match *key {
                "config_version" => Int(config_version),
                "unsafe_config_allowed" => Bool(unsafe_config_allowed),
                "identity" => Text(identity),
                "state_dir" => Text(state_dir),
                _ => return None,
            };
            (f, key_comments)
        }
        ["client", key] => {
            let c = table(&mut msg.client, create)?;
            let f = match *key {
                "parallel" => Int(&mut c.parallel),
                "reconnect" => Duration(&mut c.reconnect),
                "dial_retries" => Int(&mut c.dial_retries),
                "dht" => Bool(&mut c.dht),
                "ssh_install" => Bool(&mut c.ssh_install),
                "mdns" => Bool(&mut c.mdns),
                "relay" => Url(&mut c.relay),
                "relay_mode" => RelayMode(&mut c.relay_mode),
                "congestion" => Congestion(&mut c.congestion),
                "pq_key_exchange" => PqKeyExchange(&mut c.pq_key_exchange),
                _ => return None,
            };
            (f, &mut c.key_comments)
        }
        ["daemon", "discovery", key] => {
            let d = table(&mut msg.daemon, create)?;
            let x = table(&mut d.discovery, create)?;
            let f = match *key {
                "dht" => Bool(&mut x.dht),
                "dns" => Bool(&mut x.dns),
                "mdns" => Bool(&mut x.mdns),
                _ => return None,
            };
            (f, &mut x.key_comments)
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
                "relay" => Url(&mut d.relay),
                "relay_mode" => RelayMode(&mut d.relay_mode),
                "congestion" => Congestion(&mut d.congestion),
                "pq_key_exchange" => PqKeyExchange(&mut d.pq_key_exchange),
                "debug_addr" => Text(&mut d.debug_addr),
                "immediate" => Bool(&mut d.immediate),
                "allow_root" => Bool(&mut d.allow_root),
                _ => return None,
            };
            (f, &mut d.key_comments)
        }
        ["telemetry", key] => {
            let t = table(&mut msg.telemetry, create)?;
            let f = match *key {
                "otlp_endpoint" => Url(&mut t.otlp_endpoint),
                "otlp_protocol" => OtlpProtocol(&mut t.otlp_protocol),
                _ => return None,
            };
            (f, &mut t.key_comments)
        }
        ["servers", name, key] => {
            let h = server(msg, name, create)?;
            let f = match *key {
                "endpoint_id" => Id(&mut h.endpoint_id),
                "relay" => Url(&mut h.relay),
                "relay_mode" => RelayMode(&mut h.relay_mode),
                "congestion" => Congestion(&mut h.congestion),
                "pq_key_exchange" => PqKeyExchange(&mut h.pq_key_exchange),
                "address" => Addresses(&mut h.address, &mut h.address_set),
                "parallel" => Int(&mut h.parallel),
                "reconnect" => Duration(&mut h.reconnect),
                "dial_retries" => Int(&mut h.dial_retries),
                "dht" => Bool(&mut h.dht),
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
        ["exports", name, key] => {
            let e = export(msg, name, create)?;
            let f = match *key {
                "path" => ExportPath(&mut e.path),
                "read_only" => Bool(&mut e.read_only),
                "warm_index" => Bool(&mut e.warm_index),
                "fsync" => Bool(&mut e.fsync),
                "auto" => Bool(&mut e.auto),
                "rw" => List(&mut e.rw, &mut e.rw_set),
                "ro" => List(&mut e.ro, &mut e.ro_set),
                _ => return None,
            };
            (f, &mut e.key_comments)
        }
        ["all", key] => {
            let g = table(&mut msg.all, create)?;
            let f = match *key {
                "rw" => List(&mut g.rw, &mut g.rw_set),
                "ro" => List(&mut g.ro, &mut g.ro_set),
                _ => return None,
            };
            (f, &mut g.key_comments)
        }
        ["immediate", "rw"] => {
            let g = table(&mut msg.immediate, create)?;
            (List(&mut g.rw, &mut g.rw_set), &mut g.key_comments)
        }
        _ => return None,
    };
    Some(Slot { field, comments })
}

/// The keys of each table, in the order the writer writes them.
const TOP: &[&str] = &[
    "config_version",
    "unsafe_config_allowed",
    "identity",
    "state_dir",
];
const CLIENT: &[&str] = &[
    "parallel",
    "reconnect",
    "dial_retries",
    "dht",
    "ssh_install",
    "mdns",
    "relay",
    "relay_mode",
    "congestion",
    "pq_key_exchange",
];
const DAEMON: &[&str] = &[
    "identity",
    "idle_timeout",
    "dormant_session_timeout",
    "max_dormant_sessions_per_key",
    "listen",
    "portmap",
    "relay",
    "relay_mode",
    "congestion",
    "pq_key_exchange",
    "debug_addr",
    "immediate",
    "allow_root",
];
const DISCOVERY: &[&str] = &["dht", "dns", "mdns"];
const TELEMETRY: &[&str] = &["otlp_endpoint", "otlp_protocol"];
const SERVER: &[&str] = &[
    "endpoint_id",
    "relay",
    "relay_mode",
    "congestion",
    "pq_key_exchange",
    "address",
    "parallel",
    "reconnect",
    "dial_retries",
    "dht",
    "mdns",
    "ssh",
    "ssh_aliases",
];
const CLIENT_ENTRY: &[&str] = &["client_id"];
const EXPORT: &[&str] = &[
    "path",
    "read_only",
    "warm_index",
    "fsync",
    "auto",
    "rw",
    "ro",
];
const GRANTS: &[&str] = &["rw", "ro"];
const IMMEDIATE: &[&str] = &["rw"];

/// The path of every key the message could hold, in the writer's order.
fn key_paths(msg: &pb::Config) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut add = |table: &[&str], keys: &[&str]| {
        for k in keys {
            out.push(table.iter().chain([k]).map(|s| s.to_string()).collect());
        }
    };
    add(&[], TOP);
    add(&["client"], CLIENT);
    add(&["daemon"], DAEMON);
    add(&["daemon", "discovery"], DISCOVERY);
    add(&["telemetry"], TELEMETRY);
    for s in &msg.servers {
        add(&["servers", &s.name], SERVER);
    }
    for c in &msg.clients {
        add(&["clients", &c.name], CLIENT_ENTRY);
    }
    for e in &msg.exports {
        add(&["exports", &e.name], EXPORT);
    }
    add(&["all"], GRANTS);
    add(&["immediate"], IMMEDIATE);
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
        ["client"] => msg.client.is_some(),
        ["daemon"] => msg.daemon.is_some(),
        ["daemon", "discovery"] => msg.daemon.as_ref().is_some_and(|d| d.discovery.is_some()),
        ["telemetry"] => msg.telemetry.is_some(),
        ["servers"] => !msg.servers.is_empty(),
        ["servers", name] => msg.servers.iter().any(|s| s.name == *name),
        ["clients"] => !msg.clients.is_empty(),
        ["clients", name] => msg.clients.iter().any(|c| c.name == *name),
        ["exports"] => !msg.exports.is_empty(),
        ["exports", name] => msg.exports.iter().any(|e| e.name == *name),
        ["all"] => msg.all.is_some(),
        ["immediate"] => msg.immediate.is_some(),
        _ => false,
    }
}

/// Removes the table `path` with its comments; false when it is not there.
fn remove_table(msg: &mut pb::Config, path: &[String]) -> bool {
    let parts: Vec<&str> = path.iter().map(String::as_str).collect();
    match parts.as_slice() {
        ["client"] => msg.client.take().is_some(),
        ["daemon"] => msg.daemon.take().is_some(),
        ["daemon", "discovery"] => msg
            .daemon
            .as_mut()
            .is_some_and(|d| d.discovery.take().is_some()),
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
        ["all"] => msg.all.take().is_some(),
        ["immediate"] => msg.immediate.take().is_some(),
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
        ["client", _] => drop_if(&mut msg.client, |c| keyless!(c, Client)),
        ["daemon", rest @ ..] if !rest.is_empty() => {
            if rest.len() > 1 {
                if let Some(d) = msg.daemon.as_mut() {
                    drop_if(&mut d.discovery, |x| keyless!(x, Discovery));
                }
            }
            drop_if(&mut msg.daemon, |d| keyless!(d, Daemon));
        }
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
        ["all", _] => drop_if(&mut msg.all, |g| keyless!(g, GrantTable)),
        ["immediate", _] => drop_if(&mut msg.immediate, |g| keyless!(g, GrantTable)),
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
        Field::Text(v) | Field::Url(v) => v.as_deref().map(text),
        Field::Id(v) => (!v.is_empty()).then(|| text(&writer::id(v))),
        Field::RelayMode(v) => v.and_then(writer::relay_mode).map(text),
        Field::Congestion(v) => v.and_then(writer::congestion).map(text),
        Field::PqKeyExchange(v) => v.and_then(writer::pq_key_exchange).map(text),
        Field::OtlpProtocol(v) => v.and_then(writer::otlp_protocol).map(text),
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
        Field::Text(v) | Field::Url(v) => v.take().is_some(),
        Field::Id(v) => !std::mem::take(v).is_empty(),
        Field::RelayMode(v)
        | Field::Congestion(v)
        | Field::PqKeyExchange(v)
        | Field::OtlpProtocol(v) => v.take().is_some(),
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

/// One of two spellings of an enum key, in serde's words when it is
/// neither, as loading the file said it.
fn variant(s: &str, a: (&str, i32), b: (&str, i32)) -> Result<i32, String> {
    match s {
        _ if s == a.0 => Ok(a.1),
        _ if s == b.0 => Ok(b.1),
        _ => Err(format!(
            "unknown variant `{s}`, expected `{}` or `{}`",
            a.0, b.0
        )),
    }
}

/// An integer key holds a `u32`; the message for one out of range, in
/// serde's words, as loading the file said it.
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
        (Field::Url(f), Value::Text(s)) => {
            url::Url::parse(&s).map_err(|e| format!("expected a URL: {e}"))?;
            *f = Some(s);
        }
        (Field::Id(f), Value::Text(s)) => {
            let id = crate::identity::parse_endpoint_id(&s).map_err(|e| e.to_string())?;
            *f = id_bytes(id);
        }
        (Field::RelayMode(f), Value::Text(s)) => {
            *f = Some(variant(
                &s,
                ("default", pb::RelayMode::Default as i32),
                ("disabled", pb::RelayMode::Disabled as i32),
            )?);
        }
        (Field::Congestion(f), Value::Text(s)) => {
            *f = Some(variant(
                &s,
                ("bbr", pb::Congestion::Bbr as i32),
                ("cubic", pb::Congestion::Cubic as i32),
            )?);
        }
        (Field::PqKeyExchange(f), Value::Text(s)) => {
            *f = Some(variant(
                &s,
                ("prefer", pb::PqKeyExchange::Prefer as i32),
                ("require", pb::PqKeyExchange::Require as i32),
            )?);
        }
        (Field::OtlpProtocol(f), Value::Text(s)) => {
            *f = Some(variant(
                &s,
                ("grpc", pb::OtlpProtocol::Grpc as i32),
                ("http", pb::OtlpProtocol::Http as i32),
            )?);
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
