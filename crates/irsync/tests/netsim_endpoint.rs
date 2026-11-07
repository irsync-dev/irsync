//! No path bypasses the fault layer. Its own test binary: it sets
//! `IRSYNC_NETSIM` in this process before the first read of it.
#![cfg(feature = "netsim")]

use iroh::{SecretKey, TransportAddr};
use irsync::net::netsim::TRANSPORT_ID;
use irsync::net::{build_endpoint, EndpointOpts};

#[tokio::test]
async fn a_netsim_endpoint_has_only_netsim_addresses() {
    std::env::set_var(
        "IRSYNC_NETSIM",
        "profile=test-blackout,side=client,seed=1,stream=1",
    );
    let ep = build_endpoint(EndpointOpts::isolated(SecretKey::generate()))
        .await
        .unwrap();
    assert!(ep.bound_sockets().is_empty(), "{:?}", ep.bound_sockets());
    assert!(irsync::net::netsim::bound_addr().is_some());
    // Every address it advertises is a netsim one: no IP, no relay. iroh 1.3
    // lists only IP and relay addresses here, so for a netsim endpoint the
    // set is empty; the loop catches either kind appearing.
    let addr = ep.addr();
    for a in &addr.addrs {
        assert!(
            matches!(a, TransportAddr::Custom(c) if c.id() == TRANSPORT_ID),
            "{a:?}"
        );
    }
}
