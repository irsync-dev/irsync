//! Edits of `exports.toml` for `add`, `remove`, `allow` and `revoke`. All
//! functions are pure on a parsed document; loading and saving belong to
//! the caller.

use super::{ManageError, PeerSpec};
use crate::config::exports::RESERVED;
use crate::identity::parse_endpoint_id;
use iroh::EndpointId;
use std::path::{Component, Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item, Table, Value};

type Resolve<'a> = &'a dyn Fn(&str) -> Option<EndpointId>;

/// Add `[name]` with `path` and the flags that are true.
pub fn add_export(
    doc: &mut DocumentMut,
    name: &str,
    path: &Path,
    read_only: bool,
    warm_index: bool,
) -> Result<(), ManageError> {
    if RESERVED.contains(&name) {
        return Err(ManageError::Reserved(name.to_string()));
    }
    if doc.contains_key(name) {
        return Err(ManageError::Exists(name.to_string()));
    }
    doc[name] = toml_edit::table();
    let t = &mut doc[name];
    t["path"] = toml_edit::value(path.to_string_lossy().into_owned());
    if read_only {
        t["read_only"] = toml_edit::value(true);
    }
    if warm_index {
        t["warm_index"] = toml_edit::value(true);
    }
    Ok(())
}

/// Remove `[name]` together with its grants.
pub fn remove_export(doc: &mut DocumentMut, name: &str) -> Result<(), ManageError> {
    doc.remove(name)
        .map(|_| ())
        .ok_or_else(|| ManageError::NotFound(name.to_string()))
}

/// The table `export` names, or the top level for `None`.
fn target<'a>(
    doc: &'a mut DocumentMut,
    export: Option<&str>,
) -> Result<&'a mut Table, ManageError> {
    match export {
        Some(n) => doc
            .get_mut(n)
            .and_then(Item::as_table_mut)
            .ok_or_else(|| ManageError::NotFound(n.to_string())),
        None => Ok(doc.as_table_mut()),
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

/// Grant `who` access to `export` (the top level for `None`): read-only or
/// read-write. Any earlier grant of the same peer, under either spelling,
/// is replaced.
pub fn allow(
    doc: &mut DocumentMut,
    export: Option<&str>,
    who: &PeerSpec,
    ro: bool,
    resolve: &dyn Fn(&str) -> Option<EndpointId>,
) -> Result<(), ManageError> {
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

fn revoke_in(table: &mut Table, who: Option<&PeerSpec>, resolve: Resolve) -> usize {
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
/// arrays of `export` (or of the top level); `export = None` with a peer
/// walks the top level and every export.
pub fn revoke(
    doc: &mut DocumentMut,
    export: Option<&str>,
    who: Option<&PeerSpec>,
    resolve: &dyn Fn(&str) -> Option<EndpointId>,
) -> Result<usize, ManageError> {
    let n = match (export, who) {
        (None, Some(w)) => {
            let mut n = revoke_in(doc.as_table_mut(), Some(w), resolve);
            for (_, item) in doc.as_table_mut().iter_mut() {
                if let Some(t) = item.as_table_mut() {
                    n += revoke_in(t, Some(w), resolve);
                }
            }
            n
        }
        _ => revoke_in(target(doc, export)?, who, resolve),
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

/// `NAME:PATH` or a bare `PATH` (named by its final component). The text
/// before the first `:` is a name only when it contains no `/`. The path is
/// joined onto `cwd`, `.` and `..` are resolved lexically (symlinks are
/// left alone), and it must be an existing directory.
pub fn parse_add_target(arg: &str, cwd: &Path) -> Result<(String, PathBuf), ManageError> {
    let (name, raw) = match arg.split_once(':') {
        Some((n, p)) if !n.contains('/') => {
            if n.is_empty() {
                return Err(ManageError::BadName(n.to_string()));
            }
            (Some(n.to_string()), p)
        }
        _ => (None, arg),
    };
    let mut path = PathBuf::new();
    for c in cwd.join(raw).components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !path.pop() {
                    path.push(c);
                }
            }
            c => path.push(c),
        }
    }
    if !path.is_dir() {
        return Err(ManageError::NotADirectory(path));
    }
    let name = match name {
        Some(n) => n,
        None => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| ManageError::NotADirectory(path.clone()))?,
    };
    Ok((name, path))
}
