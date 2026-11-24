//! The config file of layout 1, with `[hosts]`, read into the message
//! of layout 2, with `[servers]` and `[clients]`.
//!
//! The reading is a copy of the stage 1 reader, with its own serde shapes
//! and its own messages, so that an old file fails as it always did. It
//! shares only the parsers of single values. [`read`] then splits each
//! `[hosts.N]` into a server, a client or both, so that every id keeps
//! the grants it had; see [`read`] for how.
//!
//! The rule "a host's `client_id`, else its `endpoint_id`" lives only
//! here.

use super::comments::{attach_with, Place, Spot};
use super::exports::{name_problem, not_absolute};
use super::names::valid_alias;
use super::settings::{listen_problem, parse_debug_addr};
use super::{duration, expand_tilde, line_of, ConfigError};
use crate::identity::parse_endpoint_id;
use crate::net::addr::HostPort;
use irsync_proto::config as pb;
use serde::{Deserialize, Deserializer};
use spelled::{Congestion, OtlpProtocol, PqKeyExchange, RelayMode};
use std::collections::{BTreeMap, BTreeSet};
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
    // Checked by the caller before this layout is chosen.
    #[serde(default, rename = "config_version")]
    _config_version: Option<serde::de::IgnoredAny>,
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
struct RawHost {
    endpoint_id: Option<Spanned<String>>,
    client_id: Option<Spanned<String>>,
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

/// A file of layout 1 in the message of layout 2.
#[derive(Debug, Clone, PartialEq)]
pub struct Converted {
    /// The message, with `config_version` 2.
    pub config: pb::Config,
    /// One note for each `[hosts.N]` that had a `client_id`, no
    /// `endpoint_id` and keys that only apply to a server, which the
    /// conversion dropped.
    pub dropped: Vec<String>,
}

/// The keys of a host that only apply to a server, as layout 1 has them.
const SERVER_KEYS: [&str; 12] = [
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

/// Reads the text of a file of layout 1 into the message of layout 2,
/// with its comments. `path` is the file named in errors.
///
/// Each `[hosts.N]` becomes, in file order:
///
/// - with an `endpoint_id`: server `N`, with that id, the keys of
///   dialling and `ssh`, `ssh_aliases`;
/// - with a `client_id`: client `N`, with that id;
/// - with only an `endpoint_id`, and `N` named in a grant list: client
///   `N` too, with the `endpoint_id` as its `client_id`, since that is
///   the id the grants naming `N` were for. A comment above it says so;
/// - with only a `client_id`: the keys of a server are dropped, and
///   named in [`Converted::dropped`].
///
/// The comments of `[hosts.N]` and its tail go to server `N` when there
/// is one, else to client `N`; those of `client_id` to client `N`; those
/// of the other keys with their key to server `N`, or to the tail of
/// client `N` when there is no server. The comments of a bare `[hosts]`
/// or of `hosts = { … }` go to the first entry made from the first host.
pub fn read(text: &str, path: &Path) -> Result<Converted, ConfigError> {
    let (old, hosts) = read_v1_values(text, path)?;
    let granted = granted_names(&old);
    let doc = text.parse::<toml_edit::DocumentMut>().ok();
    let mut config = pb::Config {
        config_version: Some(2),
        ..old
    };
    let mut dropped = Vec::new();
    let mut first = None;
    for h in hosts {
        let name = h.name.clone();
        if let Some(endpoint_id) = h.endpoint_id.clone() {
            config.servers.push(server(&h, endpoint_id));
        }
        let client = match (&h.endpoint_id, &h.client_id) {
            (_, Some(client_id)) => Some(client_entry(&name, client_id.clone(), Vec::new())),
            (Some(endpoint_id), None) if granted.contains(&name) => Some(client_entry(
                &name,
                endpoint_id.clone(),
                vec![format!(
                    "# was the endpoint_id of [hosts.{name}]; kept as a client so the grants naming {name} still apply"
                )],
            )),
            _ => None,
        };
        if h.endpoint_id.is_none() {
            let keys = server_keys_in_file(doc.as_ref(), &name);
            if !keys.is_empty() {
                dropped.push(format!(
                    "[hosts.{name}] had a client_id and no endpoint_id: dropped {} (they only apply to a server)",
                    keys.join(", ")
                ));
            }
        }
        if let Some(c) = client {
            config.clients.push(c);
        }
        let group = if h.endpoint_id.is_some() {
            "servers"
        } else {
            "clients"
        };
        first.get_or_insert_with(|| table_path(group, &name));
    }
    attach_with(text, path, &mut config, &|msg, place| {
        moved(msg, first.as_ref(), place)
    })?;
    Ok(Converted { config, dropped })
}

/// Every name or id in a grant list: `[all]`, `[immediate]` or an export.
fn granted_names(c: &pb::Config) -> BTreeSet<String> {
    let all = c.all.iter().flat_map(|g| [&g.rw, &g.ro]);
    let immediate = c.immediate.iter().flat_map(|g| [&g.rw, &g.ro]);
    let exports = c.exports.iter().flat_map(|e| [&e.rw, &e.ro]);
    all.chain(immediate)
        .chain(exports)
        .flatten()
        .map(|e| e.value.clone())
        .collect()
}

fn server(h: &Host, endpoint_id: Vec<u8>) -> pb::ServerEntry {
    pb::ServerEntry {
        name: h.name.clone(),
        endpoint_id,
        relay: h.relay.clone(),
        relay_mode: h.relay_mode,
        congestion: h.congestion,
        pq_key_exchange: h.pq_key_exchange,
        address: h.address.clone(),
        address_set: h.address_set,
        parallel: h.parallel,
        reconnect: h.reconnect,
        dial_retries: h.dial_retries,
        dht: h.dht,
        mdns: h.mdns,
        ssh: h.ssh.clone(),
        ssh_aliases: h.ssh_aliases.clone(),
        ssh_aliases_set: h.ssh_aliases_set,
        ..Default::default()
    }
}

fn client_entry(name: &str, client_id: Vec<u8>, above: Vec<String>) -> pb::ClientEntry {
    pb::ClientEntry {
        comment: (!above.is_empty()).then(|| pb::Comment {
            above,
            ..Default::default()
        }),
        name: name.to_owned(),
        client_id,
        ..Default::default()
    }
}

/// The keys of `[hosts.NAME]` that only apply to a server, in file order.
fn server_keys_in_file(doc: Option<&toml_edit::DocumentMut>, name: &str) -> Vec<String> {
    let host = doc
        .and_then(|d| d.get("hosts"))
        .and_then(toml_edit::Item::as_table_like)
        .and_then(|t| t.get(name))
        .and_then(toml_edit::Item::as_table_like);
    host.map(|t| {
        t.iter()
            .map(|(k, _)| k)
            .filter(|k| SERVER_KEYS.contains(k))
            .map(str::to_owned)
            .collect()
    })
    .unwrap_or_default()
}

fn table_path(group: &str, name: &str) -> Vec<String> {
    vec![group.to_owned(), name.to_owned()]
}

/// Where a comment the walker placed in `[hosts]` goes in the message of
/// layout 2 (see [`read`]). `first` is the table of the first entry made
/// from the first host. Places outside `[hosts]` stay as they are.
fn moved(msg: &pb::Config, first: Option<&Vec<String>>, place: Place) -> Place {
    let has_server = |name: &str| msg.servers.iter().any(|s| s.name == name);
    let entry = |name: &str| match has_server(name) {
        true => table_path("servers", name),
        false => table_path("clients", name),
    };
    let host = |p: &[String]| match p {
        [g, name] if g == "hosts" => Some(name.clone()),
        _ => None,
    };
    let is_group = |p: &[String]| matches!(p, [g] if g == "hosts");
    let to_first = |place: Place| match first {
        Some(f) => Place::Above(Spot::Table(f.clone())),
        None => place,
    };
    let (above, spot) = match place {
        Place::Tail(p) if is_group(&p) => return to_first(Place::Tail(p)),
        Place::Tail(p) => {
            return match host(&p) {
                Some(name) => Place::Tail(entry(&name)),
                None => Place::Tail(p),
            }
        }
        Place::Above(spot) => (true, spot),
        Place::Trailing(spot) => (false, spot),
        other => return other,
    };
    let wrap = |spot: Spot| match above {
        true => Place::Above(spot),
        false => Place::Trailing(spot),
    };
    match spot {
        // A bare `[hosts]`, or `hosts = { … }` at the top level.
        Spot::Table(p) if is_group(&p) => to_first(wrap(Spot::Table(p))),
        Spot::Key(p, k) if p.is_empty() && k == "hosts" => to_first(wrap(Spot::Key(p, k))),
        // `N = { … }` under `[hosts]`.
        Spot::Key(p, name) if is_group(&p) => wrap(Spot::Table(entry(&name))),
        Spot::Table(p) => match host(&p) {
            Some(name) => wrap(Spot::Table(entry(&name))),
            None => wrap(Spot::Table(p)),
        },
        Spot::Key(p, k) => match host(&p) {
            Some(name) if k == "client_id" => wrap(Spot::Key(table_path("clients", &name), k)),
            Some(name) if has_server(&name) => wrap(Spot::Key(table_path("servers", &name), k)),
            // A key that only applies to a server, of a host with none.
            Some(name) => Place::Tail(table_path("clients", &name)),
            None => wrap(Spot::Key(p, k)),
        },
        Spot::Entry(p, k, i) => match host(&p) {
            Some(name) if has_server(&name) => {
                wrap(Spot::Entry(table_path("servers", &name), k, i))
            }
            Some(name) => Place::Tail(table_path("clients", &name)),
            None => wrap(Spot::Entry(p, k, i)),
        },
    }
}

/// A `[hosts.N]` of layout 1, as the message of that layout held it.
struct Host {
    name: String,
    endpoint_id: Option<Vec<u8>>,
    client_id: Option<Vec<u8>>,
    relay: Option<String>,
    relay_mode: Option<i32>,
    congestion: Option<i32>,
    pq_key_exchange: Option<i32>,
    address: Vec<pb::Entry>,
    address_set: bool,
    parallel: Option<u32>,
    reconnect: Option<prost_types::Duration>,
    dial_retries: Option<u32>,
    dht: Option<bool>,
    mdns: Option<bool>,
    ssh: Option<String>,
    ssh_aliases: Vec<pb::Entry>,
    ssh_aliases_set: bool,
}

/// Reads the text of a file of layout 1 into the message, without its
/// hosts and comments, and its hosts. `path` is the file named in errors.
fn read_v1_values(text: &str, path: &Path) -> Result<(pb::Config, Vec<Host>), ConfigError> {
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
    let hosts = hosts(raw.hosts, &at)?;
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
    Ok((config, hosts))
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
            // Its value is checked before this layout is chosen.
            "config_version" => continue,
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

/// The message for an alias that [`valid_alias`] refuses.
fn invalid_alias(alias: &str) -> String {
    format!(
        "invalid host alias {alias:?}: must be non-empty, contain no ':' or '/', and be neither all, * nor an EndpointId"
    )
}

/// The message for a host with neither id.
fn needs_an_id(alias: &str) -> String {
    format!("host {alias:?}: needs an endpoint_id (its daemon), a client_id, or both")
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

/// Validates the `[hosts]` table: alias rules and endpoint ids. The checks
/// run in alias order, as the old parse checked them, so that the first
/// error is the same; the entries keep the order of the file.
fn hosts(
    raw: BTreeMap<String, Spanned<RawHost>>,
    at: &impl Fn(usize, String) -> ConfigError,
) -> Result<Vec<Host>, ConfigError> {
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
        let host = Host {
            name: alias,
            endpoint_id,
            client_id,
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
        };
        hosts.push((start, host));
    }
    hosts.sort_by_key(|(start, _)| *start);
    Ok(hosts.into_iter().map(|(_, h)| h).collect())
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
