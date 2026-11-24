//! Edits of the clients in the config message for `client add`,
//! `client remove` and `client rename`. Pure on a message; loading and
//! saving belong to the caller. A client is the id that grants name.

use super::grants::{lists_naming, remove_named, rename_named};
use super::{id_bytes, id_of, ManageError};
use crate::config::names::valid_alias;
use iroh::EndpointId;
use irsync_proto::config as pb;

/// Create or update client `name`: set its id. Returns the previous id
/// when the client existed with a different one.
pub fn client_add(
    msg: &mut pb::Config,
    name: &str,
    id: EndpointId,
) -> Result<Option<EndpointId>, ManageError> {
    if !valid_alias(name) {
        return Err(ManageError::BadAlias(name.to_string()));
    }
    let i = match msg.clients.iter().position(|c| c.name == name) {
        Some(i) => i,
        None => {
            msg.clients.push(pb::ClientEntry {
                name: name.to_string(),
                ..Default::default()
            });
            msg.clients.len() - 1
        }
    };
    let c = &mut msg.clients[i];
    let old = id_of(&c.client_id).filter(|old| *old != id);
    c.client_id = id_bytes(id);
    Ok(old)
}

/// Remove client `name`. A client that grants still name is not removed:
/// without it they would match nobody. With `force` those grants are
/// removed first. The number of grants removed.
///
/// Grants are matched by the name: canonicalize the message first so that
/// an EndpointId written for this client counts too.
pub fn client_remove(msg: &mut pb::Config, name: &str, force: bool) -> Result<usize, ManageError> {
    if !msg.clients.iter().any(|c| c.name == name) {
        return Err(ManageError::NotFound(name.to_string()));
    }
    let lists = lists_naming(msg, name);
    if !lists.is_empty() && !force {
        return Err(ManageError::ClientInUse {
            name: name.to_string(),
            lists,
        });
    }
    let removed = remove_named(msg, name);
    msg.clients.retain(|c| c.name != name);
    Ok(removed)
}

/// Rename client `old` to `new`, keeping its place and its comments, and
/// write every grant entry naming `old` as `new`. The number of grant
/// entries renamed. Refused, with `msg` unchanged, when `old` is no
/// client, `new` is not a valid name or `new` is a client already.
/// Renaming a client to its own name changes nothing.
///
/// Grants are matched by the name: canonicalize the message first so that
/// an EndpointId written for this client counts too.
pub fn client_rename(msg: &mut pb::Config, old: &str, new: &str) -> Result<usize, ManageError> {
    if !msg.clients.iter().any(|c| c.name == old) {
        return Err(ManageError::NotFound(old.to_string()));
    }
    if old == new {
        return Ok(0);
    }
    if !valid_alias(new) {
        return Err(ManageError::BadAlias(new.to_string()));
    }
    if msg.clients.iter().any(|c| c.name == new) {
        return Err(ManageError::ClientExists(new.to_string()));
    }
    for c in msg.clients.iter_mut().filter(|c| c.name == old) {
        c.name = new.to_string();
    }
    Ok(rename_named(msg, old, new))
}

/// The names of the clients of `msg`, in file order.
pub fn client_names(msg: &pb::Config) -> Vec<String> {
    msg.clients.iter().map(|c| c.name.clone()).collect()
}

/// The lines `client list` prints: `name: id`.
pub fn client_list(msg: &pb::Config) -> Vec<String> {
    msg.clients
        .iter()
        .map(|c| {
            let id = if c.client_id.is_empty() {
                "(missing)".to_string()
            } else {
                id_of(&c.client_id).map_or_else(|| "(invalid)".to_string(), |id| id.to_string())
            };
            format!("{}: {id}", c.name)
        })
        .collect()
}
