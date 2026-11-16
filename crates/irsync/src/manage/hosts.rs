//! Edits of the `[hosts]` table in `irsync.toml` for `host add`,
//! `host remove` and `host rename`. Pure on a parsed document; loading and saving belong to the caller.

use super::grants::{lists_naming, remove_named, rename_named};
use super::ManageError;
use crate::config::hosts::valid_alias;
use crate::config::settings::RelayMode;
use crate::identity::parse_endpoint_id;
use crate::net::addr::HostPort;
use iroh::EndpointId;
use toml_edit::{Array, DocumentMut, Item, Key, Table, TableLike};
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
    pub ssh: Option<String>,
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
    if let Some(s) = &flags.ssh {
        t["ssh"] = toml_edit::value(s.as_str());
    }
    Ok(old)
}

/// Remove `[hosts.alias]`; only a host entry (a table) is removed. A host
/// that grants still name is not removed: without it they would match
/// nobody. With `force` those grants are removed first. The number of
/// grants removed.
///
/// Grants are matched by the alias: run [`super::canonicalize_doc`] first
/// so that an EndpointId written for this host counts too.
pub fn host_remove(doc: &mut DocumentMut, alias: &str, force: bool) -> Result<usize, ManageError> {
    let is_host = doc
        .get("hosts")
        .and_then(Item::as_table_like)
        .and_then(|hosts| hosts.get(alias))
        .is_some_and(Item::is_table_like);
    if !is_host {
        return Err(ManageError::NotFound(alias.to_string()));
    }
    let lists = lists_naming(doc, alias);
    if !lists.is_empty() && !force {
        return Err(ManageError::HostInUse {
            alias: alias.to_string(),
            lists,
        });
    }
    let removed = remove_named(doc, alias);
    if let Some(hosts) = doc.get_mut("hosts").and_then(Item::as_table_like_mut) {
        hosts.remove(alias);
    }
    Ok(removed)
}

/// Rename `[hosts.old]` to `[hosts.new]`, keeping its place in the file,
/// its keys and its comments, and write every grant entry naming `old` as
/// `new`. The number of grant entries renamed. Refused, with `doc`
/// unchanged, when `old` is no host, `new` is not a valid alias or `new`
/// is a host already. Renaming a host to its own name changes nothing.
///
/// Grants are matched by the alias: run [`super::canonicalize_doc`] first
/// so that an EndpointId written for this host counts too. A list that
/// named both `old` and `new` names `new` twice until the next canonical
/// rewrite, which every save does.
pub fn host_rename(doc: &mut DocumentMut, old: &str, new: &str) -> Result<usize, ManageError> {
    let hosts = doc.get("hosts").and_then(Item::as_table_like);
    let is_host = |alias: &str| {
        hosts
            .and_then(|h| h.get(alias))
            .is_some_and(Item::is_table_like)
    };
    if !is_host(old) {
        return Err(ManageError::NotFound(old.to_string()));
    }
    if old == new {
        return Ok(0);
    }
    if !valid_alias(new) {
        return Err(ManageError::BadAlias(new.to_string()));
    }
    // Any key, not only a host table: the new entry would replace it.
    if hosts.is_some_and(|h| h.contains_key(new)) {
        return Err(ManageError::HostExists(new.to_string()));
    }
    let renamed = |key: Key| {
        Key::new(new)
            .with_leaf_decor(key.leaf_decor().clone())
            .with_dotted_decor(key.dotted_decor().clone())
    };
    // Every entry is taken out and put back in order, so the renamed one
    // keeps its place among the others.
    match doc.get_mut("hosts").expect("checked above") {
        Item::Table(t) => {
            let keys: Vec<String> = t.iter().map(|(k, _)| k.to_string()).collect();
            for k in keys {
                let (key, item) = t.remove_entry(&k).expect("listed");
                let key = if k == old { renamed(key) } else { key };
                t.insert_formatted(&key, item);
            }
        }
        Item::Value(toml_edit::Value::InlineTable(t)) => {
            let keys: Vec<String> = t.iter().map(|(k, _)| k.to_string()).collect();
            for k in keys {
                let (key, value) = t.remove_entry(&k).expect("listed");
                let key = if k == old { renamed(key) } else { key };
                t.insert_formatted(&key, value);
            }
        }
        _ => unreachable!("hosts is table-like"),
    }
    Ok(rename_named(doc, old, new))
}

/// The aliases of `doc`: the tables under `[hosts]`, in file order.
pub fn host_aliases(doc: &DocumentMut) -> Vec<String> {
    doc.get("hosts")
        .and_then(Item::as_table_like)
        .into_iter()
        .flat_map(|hosts| hosts.iter())
        .filter(|(_, item)| item.is_table_like())
        .map(|(alias, _)| alias.to_string())
        .collect()
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
