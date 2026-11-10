//! Edits of the export tables in `irsync.toml` for `export add` and
//! `export remove`. All functions are pure on a parsed document; loading
//! and saving belong to the caller.

use super::ManageError;
use crate::config::exports::RESERVED;
use iroh::EndpointId;
use std::path::{Component, Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table, TableLike};

/// The `[exports]` table, created (implicit, so only its children print
/// headers) when missing. An inline `exports = { ... }` is edited in
/// place; anything else under that key is refused, never overwritten.
fn exports_table(doc: &mut DocumentMut) -> Result<&mut dyn TableLike, ManageError> {
    let item = doc.entry("exports").or_insert_with(|| {
        let mut t = Table::new();
        t.set_implicit(true);
        Item::Table(t)
    });
    item.as_table_like_mut()
        .ok_or_else(|| ManageError::NotATable("exports".to_string()))
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
    let exports = exports_table(doc)?;
    if exports.contains_key(name) {
        return Err(ManageError::Exists(name.to_string()));
    }
    let mut t = Table::new();
    t["path"] = toml_edit::value(path.to_string_lossy().into_owned());
    if read_only {
        t["read_only"] = toml_edit::value(true);
    }
    if warm_index {
        t["warm_index"] = toml_edit::value(true);
    }
    // Under a header this is `[exports.name]`; inside an inline
    // `exports = { ... }` toml_edit stores it inline.
    exports.insert(name, Item::Table(t));
    Ok(())
}

/// The auto export name for `path`: `auto-` and the components joined by
/// `-`, every byte outside `[A-Za-z0-9._-]` as `_`; `/` is `auto-root`.
fn auto_name(path: &Path) -> String {
    let parts: Vec<String> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.as_encoded_bytes()),
            _ => None,
        })
        .map(|b| {
            b.iter()
                .map(|&c| {
                    if c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-') {
                        c as char
                    } else {
                        '_'
                    }
                })
                .collect()
        })
        .collect();
    if parts.is_empty() {
        "auto-root".to_string()
    } else {
        format!("auto-{}", parts.join("-"))
    }
}

/// Add an automatic export of `path`, read-write for `peer`; the name it
/// got (`-2`, `-3`, ... when the name is taken).
pub fn add_auto_export(
    doc: &mut DocumentMut,
    path: &Path,
    peer: EndpointId,
) -> Result<String, ManageError> {
    let exports = exports_table(doc)?;
    let base = auto_name(path);
    let mut name = base.clone();
    let mut n = 2;
    while exports.contains_key(&name) {
        name = format!("{base}-{n}");
        n += 1;
    }
    let mut t = Table::new();
    t["path"] = toml_edit::value(path.to_string_lossy().into_owned());
    t["auto"] = toml_edit::value(true);
    t["rw"] = toml_edit::value(toml_edit::Array::from_iter([peer.to_string()]));
    exports.insert(&name, Item::Table(t));
    Ok(name)
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
/// before the first `:` is a name only when it contains no `/`. A path
/// that is `~` or starts with `~/` is under the home directory (the shell
/// does not expand `~` after the colon); otherwise the path is joined onto
/// `cwd`. `.` and `..` are resolved lexically (symlinks are left alone),
/// and the result must be an existing directory.
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
    let joined = match raw.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => dirs::home_dir()
            .ok_or(ManageError::NoHome)?
            .join(rest.trim_start_matches('/')),
        _ => cwd.join(raw),
    };
    let mut path = PathBuf::new();
    for c in joined.components() {
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
