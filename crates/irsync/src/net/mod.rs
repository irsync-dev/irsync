//! Networking: the iroh endpoint and length-prefixed protobuf framing.

mod endpoint;
pub mod frame;

pub use endpoint::{build_endpoint, transport_config, EndpointOpts, NetError, RelayChoice};
