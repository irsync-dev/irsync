//! One dial's settings, decided in one place: the command line, then the
//! server's table, then `[defaults]`, then the built-in default.

use super::servers::ServerEntry;
use super::settings::{Congestion, Defaults, PqKeyExchange, RelayMode};
use crate::net::addr::HostPort;
use crate::net::RelayChoice;
use std::time::Duration;
use url::Url;

/// What the command line says about a dial; what it leaves unset, the
/// config decides.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DialFlags {
    pub parallel: Option<u32>,
    pub reconnect: Option<Duration>,
    pub dial_retries: Option<u32>,
    /// `--relay URL`.
    pub relay: Option<Url>,
    /// `--no-relay`.
    pub no_relay: bool,
    /// `--no-dht`.
    pub no_dht: bool,
    pub pq_key_exchange: Option<PqKeyExchange>,
    /// `--address`: replaces the server's `address`.
    pub address: Vec<HostPort>,
}

/// The settings of one dial, every one decided. [`Dial::default`] is the
/// built-in default of each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dial {
    /// Data streams a push keeps open at once.
    pub parallel: u32,
    /// How long to keep redialling after a dropped connection.
    pub reconnect: Duration,
    /// Retries of a server that has not answered yet.
    pub dial_retries: u32,
    /// Copy irsync to the server when `irsync pair` sets it up.
    pub ssh_install: bool,
    /// Direct addresses: with any, the relay and the lookups are off.
    pub address: Vec<HostPort>,
    pub relay: RelayChoice,
    pub dht: bool,
    pub dns: bool,
    pub mdns: bool,
    /// For pushes.
    pub congestion: Congestion,
    pub pq_key_exchange: PqKeyExchange,
}

impl Default for Dial {
    fn default() -> Dial {
        let reconnect = crate::session::reconnect::ReconnectOpts::default();
        Dial {
            parallel: crate::send::DEFAULT_PARALLEL as u32,
            reconnect: reconnect.max,
            dial_retries: reconnect.dial_retries,
            ssh_install: true,
            address: Vec::new(),
            relay: RelayChoice::N0Default,
            dht: true,
            dns: true,
            mdns: true,
            congestion: Congestion::default(),
            pq_key_exchange: PqKeyExchange::default(),
        }
    }
}

/// The server's value, if there is a server.
fn of<T>(server: Option<&ServerEntry>, f: impl Fn(&ServerEntry) -> Option<T>) -> Option<T> {
    server.and_then(f)
}

/// The first of `choices` that is set, else `builtin`.
fn first<T, const N: usize>(choices: [Option<T>; N], builtin: T) -> T {
    choices.into_iter().flatten().next().unwrap_or(builtin)
}

impl Dial {
    /// The settings for dialling `server` (`None`: a bare EndpointId)
    /// with `flags` and `defaults`. A server that `irsync pair` saved (one
    /// with `ssh`) is dialled once unless the command line asks for
    /// retries. An error for `--relay` with the server's `address`.
    pub fn resolve(
        flags: &DialFlags,
        server: Option<&ServerEntry>,
        defaults: &Defaults,
    ) -> Result<Dial, String> {
        let builtin = Dial::default();
        let address = if flags.address.is_empty() {
            server.map(|s| s.address.clone()).unwrap_or_default()
        } else {
            flags.address.clone()
        };
        if flags.relay.is_some() && !address.is_empty() {
            return Err(
                "--relay cannot be used with direct addresses (the server's entry has `address`)"
                    .into(),
            );
        }
        let relay_off = flags.no_relay
            || first(
                [of(server, |s| s.relay_mode), defaults.relay_mode],
                RelayMode::Default,
            ) == RelayMode::Disabled;
        // `--relay` on the command line beats a config file's "disabled".
        let relay = match (
            &flags.relay,
            of(server, |s| s.relay.clone()).or(defaults.relay.clone()),
        ) {
            (Some(url), _) => RelayChoice::Custom(url.clone()),
            _ if relay_off => RelayChoice::Disabled,
            (None, Some(url)) => RelayChoice::Custom(url),
            (None, None) => builtin.relay,
        };
        let paired = server.is_some_and(|s| s.ssh.is_some());
        let dial_retries = if paired {
            flags.dial_retries.unwrap_or(0)
        } else {
            first(
                [
                    flags.dial_retries,
                    of(server, |s| s.dial_retries),
                    defaults.dial_retries,
                ],
                builtin.dial_retries,
            )
        };
        Ok(Dial {
            parallel: first(
                [
                    flags.parallel,
                    of(server, |s| s.parallel),
                    defaults.parallel,
                ],
                builtin.parallel,
            ),
            reconnect: first(
                [
                    flags.reconnect,
                    of(server, |s| s.reconnect),
                    defaults.reconnect,
                ],
                builtin.reconnect,
            ),
            dial_retries,
            ssh_install: defaults.ssh_install.unwrap_or(builtin.ssh_install),
            address,
            relay,
            dht: !flags.no_dht && first([of(server, |s| s.dht), defaults.dht], builtin.dht),
            dns: first([of(server, |s| s.dns), defaults.dns], builtin.dns),
            mdns: first([of(server, |s| s.mdns), defaults.mdns], builtin.mdns),
            congestion: first(
                [of(server, |s| s.congestion), defaults.congestion],
                builtin.congestion,
            ),
            pq_key_exchange: first(
                [
                    flags.pq_key_exchange,
                    of(server, |s| s.pq_key_exchange),
                    defaults.pq_key_exchange,
                ],
                builtin.pq_key_exchange,
            ),
        })
    }
}
