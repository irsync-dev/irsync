//! Edits of the clients in the config message for `client add`,
//! `client remove` and `client rename`. Pure on a message; loading and
//! saving belong to the caller. A client is the id that grants name.

use super::grants::{lists_naming, remove_named, rename_named};
use super::{id_bytes, id_of, ManageError};
use crate::authz::Access;
use crate::config::clients::{Right, RIGHTS};
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

/// Parse one `KEY=VALUE` of `client set`: a right of [`RIGHTS`] and
/// `rw`, `ro` or `none` (`None`: remove the right).
pub fn parse_setting(s: &str) -> Result<(Right, Option<Access>), ManageError> {
    let keys = || RIGHTS.map(Right::key).join(", ");
    let Some((key, value)) = s.split_once('=') else {
        return Err(ManageError::BadValue(format!(
            "{s:?} is not KEY=VALUE (keys: {})",
            keys()
        )));
    };
    let Some(right) = RIGHTS.into_iter().find(|r| r.key() == key) else {
        return Err(ManageError::BadValue(format!(
            "{key:?} is not a right of a client (rights: {})",
            keys()
        )));
    };
    if value == "none" {
        return Ok((right, None));
    }
    match Access::parse(value) {
        Some(a) => Ok((right, Some(a))),
        None => Err(ManageError::BadValue(format!(
            "{key} must be \"rw\", \"ro\" or \"none\", not {value:?}"
        ))),
    }
}

/// Set (or, with `None`, remove) rights of client `name`. `NotFound` for
/// a name that is no client; `msg` is then unchanged.
pub fn client_set(
    msg: &mut pb::Config,
    name: &str,
    settings: &[(Right, Option<Access>)],
) -> Result<(), ManageError> {
    let c = msg
        .clients
        .iter_mut()
        .find(|c| c.name == name)
        .ok_or_else(|| ManageError::NotFound(name.to_string()))?;
    for (right, access) in settings {
        right.set(c, *access);
    }
    Ok(())
}

/// The rights of a client as `client set` reports them: `KEY=VALUE` for
/// every right, `none` for one that is not set.
pub fn rights_summary(c: &pb::ClientEntry) -> String {
    let words: Vec<String> = RIGHTS
        .iter()
        .map(|r| format!("{}={}", r.key(), r.get(c).map_or("none", Access::as_str)))
        .collect();
    words.join(" ")
}

/// The lines `client list` prints: `name: id`, then ` KEY=VALUE` for each
/// right the client has.
pub fn client_list(msg: &pb::Config) -> Vec<String> {
    msg.clients
        .iter()
        .map(|c| {
            let id = if c.client_id.is_empty() {
                "(missing)".to_string()
            } else {
                id_of(&c.client_id).map_or_else(|| "(invalid)".to_string(), |id| id.to_string())
            };
            let mut line = format!("{}: {id}", c.name);
            for right in RIGHTS {
                if let Some(a) = right.get(c) {
                    line.push_str(&format!(" {}={}", right.key(), a.as_str()));
                }
            }
            line
        })
        .collect()
}
