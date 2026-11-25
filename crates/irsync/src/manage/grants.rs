//! Edits of the grant lists in the config message for `grant add` and
//! `grant remove`: the `rw` and `ro` lists of every export. The editing functions are pure on a message; loading
//! and saving belong to the caller. The `list_*` reporters behind
//! `grant list` read the typed config.

use super::show::grant_lines;
use super::{check_comment, ClientSpec, ManageError};
use crate::authz::{Access, Snapshot};
use crate::config::clients::{Clients, Right, RIGHTS};
use crate::config::exports::Exports;
use crate::config::file::Config;
use crate::config::paths::Paths;
use crate::identity::parse_endpoint_id;
use iroh::EndpointId;
use irsync_proto::config as pb;
use std::collections::BTreeMap;
use std::fmt::Write;

type Resolve<'a> = &'a dyn Fn(&str) -> Option<EndpointId>;

/// The grant lists of one export, `rw` and `ro`, with their `*_set`
/// flags and the export's key comments.
struct Lists<'a> {
    rw: &'a mut Vec<pb::Entry>,
    rw_set: &'a mut bool,
    ro: &'a mut Vec<pb::Entry>,
    ro_set: &'a mut bool,
    key_comments: &'a mut BTreeMap<String, pb::Comment>,
}

impl<'a> Lists<'a> {
    fn of_export(e: &'a mut pb::Export) -> Self {
        Lists {
            rw: &mut e.rw,
            rw_set: &mut e.rw_set,
            ro: &mut e.ro,
            ro_set: &mut e.ro_set,
            key_comments: &mut e.key_comments,
        }
    }

    fn list(&mut self, ro: bool) -> &mut Vec<pb::Entry> {
        if ro {
            self.ro
        } else {
            self.rw
        }
    }

    /// Adds `entry` to `ro` or `rw`, last, and marks that list as set.
    fn push(&mut self, ro: bool, entry: pb::Entry) {
        let set = if ro {
            &mut *self.ro_set
        } else {
            &mut *self.rw_set
        };
        *set = true;
        self.list(ro).push(entry);
    }

    /// Removes entry `i` of `ro` or `rw`. Only the rest of its own line
    /// leaves with it: the comment lines above it stay in the file, in
    /// front of the next entry, else after the one before, else above the
    /// key.
    fn drop_at(&mut self, ro: bool, i: usize) {
        let list = self.list(ro);
        let above = match list.remove(i).comment {
            Some(c) => c.above,
            None => return,
        };
        if above.is_empty() {
            return;
        }
        let list = self.list(ro);
        if let Some(next) = list.get_mut(i) {
            let c = next.comment.get_or_insert_with(Default::default);
            c.above.splice(0..0, above);
        } else if let Some(prev) = i.checked_sub(1).and_then(|p| list.get_mut(p)) {
            prev.comment
                .get_or_insert_with(Default::default)
                .above
                .extend(above);
        } else {
            let key = if ro { "ro" } else { "rw" };
            self.key_comments
                .entry(key.to_string())
                .or_default()
                .above
                .extend(above);
        }
    }

    /// Removes the entries of `ro` or `rw` for which `pred` holds, as
    /// `drop_at` does; the count.
    fn remove_where_in(&mut self, ro: bool, pred: &dyn Fn(&str) -> bool) -> usize {
        let mut n = 0;
        let mut i = 0;
        while i < self.list(ro).len() {
            if pred(&self.list(ro)[i].value) {
                self.drop_at(ro, i);
                n += 1;
            } else {
                i += 1;
            }
        }
        n
    }

    /// Removes the entries of both lists for which `pred` holds; the count.
    fn remove_where(&mut self, pred: &dyn Fn(&str) -> bool) -> usize {
        self.remove_where_in(false, pred) + self.remove_where_in(true, pred)
    }
}

/// The grant lists of every export, read-only, each with the name
/// `lists_naming` reports it by and its `rw` and `ro` lists.
fn grant_lists(msg: &pb::Config) -> Vec<(String, [&Vec<pb::Entry>; 2])> {
    msg.exports
        .iter()
        .map(|e| (format!("exports.{}", e.name), [&e.rw, &e.ro]))
        .collect()
}

/// The grant lists of every export, for editing.
fn grant_lists_mut(msg: &mut pb::Config) -> Vec<Lists<'_>> {
    msg.exports.iter_mut().map(Lists::of_export).collect()
}

/// The lists of export `export`.
fn target<'a>(msg: &'a mut pb::Config, export: &str) -> Result<Lists<'a>, ManageError> {
    msg.exports
        .iter_mut()
        .find(|e| e.name == export)
        .map(Lists::of_export)
        .ok_or_else(|| ManageError::NotFound(export.to_string()))
}

/// The id `who` stands for, when it has one.
fn who_id(who: &ClientSpec, resolve: Resolve) -> Option<EndpointId> {
    match who {
        ClientSpec::Id { id, .. } => Some(*id),
        ClientSpec::Alias(a) => resolve(a),
    }
}

/// A grant entry names `who` by the same text or by the same id.
fn entry_matches(text: &str, who: &ClientSpec, id: Option<EndpointId>, resolve: Resolve) -> bool {
    if text == who.entry() {
        return true;
    }
    match id {
        Some(id) => resolve(text).or_else(|| parse_endpoint_id(text).ok()) == Some(id),
        None => false,
    }
}

/// The entry `who` is granted by: its text, and a public key's comment
/// `c` at the end of its line as `# c`. Refused for a comment that would
/// not stay on one line.
fn grant_entry(who: &ClientSpec) -> Result<pb::Entry, ManageError> {
    let comment = match who {
        ClientSpec::Id {
            comment: Some(c), ..
        } => {
            check_comment(c)?;
            Some(pb::Comment {
                above: Vec::new(),
                trailing: format!("# {c}"),
            })
        }
        _ => None,
    };
    Ok(pb::Entry {
        value: who.entry(),
        comment,
    })
}

/// Writes every grant entry in its canonical form: an EndpointId that
/// `clients` has a name for becomes that name (the first in
/// lexicographic order when there are several). An entry that is already
/// an alias, an id without an alias and text that names nobody are left
/// alone. Then, per table, an entry named twice in one list keeps its
/// first place, and an entry in both `rw` and `ro` leaves `ro`: `rw` wins
/// when the file is read, so nobody's access changes.
///
/// Covers every export. An entry keeps its
/// comments; a removed one takes only the rest of its line along. True when the message
/// changed; a second run changes nothing.
pub fn canonicalize(msg: &mut pb::Config, clients: &Clients) -> bool {
    let mut changed = false;
    for mut lists in grant_lists_mut(msg) {
        // What this table already names, `rw` before `ro`.
        let mut seen: Vec<String> = Vec::new();
        for ro in [false, true] {
            let mut i = 0;
            while i < lists.list(ro).len() {
                let text = &lists.list(ro)[i].value;
                let alias = parse_endpoint_id(text)
                    .ok()
                    .and_then(|id| clients.alias_of(&id));
                let name = alias.unwrap_or(text).to_string();
                if seen.contains(&name) {
                    lists.drop_at(ro, i);
                    changed = true;
                    continue;
                }
                if alias.is_some() {
                    lists.list(ro)[i].value = name.clone();
                    changed = true;
                }
                seen.push(name);
                i += 1;
            }
        }
    }
    changed
}

/// The tables with a grant entry whose text is `name`: `exports.NAME` in
/// file order, each once.
pub fn lists_naming(msg: &pb::Config, name: &str) -> Vec<String> {
    grant_lists(msg)
        .into_iter()
        .filter(|(_, lists)| lists.iter().any(|l| l.iter().any(|e| e.value == name)))
        .map(|(table, _)| table)
        .collect()
}

/// Removes every grant entry whose text is `name`, from every export;
/// the count. The comment lines above a
/// removed entry stay in the file.
pub fn remove_named(msg: &mut pb::Config, name: &str) -> usize {
    grant_lists_mut(msg)
        .into_iter()
        .map(|mut lists| lists.remove_where(&|t| t == name))
        .sum()
}

/// Writes every grant entry whose text is `old` as `new`, in every
/// export, keeping its comments; the count.
pub fn rename_named(msg: &mut pb::Config, old: &str, new: &str) -> usize {
    let mut n = 0;
    for lists in grant_lists_mut(msg) {
        for e in lists.rw.iter_mut().chain(lists.ro.iter_mut()) {
            if e.value == old {
                e.value = new.to_string();
                n += 1;
            }
        }
    }
    n
}

/// Grant `who` access to `export`: read-only or
/// read-write. Any earlier grant of the same client there, under either
/// spelling, is replaced; the new entry goes last. An earlier entry of the
/// same text moves there with its comments (the key's comment, when given,
/// replaces the rest of its line); one of another spelling leaves only the
/// comment lines above it behind.
pub fn grant_add(
    msg: &mut pb::Config,
    export: &str,
    who: &ClientSpec,
    ro: bool,
    resolve: &dyn Fn(&str) -> Option<EndpointId>,
) -> Result<(), ManageError> {
    let mut entry = grant_entry(who)?;
    let mut lists = target(msg, export)?;
    let text = who.entry();
    for from in [false, true] {
        if let Some(i) = lists.list(from).iter().position(|e| e.value == text) {
            let moved = lists.list(from).remove(i);
            if entry.comment.is_none() {
                entry.comment = moved.comment;
            } else if let (Some(new), Some(old)) = (entry.comment.as_mut(), moved.comment) {
                new.above = old.above;
            }
            break;
        }
    }
    let id = who_id(who, resolve);
    lists.remove_where(&|t| entry_matches(t, who, id, resolve));
    lists.push(ro, entry);
    Ok(())
}

/// Refuses the export `grant add` takes no grant on: `all`, which is a
/// right of a client (`all_exports`). `ro`: the grant asked for was
/// read-only.
pub(crate) fn check_add_export(export: &str, ro: bool) -> Result<(), ManageError> {
    if export == "all" {
        return Err(ManageError::BadValue(format!(
            "\"all\" is not an export: give a client every export with: irsync client set NAME all_exports={}",
            if ro { "ro" } else { "rw" }
        )));
    }
    Ok(())
}

/// Refuses what `grant remove EXPORT WHO` does not take: `*` for a client
/// (`all` is everyone).
pub(crate) fn check_remove(export: &str, who: &str) -> Result<(), ManageError> {
    if who == "*" {
        return Err(ManageError::BadValue(format!(
            "\"*\" names no client: irsync grant remove {export} all removes every grant {}",
            if export == "all" {
                "in the file".to_string()
            } else {
                format!("on {export}")
            }
        )));
    }
    Ok(())
}

/// What `grant add` reports once the grant is saved.
pub fn added_line(who: &ClientSpec, clients: &Clients, export: &str, ro: bool) -> String {
    let mode = if ro { "ro" } else { "rw" };
    format!("{}: {mode} on {export}", who.written(clients))
}

/// What `grant remove` reports once `n` entries are removed.
pub(crate) fn removed_line(n: usize) -> String {
    format!("removed {n} {}", if n == 1 { "entry" } else { "entries" })
}

/// Removes the grants of `who` (`None`: everyone) from both lists.
fn remove_in(lists: &mut Lists, who: Option<&ClientSpec>, resolve: Resolve) -> usize {
    match who {
        None => lists.remove_where(&|_| true),
        Some(w) => {
            let id = who_id(w, resolve);
            lists.remove_where(&|t| entry_matches(t, w, id, resolve))
        }
    }
}

/// Remove grants; the number of entries removed. `who = None` empties both
/// lists of `export`; `export = None` walks every export, so
/// `(None, None)` empties every export's lists. An emptied list stays in the file, as `rw = []`.
pub fn grant_remove(
    msg: &mut pb::Config,
    export: Option<&str>,
    who: Option<&ClientSpec>,
    resolve: &dyn Fn(&str) -> Option<EndpointId>,
) -> Result<usize, ManageError> {
    let n = match export {
        None => {
            let mut n = 0;
            for e in msg.exports.iter_mut() {
                n += remove_in(&mut Lists::of_export(e), who, resolve);
            }
            n
        }
        Some(name) => remove_in(&mut target(msg, name)?, who, resolve),
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

/// One line per server-wide right (of [`RIGHTS`]) still held by the clients
/// `grant remove` was asked about: `who` (a client's name or id), or, with
/// `who = None` and `export = None` (`grant remove all all`), every client.
/// `grant remove` clears only the exports' own lists, so these stay.
pub(crate) fn rights_still_held(
    clients: &Clients,
    export: Option<&str>,
    who: Option<&ClientSpec>,
    resolve: Resolve,
) -> Vec<String> {
    let id = who.and_then(|w| who_id(w, resolve));
    let mut notes = Vec::new();
    for (name, c) in &clients.entries {
        let asked = match who {
            None => export.is_none(),
            Some(w) => *name == w.entry() || id == Some(c.client_id),
        };
        if !asked {
            continue;
        }
        for right in RIGHTS {
            if let Some(a) = right.of(c) {
                notes.push(format!(
                    "{name} still has {}={}; remove it with: irsync client set {name} {}=none",
                    right.key(),
                    a.as_str(),
                    right.key()
                ));
            }
        }
    }
    notes
}

/// `NAME:` and the grant lines of one table; the header always.
fn block(name: &str, rw: &[String], ro: &[String], clients: &Clients) -> String {
    let mut out = format!("{name}:\n");
    grant_lines(&mut out, rw, ro, clients);
    out
}

/// `KEY:` and the `rw` and `ro` lines of the clients that hold the right
/// `right`; the header always.
fn right_block(right: Right, clients: &Clients) -> String {
    let names = |access: Access| -> Vec<String> {
        clients
            .entries
            .iter()
            .filter(|(_, c)| right.of(c) == Some(access))
            .map(|(name, _)| name.clone())
            .collect()
    };
    block(
        right.key(),
        &names(Access::ReadWrite),
        &names(Access::ReadOnly),
        clients,
    )
}

/// Every grant: one block per right of [`RIGHTS`] (`all_exports:`,
/// `all_paths:`, `admin:`), then one block per export in name order.
pub fn list_all(exports: &Exports, clients: &Clients) -> String {
    let mut out: String = RIGHTS.iter().map(|r| right_block(*r, clients)).collect();
    for (name, e) in &exports.exports {
        out.push_str(&block(name, &e.rw, &e.ro, clients));
    }
    out
}

/// The block of one export or of one right (`all_exports`, `all_paths`);
/// `None` when no such export.
pub fn list_export(exports: &Exports, clients: &Clients, name: &str) -> Option<String> {
    if let Some(right) = RIGHTS.into_iter().find(|r| r.key() == name) {
        return Some(right_block(right, clients));
    }
    let e = exports.exports.get(name)?;
    Some(block(name, &e.rw, &e.ro, clients))
}

/// `{label}:`, `    all_paths: rw|ro` when `id` has `all_paths`, then
/// one `    NAME: rw|ro` line per export `id` may reach: ` (all_exports)` when the
/// access comes from `all_exports` rather than the export's own entry,
/// ` (read_only export)` when a read-write grant is capped by the export's
/// `read_only` flag.
pub fn list_client(
    exports: &Exports,
    clients: &Clients,
    allow_all_paths: bool,
    label: &str,
    id: &EndpointId,
) -> String {
    let grants = Snapshot::from_files(clients, exports, allow_all_paths).grants(id);
    let mut out = format!("{label}:\n");
    if let Some(a) = grants.all_paths {
        let _ = writeln!(out, "    all_paths: {}", a.as_str());
    }
    if let Some(a) = grants.admin {
        let _ = writeln!(out, "    admin: {}", a.as_str());
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
            out.push_str(" (all_exports)");
        }
        if capped {
            out.push_str(" (read_only export)");
        }
        out.push('\n');
    }
    out
}

// The commands, from the config file's lock to the lines they say.

/// `grant add EXPORT WHO [--ro]`; the line to say.
pub fn add(paths: &Paths, export: &str, who: &str, ro: bool) -> anyhow::Result<String> {
    check_add_export(export, ro)?;
    let Config {
        servers, clients, ..
    } = Config::load(paths)?;
    let spec = ClientSpec::parse(who, &clients, &servers)?;
    let resolve = |s: &str| clients.get(s).map(|c| c.client_id);
    super::edit(&paths.config, |msg| {
        grant_add(msg, export, &spec, ro, &resolve)
    })?;
    Ok(added_line(&spec, &clients, export, ro))
}

/// A `grant remove` that failed: the error, and the access the client still
/// holds otherwise, which say why (empty when the grants were not looked
/// at yet).
#[derive(Debug)]
pub struct RemoveFailed {
    pub error: anyhow::Error,
    pub held: Vec<String>,
}

impl<E: Into<anyhow::Error>> From<E> for RemoveFailed {
    fn from(e: E) -> RemoveFailed {
        RemoveFailed {
            error: e.into(),
            held: Vec::new(),
        }
    }
}

/// `grant remove EXPORT WHO` (either may be `all`); the lines to say: how
/// many grants went, then the access the client still holds otherwise.
pub fn remove(paths: &Paths, export: &str, who: &str) -> Result<Vec<String>, RemoveFailed> {
    let Config {
        servers, clients, ..
    } = Config::load(paths)?;
    let resolve = |s: &str| clients.get(s).map(|c| c.client_id);
    check_remove(export, who)?;
    let export = (export != "all").then_some(export);
    let who = (who != "all").then_some(who);
    let spec = who
        .map(|w| ClientSpec::or_literal(w, ClientSpec::parse(w, &clients, &servers)))
        .transpose()?;
    let mut lock = super::lock_config(&paths.config)?;
    let mut msg = lock.load()?;
    let removed = grant_remove(&mut msg, export, spec.as_ref(), &resolve);
    let held = rights_still_held(&clients, export, spec.as_ref(), &resolve);
    let n = match removed {
        Ok(n) => n,
        Err(e) => {
            return Err(RemoveFailed {
                error: anyhow::Error::from(e).context(format!("in {}", paths.config.display())),
                held,
            })
        }
    };
    lock.save(&mut msg)?;
    let mut lines = vec![removed_line(n)];
    lines.extend(held);
    Ok(lines)
}
