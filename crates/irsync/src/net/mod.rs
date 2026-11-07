//! Networking: the iroh endpoint and length-prefixed protobuf framing.

pub mod addr;
mod endpoint;
pub mod frame;
#[cfg(feature = "netsim")]
pub mod netsim;

#[cfg(feature = "test-hooks")]
pub use endpoint::build_endpoint_with_stream_window;
pub use endpoint::{
    build_endpoint, client_endpoint_opts, daemon_endpoint_opts, transport_config, EndpointOpts,
    NetError, RelayChoice,
};
