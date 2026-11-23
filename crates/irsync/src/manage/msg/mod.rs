//! Edits of the config message, the replacement for the toml_edit based
//! editing in the parent module. Pure on a message; loading and saving
//! belong to the caller.

pub mod hosts;

use iroh::EndpointId;

/// An endpoint id as the message holds it: its 32 bytes.
pub fn id_bytes(id: EndpointId) -> Vec<u8> {
    id.as_bytes().to_vec()
}

/// The id of 32 bytes; `None` for bytes that are no id.
pub fn id_of(bytes: &[u8]) -> Option<EndpointId> {
    let b = <[u8; 32]>::try_from(bytes).ok()?;
    EndpointId::from_bytes(&b).ok()
}
