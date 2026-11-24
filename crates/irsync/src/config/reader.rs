//! `irsync.toml` read into the config message ([`pb::Config`]).
//!
//! It makes every check that needs the file's text or its line numbers;
//! [`super::file::Config::from_message`] makes those an edit can break
//! again, with the same messages. The message holds what the file says and
//! nothing more: a key that is not in the file stays unset, defaults are
//! applied later, and paths stay as written (`~` is not expanded).

use super::exports::{name_problem, not_absolute};
use super::names::{invalid_name, valid_alias};
use super::settings::{listen_problem, parse_debug_addr};
use super::{duration, expand_tilde, line_of, ConfigError};
use crate::identity::parse_endpoint_id;
use crate::net::addr::HostPort;
use irsync_proto::config as pb;
use serde::{Deserialize, Deserializer};
use spelled::{Congestion, OtlpProtocol, PqKeyExchange, RelayMode};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use toml::Spanned;
use url::Url;

// The serde shapes of the file. Field names and their order are those of
// the structs `Config::parse` once read into, so that serde's messages (an
// unknown field names the expected ones) stay the same.

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    config_version: Option<Spanned<toml::Value>>,
    unsafe_config_allowed: Option<bool>,
    identity: Option<String>,
    state_dir: Option<String>,
    client: Option<RawClient>,
    daemon: Option<RawDaemon>,
    telemetry: Option<RawTelemetry>,
    #[serde(default)]
    servers: BTreeMap<String, Spanned<RawServer>>,
    #[serde(default)]
    clients: BTreeMap<String, Spanned<RawClientEntry>>,
    #[serde(default)]
    exports: BTreeMap<String, Spanned<toml::Value>>,
    all: Option<RawGrants>,
    immediate: Option<RawImmediate>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawClient {
    parallel: Option<u32>,
    #[serde(default, deserialize_with = "de_opt_duration")]
    reconnect: Option<Duration>,
    dial_retries: Option<u32>,
    dht: Option<bool>,
    ssh_install: Option<bool>,
    mdns: Option<bool>,
    relay: Option<UrlText>,
    relay_mode: Option<RelayMode>,
    congestion: Option<Congestion>,
    pq_key_exchange: Option<PqKeyExchange>,
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
    relay: Option<UrlText>,
    relay_mode: Option<RelayMode>,
    congestion: Option<Congestion>,
    pq_key_exchange: Option<PqKeyExchange>,
    discovery: Option<RawDiscovery>,
    #[serde(default, deserialize_with = "de_opt_debug_addr")]
    debug_addr: Option<String>,
    immediate: Option<bool>,
    allow_root: Option<bool>,
}

// Named as the typed struct was: serde's messages name it.
#[derive(Deserialize)]
#[serde(expecting = "struct Discovery", deny_unknown_fields)]
struct RawDiscovery {
    dht: Option<bool>,
    dns: Option<bool>,
    mdns: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTelemetry {
    otlp_endpoint: Option<UrlText>,
    otlp_protocol: Option<OtlpProtocol>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServer {
    endpoint_id: Option<Spanned<String>>,
    relay: Option<UrlText>,
    relay_mode: Option<RelayMode>,
    congestion: Option<Congestion>,
    pq_key_exchange: Option<PqKeyExchange>,
    address: Option<Vec<HostPort>>,
    parallel: Option<u32>,
    #[serde(default, deserialize_with = "de_opt_duration")]
    reconnect: Option<Duration>,
    dial_retries: Option<u32>,
    dht: Option<bool>,
    mdns: Option<bool>,
    ssh: Option<String>,
    ssh_aliases: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawClientEntry {
    client_id: Option<Spanned<String>>,
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGrants {
    rw: Option<Vec<String>>,
    ro: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawImmediate {
    rw: Option<Vec<String>>,
}

/// The enums as the file spells them. Named as the typed enums are, should
/// serde name them in a message.
mod spelled {
    use serde::Deserialize;

    #[derive(Clone, Copy, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub(super) enum RelayMode {
        Default,
        Disabled,
    }

    #[derive(Clone, Copy, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub(super) enum Congestion {
        Bbr,
        Cubic,
    }

    #[derive(Clone, Copy, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub(super) enum PqKeyExchange {
        Prefer,
        Require,
    }

    #[derive(Clone, Copy, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub(super) enum OtlpProtocol {
        Grpc,
        Http,
    }
}

/// A URL as written, once it parses. Its serde messages are those of
/// `Url`'s own `Deserialize`, which the file was once read with.
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
    // `Raw` is named in serde's message for a value of neither kind.
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

/// The layout of the config file this irsync writes: `[servers]` and
/// `[clients]`. Every save writes it as `config_version`.
pub const CONFIG_VERSION: u32 = 2;

/// A config file read into the message of the current layout.
#[derive(Debug, Clone, PartialEq)]
pub struct Read {
    /// The message, with its comments.
    pub config: pb::Config,
    /// The layout of the file as it was: 1 for one with `[hosts]`, read by
    /// [`super::legacy_v1`], else [`CONFIG_VERSION`].
    pub version: u32,
    /// What a conversion from layout 1 left out, one note each; see
    /// [`super::legacy_v1::Converted::dropped`].
    pub dropped: Vec<String>,
}

/// Reads the text of the config file into the message, with its
/// comments. A file of layout 1 is read by [`super::legacy_v1`] and
/// converted. `path` is the file named in errors.
///
/// The layout is told by the top-level keys: `config_version` when the
/// file has it, else `[hosts]` without `[servers]` and `[clients]` is
/// layout 1. A file that mixes the two, or has a `config_version` this
/// irsync does not know, is an error at that key.
pub fn read(text: &str, path: &Path) -> Result<Read, ConfigError> {
    if layout(text, path)? == 1 {
        let c = super::legacy_v1::read(text, path)?;
        return Ok(Read {
            config: c.config,
            version: 1,
            dropped: c.dropped,
        });
    }
    let mut config = read_values(text, path)?;
    super::comments::attach(text, path, &mut config)?;
    Ok(Read {
        config,
        version: CONFIG_VERSION,
        dropped: Vec::new(),
    })
}

/// The message for `[hosts]` in a file of the current layout.
const HOSTS_NOT_READ: &str = "[hosts] is not read since config_version 2: hosts are now [servers.NAME] (to connect to) and [clients.NAME] (to let in)";

/// The layout of `text`, from a shape pass over its top-level keys.
fn layout(text: &str, path: &Path) -> Result<u32, ConfigError> {
    let shape: BTreeMap<String, Spanned<toml::Value>> =
        toml::from_str(text).map_err(|e| toml_err(text, path, e))?;
    let at = |v: &Spanned<toml::Value>, message: String| ConfigError::File {
        path: path.to_path_buf(),
        line: Some(line_of(text, v.span().start)),
        message,
    };
    let hosts = shape.get("hosts");
    let new = shape.contains_key("servers") || shape.contains_key("clients");
    match (shape.get("config_version"), hosts) {
        (Some(v), hosts) => match (version_of(v.get_ref()).map_err(|m| at(v, m))?, hosts) {
            (1, _) => Ok(1),
            (_, Some(h)) => Err(at(h, HOSTS_NOT_READ.to_string())),
            (n, None) => Ok(n),
        },
        (None, Some(h)) if new => Err(at(
            h,
            "this file has [hosts] and also [servers] or [clients]: move each [hosts.NAME] into [servers.NAME] (its endpoint_id and how to reach it) or [clients.NAME] (its client_id)".to_string(),
        )),
        (None, Some(_)) => Ok(1),
        (None, None) => Ok(CONFIG_VERSION),
    }
}

/// The layout a `config_version` value names, or the message when it
/// names none this irsync reads.
fn version_of(v: &toml::Value) -> Result<u32, String> {
    match v.as_integer() {
        Some(n @ 1..=2) => Ok(n as u32),
        Some(n) if n > 2 => Err(format!(
            "config_version {n}: this file was written by a newer irsync, which reads up to config_version {CONFIG_VERSION}; upgrade irsync"
        )),
        Some(n) => Err(format!("config_version: expected 1 or 2, found {n}")),
        None => Err(format!(
            "config_version: expected 1 or 2, found {}",
            v.type_str()
        )),
    }
}

/// Reads the text of a config file of the current layout into the
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
        identity: raw.identity,
        state_dir: raw.state_dir,
        client: raw.client.map(client),
        daemon: raw.daemon.map(daemon),
        telemetry: raw.telemetry.map(telemetry),
        ..Default::default()
    };
    config.servers = servers(raw.servers, &at)?;
    config.clients = clients(raw.clients, &at)?;
    config.exports = exports(raw.exports, &at)?;
    config.all = raw.all.map(|g| {
        let (rw, rw_set) = entries(g.rw);
        let (ro, ro_set) = entries(g.ro);
        pb::GrantTable {
            rw,
            rw_set,
            ro,
            ro_set,
            ..Default::default()
        }
    });
    config.immediate = raw.immediate.map(|g| {
        let (rw, rw_set) = entries(g.rw);
        pb::GrantTable {
            rw,
            rw_set,
            ..Default::default()
        }
    });
    Ok(config)
}

/// The shape pass over the top-level keys, with the old-layout hints.
fn check_shape(text: &str, path: &Path) -> Result<(), ConfigError> {
    let shape: BTreeMap<String, Spanned<toml::Value>> =
        toml::from_str(text).map_err(|e| toml_err(text, path, e))?;
    let mut keys: Vec<_> = shape.iter().collect();
    keys.sort_by_key(|(_, v)| v.span().start);
    for (name, value) in keys {
        let start = value.span().start;
        let value = value.get_ref();
        let message = match name.as_str() {
            "immediate" => match immediate_problem(value) {
                Some(m) => m,
                None => continue,
            },
            "hosts" => HOSTS_NOT_READ.to_string(),
            "config_version" => match version_of(value) {
                Ok(_) => continue,
                Err(m) => m,
            },
            "servers" | "clients" | "exports" | "all" | "client" | "daemon" | "telemetry"
                if value.is_table() =>
            {
                continue
            }
            "servers" | "clients" | "exports" | "all" | "client" | "daemon" | "telemetry" => {
                format!("{name}: expected a table, found {}", value.type_str())
            }
            "unsafe_config_allowed" if value.is_bool() => continue,
            "unsafe_config_allowed" => {
                format!("{name}: expected a boolean, found {}", value.type_str())
            }
            "identity" | "state_dir" if value.is_str() => continue,
            "identity" | "state_dir" => {
                format!("{name}: expected a string, found {}", value.type_str())
            }
            // A bare table is an old-layout host or export: say which
            // by its keys, all of them when it has none.
            _ if value.is_table() => {
                let t = value.as_table().expect("checked");
                if t.contains_key("endpoint_id") || t.contains_key("client_id") {
                    format!(
                        "unknown table {name:?}; servers live under [servers.{name}], clients under [clients.{name}]"
                    )
                } else if t.contains_key("path") {
                    format!("unknown table {name:?}; exports live under [exports.{name}]")
                } else {
                    format!(
                        "unknown table {name:?}; servers live under [servers.{name}], clients under [clients.{name}], exports under [exports.{name}]"
                    )
                }
            }
            "rw" | "ro" => {
                format!("unknown key {name:?}; grants for every export live under [all]")
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

/// The message for a bad `[immediate]` table, or `None` when it is fine.
fn immediate_problem(value: &toml::Value) -> Option<String> {
    let Some(t) = value.as_table() else {
        return Some(format!(
            "immediate: expected a table, found {}",
            value.type_str()
        ));
    };
    t.keys().find(|k| *k != "rw").map(|k| {
        format!("[immediate]: unknown key {k:?}; only `rw` is allowed (immediate grants are read-write)")
    })
}

fn client(r: RawClient) -> pb::Client {
    pb::Client {
        parallel: r.parallel,
        reconnect: r.reconnect.map(duration::to_proto),
        dial_retries: r.dial_retries,
        dht: r.dht,
        ssh_install: r.ssh_install,
        mdns: r.mdns,
        relay: r.relay.map(|u| u.0),
        relay_mode: r.relay_mode.map(relay_mode),
        congestion: r.congestion.map(congestion),
        pq_key_exchange: r.pq_key_exchange.map(pq_key_exchange),
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
        relay: r.relay.map(|u| u.0),
        relay_mode: r.relay_mode.map(relay_mode),
        congestion: r.congestion.map(congestion),
        pq_key_exchange: r.pq_key_exchange.map(pq_key_exchange),
        discovery: r.discovery.map(|d| pb::Discovery {
            dht: d.dht,
            dns: d.dns,
            mdns: d.mdns,
            ..Default::default()
        }),
        debug_addr: r.debug_addr,
        immediate: r.immediate,
        allow_root: r.allow_root,
        ..Default::default()
    }
}

fn telemetry(r: RawTelemetry) -> pb::Telemetry {
    pb::Telemetry {
        otlp_endpoint: r.otlp_endpoint.map(|u| u.0),
        otlp_protocol: r.otlp_protocol.map(|p| {
            (match p {
                OtlpProtocol::Grpc => pb::OtlpProtocol::Grpc,
                OtlpProtocol::Http => pb::OtlpProtocol::Http,
            }) as i32
        }),
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
            relay: r.relay.map(|u| u.0),
            relay_mode: r.relay_mode.map(relay_mode),
            congestion: r.congestion.map(congestion),
            pq_key_exchange: r.pq_key_exchange.map(pq_key_exchange),
            address,
            address_set,
            parallel: r.parallel,
            reconnect: r.reconnect.map(duration::to_proto),
            dial_retries: r.dial_retries,
            dht: r.dht,
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
        let Some(client_id) = id_bytes(r.into_inner().client_id, &what, at)? else {
            return Err(at(start, format!("{what}: needs a client_id")));
        };
        let client = pb::ClientEntry {
            name,
            client_id,
            ..Default::default()
        };
        clients.push((start, client));
    }
    clients.sort_by_key(|(start, _)| *start);
    Ok(clients.into_iter().map(|(_, c)| c).collect())
}

/// Validates every `[exports.NAME]`: reserved names, the export schema,
/// absolute paths. In name order, as the old parse checked them; the entries
/// keep the order of the file.
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

fn relay_mode(m: RelayMode) -> i32 {
    (match m {
        RelayMode::Default => pb::RelayMode::Default,
        RelayMode::Disabled => pb::RelayMode::Disabled,
    }) as i32
}

fn congestion(c: Congestion) -> i32 {
    (match c {
        Congestion::Bbr => pb::Congestion::Bbr,
        Congestion::Cubic => pb::Congestion::Cubic,
    }) as i32
}

fn pq_key_exchange(p: PqKeyExchange) -> i32 {
    (match p {
        PqKeyExchange::Prefer => pb::PqKeyExchange::Prefer,
        PqKeyExchange::Require => pb::PqKeyExchange::Require,
    }) as i32
}
