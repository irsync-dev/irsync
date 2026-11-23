//! Edits of the hosts in the config message for `host add`, `host remove`
//! and `host rename`. Pure on a message; loading and saving belong to the
//! caller.

use super::grants::{lists_naming, remove_named, rename_named};
use super::{id_bytes, id_of, ManageError};
use crate::config::hosts::valid_alias;
use crate::config::settings::RelayMode;
use crate::config::writer;
use crate::net::addr::HostPort;
use iroh::EndpointId;
use irsync_proto::config as pb;
use std::time::Duration;
use url::Url;

/// The optional keys `host add` writes; `None` leaves a key untouched.
#[derive(Default)]
pub struct HostFlags {
    pub relay: Option<Url>,
    pub relay_mode: Option<RelayMode>,
    /// Replaces the whole `address` array.
    pub address: Option<Vec<HostPort>>,
    pub parallel: Option<u32>,
    pub reconnect: Option<Duration>,
    pub dial_retries: Option<u32>,
    pub dht: Option<bool>,
    pub mdns: Option<bool>,
    pub ssh: Option<String>,
}

fn entry(value: &str) -> pb::Entry {
    pb::Entry {
        value: value.to_string(),
        comment: None,
    }
}

/// The host `alias`, created last when missing.
fn host_entry<'a>(msg: &'a mut pb::Config, alias: &str) -> &'a mut pb::Host {
    let i = match msg.hosts.iter().position(|h| h.name == alias) {
        Some(i) => i,
        None => {
            msg.hosts.push(pb::Host {
                name: alias.to_string(),
                ..Default::default()
            });
            msg.hosts.len() - 1
        }
    };
    &mut msg.hosts[i]
}

/// Create or update host `alias`: set `endpoint_id` and each key given in
/// `flags`, keeping everything else. Returns the previous id when the alias
/// existed with a different one.
pub fn host_add(
    msg: &mut pb::Config,
    alias: &str,
    id: EndpointId,
    flags: &HostFlags,
) -> Result<Option<EndpointId>, ManageError> {
    if !valid_alias(alias) {
        return Err(ManageError::BadAlias(alias.to_string()));
    }
    let h = host_entry(msg, alias);
    let old = h
        .endpoint_id
        .as_deref()
        .and_then(id_of)
        .filter(|old| *old != id);
    h.endpoint_id = Some(id_bytes(id));
    if let Some(r) = &flags.relay {
        h.relay = Some(r.as_str().to_string());
    }
    if let Some(m) = flags.relay_mode {
        h.relay_mode = Some(match m {
            RelayMode::Default => pb::RelayMode::Default,
            RelayMode::Disabled => pb::RelayMode::Disabled,
        } as i32);
    }
    if let Some(a) = &flags.address {
        h.address = a.iter().map(|a| entry(&a.to_string())).collect();
        h.address_set = true;
    }
    if let Some(p) = flags.parallel {
        h.parallel = Some(p);
    }
    if let Some(r) = flags.reconnect {
        h.reconnect = Some(crate::config::duration::to_proto(r));
    }
    if let Some(n) = flags.dial_retries {
        h.dial_retries = Some(n);
    }
    if let Some(d) = flags.dht {
        h.dht = Some(d);
    }
    if let Some(m) = flags.mdns {
        h.mdns = Some(m);
    }
    if let Some(s) = &flags.ssh {
        h.ssh = Some(s.clone());
    }
    Ok(old)
}

/// Add `dest` to the `ssh_aliases` list of host `alias`, unless it is
/// there: another ssh destination that was typed for this host. Nothing
/// when there is no such host.
pub fn host_add_ssh_alias(msg: &mut pb::Config, alias: &str, dest: &str) {
    let Some(h) = msg.hosts.iter_mut().find(|h| h.name == alias) else {
        return;
    };
    if !h.ssh_aliases.iter().any(|e| e.value == dest) {
        h.ssh_aliases.push(entry(dest));
        h.ssh_aliases_set = true;
    }
}

/// Create host `alias` when it is missing and set its `client_id`: the id
/// `alias` has as a client, the one grants naming it are for. Its other
/// keys, the daemon's `endpoint_id` among them, stay as they are.
pub fn host_set_client(
    msg: &mut pb::Config,
    alias: &str,
    id: EndpointId,
) -> Result<(), ManageError> {
    if !valid_alias(alias) {
        return Err(ManageError::BadAlias(alias.to_string()));
    }
    host_entry(msg, alias).client_id = Some(id_bytes(id));
    Ok(())
}

/// Remove host `alias`. A host that grants still name is not removed:
/// without it they would match nobody. With `force` those grants are
/// removed first. The number of grants removed.
///
/// Grants are matched by the alias: canonicalize the message first so that
/// an EndpointId written for this host counts too.
pub fn host_remove(msg: &mut pb::Config, alias: &str, force: bool) -> Result<usize, ManageError> {
    if !msg.hosts.iter().any(|h| h.name == alias) {
        return Err(ManageError::NotFound(alias.to_string()));
    }
    let lists = lists_naming(msg, alias);
    if !lists.is_empty() && !force {
        return Err(ManageError::HostInUse {
            alias: alias.to_string(),
            lists,
        });
    }
    let removed = remove_named(msg, alias);
    msg.hosts.retain(|h| h.name != alias);
    Ok(removed)
}

/// Rename host `old` to `new`, keeping its place, its keys and its
/// comments, and write every grant entry naming `old` as `new`. The number
/// of grant entries renamed. Refused, with `msg` unchanged, when `old` is no
/// host, `new` is not a valid alias or `new` is a host already. Renaming a
/// host to its own name changes nothing.
///
/// Grants are matched by the alias: canonicalize the message first so that
/// an EndpointId written for this host counts too. A list that named both
/// `old` and `new` names `new` twice until the next canonical rewrite, which
/// every save does.
pub fn host_rename(msg: &mut pb::Config, old: &str, new: &str) -> Result<usize, ManageError> {
    if !msg.hosts.iter().any(|h| h.name == old) {
        return Err(ManageError::NotFound(old.to_string()));
    }
    if old == new {
        return Ok(0);
    }
    if !valid_alias(new) {
        return Err(ManageError::BadAlias(new.to_string()));
    }
    if msg.hosts.iter().any(|h| h.name == new) {
        return Err(ManageError::HostExists(new.to_string()));
    }
    for h in msg.hosts.iter_mut().filter(|h| h.name == old) {
        h.name = new.to_string();
    }
    Ok(rename_named(msg, old, new))
}

/// The aliases of `msg`, in file order.
pub fn host_aliases(msg: &pb::Config) -> Vec<String> {
    msg.hosts.iter().map(|h| h.name.clone()).collect()
}

/// The lines `host list` prints: `alias: id`, then one indented
/// `key = value` per other key, in the writer's order and spelling.
pub fn host_list(msg: &pb::Config) -> Vec<String> {
    let mut out = Vec::new();
    for h in &msg.hosts {
        // A host known only as a client has no daemon id to dial.
        let id = match h.endpoint_id.as_deref() {
            Some(b) => id_of(b).map_or_else(|| "(invalid)".to_string(), |id| id.to_string()),
            None => "(client only)".to_string(),
        };
        out.push(format!("{}: {id}", h.name));
        // The writer does the spelling: write the host alone, without its
        // comments and its endpoint_id, and keep the key lines.
        let mut bare = h.clone();
        bare.comment = None;
        bare.key_comments.clear();
        bare.tail.clear();
        bare.endpoint_id = None;
        for e in bare.address.iter_mut().chain(&mut bare.ssh_aliases) {
            e.comment = None;
        }
        let text = writer::write(&pb::Config {
            hosts: vec![bare],
            ..Default::default()
        });
        out.extend(
            text.lines()
                .filter(|l| !l.is_empty() && !l.starts_with('['))
                .map(|l| format!("    {l}")),
        );
    }
    out
}
