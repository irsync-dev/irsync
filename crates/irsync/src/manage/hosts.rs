//! Edits of the `[hosts]` table in `irsync.toml` for `host add` and
//! `host remove`. Pure on a parsed document; loading and saving belong to the caller.

use super::ManageError;
use crate::config::hosts::valid_alias;
use crate::config::settings::RelayMode;
use crate::identity::parse_endpoint_id;
use crate::net::addr::HostPort;
use iroh::EndpointId;
use toml_edit::{Array, DocumentMut, Item, Table, TableLike};
use url::Url;

/// The optional keys `host add` writes; `None` leaves a key untouched.
#[derive(Default)]
pub struct HostFlags {
    pub relay: Option<Url>,
    pub relay_mode: Option<RelayMode>,
    /// Replaces the whole `address` array.
    pub address: Option<Vec<HostPort>>,
    pub parallel: Option<u32>,
    /// The validated duration text, written verbatim.
    pub reconnect: Option<String>,
    pub dial_retries: Option<u32>,
    pub dht: Option<bool>,
    pub mdns: Option<bool>,
}

/// The `[hosts]` table, created (implicit, so only its children print
/// headers) when missing.
fn hosts_table(doc: &mut DocumentMut) -> &mut dyn TableLike {
    let implicit = || {
        let mut t = Table::new();
        t.set_implicit(true);
        Item::Table(t)
    };
    let item = doc.entry("hosts").or_insert_with(implicit);
    if !item.is_table_like() {
        *item = implicit();
    }
    item.as_table_like_mut().expect("hosts is a table")
}

/// Create or update `[hosts.alias]`: set `endpoint_id` and each key given
/// in `flags`, keeping everything else. Returns the previous id when the
/// alias existed with a different one.
pub fn host_add(
    doc: &mut DocumentMut,
    alias: &str,
    id: EndpointId,
    flags: &HostFlags,
) -> Result<Option<EndpointId>, ManageError> {
    if !valid_alias(alias) {
        return Err(ManageError::BadAlias(alias.to_string()));
    }
    let hosts = hosts_table(doc);
    let old = hosts
        .get(alias)
        .and_then(|t| t.get("endpoint_id"))
        .and_then(Item::as_str)
        .and_then(|s| parse_endpoint_id(s).ok())
        .filter(|old| *old != id);
    if !hosts.get(alias).is_some_and(Item::is_table_like) {
        hosts.insert(alias, toml_edit::table());
    }
    let t = hosts.get_mut(alias).expect("just inserted");
    t["endpoint_id"] = toml_edit::value(id.to_string());
    if let Some(r) = &flags.relay {
        t["relay"] = toml_edit::value(r.as_str());
    }
    if let Some(m) = flags.relay_mode {
        t["relay_mode"] = toml_edit::value(match m {
            RelayMode::Default => "default",
            RelayMode::Disabled => "disabled",
        });
    }
    if let Some(a) = &flags.address {
        let arr: Array = a.iter().map(|h| h.to_string()).collect();
        t["address"] = toml_edit::value(arr);
    }
    if let Some(p) = flags.parallel {
        t["parallel"] = toml_edit::value(i64::from(p));
    }
    if let Some(r) = &flags.reconnect {
        t["reconnect"] = toml_edit::value(r.as_str());
    }
    if let Some(n) = flags.dial_retries {
        t["dial_retries"] = toml_edit::value(i64::from(n));
    }
    if let Some(d) = flags.dht {
        t["dht"] = toml_edit::value(d);
    }
    if let Some(m) = flags.mdns {
        t["mdns"] = toml_edit::value(m);
    }
    Ok(old)
}

/// Remove `[hosts.alias]`; only a host entry (a table) is removed.
pub fn host_remove(doc: &mut DocumentMut, alias: &str) -> Result<(), ManageError> {
    let hosts = doc.get_mut("hosts").and_then(Item::as_table_like_mut);
    let Some(hosts) = hosts else {
        return Err(ManageError::NotFound(alias.to_string()));
    };
    if !hosts.get(alias).is_some_and(Item::is_table_like) {
        return Err(ManageError::NotFound(alias.to_string()));
    }
    hosts.remove(alias);
    Ok(())
}

/// The lines `host list` prints: `alias: id`, then one indented
/// `key = value` per other key, in file order.
pub fn host_list(doc: &DocumentMut) -> Vec<String> {
    let mut out = Vec::new();
    let Some(hosts) = doc.get("hosts").and_then(Item::as_table_like) else {
        return out;
    };
    for (alias, item) in hosts.iter() {
        let Some(t) = item.as_table_like() else {
            continue;
        };
        let id = t.get("endpoint_id").and_then(Item::as_str).unwrap_or("?");
        out.push(format!("{alias}: {id}"));
        for (k, v) in t.iter().filter(|(k, _)| *k != "endpoint_id") {
            if let Some(v) = v.as_value() {
                out.push(format!("    {k} = {}", v.to_string().trim()));
            }
        }
    }
    out
}
