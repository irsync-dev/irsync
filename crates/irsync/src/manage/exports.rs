//! Edits of the exports in the config message for `export add` and
//! `export remove`, and the reading of an `export add` target. The edits
//! are pure on a message; loading and saving belong to the caller.

use super::ManageError;
use crate::authz::Access;
use crate::config::exports::RESERVED;
use iroh::EndpointId;
use irsync_proto::config as pb;
use std::path::{Component, Path, PathBuf};

/// Add export `name` with `path` and the flags that are true, last.
pub fn add_export(
    msg: &mut pb::Config,
    name: &str,
    path: &Path,
    read_only: bool,
    warm_index: bool,
) -> Result<(), ManageError> {
    if RESERVED.contains(&name) {
        return Err(ManageError::Reserved(name.to_string()));
    }
    if msg.exports.iter().any(|e| e.name == name) {
        return Err(ManageError::Exists(name.to_string()));
    }
    msg.exports.push(pb::Export {
        name: name.to_string(),
        path: path.to_string_lossy().into_owned(),
        read_only: read_only.then_some(true),
        warm_index: warm_index.then_some(true),
        ..Default::default()
    });
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

/// The auto export name for `path` that `taken` does not hold yet: the
/// plain name, else it with `-2`, `-3`, ...
pub(crate) fn unique_auto_name(path: &Path, taken: impl Fn(&str) -> bool) -> String {
    let base = auto_name(path);
    let mut name = base.clone();
    let mut n = 2;
    while taken(&name) {
        name = format!("{base}-{n}");
        n += 1;
    }
    name
}

/// Add an automatic export of `path` that lists `peer` under `ro` for
/// read-only `access`, else under `rw`; the name it got (`-2`, `-3`, ...
/// when the name is taken).
pub fn add_auto_export(
    msg: &mut pb::Config,
    path: &Path,
    peer: EndpointId,
    access: Access,
) -> Result<String, ManageError> {
    let name = unique_auto_name(path, |n| msg.exports.iter().any(|e| e.name == n));
    let mut export = pb::Export {
        name: name.clone(),
        path: path.to_string_lossy().into_owned(),
        auto: Some(true),
        ..Default::default()
    };
    let listed = vec![pb::Entry {
        value: peer.to_string(),
        comment: None,
    }];
    match access {
        Access::ReadOnly => (export.ro, export.ro_set) = (listed, true),
        Access::ReadWrite => (export.rw, export.rw_set) = (listed, true),
    }
    msg.exports.push(export);
    Ok(name)
}

/// Remove export `name` together with its grants.
pub fn remove_export(msg: &mut pb::Config, name: &str) -> Result<(), ManageError> {
    let i = msg
        .exports
        .iter()
        .position(|e| e.name == name)
        .ok_or_else(|| ManageError::NotFound(name.to_string()))?;
    msg.exports.remove(i);
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
        Some(rest) if rest.is_empty() || rest.starts_with('/') => crate::config::paths::home_dir()
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
