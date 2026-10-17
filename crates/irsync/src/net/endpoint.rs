//! Building the iroh endpoint used by both client and daemon.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use iroh::address_lookup::{DnsAddressLookup, PkarrPublisher, PkarrResolver};
use iroh::endpoint::{
    presets, BindError, IdleTimeout, InvalidSocketAddr, QuicTransportConfig, VarInt,
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
    /// Publish our address (daemon) or only resolve others (client).
    pub publish: bool,
    /// Bind to this address instead of the unspecified default (tests use loopback).
    pub bind_addr: Option<SocketAddr>,
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
    if let Some(addr) = opts.bind_addr {
        builder = builder.bind_addr(addr)?;
    }

    Ok(builder.bind().await?)
}
