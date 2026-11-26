//! Every key of `irsync.toml`, in one table: its dotted path, what it
//! holds and its default. The reader refuses a key that is not here, and
//! `irsync config` knows the keys and their defaults from here.
//!
//! A new key goes into the config message (`config.proto`), the reader's
//! serde shape, the writer, this table and `config set`'s field for it.
//! Tests hold them together: `full.toml` must set exactly the keys of
//! this table, which the message must have too.

use super::dial::Dial;
use super::reader::CONFIG_VERSION;
use super::settings::{OtlpProtocol, Settings, Spelled};
use std::path::Path;

/// What a key holds, which decides how `config set` reads a value and
/// how `config get` prints one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Int,
    Duration,
    Str,
    Path,
    Url,
    /// `default`, `disabled` or a URL.
    Relay,
    Id,
    StrArray,
    AddrArray,
    Table,
}

/// One key of the file.
pub struct Key {
    /// The dotted path, with `*` for the name of a server, a client or
    /// an export.
    pub path: &'static str,
    pub kind: Kind,
    /// What `config get` prints for the key when it is unset, one line
    /// per element; `None`: it has no default.
    pub default: Option<fn() -> Vec<String>>,
}

/// One value, printed.
fn one(v: impl ToString) -> Vec<String> {
    vec![v.to_string()]
}

/// A default path as the file would hold it: the home directory as `~`.
fn as_written(p: &Path) -> Vec<String> {
    one(
        match super::paths::home_dir().and_then(|h| p.strip_prefix(h).ok()) {
            Some(rest) => format!("~/{}", rest.display()),
            None => p.display().to_string(),
        },
    )
}

const fn key(path: &'static str, kind: Kind) -> Key {
    Key {
        path,
        kind,
        default: None,
    }
}

const fn with(path: &'static str, kind: Kind, default: fn() -> Vec<String>) -> Key {
    Key {
        path,
        kind,
        default: Some(default),
    }
}

use Kind::*;

/// Every key, each table's keys in the order the writer writes them, the
/// tables in the file's order.
pub static KEYS: &[Key] = &[
    with("config_version", Int, || one(CONFIG_VERSION)),
    with("unsafe_config_allowed", Bool, || {
        one(Settings::default().unsafe_config_allowed)
    }),
    with("state_dir", Path, || {
        as_written(&Settings::default().state_dir)
    }),
    key("defaults", Table),
    with("defaults.identity", Path, || {
        as_written(&Settings::default().identity)
    }),
    with("defaults.parallel", Int, || one(Dial::default().parallel)),
    with("defaults.reconnect", Duration, || {
        one(super::duration::format(Dial::default().reconnect))
    }),
    with("defaults.dial_retries", Int, || {
        one(Dial::default().dial_retries)
    }),
    with("defaults.ssh_install", Bool, || {
        one(Dial::default().ssh_install)
    }),
    with("defaults.relay", Relay, || one(Dial::default().relay)),
    with("defaults.dht", Bool, || one(Dial::default().dht)),
    with("defaults.dns", Bool, || one(Dial::default().dns)),
    with("defaults.mdns", Bool, || one(Dial::default().mdns)),
    with("defaults.congestion", Str, || {
        one(Dial::default().congestion.as_str())
    }),
    with("defaults.pq_key_exchange", Str, || {
        one(Dial::default().pq_key_exchange.as_str())
    }),
    with("defaults.idle_timeout", Duration, || {
        one(super::duration::format(Dial::default().idle_timeout))
    }),
    key("daemon", Table),
    with("daemon.identity", Path, || {
        as_written(&Settings::default().daemon.identity)
    }),
    with("daemon.idle_timeout", Duration, || {
        one(super::duration::format(
            Settings::default().daemon.idle_timeout,
        ))
    }),
    with("daemon.dormant_session_timeout", Duration, || {
        one(super::duration::format(
            Settings::default().daemon.dormant_session_timeout,
        ))
    }),
    with("daemon.max_dormant_sessions_per_key", Int, || {
        one(Settings::default().daemon.max_dormant_sessions_per_key)
    }),
    with("daemon.listen", AddrArray, || {
        let listen = Settings::default().daemon.listen;
        listen.iter().map(ToString::to_string).collect()
    }),
    with("daemon.portmap", Bool, || {
        one(Settings::default().daemon.portmap)
    }),
    with("daemon.relay", Relay, || {
        one(Settings::default().daemon.relay)
    }),
    with("daemon.dht", Bool, || {
        one(Settings::default().daemon.discovery.dht)
    }),
    with("daemon.dns", Bool, || {
        one(Settings::default().daemon.discovery.dns)
    }),
    with("daemon.mdns", Bool, || {
        one(Settings::default().daemon.discovery.mdns)
    }),
    with("daemon.congestion", Str, || {
        one(Settings::default().daemon.congestion.as_str())
    }),
    with("daemon.pq_key_exchange", Str, || {
        one(Settings::default().daemon.pq_key_exchange.as_str())
    }),
    // Off unless set.
    key("daemon.debug_addr", Str),
    with("daemon.allow_all_paths", Bool, || {
        one(Settings::default().daemon.allow_all_paths)
    }),
    with("daemon.allow_root", Bool, || {
        one(Settings::default().daemon.allow_root)
    }),
    key("telemetry", Table),
    // No export unless set.
    key("telemetry.otlp_endpoint", Url),
    with("telemetry.otlp_protocol", Str, || {
        one(OtlpProtocol::DEFAULT.as_str())
    }),
    // A server's keys fall back to [defaults]'s: none has a default of
    // its own.
    key("servers", Table),
    key("servers.*", Table),
    key("servers.*.endpoint_id", Id),
    key("servers.*.address", AddrArray),
    key("servers.*.relay", Relay),
    key("servers.*.dht", Bool),
    key("servers.*.dns", Bool),
    key("servers.*.mdns", Bool),
    key("servers.*.congestion", Str),
    key("servers.*.pq_key_exchange", Str),
    key("servers.*.parallel", Int),
    key("servers.*.reconnect", Duration),
    key("servers.*.dial_retries", Int),
    key("servers.*.ssh", Str),
    key("servers.*.ssh_aliases", StrArray),
    key("clients", Table),
    key("clients.*", Table),
    key("clients.*.client_id", Id),
    // The rights of [`super::clients::RIGHTS`]: none unless set.
    key("clients.*.all_exports", Str),
    key("clients.*.all_paths", Str),
    key("clients.*.admin", Str),
    key("exports", Table),
    key("exports.*", Table),
    key("exports.*.path", Path),
    // A flag left out of an export is off.
    with("exports.*.read_only", Bool, || one(false)),
    with("exports.*.index_at_start", Bool, || one(false)),
    with("exports.*.fsync", Bool, || one(false)),
    with("exports.*.auto", Bool, || one(false)),
    key("exports.*.rw", StrArray),
    key("exports.*.ro", StrArray),
];

/// Whether `path` is the key `pattern`, `*` standing for any name.
fn matches(pattern: &str, path: &[String]) -> bool {
    let parts: Vec<&str> = pattern.split('.').collect();
    parts.len() == path.len() && parts.iter().zip(path).all(|(p, s)| *p == "*" || p == s)
}

/// The key at `path`; `None` for a key the file does not know.
pub fn find(path: &[String]) -> Option<&'static Key> {
    KEYS.iter().find(|k| matches(k.path, path))
}

impl Key {
    /// The path of the table the key is in; `""` at the top.
    pub fn table(&self) -> &'static str {
        self.path.rsplit_once('.').map_or("", |(t, _)| t)
    }

    /// The last segment of the path.
    pub fn name(&self) -> &'static str {
        self.path.rsplit_once('.').map_or(self.path, |(_, n)| n)
    }
}
