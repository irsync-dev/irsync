//! No path bypasses the fault layer. Its own test binary: it sets
//! `IRSYNC_NETSIM` in this process before the first read of it.
#![cfg(feature = "netsim")]

use iroh::SecretKey;
use irsync::net::{build_endpoint, EndpointOpts};

#[tokio::test]
async fn a_netsim_endpoint_has_no_ip_socket() {
    std::env::set_var(
        "IRSYNC_NETSIM",
        "profile=test-blackout,side=client,seed=1,stream=1",
    );
    let ep = build_endpoint(EndpointOpts::isolated(SecretKey::generate()))
        .await
        .unwrap();
    assert!(ep.bound_sockets().is_empty(), "{:?}", ep.bound_sockets());
    assert!(irsync::net::netsim::bound_addr().is_some());
}
