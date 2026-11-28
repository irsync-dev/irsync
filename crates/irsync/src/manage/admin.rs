//! The admin engine: what a client with `admin` asks of the server's
//! configuration over an admin session, pure on a config message. A
//! transaction of changes is applied all or nothing, each change with the
//! functions the local command uses; the reads answer from a message. The
//! daemon loads and saves the message and holds the config lock around
//! [`apply`].

use super::{clients, config, exports, grants, typed, ClientSpec, ManageError};
use crate::authz::Access;
use crate::config::clients::{Clients, RIGHTS};
use crate::platform::Platform;
use iroh::EndpointId;
use irsync_proto::config as pb;
use irsync_proto::v1;
use irsync_proto::v1::change::Op;
use std::path::Path;

/// The NACK reason for a committer without `admin = "rw"`.
pub const NOT_ALLOWED: &str = "not allowed";

/// The NACK reason for a transaction that would leave the committer
/// without its own `admin = "rw"`.
pub const OWN_ADMIN: &str = "this would take away your own admin right";

/// The client that commits a transaction: the key of the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Committer {
    pub id: EndpointId,
}

/// The highest `admin` right over the client entries of `msg` with
/// `id` (`rw` beats `ro`); none when no entry gives it, or when the
/// clients of `msg` do not load (`path` is the file they are from).
pub fn admin_of(msg: &pb::Config, path: &Path, id: &EndpointId) -> Option<Access> {
    let clients = Clients::from_message(&msg.clients, path).ok()?;
    clients
        .entries
        .values()
        .filter(|c| c.client_id == *id)
        .filter_map(|c| c.admin)
        .fold(None, |best, a| match (best, a) {
            (Some(Access::ReadWrite), _) | (_, Access::ReadWrite) => Some(Access::ReadWrite),
            _ => Some(Access::ReadOnly),
        })
}

/// The names of the client entries of `msg` that give `id`
/// `admin = "rw"`.
fn admin_names(msg: &pb::Config, path: &Path, id: &EndpointId) -> Vec<String> {
    let Ok(clients) = Clients::from_message(&msg.clients, path) else {
        return Vec::new();
    };
    clients
        .entries
        .into_iter()
        .filter(|(_, c)| c.client_id == *id && c.admin == Some(Access::ReadWrite))
        .map(|(name, _)| name)
        .collect()
}

/// Applies `changes` in order on `msg`, loaded from the config file
/// `path` under its lock; `home` is the home directory a `~` path of
/// `export add` is under. One result line per change (several lines
/// joined by `\n`), what the local command prints.
///
/// All or nothing: on a NACK `msg` is left as it was. The committer
/// must have `admin = "rw"` in `msg` (else NACK 0 [`NOT_ALLOWED`]), and
/// every entry that gave it so must still exist, with its id and
/// `admin = "rw"`, once all the changes are applied (else NACK 0
/// [`OWN_ADMIN`]): no change, in any order, removes, renames or
/// lowers the committer. A change that fails, or after which the file
/// would not load, is NACKed with its number (1-based) and the reason.
pub fn apply(
    msg: &mut pb::Config,
    path: &Path,
    home: Option<&Path>,
    who: &Committer,
    changes: &[v1::Change],
) -> Result<Vec<String>, v1::Nack> {
    let whole = |reason: &str| v1::Nack {
        change: 0,
        reason: reason.to_string(),
    };
    if admin_of(msg, path, &who.id) != Some(Access::ReadWrite) {
        return Err(whole(NOT_ALLOWED));
    }
    let mine = admin_names(msg, path, &who.id);
    let mut work = msg.clone();
    let mut results = Vec::with_capacity(changes.len());
    for (i, c) in changes.iter().enumerate() {
        let line = apply_one(&mut work, path, home, c)
            .and_then(|line| typed(&work, path).map(|_| line))
            .map_err(|e| v1::Nack {
                change: u32::try_from(i + 1).unwrap_or(u32::MAX),
                reason: e.to_string(),
            })?;
        results.push(line);
    }
    let kept = admin_names(&work, path, &who.id);
    if !mine.iter().all(|n| kept.contains(n)) {
        return Err(whole(OWN_ADMIN));
    }
    *msg = work;
    Ok(results)
}

/// One change on `msg`, as the local command makes it; its result line.
fn apply_one(
    msg: &mut pb::Config,
    path: &Path,
    home: Option<&Path>,
    change: &v1::Change,
) -> Result<String, ManageError> {
    let Some(op) = &change.op else {
        return Err(ManageError::BadValue("an empty change".to_string()));
    };
    match op {
        Op::ExportAdd(a) => {
            let (name, written) = exports::parse_remote_target(&a.target, home)?;
            let written = Path::new(&written);
            let flags = exports::ExportFlags {
                read_only: a.read_only,
                index_at_start: a.index_at_start,
                fsync: false,
            };
            let old = exports::add_export(msg, &name, written, flags)?;
            Ok(exports::added_line(&name, written, old.as_deref(), path))
        }
        Op::ExportRemove(r) => {
            exports::remove_export(msg, &r.name)?;
            Ok(exports::removed_line(&r.name, path))
        }
        Op::ExportSet(s) => {
            // Every pair is read before anything changes.
            let settings = s
                .settings
                .iter()
                .map(|s| exports::parse_setting(s))
                .collect::<Result<Vec<_>, _>>()?;
            exports::export_set(msg, &s.name, &settings)?;
            Ok(exports::set_line(msg, &s.name).unwrap_or_default())
        }
        Op::ExportRename(r) => {
            exports::rename_export(msg, &r.old, &r.new)?;
            Ok(exports::renamed_line(&r.old, &r.new, path))
        }
        Op::GrantAdd(g) => {
            grants::check_add_export(&g.export, g.ro)?;
            let config = typed(msg, path)?;
            let spec = ClientSpec::parse_text(&g.who, &config.clients, &config.servers)?;
            let resolve = |s: &str| config.clients.get(s).map(|c| c.client_id);
            grants::grant_add(msg, &g.export, &spec, g.ro, &resolve)?;
            Ok(grants::added_line(&spec, &config.clients, &g.export, g.ro))
        }
        Op::GrantRemove(g) => {
            grants::check_remove(&g.export, &g.who)?;
            let config = typed(msg, path)?;
            let (clients, servers) = (&config.clients, &config.servers);
            let resolve = |s: &str| clients.get(s).map(|c| c.client_id);
            let export = (g.export != "all").then_some(g.export.as_str());
            let spec = (g.who != "all")
                .then(|| {
                    ClientSpec::or_literal(&g.who, ClientSpec::parse_text(&g.who, clients, servers))
                })
                .transpose()?;
            let n = grants::grant_remove(msg, export, spec.as_ref(), &resolve)?;
            let mut lines = vec![grants::removed_line(n)];
            lines.extend(grants::rights_still_held(
                clients,
                export,
                spec.as_ref(),
                &resolve,
            ));
            Ok(lines.join("\n"))
        }
        Op::ClientAdd(c) => {
            let id = super::id_of(&c.id).ok_or_else(|| {
                ManageError::BadValue(format!("the id of client {:?} is no EndpointId", c.name))
            })?;
            let before = clients::client_ids(msg, path);
            let old = clients::client_add(msg, &c.name, id)?;
            let mut lines = vec![clients::added_line(&c.name, old, id)];
            lines.extend(clients::moved_grant_notes("client add", &before, msg, path));
            Ok(lines.join("\n"))
        }
        Op::ClientRemove(r) => {
            let n = clients::remove_client(msg, path, &r.name, r.force)?;
            Ok(clients::removed_line(&r.name, n, path))
        }
        Op::ClientRename(r) => {
            let n = clients::rename_client(msg, path, &r.old, &r.new)?;
            Ok(clients::renamed_line(&r.old, &r.new, n, path))
        }
        Op::ClientSet(s) => {
            // Every pair is read before anything changes.
            let settings = s
                .settings
                .iter()
                .map(|s| clients::parse_setting(s))
                .collect::<Result<Vec<_>, _>>()?;
            clients::client_set(msg, &s.name, &settings)?;
            Ok(clients::set_line(msg, &s.name).unwrap_or_default())
        }
    }
}

/// What a read of the exports and clients answers: the daemon's id, its
/// `allow_all_paths`, each export (its path with `~` expanded to the
/// daemon's `home`, as `export list` prints it; left as written with no
/// home) and each client with the rights it has, in file order.
pub fn view(
    msg: &pb::Config,
    daemon_id: EndpointId,
    allow_all_paths: bool,
    home: Option<&Path>,
) -> v1::View {
    let values = |list: &[pb::Entry]| list.iter().map(|e| e.value.clone()).collect();
    v1::View {
        daemon_id: daemon_id.as_bytes().to_vec(),
        allow_all_paths,
        exports: msg
            .exports
            .iter()
            .map(|e| v1::ExportView {
                name: e.name.clone(),
                path: under(&e.path, home),
                read_only: e.read_only.unwrap_or(false),
                index_at_start: e.index_at_start.unwrap_or(false),
                fsync: e.fsync.unwrap_or(false),
                is_auto: e.auto.unwrap_or(false),
                rw: values(&e.rw),
                ro: values(&e.ro),
            })
            .collect(),
        clients: msg
            .clients
            .iter()
            .map(|c| v1::ClientView {
                name: c.name.clone(),
                id: c.client_id.clone(),
                rights: RIGHTS
                    .iter()
                    .filter_map(|r| Some((r.key().to_string(), r.get(c)?.as_str().to_string())))
                    .collect(),
            })
            .collect(),
    }
}

/// `p` with a leading `~` (`~`, `~/`; on Windows also `~\`) as `home`; as
/// written when there is no home.
fn under(p: &str, home: Option<&Path>) -> String {
    let sep = |r: &&str| {
        r.bytes()
            .next()
            .is_some_and(|b| Platform::CURRENT.is_separator(b))
    };
    let rest = if p == "~" {
        Some("")
    } else {
        p.strip_prefix('~').filter(sep).map(|r| &r[1..])
    };
    match (rest, home) {
        (Some(""), Some(h)) => h.to_string_lossy().into_owned(),
        (Some(rest), Some(h)) => h.join(rest).to_string_lossy().into_owned(),
        _ => p.to_string(),
    }
}

/// What `config get KEY` prints, or with an empty `key` what
/// `config list` prints. No lines for a key that is unset and has no
/// default (the local `config get` exits 1). The error is its text.
pub fn config_text(msg: &pb::Config, key: &str) -> Result<v1::ConfigText, String> {
    if key.is_empty() {
        return Ok(v1::ConfigText {
            lines: config::list(msg),
        });
    }
    let lines = config::key_path(key)
        .and_then(|path| config::get_or_default(msg, &path))
        .map_err(|e| e.to_string())?;
    Ok(v1::ConfigText {
        lines: lines.unwrap_or_default(),
    })
}

/// The audit text of a change, such as `added export photos at ~/Photos` or
/// `set laptop admin=ro`. The client's text is written with its control
/// characters escaped, so it cannot start a log line of its own.
pub fn summary(change: &v1::Change) -> String {
    let text = match &change.op {
        None => "sent an empty change".to_string(),
        Some(Op::ExportAdd(a)) => {
            let (_, path) = exports::split_target(&a.target);
            let mut s = format!("added export {} at {path}", exports::target_name(&a.target));
            if a.read_only {
                s.push_str(" read_only");
            }
            if a.index_at_start {
                s.push_str(" index_at_start");
            }
            s
        }
        Some(Op::ExportRemove(r)) => format!("removed export {}", r.name),
        Some(Op::ExportSet(s)) => format!("set export {} {}", s.name, s.settings.join(" ")),
        Some(Op::ExportRename(r)) => format!("renamed export {} to {}", r.old, r.new),
        Some(Op::GrantAdd(g)) => {
            let mode = if g.ro { "ro" } else { "rw" };
            format!("granted {} {mode} on {}", g.who, g.export)
        }
        Some(Op::GrantRemove(g)) => format!("removed the grants of {} on {}", g.who, g.export),
        Some(Op::ClientAdd(c)) => {
            let id =
                super::id_of(&c.id).map_or_else(|| "(invalid id)".to_string(), |id| id.to_string());
            format!("added client {} {id}", c.name)
        }
        Some(Op::ClientRemove(r)) => {
            let force = if r.force { " --force" } else { "" };
            format!("removed client {}{force}", r.name)
        }
        Some(Op::ClientRename(r)) => format!("renamed client {} to {}", r.old, r.new),
        Some(Op::ClientSet(s)) => format!("set {} {}", s.name, s.settings.join(" ")),
    };
    escape_controls(&text)
}

/// `s` with each control character as its Rust escape (`\n`, `\u{7}`).
pub(crate) fn escape_controls(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() {
                c.escape_default().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}
