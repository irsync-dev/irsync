//! Edits of the grant arrays in `exports.toml` for `grant add` and
//! `grant remove`: the `[all]` table and the `rw`/`ro` arrays of each
//! export. All functions are pure on a parsed document; loading and
//! saving belong to the caller.

use super::{check_comment, ManageError, PeerSpec};
use crate::identity::parse_endpoint_id;
use iroh::EndpointId;
use toml_edit::{Array, DocumentMut, Item, Table, Value};

type Resolve<'a> = &'a dyn Fn(&str) -> Option<EndpointId>;

/// The table `export` names under `[exports]`, or `[all]` for `None`
/// (created when missing).
fn target<'a>(
    doc: &'a mut DocumentMut,
    export: Option<&str>,
) -> Result<&'a mut Table, ManageError> {
    match export {
        Some(n) => doc
            .get_mut("exports")
            .and_then(Item::as_table_mut)
            .and_then(|t| t.get_mut(n))
            .and_then(Item::as_table_mut)
            .ok_or_else(|| ManageError::NotFound(n.to_string())),
        None => doc
            .entry("all")
            .or_insert_with(toml_edit::table)
            .as_table_mut()
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
fn remove_where(table: &mut Table, key: &str, pred: &dyn Fn(&str) -> bool) -> usize {
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

fn push_entry(table: &mut Table, key: &str, text: &str, comment: Option<&str>) {
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

fn remove_in(table: &mut Table, who: Option<&PeerSpec>, resolve: Resolve) -> usize {
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
/// arrays of `export` (or of `[all]`); `export = None` with a peer walks
/// `[all]` and every export.
pub fn grant_remove(
    doc: &mut DocumentMut,
    export: Option<&str>,
    who: Option<&PeerSpec>,
    resolve: &dyn Fn(&str) -> Option<EndpointId>,
) -> Result<usize, ManageError> {
    let n = match (export, who) {
        (None, Some(w)) => {
            let mut n = match doc.get_mut("all").and_then(Item::as_table_mut) {
                Some(all) => remove_in(all, Some(w), resolve),
                None => 0,
            };
            if let Some(exports) = doc.get_mut("exports").and_then(Item::as_table_mut) {
                for (_, item) in exports.iter_mut() {
                    if let Some(t) = item.as_table_mut() {
                        n += remove_in(t, Some(w), resolve);
                    }
                }
            }
            n
        }
        _ => remove_in(target(doc, export)?, who, resolve),
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
