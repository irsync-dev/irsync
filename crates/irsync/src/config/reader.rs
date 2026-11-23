//! `irsync.toml` read into the config message ([`pb::Config`]).
//!
//! It makes every check that needs the file's text or its line numbers;
//! [`super::file::Config::from_message`] makes those an edit can break
//! again, with the same messages. The message holds what the file says and
//! nothing more: a key that is not in the file stays unset, defaults are
//! applied later, and paths stay as written (`~` is not expanded).

use super::exports::{name_problem, not_absolute};
use super::hosts::{invalid_alias, needs_an_id, valid_alias};
use super::settings::{listen_problem, parse_debug_addr};
use super::{duration, expand_tilde, line_of, toml_err, ConfigError};
use crate::identity::parse_endpoint_id;
use crate::net::addr::HostPort;
use irsync_proto::config as pb;
use serde::{Deserialize, Deserializer};
use spelled::{Congestion, OtlpProtocol, RelayMode};
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
    unsafe_config_allowed: Option<bool>,
    identity: Option<String>,
    state_dir: Option<String>,
    client: Option<RawClient>,
    daemon: Option<RawDaemon>,
    telemetry: Option<RawTelemetry>,
    #[serde(default)]
    hosts: BTreeMap<String, Spanned<RawHost>>,
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
    relay: Option<Url>,
    relay_mode: Option<RelayMode>,
    congestion: Option<Congestion>,
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
    relay: Option<Url>,
    relay_mode: Option<RelayMode>,
    congestion: Option<Congestion>,
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
    otlp_endpoint: Option<Url>,
    otlp_protocol: Option<OtlpProtocol>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHost {
    endpoint_id: Option<Spanned<String>>,
    client_id: Option<Spanned<String>>,
    relay: Option<Url>,
    relay_mode: Option<RelayMode>,
    congestion: Option<Congestion>,
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
    pub(super) enum OtlpProtocol {
        Grpc,
        Http,
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

/// Reads the text of the config file into the message, without its
/// comments. `path` is the file named in errors.
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
        unsafe_config_allowed: raw.unsafe_config_allowed,
        identity: raw.identity,
        state_dir: raw.state_dir,
        client: raw.client.map(client),
        daemon: raw.daemon.map(daemon),
        telemetry: raw.telemetry.map(telemetry),
        ..Default::default()
    };
    config.hosts = hosts(raw.hosts, &at)?;
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
            "hosts" | "exports" | "all" | "client" | "daemon" | "telemetry" if value.is_table() => {
                continue
            }
            "hosts" | "exports" | "all" | "client" | "daemon" | "telemetry" => {
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
            // by its keys, both when it has neither.
            _ if value.is_table() => {
                let t = value.as_table().expect("checked");
                if t.contains_key("endpoint_id") || t.contains_key("client_id") {
                    format!("unknown table {name:?}; hosts live under [hosts.{name}]")
                } else if t.contains_key("path") {
                    format!("unknown table {name:?}; exports live under [exports.{name}]")
                } else {
                    format!(
                        "unknown table {name:?}; hosts live under [hosts.{name}], exports under [exports.{name}]"
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
        reconnect: r.reconnect.map(duration),
        dial_retries: r.dial_retries,
        dht: r.dht,
        ssh_install: r.ssh_install,
        mdns: r.mdns,
        relay: r.relay.map(String::from),
        relay_mode: r.relay_mode.map(relay_mode),
        congestion: r.congestion.map(congestion),
        ..Default::default()
    }
}

fn daemon(r: RawDaemon) -> pb::Daemon {
    let (listen, listen_set) = addresses(r.listen);
    pb::Daemon {
        identity: r.identity.map(path_text),
        idle_timeout: r.idle_timeout.map(duration),
        dormant_session_timeout: r.dormant_session_timeout.map(duration),
        max_dormant_sessions_per_key: r.max_dormant_sessions_per_key,
        listen,
        listen_set,
        portmap: r.portmap,
        relay: r.relay.map(String::from),
        relay_mode: r.relay_mode.map(relay_mode),
        congestion: r.congestion.map(congestion),
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
        otlp_endpoint: r.otlp_endpoint.map(String::from),
        otlp_protocol: r.otlp_protocol.map(|p| {
            (match p {
                OtlpProtocol::Grpc => pb::OtlpProtocol::Grpc,
                OtlpProtocol::Http => pb::OtlpProtocol::Http,
            }) as i32
        }),
        ..Default::default()
    }
}

/// Validates the `[hosts]` table: alias rules and endpoint ids. The checks
/// run in alias order, as in `hosts_from_raw`, so that the first error is
/// the same; the entries keep the order of the file.
fn hosts(
    raw: BTreeMap<String, Spanned<RawHost>>,
    at: &impl Fn(usize, String) -> ConfigError,
) -> Result<Vec<pb::Host>, ConfigError> {
    let mut hosts = Vec::new();
    for (alias, r) in raw {
        if !valid_alias(&alias) {
            return Err(at(r.span().start, invalid_alias(&alias)));
        }
        let start = r.span().start;
        let r = r.into_inner();
        let id_of = |key: &Option<Spanned<String>>| {
            key.as_ref()
                .map(|s| {
                    parse_endpoint_id(s.get_ref())
                        .map(|id| id.as_bytes().to_vec())
                        .map_err(|e| at(s.span().start, format!("host {alias:?}: {e}")))
                })
                .transpose()
        };
        let endpoint_id = id_of(&r.endpoint_id)?;
        let client_id = id_of(&r.client_id)?;
        if endpoint_id.is_none() && client_id.is_none() {
            return Err(at(start, needs_an_id(&alias)));
        }
        let (address, address_set) = addresses(r.address);
        let (ssh_aliases, ssh_aliases_set) = entries(r.ssh_aliases);
        let host = pb::Host {
            name: alias,
            endpoint_id,
            client_id,
            relay: r.relay.map(String::from),
            relay_mode: r.relay_mode.map(relay_mode),
            congestion: r.congestion.map(congestion),
            address,
            address_set,
            parallel: r.parallel,
            reconnect: r.reconnect.map(duration),
            dial_retries: r.dial_retries,
            dht: r.dht,
            mdns: r.mdns,
            ssh: r.ssh,
            ssh_aliases,
            ssh_aliases_set,
            ..Default::default()
        };
        hosts.push((start, host));
    }
    hosts.sort_by_key(|(start, _)| *start);
    Ok(hosts.into_iter().map(|(_, h)| h).collect())
}

/// Validates every `[exports.NAME]`: reserved names, the export schema,
/// absolute paths. In name order, as in `exports_from_raw`; the entries
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

fn duration(d: Duration) -> prost_types::Duration {
    prost_types::Duration {
        seconds: i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(d.subsec_nanos()).expect("under a second"),
    }
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
