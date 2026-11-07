//! Building the iroh endpoint used by both client and daemon.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;

use crate::config::hosts::HostEntry;
use crate::config::settings::{
    ClientSettings, Congestion, DaemonSettings, RelayMode as DaemonRelayMode,
};
use crate::net::addr::HostPort;

use iroh::address_lookup::{DnsAddressLookup, PkarrPublisher, PkarrResolver};
use iroh::endpoint::{
    presets, AckFrequencyConfig, BindError, IdleTimeout, InvalidSocketAddr, PortmapperConfig,
    QuicTransportConfig, VarInt,
};
use iroh::{Endpoint, RelayMap, RelayMode, RelayUrl, SecretKey};
use iroh_mainline_address_lookup::DhtAddressLookup;
use iroh_mdns_address_lookup::MdnsAddressLookup;
use noq_proto::congestion::{Bbr3Config, ControllerFactory, CubicConfig};

const MIB: u32 = 1024 * 1024;

/// Which relay servers the endpoint uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayChoice {
    /// Number 0's public relays.
    N0Default,
    /// A single self-hosted relay.
    Custom(url::Url),
    /// No relay: direct connections only.
    Disabled,
}

/// Everything needed to build an endpoint.
#[derive(Debug, Clone)]
pub struct EndpointOpts {
    pub secret: SecretKey,
    pub relay: RelayChoice,
    /// Mainline DHT address lookup.
    pub dht: bool,
    /// n0 DNS / pkarr address lookup.
    pub dns: bool,
    /// Local-network mDNS address lookup.
    pub mdns: bool,
    /// Publish our address (daemon) or only resolve others (client). Applies to
    /// DHT and DNS publishing and also controls whether mDNS advertises us.
    pub publish: bool,
    /// Bind exactly these instead of iroh's unspecified IPv4/IPv6 defaults
    /// (tests use loopback). Empty keeps the defaults.
    pub bind_addrs: Vec<SocketAddr>,
    /// iroh's UPnP/PCP/NAT-PMP port mapper.
    pub portmap: bool,
    /// The congestion controller for what this endpoint sends.
    pub congestion: Congestion,
}

impl EndpointOpts {
    /// No relay, no lookups, no publishing, no port mapper, default binds:
    /// an endpoint that talks to nobody but the peers it is pointed at.
    pub fn isolated(secret: SecretKey) -> EndpointOpts {
        EndpointOpts {
            secret,
            relay: RelayChoice::Disabled,
            dht: false,
            dns: false,
            mdns: false,
            publish: false,
            bind_addrs: Vec::new(),
            portmap: false,
            congestion: Congestion::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("invalid bind address: {0}")]
    BindAddr(#[from] InvalidSocketAddr),
    #[error("binding endpoint: {0}")]
    Bind(#[from] BindError),
    #[cfg(feature = "netsim")]
    #[error("{0:#}")]
    NetSim(anyhow::Error),
}

/// Ask the peer to ACK every packet, within 1 ms. A data stream holds one
/// of the sender's send permits until `stopped()` resolves, which is when
/// the peer has ACKed every byte (see `send.rs`, where the permit is held
/// across `stopped()`). With the default delayed ACKs (every second packet
/// or 25 ms) all permits go idle together and the sender stalls for the
/// delay.
fn immediate_acks() -> AckFrequencyConfig {
    let mut ack = AckFrequencyConfig::default();
    ack.ack_eliciting_threshold(VarInt::from_u32(0));
    ack.max_ack_delay(Some(Duration::from_millis(1)));
    ack
}

/// Bytes each stream may receive ahead of what the application has read.
const STREAM_WINDOW: u32 = 8 * MIB;

/// QUIC transport tuned for bulk transfer: 60 s idle, 5 s keep-alive, large
/// windows, the congestion controller `congestion`, and immediate ACKs
/// asked of the peer (see [`immediate_acks`]).
pub fn transport_config(congestion: Congestion) -> QuicTransportConfig {
    transport_config_with(STREAM_WINDOW, congestion)
}

/// [`transport_config`] with each stream's receive window `stream_window`
/// bytes.
fn transport_config_with(stream_window: u32, congestion: Congestion) -> QuicTransportConfig {
    let controller: Arc<dyn ControllerFactory + Send + Sync> = match congestion {
        Congestion::Bbr => Arc::new(Bbr3Config::default()),
        Congestion::Cubic => Arc::new(CubicConfig::default()),
    };
    QuicTransportConfig::builder()
        .max_idle_timeout(Some(
            IdleTimeout::try_from(Duration::from_secs(60)).expect("60 s fits in an IdleTimeout"),
        ))
        .keep_alive_interval(Duration::from_secs(5))
        .receive_window(VarInt::from_u32(64 * MIB))
        .stream_receive_window(VarInt::from_u32(stream_window))
        .send_window(64 * u64::from(MIB))
        .max_concurrent_uni_streams(VarInt::from_u32(256))
        .max_concurrent_bidi_streams(VarInt::from_u32(8))
        .congestion_controller_factory(controller)
        .ack_frequency_config(Some(immediate_acks()))
        .build()
}

/// The client's endpoint options. Any address in `addresses` (from the
/// command line, else the host entry) means isolated mode: no relay, no
/// lookups, no port mapper, whatever the config says; `cli_relay` together
/// with addresses is an error. Without addresses: the relay from `cli_relay`, else none when
/// `no_relay` or a `relay_mode = "disabled"` (host entry, then client
/// settings) says so, else the host entry's, else the client settings',
/// else n0's; lookups likewise (host entry, then client settings, then on).
pub fn client_endpoint_opts(
    secret: SecretKey,
    cli_relay: Option<&str>,
    no_relay: bool,
    no_dht: bool,
    client: &ClientSettings,
    host: Option<&HostEntry>,
    addresses: &[HostPort],
) -> anyhow::Result<EndpointOpts> {
    let host_relay = host.and_then(|h| h.relay.as_ref());
    // `--no-relay`, else the host entry's mode, else the client settings'.
    let relay_off = no_relay
        || host
            .and_then(|h| h.relay_mode)
            .or(client.relay_mode)
            .is_some_and(|m| m == DaemonRelayMode::Disabled);
    let host_dht = host.and_then(|h| h.dht);
    let host_mdns = host.and_then(|h| h.mdns);
    // The host entry's, else the client settings', else BBR.
    let congestion = host
        .and_then(|h| h.congestion)
        .or(client.congestion)
        .unwrap_or_default();
    if !addresses.is_empty() {
        // The host entry's addresses with `--relay` on the command line: the
        // same contradiction as `--address` with `--relay`.
        if cli_relay.is_some() {
            anyhow::bail!(
                "--relay cannot be used with direct addresses (the host entry has `address`)"
            );
        }
        if host_relay.is_some()
            || host_dht.is_some()
            || host_mdns.is_some()
            || client.relay.is_some()
            || client.dht.is_some()
            || client.mdns.is_some()
        {
            tracing::debug!("address given: ignoring relay/dht/mdns from the config");
        }
        return Ok(EndpointOpts {
            congestion,
            ..EndpointOpts::isolated(secret)
        });
    }
    // A `--relay` on the command line beats a config file's "disabled".
    let relay = match (cli_relay, host_relay.or(client.relay.as_ref())) {
        (Some(r), _) => RelayChoice::Custom(r.parse().context("bad --relay URL")?),
        _ if relay_off => RelayChoice::Disabled,
        (None, Some(u)) => RelayChoice::Custom(u.clone()),
        (None, None) => RelayChoice::N0Default,
    };
    Ok(EndpointOpts {
        secret,
        relay,
        dht: !no_dht && host_dht.or(client.dht).unwrap_or(true),
        dns: true,
        mdns: host_mdns.or(client.mdns).unwrap_or(true),
        publish: false,
        bind_addrs: Vec::new(),
        portmap: true,
        congestion,
    })
}

/// The daemon's endpoint options from its settings; `bind_addrs` are the
/// already resolved `listen` sockets.
pub fn daemon_endpoint_opts(
    cfg: &DaemonSettings,
    secret: SecretKey,
    bind_addrs: Vec<SocketAddr>,
) -> EndpointOpts {
    let relay = match (&cfg.relay_mode, &cfg.relay) {
        (DaemonRelayMode::Disabled, _) => RelayChoice::Disabled,
        (_, Some(url)) => RelayChoice::Custom(url.clone()),
        _ => RelayChoice::N0Default,
    };
    EndpointOpts {
        secret,
        relay,
        dht: cfg.discovery.dht,
        dns: cfg.discovery.dns,
        mdns: cfg.discovery.mdns,
        publish: true,
        bind_addrs,
        portmap: cfg.portmap,
        congestion: cfg.congestion,
    }
}

/// Builds and binds the endpoint speaking [`irsync_proto::ALPN`].
pub async fn build_endpoint(opts: EndpointOpts) -> Result<Endpoint, NetError> {
    let transport = transport_config(opts.congestion);
    build(opts, transport).await
}

/// Test hook: [`build_endpoint`] with each stream's receive window
/// `stream_window` bytes instead of 8 MiB, so a test can fill the buffers
/// between two sides with fewer bytes (for one, where paths must be short).
#[cfg(feature = "test-hooks")]
pub async fn build_endpoint_with_stream_window(
    opts: EndpointOpts,
    stream_window: u32,
) -> Result<Endpoint, NetError> {
    let transport = transport_config_with(stream_window, opts.congestion);
    build(opts, transport).await
}

async fn build(opts: EndpointOpts, transport: QuicTransportConfig) -> Result<Endpoint, NetError> {
    let builder = match &opts.relay {
        RelayChoice::N0Default => Endpoint::builder(presets::N0),
        RelayChoice::Custom(url) => Endpoint::builder(presets::N0).relay_mode(RelayMode::Custom(
            RelayMap::from(RelayUrl::from(url.clone())),
        )),
        RelayChoice::Disabled => {
            Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled)
        }
    };

    // Start from no lookups and add exactly what was asked for, so the N0
    // preset's own pkarr/DNS services are neither duplicated nor forced on.
    let mut builder = builder
        .clear_address_lookup()
        .secret_key(opts.secret.clone())
        .alpns(vec![irsync_proto::ALPN.to_vec()])
        .transport_config(transport);

    if opts.dns {
        if opts.publish {
            builder = builder.address_lookup(PkarrPublisher::n0_dns());
        }
        builder = builder
            .address_lookup(PkarrResolver::n0_dns())
            .address_lookup(DnsAddressLookup::n0_dns());
    }
    if opts.dht {
        let dht = DhtAddressLookup::builder();
        let dht = if opts.publish {
            dht.secret_key(opts.secret.clone())
        } else {
            dht.no_publish()
        };
        builder = builder.address_lookup(dht);
    }
    if opts.mdns {
        builder = builder.address_lookup(MdnsAddressLookup::builder().advertise(opts.publish));
    }
    #[cfg(feature = "netsim")]
    if let Some(spec) = crate::net::netsim::active().map_err(NetError::NetSim)? {
        use std::net::{Ipv4Addr, SocketAddrV4};
        crate::net::netsim::log_active(spec);
        let bind = opts
            .bind_addrs
            .iter()
            .find_map(|a| match a {
                SocketAddr::V4(a) => Some(*a),
                SocketAddr::V6(_) => None,
            })
            .unwrap_or(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0));
        // Every packet goes through the fault layer: no IP socket, no relay.
        builder = builder
            .clear_ip_transports()
            .clear_relay_transports()
            .add_custom_transport(crate::net::netsim::transport(spec, bind));
        if !opts.portmap {
            builder = builder.portmapper_config(PortmapperConfig::Disabled);
        }
        return Ok(builder.bind().await?);
    }
    if !opts.bind_addrs.is_empty() {
        builder = builder.clear_ip_transports();
        for addr in &opts.bind_addrs {
            builder = builder.bind_addr(*addr)?;
        }
    }
    if !opts.portmap {
        builder = builder.portmapper_config(PortmapperConfig::Disabled);
    }

    Ok(builder.bind().await?)
}
