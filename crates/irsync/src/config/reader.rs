//! `irsync.toml` read into the config message ([`pb::Config`]).
//!
//! It makes every check that needs the file's text or its line numbers;
//! [`super::file::Config::from_message`] makes those an edit can break
//! again, with the same messages. The message holds what the file says and
//! nothing more: a key that is not in the file stays unset, defaults are
//! applied later, and paths stay as written (`~` is not expanded).

use super::clients::{Right, RIGHTS};
use super::exports::{name_problem, not_absolute};
use super::names::{invalid_name, valid_alias};
use super::settings::{
    listen_problem, parse_debug_addr, Congestion, OtlpProtocol, PqKeyExchange, Relay, Spelled,
};
use super::{duration, expand_tilde, line_of, ConfigError};
use crate::authz::Access;
use crate::identity::parse_endpoint_id;
use crate::net::addr::HostPort;
use irsync_proto::config as pb;
use serde::{Deserialize, Deserializer};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use toml::Spanned;
use url::Url;

// The serde shapes of the file. Each field is a key, in the order the
// writer writes them.

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    config_version: Option<Spanned<toml::Value>>,
    unsafe_config_allowed: Option<bool>,
    state_dir: Option<String>,
    defaults: Option<RawDefaults>,
    daemon: Option<RawDaemon>,
    telemetry: Option<RawTelemetry>,
    #[serde(default)]
    servers: BTreeMap<String, Spanned<RawServer>>,
    #[serde(default)]
    clients: BTreeMap<String, Spanned<RawClientEntry>>,
    #[serde(default)]
    exports: BTreeMap<String, Spanned<toml::Value>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDefaults {
    identity: Option<PathBuf>,
    parallel: Option<u32>,
    #[serde(default, deserialize_with = "de_opt_duration")]
    reconnect: Option<Duration>,
    dial_retries: Option<u32>,
    ssh_install: Option<bool>,
    relay: Option<RelayText>,
    dht: Option<bool>,
    dns: Option<bool>,
    mdns: Option<bool>,
    congestion: Option<Text<Congestion>>,
    pq_key_exchange: Option<Text<PqKeyExchange>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDaemon {
    identity: Option<PathBuf>,
    #[serde(default, deserialize_with = "de_opt_duration")]
    idle_timeout: Option<Duration>,
    #[serde(default, deserialize_with = "de_opt_duration")]
    dormant_session_timeout: Option<Duration>,
    max_dormant_sessions_per_key: Option<u32>,
    #[serde(default, deserialize_with = "de_opt_listen")]
    listen: Option<Vec<HostPort>>,
    portmap: Option<bool>,
    relay: Option<RelayText>,
    dht: Option<bool>,
    dns: Option<bool>,
    mdns: Option<bool>,
    congestion: Option<Text<Congestion>>,
    pq_key_exchange: Option<Text<PqKeyExchange>>,
    #[serde(default, deserialize_with = "de_opt_debug_addr")]
    debug_addr: Option<String>,
    allow_all_paths: Option<bool>,
    allow_root: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTelemetry {
    otlp_endpoint: Option<UrlText>,
    otlp_protocol: Option<Text<OtlpProtocol>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServer {
    endpoint_id: Option<Spanned<String>>,
    address: Option<Vec<HostPort>>,
    relay: Option<RelayText>,
    dht: Option<bool>,
    dns: Option<bool>,
    mdns: Option<bool>,
    congestion: Option<Text<Congestion>>,
    pq_key_exchange: Option<Text<PqKeyExchange>>,
    parallel: Option<u32>,
    #[serde(default, deserialize_with = "de_opt_duration")]
    reconnect: Option<Duration>,
    dial_retries: Option<u32>,
    ssh: Option<String>,
    ssh_aliases: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawClientEntry {
    client_id: Option<Spanned<String>>,
    all_exports: Option<Spanned<String>>,
    all_paths: Option<Spanned<String>>,
    admin: Option<Spanned<String>>,
}

impl RawClientEntry {
    /// The right's value as written, with its place in the file.
    fn right(&self, right: Right) -> Option<&Spanned<String>> {
        match right {
            Right::AllExports => self.all_exports.as_ref(),
            Right::AllPaths => self.all_paths.as_ref(),
            Right::Admin => self.admin.as_ref(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExport {
    path: PathBuf,
    read_only: Option<bool>,
    warm_index: Option<bool>,
    fsync: Option<bool>,
    auto: Option<bool>,
    rw: Option<Vec<String>>,
    ro: Option<Vec<String>>,
}

/// An enum key's value, read with its one spelling ([`Spelled`]).
struct Text<T>(T);

impl<'de, T: Spelled> Deserialize<'de> for Text<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Text<T>, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map(Text).map_err(serde::de::Error::custom)
    }
}

/// The message's number for an enum key's value.
fn number<T: Spelled>(t: Text<T>) -> i32 {
    t.0.to_message()
}

/// A relay as written, once it parses as one ([`Relay`]).
struct RelayText(String);

impl<'de> Deserialize<'de> for RelayText {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<RelayText, D::Error> {
        let s = String::deserialize(d)?;
        s.parse::<Relay>().map_err(serde::de::Error::custom)?;
        Ok(RelayText(s))
    }
}

/// A URL as written, once it parses.
struct UrlText(String);

impl<'de> Deserialize<'de> for UrlText {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<UrlText, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = UrlText;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a string representing an URL")
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<UrlText, E> {
                Url::parse(s).map_err(|err| E::custom(format!("{err}: {s:?}")))?;
                Ok(UrlText(s.to_string()))
            }
        }
        d.deserialize_str(Visitor)
    }
}

/// A duration: whole seconds, or text such as `"90m"`.
fn de_opt_duration<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Secs(u64),
        Text(String),
    }
    match Option::<Raw>::deserialize(d)? {
        None => Ok(None),
        Some(Raw::Secs(n)) => Ok(Some(Duration::from_secs(n))),
        Some(Raw::Text(s)) => duration::parse(&s)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

fn de_opt_listen<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<HostPort>>, D::Error> {
    let listen = Vec::<HostPort>::deserialize(d)?;
    if let Some(problem) = listen_problem(&listen) {
        return Err(serde::de::Error::custom(problem));
    }
    Ok(Some(listen))
}

/// The debug address as written, once [`parse_debug_addr`] accepts it.
fn de_opt_debug_addr<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    let s = String::deserialize(d)?;
    parse_debug_addr(&s).map_err(serde::de::Error::custom)?;
    Ok(Some(s))
}

/// A toml parse error as a config error at its line.
fn toml_err(text: &str, path: &Path, e: toml::de::Error) -> ConfigError {
    ConfigError::File {
        path: path.to_path_buf(),
        line: e.span().map(|s| line_of(text, s.start)),
        message: e.message().to_string(),
    }
}

/// The segments of a dotted key in TOML's key grammar:
/// `exports."www.example.com".path` is three. `None` for text that is
/// no key.
pub fn dotted_key(key: &str) -> Option<Vec<String>> {
    let keys = toml_edit::Key::parse(key).ok()?;
    Some(keys.into_iter().map(|k| k.get().to_string()).collect())
}

/// The layout of the config file this irsync writes and reads. Every
/// save writes it as `config_version`; a file without the key is read as
/// this layout too.
pub const CONFIG_VERSION: u32 = 2;

/// Reads the text of the config file into the message, with its
/// comments. `path` is the file named in errors.
pub fn read(text: &str, path: &Path) -> Result<pb::Config, ConfigError> {
    let mut config = read_values(text, path)?;
    super::comments::attach(text, path, &mut config)?;
    Ok(config)
}

/// Checks a `config_version` value: only [`CONFIG_VERSION`] is read.
fn version_problem(v: &toml::Value) -> Option<String> {
    match v.as_integer() {
        Some(n) if n == i64::from(CONFIG_VERSION) => None,
        Some(n) if n > i64::from(CONFIG_VERSION) => Some(format!(
            "config_version {n}: this file was written by a newer irsync, which reads config_version {CONFIG_VERSION}; upgrade irsync"
        )),
        Some(n) => Some(format!("config_version: expected {CONFIG_VERSION}, found {n}")),
        None => Some(format!(
            "config_version: expected {CONFIG_VERSION}, found {}",
            v.type_str()
        )),
    }
}

/// Reads the text of a config file into the
/// message, without its comments. `path` is the file named in errors.
pub fn read_values(text: &str, path: &Path) -> Result<pb::Config, ConfigError> {
    let at = |offset: usize, message: String| ConfigError::File {
        path: path.to_path_buf(),
        line: Some(line_of(text, offset)),
        message,
    };
    check_shape(text, path)?;
    // Typed pass over the same text, so the spans match.
    let raw: RawFile = toml::from_str(text).map_err(|e| toml_err(text, path, e))?;
    let mut config = pb::Config {
        // Checked by the shape pass.
        config_version: raw
            .config_version
            .and_then(|v| v.get_ref().as_integer())
            .and_then(|n| u32::try_from(n).ok()),
        unsafe_config_allowed: raw.unsafe_config_allowed,
        state_dir: raw.state_dir,
        defaults: raw.defaults.map(defaults),
        daemon: raw.daemon.map(daemon),
        telemetry: raw.telemetry.map(telemetry),
        ..Default::default()
    };
    config.servers = servers(raw.servers, &at)?;
    config.clients = clients(raw.clients, &at)?;
    config.exports = exports(raw.exports, &at)?;
    Ok(config)
}

/// The shape pass over the top-level keys.
fn check_shape(text: &str, path: &Path) -> Result<(), ConfigError> {
    let shape: BTreeMap<String, Spanned<toml::Value>> =
        toml::from_str(text).map_err(|e| toml_err(text, path, e))?;
    let mut keys: Vec<_> = shape.iter().collect();
    keys.sort_by_key(|(_, v)| v.span().start);
    for (name, value) in keys {
        let start = value.span().start;
        let value = value.get_ref();
        let message = match name.as_str() {
            "config_version" => match version_problem(value) {
                None => continue,
                Some(m) => m,
            },
            "servers" | "clients" | "exports" | "defaults" | "daemon" | "telemetry"
                if value.is_table() =>
            {
                continue
            }
            "servers" | "clients" | "exports" | "defaults" | "daemon" | "telemetry" => {
                format!("{name}: expected a table, found {}", value.type_str())
            }
            "unsafe_config_allowed" if value.is_bool() => continue,
            "unsafe_config_allowed" => {
                format!("{name}: expected a boolean, found {}", value.type_str())
            }
            "state_dir" if value.is_str() => continue,
            "state_dir" => {
                format!("{name}: expected a string, found {}", value.type_str())
            }
            _ => format!("unknown key {name:?}"),
        };
        return Err(ConfigError::File {
            path: path.to_path_buf(),
            line: Some(line_of(text, start)),
            message,
        });
    }
    Ok(())
}

fn defaults(r: RawDefaults) -> pb::Defaults {
    pb::Defaults {
        identity: r.identity.map(path_text),
        parallel: r.parallel,
        reconnect: r.reconnect.map(duration::to_proto),
        dial_retries: r.dial_retries,
        ssh_install: r.ssh_install,
        relay: r.relay.map(|r| r.0),
        dht: r.dht,
        dns: r.dns,
        mdns: r.mdns,
        congestion: r.congestion.map(number),
        pq_key_exchange: r.pq_key_exchange.map(number),
        ..Default::default()
    }
}

fn daemon(r: RawDaemon) -> pb::Daemon {
    let (listen, listen_set) = addresses(r.listen);
    pb::Daemon {
        identity: r.identity.map(path_text),
        idle_timeout: r.idle_timeout.map(duration::to_proto),
        dormant_session_timeout: r.dormant_session_timeout.map(duration::to_proto),
        max_dormant_sessions_per_key: r.max_dormant_sessions_per_key,
        listen,
        listen_set,
        portmap: r.portmap,
        relay: r.relay.map(|r| r.0),
        dht: r.dht,
        dns: r.dns,
        mdns: r.mdns,
        congestion: r.congestion.map(number),
        pq_key_exchange: r.pq_key_exchange.map(number),
        debug_addr: r.debug_addr,
        allow_all_paths: r.allow_all_paths,
        allow_root: r.allow_root,
        ..Default::default()
    }
}

fn telemetry(r: RawTelemetry) -> pb::Telemetry {
    pb::Telemetry {
        otlp_endpoint: r.otlp_endpoint.map(|u| u.0),
        otlp_protocol: r.otlp_protocol.map(number),
        ..Default::default()
    }
}

/// An id of the file as the message holds it; the error names `what`
/// (`server "nas"`) at the line of the id.
fn id_bytes(
    key: Option<Spanned<String>>,
    what: &str,
    at: &impl Fn(usize, String) -> ConfigError,
) -> Result<Option<Vec<u8>>, ConfigError> {
    key.map(|s| {
        parse_endpoint_id(s.get_ref())
            .map(|id| id.as_bytes().to_vec())
            .map_err(|e| at(s.span().start, format!("{what}: {e}")))
    })
    .transpose()
}

/// Validates every `[servers.NAME]`: name rules and endpoint ids. In name
/// order, so that the first error does not depend on the file's order;
/// the entries keep the order of the file.
fn servers(
    raw: BTreeMap<String, Spanned<RawServer>>,
    at: &impl Fn(usize, String) -> ConfigError,
) -> Result<Vec<pb::ServerEntry>, ConfigError> {
    let mut servers = Vec::new();
    for (name, r) in raw {
        let start = r.span().start;
        if !valid_alias(&name) {
            return Err(at(start, invalid_name(&name)));
        }
        let r = r.into_inner();
        let what = format!("server {name:?}");
        let Some(endpoint_id) = id_bytes(r.endpoint_id, &what, at)? else {
            return Err(at(start, format!("{what}: needs an endpoint_id")));
        };
        let (address, address_set) = addresses(r.address);
        let (ssh_aliases, ssh_aliases_set) = entries(r.ssh_aliases);
        let server = pb::ServerEntry {
            name,
            endpoint_id,
            relay: r.relay.map(|r| r.0),
            congestion: r.congestion.map(number),
            pq_key_exchange: r.pq_key_exchange.map(number),
            address,
            address_set,
            parallel: r.parallel,
            reconnect: r.reconnect.map(duration::to_proto),
            dial_retries: r.dial_retries,
            dht: r.dht,
            dns: r.dns,
            mdns: r.mdns,
            ssh: r.ssh,
            ssh_aliases,
            ssh_aliases_set,
            ..Default::default()
        };
        servers.push((start, server));
    }
    servers.sort_by_key(|(start, _)| *start);
    Ok(servers.into_iter().map(|(_, s)| s).collect())
}

/// Validates every `[clients.NAME]`: name rules and client ids. In name
/// order; the entries keep the order of the file.
fn clients(
    raw: BTreeMap<String, Spanned<RawClientEntry>>,
    at: &impl Fn(usize, String) -> ConfigError,
) -> Result<Vec<pb::ClientEntry>, ConfigError> {
    let mut clients = Vec::new();
    for (name, r) in raw {
        let start = r.span().start;
        if !valid_alias(&name) {
            return Err(at(start, invalid_name(&name)));
        }
        let what = format!("client {name:?}");
        let r = r.into_inner();
        let mut rights = Vec::new();
        for right in RIGHTS {
            if let Some(v) = r.right(right) {
                let Some(access) = Access::parse(v.get_ref()) else {
                    return Err(at(
                        v.span().start,
                        format!("{what}: {}", right.not_an_access()),
                    ));
                };
                rights.push((right, access));
            }
        }
        let Some(client_id) = id_bytes(r.client_id, &what, at)? else {
            return Err(at(start, format!("{what}: needs a client_id")));
        };
        let mut client = pb::ClientEntry {
            name,
            client_id,
            ..Default::default()
        };
        for (right, access) in rights {
            right.set(&mut client, Some(access));
        }
        clients.push((start, client));
    }
    clients.sort_by_key(|(start, _)| *start);
    Ok(clients.into_iter().map(|(_, c)| c).collect())
}

/// Validates every `[exports.NAME]`: reserved names, the export schema,
/// absolute paths. In name order; the entries keep the order of the file.
fn exports(
    raw: BTreeMap<String, Spanned<toml::Value>>,
    at: &impl Fn(usize, String) -> ConfigError,
) -> Result<Vec<pb::Export>, ConfigError> {
    let mut exports = Vec::new();
    for (name, value) in raw {
        let start = value.span().start;
        if let Some(problem) = name_problem(&name) {
            return Err(at(start, problem));
        }
        let value = value.into_inner();
        if !value.is_table() {
            return Err(at(
                start,
                format!(
                    "exports.{name}: expected a table (an export), found {}",
                    value.type_str()
                ),
            ));
        }
        let e: RawExport = value.try_into().map_err(|e: toml::de::Error| {
            at(start, format!("[exports.{name}]: {}", e.message()))
        })?;
        let written = path_text(e.path);
        let expanded = expand_tilde(&written);
        if !expanded.is_absolute() {
            return Err(at(start, not_absolute(&name, &expanded)));
        }
        let (rw, rw_set) = entries(e.rw);
        let (ro, ro_set) = entries(e.ro);
        let export = pb::Export {
            name,
            path: written,
            read_only: e.read_only,
            warm_index: e.warm_index,
            fsync: e.fsync,
            auto: e.auto,
            rw,
            rw_set,
            ro,
            ro_set,
            ..Default::default()
        };
        exports.push((start, export));
    }
    exports.sort_by_key(|(start, _)| *start);
    Ok(exports.into_iter().map(|(_, e)| e).collect())
}

/// A string array as entries, and whether its key is in the file.
fn entries(values: Option<Vec<String>>) -> (Vec<pb::Entry>, bool) {
    let set = values.is_some();
    let entries = values
        .unwrap_or_default()
        .into_iter()
        .map(|value| pb::Entry {
            value,
            comment: None,
        })
        .collect();
    (entries, set)
}

/// An address array as entries, each as `HostPort` prints it.
fn addresses(values: Option<Vec<HostPort>>) -> (Vec<pb::Entry>, bool) {
    entries(values.map(|v| v.iter().map(HostPort::to_string).collect()))
}

/// A path from a TOML string, which is always UTF-8.
fn path_text(p: PathBuf) -> String {
    p.to_string_lossy().into_owned()
}
