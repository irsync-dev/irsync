//! Edits of the grant arrays in `irsync.toml` for `grant add` and
//! `grant remove`: the `[all]` table and the `rw`/`ro` arrays of each
//! export. The editing functions are pure on a parsed document; loading
//! and saving belong to the caller. The `list_*` reporters behind
//! `grant list` read the parsed files.

use super::show::grant_lines;
use super::{check_comment, ManageError, PeerSpec};
use crate::authz::{Access, Snapshot};
use crate::config::exports::Exports;
use crate::config::hosts::Hosts;
use crate::identity::parse_endpoint_id;
use iroh::EndpointId;
use std::fmt::Write;
use toml_edit::{Array, DocumentMut, Item, TableLike, Value};

type Resolve<'a> = &'a dyn Fn(&str) -> Option<EndpointId>;

/// The table `export` names under `[exports]`, or `[all]` for `None`
/// (created when missing). Inline tables count: anything the parser reads
/// as a table, the editors edit.
fn target<'a>(
    doc: &'a mut DocumentMut,
    export: Option<&str>,
) -> Result<&'a mut dyn TableLike, ManageError> {
    match export {
        Some(n) => doc
            .get_mut("exports")
            .and_then(Item::as_table_like_mut)
            .and_then(|t| t.get_mut(n))
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| ManageError::NotFound(n.to_string())),
        None => doc
            .entry("all")
            .or_insert_with(toml_edit::table)
            .as_table_like_mut()
            .ok_or_else(|| ManageError::NotFound("all".to_string())),
    }
}

/// The id `who` stands for, when it has one.
fn who_id(who: &PeerSpec, resolve: Resolve) -> Option<EndpointId> {
    match who {
        PeerSpec::Id { id, .. } => Some(*id),
        PeerSpec::Alias(a) => resolve(a),
    }
}

/// An array entry names `who` by the same text or by the same id.
fn entry_matches(text: &str, who: &PeerSpec, id: Option<EndpointId>, resolve: Resolve) -> bool {
    if text == who.entry() {
        return true;
    }
    match id {
        Some(id) => resolve(text).or_else(|| parse_endpoint_id(text).ok()) == Some(id),
        None => false,
    }
}

/// Remove the entries of `table[key]` for which `pred` holds; the count.
fn remove_where(table: &mut dyn TableLike, key: &str, pred: &dyn Fn(&str) -> bool) -> usize {
    let Some(arr) = table.get_mut(key).and_then(Item::as_array_mut) else {
        return 0;
    };
    let mut n = 0;
    for i in (0..arr.len()).rev() {
        if arr.get(i).and_then(Value::as_str).is_some_and(pred) {
            arr.remove(i);
            n += 1;
        }
    }
    // Do not leave `[ "b"]` behind when the first entry went.
    if let Some(first) = arr.get_mut(0) {
        if first.decor().prefix().and_then(|p| p.as_str()) == Some(" ") {
            first.decor_mut().set_prefix("");
        }
    }
    n
}

fn push_entry(table: &mut dyn TableLike, key: &str, text: &str, comment: Option<&str>) {
    let item = table
        .entry(key)
        .or_insert_with(|| toml_edit::value(Array::new()));
    let Some(arr) = item.as_array_mut() else {
        return;
    };
    // A comment kept after the last entry's comma moves in front of the
    // new entry, so appending never swallows a comma or the bracket.
    let pending = arr
        .trailing()
        .as_str()
        .filter(|t| t.contains('#'))
        .map(str::to_string);
    let mut v = Value::from(text);
    let prefix = match (&pending, arr.is_empty()) {
        (Some(p), _) => format!("{p}    "),
        (None, true) => String::new(),
        (None, false) => " ".to_string(),
    };
    v.decor_mut().set_prefix(prefix);
    arr.push_formatted(v);
    if pending.is_some() {
        arr.set_trailing("\n");
    }
    if let Some(c) = comment {
        arr.set_trailing(format!(" # {c}\n"));
        arr.set_trailing_comma(true);
    }
}

/// Removes entry `i` of `arr` together with the comment after it, which
/// toml_edit keeps in front of the next entry (or, for the last entry,
/// before the closing bracket, or behind the entry itself when no comma
/// follows it). The comment after the entry before `i` sits in front of
/// `i` and stays, and so do comment lines below a last entry.
fn remove_entry(arr: &mut Array, i: usize) {
    let text = |raw: Option<&toml_edit::RawString>| {
        raw.and_then(|r| r.as_str()).unwrap_or_default().to_string()
    };
    // What follows the first line of `s`; that line itself when it holds
    // no comment and `keep_plain` is set.
    let below = |s: &str, keep_plain: bool| -> String {
        match s.split_once('\n') {
            Some((line, rest)) if line.contains('#') || !keep_plain => rest.to_string(),
            Some(_) => s.to_string(),
            None if s.contains('#') || !keep_plain => String::new(),
            None => s.to_string(),
        }
    };
    let before = text(arr.get(i).and_then(|v| v.decor().prefix()));
    // Behind the entry: the rest of its line is its own, lines below are not.
    let behind = below(&text(arr.get(i).and_then(|v| v.decor().suffix())), false);
    let last = i + 1 == arr.len();
    let after = if last {
        text(Some(arr.trailing()))
    } else {
        text(arr.get(i + 1).and_then(|v| v.decor().prefix()))
    };
    // After the comma, the rest of the line is the removed entry's comment.
    let after = below(&after, true);
    let joined = format!("{}{behind}{after}", before.trim_end_matches([' ', '\t']));
    arr.remove(i);
    if last {
        // On one line nothing but blanks can be left before the bracket.
        let joined = if joined.trim().is_empty() && !joined.contains('\n') {
            String::new()
        } else {
            joined
        };
        arr.set_trailing(joined);
    } else if let Some(next) = arr.get_mut(i) {
        // Do not leave `[ "b"]` behind when the first entry went.
        let joined = if i == 0 && joined == " " {
            String::new()
        } else {
            joined
        };
        next.decor_mut().set_prefix(joined);
    }
}

/// Writes every grant entry in its canonical form: an EndpointId that
/// `hosts` has an alias for becomes that alias (the first in
/// lexicographic order when there are several). An entry that is already
/// an alias, an id without an alias and text that names nobody are left
/// alone. Then, per table, an entry named twice in one list keeps its
/// first place, and an entry in both `rw` and `ro` leaves `ro`: `rw` wins
/// when the file is read, so nobody's access changes.
///
/// Covers `[all]`, `[immediate]` and every table under `[exports]`.
/// Comments and layout are kept. True when the document changed; a second
/// run changes nothing.
pub fn canonicalize(doc: &mut DocumentMut, hosts: &Hosts) -> bool {
    let mut changed = false;
    let mut table = |table: &mut dyn TableLike| {
        // What this table already names, `rw` before `ro`.
        let mut seen: Vec<String> = Vec::new();
        for key in ["rw", "ro"] {
            let Some(arr) = table.get_mut(key).and_then(Item::as_array_mut) else {
                continue;
            };
            let mut i = 0;
            while i < arr.len() {
                let Some(text) = arr.get(i).and_then(Value::as_str) else {
                    i += 1;
                    continue;
                };
                let alias = parse_endpoint_id(text)
                    .ok()
                    .and_then(|id| hosts.alias_of(&id));
                let name = alias.unwrap_or(text).to_string();
                if seen.contains(&name) {
                    remove_entry(arr, i);
                    changed = true;
                    continue;
                }
                if alias.is_some() {
                    let v = arr.get_mut(i).expect("in range");
                    let decor = v.decor().clone();
                    *v = Value::from(name.as_str());
                    *v.decor_mut() = decor;
                    changed = true;
                }
                seen.push(name);
                i += 1;
            }
        }
    };
    for name in ["all", "immediate"] {
        if let Some(t) = doc.get_mut(name).and_then(Item::as_table_like_mut) {
            table(t);
        }
    }
    if let Some(exports) = doc.get_mut("exports").and_then(Item::as_table_like_mut) {
        for (_, item) in exports.iter_mut() {
            if let Some(t) = item.as_table_like_mut() {
                table(t);
            }
        }
    }
    changed
}

/// The tables with a grant entry whose text is `name`: `all`,
/// `immediate`, then `exports.NAME` in file order, each once.
pub fn lists_naming(doc: &DocumentMut, name: &str) -> Vec<String> {
    let names = |table: &dyn TableLike| {
        ["rw", "ro"].iter().any(|key| {
            table
                .get(key)
                .and_then(Item::as_array)
                .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(name)))
        })
    };
    let mut out = Vec::new();
    for top in ["all", "immediate"] {
        if doc
            .get(top)
            .and_then(Item::as_table_like)
            .is_some_and(names)
        {
            out.push(top.to_string());
        }
    }
    if let Some(exports) = doc.get("exports").and_then(Item::as_table_like) {
        for (export, item) in exports.iter() {
            if item.as_table_like().is_some_and(names) {
                out.push(format!("exports.{export}"));
            }
        }
    }
    out
}

/// Removes every grant entry whose text is `name`, from `[all]`,
/// `[immediate]` and every export; the count.
pub fn remove_named(doc: &mut DocumentMut, name: &str) -> usize {
    let mut n = 0;
    let mut table = |table: &mut dyn TableLike| {
        for key in ["rw", "ro"] {
            let Some(arr) = table.get_mut(key).and_then(Item::as_array_mut) else {
                continue;
            };
            for i in (0..arr.len()).rev() {
                if arr.get(i).and_then(Value::as_str) == Some(name) {
                    remove_entry(arr, i);
                    n += 1;
                }
            }
        }
    };
    for top in ["all", "immediate"] {
        if let Some(t) = doc.get_mut(top).and_then(Item::as_table_like_mut) {
            table(t);
        }
    }
    if let Some(exports) = doc.get_mut("exports").and_then(Item::as_table_like_mut) {
        for (_, item) in exports.iter_mut() {
            if let Some(t) = item.as_table_like_mut() {
                table(t);
            }
        }
    }
    n
}

/// Grant `who` access to `export` (`[all]` for `None`): read-only or
/// read-write. Any earlier grant of the same peer there, under either
/// spelling, is replaced.
pub fn grant_add(
    doc: &mut DocumentMut,
    export: Option<&str>,
    who: &PeerSpec,
    ro: bool,
    resolve: &dyn Fn(&str) -> Option<EndpointId>,
) -> Result<(), ManageError> {
    if let PeerSpec::Id {
        comment: Some(c), ..
    } = who
    {
        check_comment(c)?;
    }
    let table = target(doc, export)?;
    let id = who_id(who, resolve);
    let pred = |t: &str| entry_matches(t, who, id, resolve);
    remove_where(table, "rw", &pred);
    remove_where(table, "ro", &pred);
    let comment = match who {
        PeerSpec::Id { comment, .. } => comment.as_deref(),
        PeerSpec::Alias(_) => None,
    };
    push_entry(table, if ro { "ro" } else { "rw" }, &who.entry(), comment);
    Ok(())
}

/// Grant `who` immediate access (`[immediate].rw`). False when the peer
/// was already granted, under either spelling.
pub fn grant_immediate(
    doc: &mut DocumentMut,
    who: &PeerSpec,
    resolve: &dyn Fn(&str) -> Option<EndpointId>,
) -> Result<bool, ManageError> {
    if let PeerSpec::Id {
        comment: Some(c), ..
    } = who
    {
        check_comment(c)?;
    }
    let table = doc
        .entry("immediate")
        .or_insert_with(toml_edit::table)
        .as_table_like_mut()
        .ok_or_else(|| ManageError::NotATable("immediate".to_string()))?;
    let id = who_id(who, resolve);
    let present = table.get("rw").and_then(Item::as_array).is_some_and(|a| {
        a.iter()
            .filter_map(Value::as_str)
            .any(|t| entry_matches(t, who, id, resolve))
    });
    if present {
        return Ok(false);
    }
    let comment = match who {
        PeerSpec::Id { comment, .. } => comment.as_deref(),
        PeerSpec::Alias(_) => None,
    };
    push_entry(table, "rw", &who.entry(), comment);
    Ok(true)
}

/// Remove `who` from `[immediate].rw` (`None`: everyone); the count.
pub fn grant_remove_immediate(
    doc: &mut DocumentMut,
    who: Option<&PeerSpec>,
    resolve: &dyn Fn(&str) -> Option<EndpointId>,
) -> Result<usize, ManageError> {
    let n = match doc.get_mut("immediate").and_then(Item::as_table_like_mut) {
        Some(t) => match who {
            None => remove_where(t, "rw", &|_| true),
            Some(w) => {
                let id = who_id(w, resolve);
                remove_where(t, "rw", &|s| entry_matches(s, w, id, resolve))
            }
        },
        None => 0,
    };
    if n == 0 {
        let what = who.map_or_else(|| "immediate".to_string(), PeerSpec::entry);
        return Err(ManageError::NothingToRemove(what));
    }
    Ok(n)
}

fn remove_in(table: &mut dyn TableLike, who: Option<&PeerSpec>, resolve: Resolve) -> usize {
    match who {
        None => remove_where(table, "rw", &|_| true) + remove_where(table, "ro", &|_| true),
        Some(w) => {
            let id = who_id(w, resolve);
            let pred = |t: &str| entry_matches(t, w, id, resolve);
            remove_where(table, "rw", &pred) + remove_where(table, "ro", &pred)
        }
    }
}

/// Remove grants; the number of entries removed. `who = None` empties both
/// arrays of `export` (or of `[all]`); `export = None` walks `[all]` and
/// every export, so `(None, None)` empties every array in the file.
pub fn grant_remove(
    doc: &mut DocumentMut,
    export: Option<&str>,
    who: Option<&PeerSpec>,
    resolve: &dyn Fn(&str) -> Option<EndpointId>,
) -> Result<usize, ManageError> {
    let n = match export {
        None => {
            let mut n = match doc.get_mut("all").and_then(Item::as_table_like_mut) {
                Some(all) => remove_in(all, who, resolve),
                None => 0,
            };
            if let Some(exports) = doc.get_mut("exports").and_then(Item::as_table_like_mut) {
                for (_, item) in exports.iter_mut() {
                    if let Some(t) = item.as_table_like_mut() {
                        n += remove_in(t, who, resolve);
                    }
                }
            }
            n
        }
        Some(_) => remove_in(target(doc, export)?, who, resolve),
    };
    if n == 0 {
        let what = match (who, export) {
            (Some(w), _) => w.entry(),
            (None, Some(e)) => e.to_string(),
            (None, None) => "all".to_string(),
        };
        return Err(ManageError::NothingToRemove(what));
    }
    Ok(n)
}

/// `NAME:` and the grant lines of one table; the header always.
fn block(name: &str, rw: &[String], ro: &[String], hosts: &Hosts) -> String {
    let mut out = format!("{name}:\n");
    grant_lines(&mut out, rw, ro, hosts);
    out
}

/// Every grant: the `all:` block, the `immediate:` block, then one block
/// per export in name order.
pub fn list_all(exports: &Exports, hosts: &Hosts) -> String {
    let mut out = block("all", &exports.rw, &exports.ro, hosts);
    out.push_str(&block("immediate", &exports.immediate_rw, &[], hosts));
    for (name, e) in &exports.exports {
        out.push_str(&block(name, &e.rw, &e.ro, hosts));
    }
    out
}

/// The block of one export, of `all` or of `immediate`; `None` when no
/// such export.
pub fn list_export(exports: &Exports, hosts: &Hosts, name: &str) -> Option<String> {
    match name {
        "all" => return Some(block("all", &exports.rw, &exports.ro, hosts)),
        "immediate" => return Some(block("immediate", &exports.immediate_rw, &[], hosts)),
        _ => {}
    }
    let e = exports.exports.get(name)?;
    Some(block(name, &e.rw, &e.ro, hosts))
}

/// `{label}:`, `    immediate: rw` when `id` is in `[immediate]`, then
/// one `    NAME: rw|ro` line per export `id` may reach: ` (all)` when the
/// access comes from `[all]` rather than the export's own entry,
/// ` (read_only export)` when a read-write grant is capped by the export's
/// `read_only` flag.
pub fn list_peer(exports: &Exports, hosts: &Hosts, label: &str, id: &EndpointId) -> String {
    let grants = Snapshot::from_files(hosts, exports, true).grants(id);
    let mut out = format!("{label}:\n");
    if grants.immediate {
        out.push_str("    immediate: rw\n");
    }
    for (name, e) in &exports.exports {
        let Some(access) = grants.access_to(name, e) else {
            continue;
        };
        let inherited = !grants.exports.contains_key(name);
        let capped = e.read_only && access == Access::ReadWrite;
        let mode = if capped || access == Access::ReadOnly {
            "ro"
        } else {
            "rw"
        };
        let _ = write!(out, "    {name}: {mode}");
        if inherited {
            out.push_str(" (all)");
        }
        if capped {
            out.push_str(" (read_only export)");
        }
        out.push('\n');
    }
    out
}
