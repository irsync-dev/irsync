//! Edits of the grant lists in the config message for `grant add` and
//! `grant remove`: the `rw` and `ro` lists of `[all]`, `[immediate]`
//! and every export. Pure on a message; loading and saving belong to the
//! caller.

use crate::config::hosts::Hosts;
use crate::identity::parse_endpoint_id;
use crate::manage::{check_comment, ManageError, PeerSpec};
use iroh::EndpointId;
use irsync_proto::config as pb;
use std::collections::BTreeMap;

type Resolve<'a> = &'a dyn Fn(&str) -> Option<EndpointId>;

/// The grant lists of one table, `rw` and `ro`, with their `*_set`
/// flags and the table's key comments.
struct Lists<'a> {
    rw: &'a mut Vec<pb::Entry>,
    rw_set: &'a mut bool,
    ro: &'a mut Vec<pb::Entry>,
    ro_set: &'a mut bool,
    key_comments: &'a mut BTreeMap<String, pb::Comment>,
}

impl<'a> Lists<'a> {
    fn of_table(t: &'a mut pb::GrantTable) -> Self {
        Lists {
            rw: &mut t.rw,
            rw_set: &mut t.rw_set,
            ro: &mut t.ro,
            ro_set: &mut t.ro_set,
            key_comments: &mut t.key_comments,
        }
    }

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

/// The grant tables, read-only: `[all]`, `[immediate]` and every export,
/// each with the name `lists_naming` reports it by and its `rw` and
/// `ro` lists.
fn grant_lists(msg: &pb::Config) -> Vec<(String, [&Vec<pb::Entry>; 2])> {
    let mut out = Vec::new();
    if let Some(t) = &msg.all {
        out.push(("all".to_string(), [&t.rw, &t.ro]));
    }
    if let Some(t) = &msg.immediate {
        out.push(("immediate".to_string(), [&t.rw, &t.ro]));
    }
    for e in &msg.exports {
        out.push((format!("exports.{}", e.name), [&e.rw, &e.ro]));
    }
    out
}

/// The grant tables, as `grant_lists`, for editing.
fn grant_lists_mut(msg: &mut pb::Config) -> Vec<Lists<'_>> {
    let mut out = Vec::new();
    if let Some(t) = msg.all.as_mut() {
        out.push(Lists::of_table(t));
    }
    if let Some(t) = msg.immediate.as_mut() {
        out.push(Lists::of_table(t));
    }
    out.extend(msg.exports.iter_mut().map(Lists::of_export));
    out
}

/// The lists of export `export`, or of `[all]` for `None` (created when
/// missing).
fn target<'a>(msg: &'a mut pb::Config, export: Option<&str>) -> Result<Lists<'a>, ManageError> {
    match export {
        Some(n) => msg
            .exports
            .iter_mut()
            .find(|e| e.name == n)
            .map(Lists::of_export)
            .ok_or_else(|| ManageError::NotFound(n.to_string())),
        None => Ok(Lists::of_table(
            msg.all.get_or_insert_with(Default::default),
        )),
    }
}

/// The id `who` stands for, when it has one.
fn who_id(who: &PeerSpec, resolve: Resolve) -> Option<EndpointId> {
    match who {
        PeerSpec::Id { id, .. } => Some(*id),
        PeerSpec::Alias(a) => resolve(a),
    }
}

/// A grant entry names `who` by the same text or by the same id.
fn entry_matches(text: &str, who: &PeerSpec, id: Option<EndpointId>, resolve: Resolve) -> bool {
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
fn grant_entry(who: &PeerSpec) -> Result<pb::Entry, ManageError> {
    let comment = match who {
        PeerSpec::Id {
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
/// `hosts` has an alias for becomes that alias (the first in
/// lexicographic order when there are several). An entry that is already
/// an alias, an id without an alias and text that names nobody are left
/// alone. Then, per table, an entry named twice in one list keeps its
/// first place, and an entry in both `rw` and `ro` leaves `ro`: `rw` wins
/// when the file is read, so nobody's access changes.
///
/// Covers `[all]`, `[immediate]` and every export. An entry keeps its
/// comments; a removed one takes only the rest of its line along. True when the message
/// changed; a second run changes nothing.
pub fn canonicalize(msg: &mut pb::Config, hosts: &Hosts) -> bool {
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
                    .and_then(|id| hosts.alias_of(&id));
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

/// The tables with a grant entry whose text is `name`: `all`,
/// `immediate`, then `exports.NAME` in file order, each once.
pub fn lists_naming(msg: &pb::Config, name: &str) -> Vec<String> {
    grant_lists(msg)
        .into_iter()
        .filter(|(_, lists)| lists.iter().any(|l| l.iter().any(|e| e.value == name)))
        .map(|(table, _)| table)
        .collect()
}

/// Removes every grant entry whose text is `name`, from `[all]`,
/// `[immediate]` and every export; the count. The comment lines above a
/// removed entry stay in the file.
pub fn remove_named(msg: &mut pb::Config, name: &str) -> usize {
    grant_lists_mut(msg)
        .into_iter()
        .map(|mut lists| lists.remove_where(&|t| t == name))
        .sum()
}

/// Writes every grant entry whose text is `old` as `new`, in `[all]`,
/// `[immediate]` and every export, keeping its comments; the count.
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

/// Grant `who` access to `export` (`[all]` for `None`): read-only or
/// read-write. Any earlier grant of the same peer there, under either
/// spelling, is replaced; the new entry goes last. An earlier entry of the
/// same text moves there with its comments (the key's comment, when given,
/// replaces the rest of its line); one of another spelling leaves only the
/// comment lines above it behind.
pub fn grant_add(
    msg: &mut pb::Config,
    export: Option<&str>,
    who: &PeerSpec,
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

/// Grant `who` immediate access (`[immediate].rw`). False when the peer
/// was already granted, under either spelling.
pub fn grant_immediate(
    msg: &mut pb::Config,
    who: &PeerSpec,
    resolve: &dyn Fn(&str) -> Option<EndpointId>,
) -> Result<bool, ManageError> {
    let entry = grant_entry(who)?;
    let mut lists = Lists::of_table(msg.immediate.get_or_insert_with(Default::default));
    let id = who_id(who, resolve);
    if lists
        .rw
        .iter()
        .any(|e| entry_matches(&e.value, who, id, resolve))
    {
        return Ok(false);
    }
    lists.push(false, entry);
    Ok(true)
}

/// Remove `who` from `[immediate].rw` (`None`: everyone); the count.
pub fn grant_remove_immediate(
    msg: &mut pb::Config,
    who: Option<&PeerSpec>,
    resolve: &dyn Fn(&str) -> Option<EndpointId>,
) -> Result<usize, ManageError> {
    let n = match msg.immediate.as_mut() {
        Some(t) => {
            let mut lists = Lists::of_table(t);
            match who {
                None => lists.remove_where_in(false, &|_| true),
                Some(w) => {
                    let id = who_id(w, resolve);
                    lists.remove_where_in(false, &|s| entry_matches(s, w, id, resolve))
                }
            }
        }
        None => 0,
    };
    if n == 0 {
        let what = who.map_or_else(|| "immediate".to_string(), PeerSpec::entry);
        return Err(ManageError::NothingToRemove(what));
    }
    Ok(n)
}

/// Removes the grants of `who` (`None`: everyone) from both lists.
fn remove_in(lists: &mut Lists, who: Option<&PeerSpec>, resolve: Resolve) -> usize {
    match who {
        None => lists.remove_where(&|_| true),
        Some(w) => {
            let id = who_id(w, resolve);
            lists.remove_where(&|t| entry_matches(t, w, id, resolve))
        }
    }
}

/// Remove grants; the number of entries removed. `who = None` empties both
/// lists of `export` (or of `[all]`); `export = None` walks `[all]` and
/// every export, so `(None, None)` empties every list but `[immediate]`.
/// An emptied list stays in the file, as `rw = []`.
pub fn grant_remove(
    msg: &mut pb::Config,
    export: Option<&str>,
    who: Option<&PeerSpec>,
    resolve: &dyn Fn(&str) -> Option<EndpointId>,
) -> Result<usize, ManageError> {
    let n = match export {
        None => {
            let mut n = match msg.all.as_mut() {
                Some(all) => remove_in(&mut Lists::of_table(all), who, resolve),
                None => 0,
            };
            for e in msg.exports.iter_mut() {
                n += remove_in(&mut Lists::of_export(e), who, resolve);
            }
            n
        }
        Some(_) => remove_in(&mut target(msg, export)?, who, resolve),
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
