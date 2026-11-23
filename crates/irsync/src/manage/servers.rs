//! Edits of the servers in the config message for `server add`,
//! `server remove` and `server rename`. Pure on a message; loading and
//! saving belong to the caller.

use super::{id_bytes, id_of, ManageError};
use crate::config::names::valid_alias;
use crate::config::settings::RelayMode;
use crate::config::writer;
use crate::net::addr::HostPort;
use iroh::EndpointId;
use irsync_proto::config as pb;
use std::time::Duration;
use url::Url;

/// The optional keys `server add` writes; `None` leaves a key untouched.
#[derive(Default)]
pub struct ServerFlags {
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

/// The server `name`, created last when missing.
fn server_entry<'a>(msg: &'a mut pb::Config, name: &str) -> &'a mut pb::ServerEntry {
    let i = match msg.servers.iter().position(|h| h.name == name) {
        Some(i) => i,
        None => {
            msg.servers.push(pb::ServerEntry {
                name: name.to_string(),
                ..Default::default()
            });
            msg.servers.len() - 1
        }
    };
    &mut msg.servers[i]
}

/// Create or update server `name`: set `endpoint_id` and each key given in
/// `flags`, keeping everything else. Returns the previous id when the name
/// existed with a different one.
pub fn server_add(
    msg: &mut pb::Config,
    name: &str,
    id: EndpointId,
    flags: &ServerFlags,
) -> Result<Option<EndpointId>, ManageError> {
    if !valid_alias(name) {
        return Err(ManageError::BadAlias(name.to_string()));
    }
    let h = server_entry(msg, name);
    let old = id_of(&h.endpoint_id).filter(|old| *old != id);
    h.endpoint_id = id_bytes(id);
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

/// Add `dest` to the `ssh_aliases` list of server `name`, unless it is
/// there: another ssh destination that was typed for this host. Nothing
/// when there is no such server.
pub fn server_add_ssh_alias(msg: &mut pb::Config, name: &str, dest: &str) {
    let Some(h) = msg.servers.iter_mut().find(|h| h.name == name) else {
        return;
    };
    if !h.ssh_aliases.iter().any(|e| e.value == dest) {
        h.ssh_aliases.push(entry(dest));
        h.ssh_aliases_set = true;
    }
}

/// The names of the servers of `msg`, in file order.
pub fn server_names(msg: &pb::Config) -> Vec<String> {
    msg.servers.iter().map(|h| h.name.clone()).collect()
}

/// The lines `server list` prints: `name: id`, then one indented
/// `key = value` per other key, in the writer's order and spelling.
pub fn server_list(msg: &pb::Config) -> Vec<String> {
    let mut out = Vec::new();
    for h in &msg.servers {
        let id = if h.endpoint_id.is_empty() {
            "(missing)".to_string()
        } else {
            id_of(&h.endpoint_id).map_or_else(|| "(invalid)".to_string(), |id| id.to_string())
        };
        out.push(format!("{}: {id}", h.name));
        // The writer does the spelling: write the host alone, without its
        // comments and its endpoint_id, and keep the key lines.
        let mut bare = h.clone();
        bare.comment = None;
        bare.key_comments.clear();
        bare.tail.clear();
        bare.endpoint_id.clear();
        for e in bare.address.iter_mut().chain(&mut bare.ssh_aliases) {
            e.comment = None;
        }
        let text = writer::write(&pb::Config {
            servers: vec![bare],
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
/// Remove server `name`. Grants name clients only, so none is checked.
pub fn server_remove(msg: &mut pb::Config, name: &str) -> Result<(), ManageError> {
    if !msg.servers.iter().any(|h| h.name == name) {
        return Err(ManageError::NotFound(name.to_string()));
    }
    msg.servers.retain(|h| h.name != name);
    Ok(())
}

/// Rename server `old` to `new`, keeping its place, its keys and its
/// comments. Touches no grant. Refused, with `msg` unchanged, when `old`
/// is no server, `new` is not a valid name or `new` is a server already.
/// Renaming a server to its own name changes nothing.
pub fn server_rename(msg: &mut pb::Config, old: &str, new: &str) -> Result<(), ManageError> {
    if !msg.servers.iter().any(|h| h.name == old) {
        return Err(ManageError::NotFound(old.to_string()));
    }
    if old == new {
        return Ok(());
    }
    if !valid_alias(new) {
        return Err(ManageError::BadAlias(new.to_string()));
    }
    if msg.servers.iter().any(|h| h.name == new) {
        return Err(ManageError::ServerExists(new.to_string()));
    }
    for h in msg.servers.iter_mut().filter(|h| h.name == old) {
        h.name = new.to_string();
    }
    Ok(())
}
