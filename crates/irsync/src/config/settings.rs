//! `irsync.toml`: client and daemon settings.

use super::{expand_tilde, paths, ConfigError};
use crate::net::addr::HostPort;
use irsync_proto::config as pb;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;
use url::Url;

/// An enum key of the config file: its values as the file spells them
/// and as the message numbers them. [`spelled!`] writes each spelling
/// once; the reader, the writer and `irsync config set` all go through it.
pub trait Spelled: Copy + std::str::FromStr<Err = String> {
    /// The value as the file spells it.
    fn as_str(self) -> &'static str;
    /// The value as the message holds it.
    fn to_message(self) -> i32;
    /// A value of the message; an error for a number no value has.
    fn from_message(n: i32) -> Result<Self, String>;

    /// The value of the message, if set.
    fn from_optional(n: Option<i32>) -> Result<Option<Self>, String> {
        n.map(Self::from_message).transpose()
    }
}

/// `"a or b"`, `"a, b or c"`: the spellings a value may take.
fn alternatives(spellings: &[&str]) -> String {
    match spellings {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

/// Implements [`Spelled`] and `FromStr` for `$T`, whose message enum is
/// `pb::$pb` with variants of the same names; `$key` names the key in
/// errors.
macro_rules! spelled {
    ($T:ident, $pb:ident, $key:literal, $($V:ident => $s:literal),+ $(,)?) => {
        impl Spelled for $T {
            fn as_str(self) -> &'static str {
                match self {
                    $($T::$V => $s),+
                }
            }

            fn to_message(self) -> i32 {
                (match self {
                    $($T::$V => pb::$pb::$V),+
                }) as i32
            }

            fn from_message(n: i32) -> Result<$T, String> {
                match pb::$pb::try_from(n) {
                    $(Ok(pb::$pb::$V) => Ok($T::$V),)+
                    _ => Err(format!(concat!($key, ": unknown value {}"), n)),
                }
            }
        }

        impl std::str::FromStr for $T {
            type Err = String;
            fn from_str(s: &str) -> Result<$T, String> {
                match s {
                    $($s => Ok($T::$V),)+
                    _ => Err(format!("{s:?}: expected {}", alternatives(&[$($s),+]))),
                }
            }
        }
    };
}

/// Which relay servers an endpoint uses: `relay` in `[daemon]`,
/// `[defaults]` and a server's table, and `--relay`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Relay {
    /// n0's public relays.
    #[default]
    Default,
    /// No relay: direct connections only. The lookups stay on.
    Disabled,
    /// A self-hosted relay.
    Url(Url),
}

impl std::fmt::Display for Relay {
    /// As the file spells it.
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Relay::Default => f.write_str("default"),
            Relay::Disabled => f.write_str("disabled"),
            Relay::Url(u) => f.write_str(u.as_str()),
        }
    }
}

impl std::str::FromStr for Relay {
    type Err = String;
    fn from_str(s: &str) -> Result<Relay, String> {
        match s {
            "default" => Ok(Relay::Default),
            "disabled" => Ok(Relay::Disabled),
            _ => Url::parse(s)
                .map(Relay::Url)
                .map_err(|e| format!("{s:?}: expected default, disabled or a URL ({e})")),
        }
    }
}

/// A relay of the message, if set.
pub(crate) fn relay_from(s: Option<&str>) -> Result<Option<Relay>, String> {
    s.map(str::parse).transpose()
}

/// The congestion controller of the side that sends: the client's for a
/// push, the daemon's for a pull. BBR keeps the bottleneck's queue short
/// and copes with random loss (mobile links); Cubic fills the queue but
/// keeps a link busy when the reverse path delays acknowledgements, e.g.
/// an upload beside a saturated download (see `NETSTRESS_DECISIONS.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Congestion {
    #[default]
    Bbr,
    Cubic,
}

spelled!(Congestion, Congestion, "congestion", Bbr => "bbr", Cubic => "cubic");

/// Whether a connection must have used the post-quantum hybrid key
/// exchange (`X25519MLKEM768`). Every endpoint offers it first either way;
/// this says what happens when the other side does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PqKeyExchange {
    /// Use the hybrid when the other side offers it, a classical key
    /// exchange otherwise.
    #[default]
    Prefer,
    /// Refuse a connection that did not get the hybrid.
    Require,
}

spelled!(PqKeyExchange, PqKeyExchange, "pq_key_exchange", Prefer => "prefer", Require => "require");

/// How clients may find the daemon: `dht`, `dns` and `mdns` in
/// `[daemon]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    pub dht: bool,
    pub dns: bool,
    pub mdns: bool,
}

impl Default for Discovery {
    fn default() -> Self {
        Discovery {
            dht: true,
            dns: true,
            mdns: true,
        }
    }
}

/// The message for a `listen` with two addresses of one family, or
/// `None` when it has at most one of each.
pub(crate) fn listen_problem(listen: &[HostPort]) -> Option<&'static str> {
    let (mut v4, mut v6) = (0, 0);
    for entry in listen {
        match entry {
            HostPort::Ip(a) if a.is_ipv4() => v4 += 1,
            HostPort::Ip(_) => v6 += 1,
            HostPort::Name { .. } => {}
        }
    }
    (v4 > 1 || v6 > 1).then_some("at most one IPv4 and one IPv6 listen address")
}

/// Resolves `listen` to the sockets to bind: each entry contributes the
/// first IPv4 and the first IPv6 address it yields; two entries landing in
/// one family is an error naming both.
pub async fn listen_sockets(listen: &[HostPort]) -> anyhow::Result<Vec<SocketAddr>> {
    use anyhow::{anyhow, Context};
    let mut out: Vec<(SocketAddr, &HostPort)> = Vec::new();
    for entry in listen {
        let resolved = entry
            .resolve()
            .await
            .with_context(|| format!("resolving listen address {entry}"))?;
        if resolved.is_empty() {
            return Err(anyhow!("listen address {entry} resolved to no addresses"));
        }
        let mut taken = [false, false];
        for addr in resolved {
            let fam = usize::from(addr.is_ipv6());
            if taken[fam] {
                continue;
            }
            taken[fam] = true;
            if let Some((_, prev)) = out.iter().find(|(a, _)| a.is_ipv6() == addr.is_ipv6()) {
                let family = if addr.is_ipv6() { "IPv6" } else { "IPv4" };
                return Err(anyhow!(
                    "listen: {entry} and {prev} are both {family}; at most one per family"
                ));
            }
            out.push((addr, entry));
        }
    }
    Ok(out.into_iter().map(|(a, _)| a).collect())
}

/// A debug HTTP address: `IP:PORT`, or a bare port on `127.0.0.1`.
pub fn parse_debug_addr(s: &str) -> Result<SocketAddr, String> {
    if let Ok(port) = s.parse::<u16>() {
        return Ok(SocketAddr::from(([127, 0, 0, 1], port)));
    }
    s.parse()
        .map_err(|_| format!("bad debug address {s:?}: expected PORT or IP:PORT"))
}

/// `[defaults]` as the file says it, but for `identity` (see
/// [`Settings::identity`]): a key left out is `None`. A dial decides
/// each from here, the command line and the server's table in
/// [`super::dial::Dial::resolve`], which also holds the built-in
/// defaults.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Defaults {
    pub parallel: Option<u32>,
    pub reconnect: Option<Duration>,
    /// Retries for a server that has not answered yet.
    pub dial_retries: Option<u32>,
    /// Set the server up over ssh by copying irsync there. `false` is
    /// `--no-install` for every run.
    pub ssh_install: Option<bool>,
    pub relay: Option<Relay>,
    pub dht: Option<bool>,
    pub dns: Option<bool>,
    pub mdns: Option<bool>,
    /// For pushes.
    pub congestion: Option<Congestion>,
    pub pq_key_exchange: Option<PqKeyExchange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonSettings {
    pub identity: PathBuf,
    pub idle_timeout: Duration,
    pub dormant_session_timeout: Duration,
    /// How many dormant sessions one key may leave in an export; the oldest
    /// goes when there are more. Zero: no limit.
    pub max_dormant_sessions_per_key: u32,
    pub listen: Vec<HostPort>,
    pub portmap: bool,
    pub relay: Relay,
    /// For pulls.
    pub congestion: Congestion,
    /// `require`: close a connection that did not get the hybrid key
    /// exchange. For the whole daemon; read at every connection, so a
    /// reload applies.
    pub pq_key_exchange: PqKeyExchange,
    pub discovery: Discovery,
    /// The debug HTTP port's address (see [`parse_debug_addr`]); off when unset.
    pub debug_addr: Option<SocketAddr>,
    /// Let a client with the `all_paths` right reach any path (and create
    /// auto exports for them): `allow_all_paths`.
    pub allow_all_paths: bool,
    /// Let the daemon run as root (see `irsync daemon`'s refusal).
    pub allow_root: bool,
}

/// How OTLP export talks to the collector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtlpProtocol {
    Grpc,
    Http,
}

impl OtlpProtocol {
    /// The protocol when neither the config file nor the environment
    /// names one.
    pub const DEFAULT: OtlpProtocol = OtlpProtocol::Http;
}

spelled!(OtlpProtocol, OtlpProtocol, "otlp_protocol", Grpc => "grpc", Http => "http");

/// `[telemetry]`: OTLP export, for either side.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TelemetrySettings {
    pub otlp_endpoint: Option<Url>,
    pub otlp_protocol: Option<OtlpProtocol>,
}

impl Default for DaemonSettings {
    fn default() -> Self {
        DaemonSettings {
            identity: paths::config_dir().join("daemon_ed25519"),
            idle_timeout: crate::session::engine::DAEMON_IDLE_TIMEOUT,
            dormant_session_timeout: Duration::from_secs(7 * 86400),
            max_dormant_sessions_per_key: 25,
            listen: Vec::new(),
            portmap: true,
            relay: Relay::Default,
            congestion: Congestion::Bbr,
            pq_key_exchange: PqKeyExchange::Prefer,
            discovery: Discovery::default(),
            debug_addr: None,
            allow_all_paths: true,
            allow_root: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// True: the config file, a private key and a passphrase file are used
    /// even when other users can read or change them, or own them.
    pub unsafe_config_allowed: bool,
    /// The client identity, `[defaults] identity`: the key to load, or to
    /// generate when nothing else serves (see `ssh_identity`).
    pub identity: PathBuf,
    /// `~/.ssh/id_ed25519`, used when `identity` is not set in the file and
    /// the key at `identity` does not exist yet; never generated. Not a file
    /// key: `None` once the file names an identity.
    pub ssh_identity: Option<PathBuf>,
    pub state_dir: PathBuf,
    /// `[defaults]`: this machine's defaults for dialling a server.
    pub defaults: Defaults,
    pub daemon: DaemonSettings,
    pub telemetry: TelemetrySettings,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            unsafe_config_allowed: false,
            identity: paths::config_dir().join("id_ed25519"),
            ssh_identity: paths::ssh_identity(),
            state_dir: paths::state_dir(),
            defaults: Defaults::default(),
            daemon: DaemonSettings::default(),
            telemetry: TelemetrySettings::default(),
        }
    }
}

/// A duration of the message, if set.
pub(crate) fn duration_from(d: Option<&prost_types::Duration>) -> Result<Option<Duration>, String> {
    d.map(|d| Duration::try_from(*d).map_err(|e| format!("bad duration: {e}")))
        .transpose()
}

/// A URL of the message, if set; the message the reader gives for a bad one.
pub(crate) fn url_from(s: Option<&str>) -> Result<Option<Url>, String> {
    s.map(|s| Url::parse(s).map_err(|e| format!("{e}: {s:?}")))
        .transpose()
}

/// An address array of the message.
pub(crate) fn addresses_from(entries: &[pb::Entry]) -> Result<Vec<HostPort>, String> {
    entries
        .iter()
        .map(|e| e.value.parse::<HostPort>().map_err(|e| e.to_string()))
        .collect()
}

impl Defaults {
    fn from_message(c: &pb::Defaults) -> Result<Defaults, String> {
        Ok(Defaults {
            parallel: c.parallel,
            reconnect: duration_from(c.reconnect.as_ref())?,
            dial_retries: c.dial_retries,
            dht: c.dht,
            dns: c.dns,
            ssh_install: c.ssh_install,
            mdns: c.mdns,
            relay: relay_from(c.relay.as_deref())?,
            congestion: Congestion::from_optional(c.congestion)?,
            pq_key_exchange: PqKeyExchange::from_optional(c.pq_key_exchange)?,
        })
    }
}

impl DaemonSettings {
    fn from_message(d: &pb::Daemon) -> Result<DaemonSettings, String> {
        let defaults = DaemonSettings::default();
        let listen = addresses_from(&d.listen)?;
        if let Some(problem) = listen_problem(&listen) {
            return Err(problem.to_string());
        }
        let discovery = Discovery {
            dht: d.dht.unwrap_or(defaults.discovery.dht),
            dns: d.dns.unwrap_or(defaults.discovery.dns),
            mdns: d.mdns.unwrap_or(defaults.discovery.mdns),
        };
        Ok(DaemonSettings {
            identity: d.identity.as_ref().map_or(defaults.identity, PathBuf::from),
            idle_timeout: duration_from(d.idle_timeout.as_ref())?.unwrap_or(defaults.idle_timeout),
            dormant_session_timeout: duration_from(d.dormant_session_timeout.as_ref())?
                .unwrap_or(defaults.dormant_session_timeout),
            max_dormant_sessions_per_key: d
                .max_dormant_sessions_per_key
                .unwrap_or(defaults.max_dormant_sessions_per_key),
            listen,
            portmap: d.portmap.unwrap_or(defaults.portmap),
            relay: relay_from(d.relay.as_deref())?.unwrap_or(defaults.relay),
            congestion: Congestion::from_optional(d.congestion)?.unwrap_or(defaults.congestion),
            pq_key_exchange: PqKeyExchange::from_optional(d.pq_key_exchange)?
                .unwrap_or(defaults.pq_key_exchange),
            discovery,
            debug_addr: d.debug_addr.as_deref().map(parse_debug_addr).transpose()?,
            allow_all_paths: d.allow_all_paths.unwrap_or(defaults.allow_all_paths),
            allow_root: d.allow_root.unwrap_or(defaults.allow_root),
        })
    }
}

impl TelemetrySettings {
    fn from_message(t: &pb::Telemetry) -> Result<TelemetrySettings, String> {
        Ok(TelemetrySettings {
            otlp_endpoint: url_from(t.otlp_endpoint.as_deref())?,
            otlp_protocol: OtlpProtocol::from_optional(t.otlp_protocol)?,
        })
    }
}

impl Settings {
    /// The settings the message holds: the defaults for what it leaves
    /// unset, and `~` expanded in paths. `path` is the file named in
    /// errors.
    pub fn from_message(msg: &pb::Config, path: &Path) -> Result<Settings, ConfigError> {
        let in_table = |table: &'static str| {
            move |message: String| super::file_error(path, format!("[{table}]: {message}"))
        };
        let defaults = Settings::default();
        let mut settings = Settings {
            unsafe_config_allowed: msg.unsafe_config_allowed.unwrap_or(false),
            // A named identity is the whole answer; the ssh key is only
            // consulted for the built-in default.
            ssh_identity: msg
                .defaults
                .as_ref()
                .and_then(|d| d.identity.as_ref())
                .is_none()
                .then_some(defaults.ssh_identity)
                .flatten(),
            identity: msg
                .defaults
                .as_ref()
                .and_then(|d| d.identity.as_ref())
                .map_or(defaults.identity, PathBuf::from),
            state_dir: msg
                .state_dir
                .as_ref()
                .map_or(defaults.state_dir, PathBuf::from),
            defaults: msg
                .defaults
                .as_ref()
                .map(Defaults::from_message)
                .transpose()
                .map_err(in_table("defaults"))?
                .unwrap_or_default(),
            daemon: msg
                .daemon
                .as_ref()
                .map(DaemonSettings::from_message)
                .transpose()
                .map_err(in_table("daemon"))?
                .unwrap_or_default(),
            telemetry: msg
                .telemetry
                .as_ref()
                .map(TelemetrySettings::from_message)
                .transpose()
                .map_err(in_table("telemetry"))?
                .unwrap_or_default(),
        };
        for p in [
            &mut settings.identity,
            &mut settings.state_dir,
            &mut settings.daemon.identity,
        ] {
            *p = expand_tilde(&p.to_string_lossy());
        }
        Ok(settings)
    }

    /// The settings of a merged document (see [`super::file::Config`]).
    pub fn parse(text: &str, path: &Path) -> Result<Settings, ConfigError> {
        super::file::Config::parse(text, path).map(|c| c.settings)
    }
}
