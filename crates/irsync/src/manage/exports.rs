//! Edits of the exports in the config message for `export add`, `set`,
//! `rename` and `remove`, and the reading of an `export add` target. The edits
//! are pure on a message; loading and saving belong to the caller.

use super::ManageError;
use crate::authz::Access;
use crate::config::exports::RESERVED;
use crate::config::paths::Paths;
use iroh::EndpointId;
use irsync_proto::config as pb;
use std::path::{Component, Path, PathBuf};

/// The flags `export add` turns on. One that is false is left as it is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExportFlags {
    pub read_only: bool,
    pub index_at_start: bool,
    pub fsync: bool,
}

/// The settings `export set` changes, each `true` or `false`.
pub const SETTINGS: [&str; 3] = ["read_only", "index_at_start", "fsync"];

/// The value of setting `key` (one of [`SETTINGS`]) of `e`.
fn setting<'a>(e: &'a mut pb::Export, key: &str) -> &'a mut Option<bool> {
    match key {
        "read_only" => &mut e.read_only,
        "index_at_start" => &mut e.index_at_start,
        "fsync" => &mut e.fsync,
        _ => unreachable!("not an export setting: {key}"),
    }
}

/// Add export `name` at `path` with `flags`, last; for a name that is an
/// export already, set its path and turn `flags` on, keeping its other
/// settings and its grants. The path it had before, for an existing
/// export.
pub fn add_export(
    msg: &mut pb::Config,
    name: &str,
    path: &Path,
    flags: ExportFlags,
) -> Result<Option<String>, ManageError> {
    if RESERVED.contains(&name) {
        return Err(ManageError::Reserved(name.to_string()));
    }
    let path = path.to_string_lossy().into_owned();
    let on = |set: bool| set.then_some(true);
    if let Some(e) = msg.exports.iter_mut().find(|e| e.name == name) {
        let old = std::mem::replace(&mut e.path, path);
        for (key, set) in [
            ("read_only", flags.read_only),
            ("index_at_start", flags.index_at_start),
            ("fsync", flags.fsync),
        ] {
            if set {
                *setting(e, key) = Some(true);
            }
        }
        return Ok(Some(old));
    }
    msg.exports.push(pb::Export {
        name: name.to_string(),
        path,
        read_only: on(flags.read_only),
        index_at_start: on(flags.index_at_start),
        fsync: on(flags.fsync),
        ..Default::default()
    });
    Ok(None)
}

/// One `KEY=VALUE` of `export set`: a key of [`SETTINGS`] and `true`
/// or `false`.
pub fn parse_setting(s: &str) -> Result<(&'static str, bool), ManageError> {
    let keys = || SETTINGS.join(", ");
    let Some((key, value)) = s.split_once('=') else {
        return Err(ManageError::BadValue(format!(
            "{s:?} is not KEY=VALUE (keys: {})",
            keys()
        )));
    };
    let Some(key) = SETTINGS.into_iter().find(|k| *k == key) else {
        return Err(ManageError::BadValue(format!(
            "{key:?} is not a setting of an export (settings: {})",
            keys()
        )));
    };
    match value {
        "true" => Ok((key, true)),
        "false" => Ok((key, false)),
        _ => Err(ManageError::BadValue(format!(
            "{key} must be \"true\" or \"false\", not {value:?}"
        ))),
    }
}

/// Set the settings of export `name`; `false` removes the key, the
/// default. `NotFound` for a name that is no export; `msg` is then
/// unchanged.
pub fn export_set(
    msg: &mut pb::Config,
    name: &str,
    settings: &[(&str, bool)],
) -> Result<(), ManageError> {
    let e = msg
        .exports
        .iter_mut()
        .find(|e| e.name == name)
        .ok_or_else(|| ManageError::NotFound(name.to_string()))?;
    for (key, value) in settings {
        *setting(e, key) = value.then_some(true);
    }
    Ok(())
}

/// What `export set` reports: `NAME: read_only=… index_at_start=…
/// fsync=…`.
pub fn set_line(msg: &pb::Config, name: &str) -> Option<String> {
    let mut e = msg.exports.iter().find(|e| e.name == name)?.clone();
    let words: Vec<String> = SETTINGS
        .iter()
        .map(|k| format!("{k}={}", setting(&mut e, k).unwrap_or(false)))
        .collect();
    Some(format!("{name}: {}", words.join(" ")))
}

/// Rename export `old` to `new`. It keeps its place, path, settings and
/// grants. Refused when `old` is no export, `new` is not a valid export
/// name, or `new` is an export already.
pub fn rename_export(msg: &mut pb::Config, old: &str, new: &str) -> Result<(), ManageError> {
    if !msg.exports.iter().any(|e| e.name == old) {
        return Err(ManageError::NotFound(old.to_string()));
    }
    if RESERVED.contains(&new) {
        return Err(ManageError::Reserved(new.to_string()));
    }
    if crate::config::exports::name_problem(new).is_some() {
        return Err(ManageError::BadExportName(new.to_string()));
    }
    if msg.exports.iter().any(|e| e.name == new) {
        return Err(ManageError::Exists(new.to_string()));
    }
    let e = msg
        .exports
        .iter_mut()
        .find(|e| e.name == old)
        .expect("checked above");
    e.name = new.to_string();
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

/// Add an automatic export of `path` that lists `client` under `ro` for
/// read-only `access`, else under `rw`; the name it got (`-2`, `-3`, ...
/// when the name is taken).
pub fn add_auto_export(
    msg: &mut pb::Config,
    path: &Path,
    client: EndpointId,
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
        value: client.to_string(),
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
    add_target_in(arg, cwd, crate::config::paths::home_dir().as_deref())
}

/// The name, if any, and the path of an `export add` target: the text
/// before the first `:` is a name only when it contains no `/`.
pub fn split_target(arg: &str) -> (Option<&str>, &str) {
    match arg.split_once(':') {
        Some((n, p)) if !n.contains('/') => (Some(n), p),
        _ => (None, arg),
    }
}

/// Whether `raw` is `~` or starts with `~/`: a path under the home
/// directory.
fn under_home(raw: &str) -> bool {
    raw == "~" || raw.starts_with("~/")
}

/// The name an `export add` target gives, without looking at the disk:
/// the one before the `:`, else the final component of the path as typed.
pub fn target_name(arg: &str) -> String {
    match split_target(arg) {
        (Some(n), _) => n.to_string(),
        (None, raw) => Path::new(raw)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| raw.to_string()),
    }
}

/// An `export add` target sent by a remote client: as
/// [`parse_add_target`] with `home` as the home directory, but the path
/// must be absolute or start with `~`, since there is no working
/// directory to join it onto. The name and the path as the file is to
/// hold it: under `~` when it was typed so and stays under `home`.
pub(crate) fn parse_remote_target(
    arg: &str,
    home: Option<&Path>,
) -> Result<(String, String), ManageError> {
    let (_, raw) = split_target(arg);
    let tilde = under_home(raw);
    if !tilde && !raw.starts_with('/') {
        return Err(ManageError::BadValue(
            "the path must be absolute or start with ~".to_string(),
        ));
    }
    let (name, path) = add_target_in(arg, Path::new("/"), home)?;
    let rest = home
        .filter(|_| tilde)
        .and_then(|h| path.strip_prefix(h).ok());
    let written = match rest {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    };
    Ok((name, written))
}

/// What `export add` reports once the export is saved; `old`: the path
/// an existing export had.
pub fn added_line(name: &str, path: &Path, old: Option<&str>, config: &Path) -> String {
    match old {
        None => format!(
            "added export {name} ({}) to {}",
            path.display(),
            config.display()
        ),
        Some(old) if Path::new(old) == path => {
            format!("updated export {name} ({old}) in {}", config.display())
        }
        Some(old) => format!(
            "updated export {name} ({old} -> {}) in {}",
            path.display(),
            config.display()
        ),
    }
}

/// What `export rename` reports.
pub(crate) fn renamed_line(old: &str, new: &str, config: &Path) -> String {
    format!("renamed export {old} to {new} in {}", config.display())
}

/// What `export remove` reports once the export is gone.
pub(crate) fn removed_line(name: &str, config: &Path) -> String {
    format!("removed export {name} from {}", config.display())
}

/// [`parse_add_target`] with `home` for `~`.
fn add_target_in(
    arg: &str,
    cwd: &Path,
    home: Option<&Path>,
) -> Result<(String, PathBuf), ManageError> {
    let (name, raw) = match split_target(arg) {
        (Some(n), p) => {
            if n.is_empty() {
                return Err(ManageError::BadExportName(n.to_string()));
            }
            (Some(n.to_string()), p)
        }
        (None, p) => (None, p),
    };
    let joined = match raw.strip_prefix('~') {
        Some(rest) if under_home(raw) => home
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

// The commands, from the config file's lock to the line they say.

/// `export remove NAME`; the line to say.
pub fn remove(paths: &Paths, name: &str) -> anyhow::Result<String> {
    super::edit(&paths.config, |msg| remove_export(msg, name))?;
    Ok(removed_line(name, &paths.config))
}

/// `export set NAME KEY=VALUE...`; the line to say, if any.
pub fn set(paths: &Paths, name: &str, settings: &[String]) -> anyhow::Result<Option<String>> {
    // Every pair is read before anything changes.
    let settings: Vec<_> = settings
        .iter()
        .map(|s| parse_setting(s))
        .collect::<Result<_, _>>()?;
    super::load_first(paths)?;
    let ((), msg) = super::edit(&paths.config, |msg| export_set(msg, name, &settings))?;
    Ok(set_line(&msg, name))
}

/// `export rename OLD NEW`; the line to say.
pub fn rename(paths: &Paths, old: &str, new: &str) -> anyhow::Result<String> {
    super::load_first(paths)?;
    super::edit(&paths.config, |msg| rename_export(msg, old, new))?;
    Ok(renamed_line(old, new, &paths.config))
}
