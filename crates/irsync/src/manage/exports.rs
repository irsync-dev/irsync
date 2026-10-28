//! Edits of the export tables in `exports.toml` for `export add` and
//! `export remove`. All functions are pure on a parsed document; loading
//! and saving belong to the caller.

use super::ManageError;
use crate::config::exports::RESERVED;
use std::path::{Component, Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table};

/// The `[exports]` table, created (implicit, so only its children print
/// headers) when missing.
fn exports_table(doc: &mut DocumentMut) -> &mut Table {
    let item = doc.entry("exports").or_insert_with(|| {
        let mut t = Table::new();
        t.set_implicit(true);
        Item::Table(t)
    });
    if !item.is_table() {
        *item = {
            let mut t = Table::new();
            t.set_implicit(true);
            Item::Table(t)
        };
    }
    item.as_table_mut().expect("exports is a table")
}

/// Add `[exports.name]` with `path` and the flags that are true.
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
    let exports = exports_table(doc);
    if exports.contains_key(name) {
        return Err(ManageError::Exists(name.to_string()));
    }
    exports[name] = toml_edit::table();
    let t = &mut exports[name];
    t["path"] = toml_edit::value(path.to_string_lossy().into_owned());
    if read_only {
        t["read_only"] = toml_edit::value(true);
    }
    if warm_index {
        t["warm_index"] = toml_edit::value(true);
    }
    Ok(())
}

/// Remove `[exports.name]` together with its grants. Only an export (a
/// table under `[exports]`) is removed.
pub fn remove_export(doc: &mut DocumentMut, name: &str) -> Result<(), ManageError> {
    let exports = doc.get_mut("exports").and_then(Item::as_table_like_mut);
    let Some(exports) = exports else {
        return Err(ManageError::NotFound(name.to_string()));
    };
    if !exports.get(name).is_some_and(Item::is_table_like) {
        return Err(ManageError::NotFound(name.to_string()));
    }
    exports.remove(name);
    Ok(())
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
