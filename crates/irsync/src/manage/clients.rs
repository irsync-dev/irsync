//! Edits of the clients in the config message for `client add`,
//! `client remove` and `client rename`. Pure on a message; loading and
//! saving belong to the caller. A client is the id that grants name.

use super::grants::{lists_naming, remove_named, rename_named};
use super::{canonicalize_doc, canonicalize_without, id_bytes, id_of, ManageError};
use crate::authz::Access;
use crate::config::clients::{Right, RIGHTS};
use crate::config::file::Config;
use crate::config::names::valid_alias;
use crate::config::paths::Paths;
use crate::identity::parse_endpoint_id;
use anyhow::Context;
use iroh::EndpointId;
use irsync_proto::config as pb;
use std::collections::BTreeMap;
use std::path::Path;

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

/// `client remove` on a message loaded from `path`: as [`client_remove`],
/// with the grants canonicalized first (see [`canonicalize_without`]), so
/// an EndpointId written for this client is found as a grant naming it.
pub fn remove_client(
    msg: &mut pb::Config,
    path: &Path,
    name: &str,
    force: bool,
) -> Result<usize, ManageError> {
    if !msg.clients.iter().any(|c| c.name == name) {
        return Err(ManageError::NotFound(name.to_string()));
    }
    canonicalize_without(msg, path, name)?;
    client_remove(msg, name, force)
}

/// `client rename` on a message loaded from `path`: as
/// [`client_rename`], with the grants canonicalized first (see
/// [`canonicalize_doc`]), so an EndpointId written for this client is
/// renamed too.
pub fn rename_client(
    msg: &mut pb::Config,
    path: &Path,
    old: &str,
    new: &str,
) -> Result<usize, ManageError> {
    if !msg.clients.iter().any(|c| c.name == old) {
        return Err(ManageError::NotFound(old.to_string()));
    }
    canonicalize_doc(msg, path)?;
    client_rename(msg, old, new)
}

/// What `client add` reports: the id, and the one it replaced.
pub fn added_line(name: &str, old: Option<EndpointId>, id: EndpointId) -> String {
    match old {
        Some(old) => format!("{name}: {old} -> {id}"),
        None => format!("{name}: {id}"),
    }
}

/// What `client remove` reports, with the `grants` it removed.
pub fn removed_line(name: &str, grants: usize, config: &Path) -> String {
    let also = match grants {
        0 => String::new(),
        1 => " and 1 grant".to_string(),
        n => format!(" and {n} grants"),
    };
    format!("removed client {name}{also} from {}", config.display())
}

/// What `client rename` reports, with the `grants` it updated.
pub fn renamed_line(old: &str, new: &str, grants: usize, config: &Path) -> String {
    let also = match grants {
        0 => String::new(),
        1 => "; updated 1 grant".to_string(),
        n => format!("; updated {n} grants"),
    };
    format!(
        "renamed client {old} to {new}{also} in {}",
        config.display()
    )
}

/// What `client set` reports: every right of client `name`; none when
/// there is no such client.
pub fn set_line(msg: &pb::Config, name: &str) -> Option<String> {
    let c = msg.clients.iter().find(|c| c.name == name)?;
    Some(format!("{name}: {}", rights_summary(c)))
}

/// The id of each client of `msg`, the id grants naming it are for; none
/// when `msg` does not load.
pub fn client_ids(msg: &pb::Config, path: &Path) -> BTreeMap<String, EndpointId> {
    let Ok(config) = Config::from_message(msg, path) else {
        return BTreeMap::new();
    };
    config
        .clients
        .entries
        .into_iter()
        .map(|(name, c)| (name, c.client_id))
        .collect()
}

/// A grant by name follows the name. After an edit that changed which id
/// a client's grants are for (`before`: [`client_ids`] from before it),
/// one note per such client that grants name, for the command `cmd`.
pub fn moved_grant_notes(
    cmd: &str,
    before: &BTreeMap<String, EndpointId>,
    msg: &pb::Config,
    path: &Path,
) -> Vec<String> {
    let mut notes = Vec::new();
    for (alias, new) in client_ids(msg, path) {
        let Some(old) = before.get(&alias).filter(|old| **old != new) else {
            continue;
        };
        let lists = lists_naming(msg, &alias);
        if !lists.is_empty() {
            notes.push(format!(
                "irsync {cmd}: note: the grants that name {alias} (in {}) now apply to {new} (was {old})",
                lists.join(", ")
            ));
        }
    }
    notes
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

// The commands, from the config file's lock to the lines they say.

/// `client add NAME ID`: the line to print, and notes on grants that now
/// apply to another id (see [`moved_grant_notes`]).
pub fn add(paths: &Paths, name: &str, id: &str) -> anyhow::Result<(String, Vec<String>)> {
    let id = parse_endpoint_id(id).with_context(|| format!("ID {id:?}"))?;
    let ((before, old), msg) = super::edit_as_is(&paths.config, |msg| {
        let before = client_ids(msg, &paths.config);
        Ok((before, client_add(msg, name, id)?))
    })?;
    let notes = moved_grant_notes("client add", &before, &msg, &paths.config);
    Ok((added_line(name, old, id), notes))
}

/// `client remove NAME [--force]`; the line to say.
pub fn remove(paths: &Paths, name: &str, force: bool) -> anyhow::Result<String> {
    super::load_first(paths)?;
    let (grants, _) = super::edit(&paths.config, |msg| {
        remove_client(msg, &paths.config, name, force)
    })?;
    Ok(removed_line(name, grants, &paths.config))
}

/// `client rename OLD NEW`; the line to say.
pub fn rename(paths: &Paths, old: &str, new: &str) -> anyhow::Result<String> {
    super::load_first(paths)?;
    let (grants, _) = super::edit(&paths.config, |msg| {
        rename_client(msg, &paths.config, old, new)
    })?;
    Ok(renamed_line(old, new, grants, &paths.config))
}

/// `client set NAME RIGHT=ACCESS...`; the line to say, if any.
pub fn set(paths: &Paths, name: &str, settings: &[String]) -> anyhow::Result<Option<String>> {
    // Every pair is read before anything changes.
    let settings: Vec<_> = settings
        .iter()
        .map(|s| parse_setting(s))
        .collect::<Result<_, _>>()?;
    super::load_first(paths)?;
    let ((), msg) = super::edit(&paths.config, |msg| client_set(msg, name, &settings))?;
    Ok(set_line(&msg, name))
}
