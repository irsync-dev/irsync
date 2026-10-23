//! Building the iroh endpoint used by both client and daemon.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;

use crate::config::client::HostConfig;
use crate::config::settings::{DaemonSettings, RelayMode as DaemonRelayMode};
use crate::net::addr::HostPort;

use iroh::address_lookup::{DnsAddressLookup, PkarrPublisher, PkarrResolver};
use iroh::endpoint::{
    presets, BindError, IdleTimeout, InvalidSocketAddr, PortmapperConfig, QuicTransportConfig,
    VarInt,
};
use iroh::{Endpoint, RelayMap, RelayMode, RelayUrl, SecretKey};
use iroh_mainline_address_lookup::DhtAddressLookup;
use iroh_mdns_address_lookup::MdnsAddressLookup;
use noq_proto::congestion::Bbr3Config;

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
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("invalid bind address: {0}")]
    BindAddr(#[from] InvalidSocketAddr),
    #[error("binding endpoint: {0}")]
    Bind(#[from] BindError),
}

/// QUIC transport tuned for bulk transfer: 60 s idle, 5 s keep-alive, large
/// windows, BBR congestion control.
pub fn transport_config() -> QuicTransportConfig {
    QuicTransportConfig::builder()
        .max_idle_timeout(Some(
            IdleTimeout::try_from(Duration::from_secs(60)).expect("60 s fits in an IdleTimeout"),
        ))
        .keep_alive_interval(Duration::from_secs(5))
        .receive_window(VarInt::from_u32(64 * MIB))
        .stream_receive_window(VarInt::from_u32(8 * MIB))
        .send_window(64 * u64::from(MIB))
        .max_concurrent_uni_streams(VarInt::from_u32(256))
        .max_concurrent_bidi_streams(VarInt::from_u32(8))
        .congestion_controller_factory(Arc::new(Bbr3Config::default()))
        .build()
}

/// The client's endpoint options. Any address in `addresses` (from the
/// command line, else the host's config) means isolated mode: no relay, no
/// lookups, no port mapper, whatever `cli_relay` is absent and the config
/// says. Without addresses: the relay from `cli_relay`, else the config,
/// else n0's; lookups as configured.
pub fn client_endpoint_opts(
    secret: SecretKey,
    cli_relay: Option<&str>,
    no_dht: bool,
    hc: &HostConfig,
    addresses: &[HostPort],
) -> anyhow::Result<EndpointOpts> {
    if !addresses.is_empty() {
        if hc.relay.is_some() || hc.dht.is_some() || hc.mdns.is_some() {
            tracing::debug!("Address given: ignoring Relay/Dht/Mdns from the client config");
        }
        return Ok(EndpointOpts::isolated(secret));
    }
    let relay = match (cli_relay, &hc.relay) {
        (Some(r), _) => RelayChoice::Custom(r.parse().context("bad --relay URL")?),
        (None, Some(u)) => RelayChoice::Custom(u.clone()),
        (None, None) => RelayChoice::N0Default,
    };
    Ok(EndpointOpts {
        secret,
        relay,
        dht: !no_dht && hc.dht.unwrap_or(true),
        dns: true,
        mdns: hc.mdns.unwrap_or(true),
        publish: false,
        bind_addrs: Vec::new(),
        portmap: true,
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
    }
}

/// Builds and binds the endpoint speaking [`irsync_proto::ALPN`].
pub async fn build_endpoint(opts: EndpointOpts) -> Result<Endpoint, NetError> {
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
        .transport_config(transport_config());

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
