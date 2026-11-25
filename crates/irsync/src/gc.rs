//! Garbage collection of resume state: abandoned `.part` files, their
//! records, dormant sessions and index entries of files that are gone.
//! The daemon runs it on a timer; the client runs it on its own state
//! after a pull and with `irsync gc`.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use irsync_proto::v1::SessionPlan;
use prost::Message;

use crate::filter::FilterSet;
use crate::fs::paths::RelPath;
use crate::fs::temp_name::is_part_name;
use crate::index::{now_secs, IndexHandle};
use crate::scan::WalkOpts;

/// What a [`gc`] run removed (or, on a dry run, would remove).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
    /// Relative paths of the removed `.irsync.<name>.part` files.
    pub parts_removed: Vec<RelPath>,
    /// Stale-looking partial records kept because a session still references them.
    pub parts_kept: usize,
    /// Sessions removed (on a dry run: that would be removed).
    pub sessions_removed: u32,
    /// Index entries of regular files that are gone or were replaced
    /// (another inode or size) since they were indexed, removed with their
    /// chunk rows (on a dry run: that would be removed).
    pub entries_removed: usize,
    /// Stale records whose in-place `.part` file does not exist (for example
    /// it lives in a `--partial-dir`, which records do not remember). The
    /// record is dropped; the file, if any, is collected as an orphan later.
    pub rows_without_file: usize,
    /// Paths that could not be removed or walked; the pass carries on.
    pub errors: Vec<(RelPath, String)>,
}

fn io_err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

/// The `.part` file the receiver keeps for destination `rel`. The name may
/// be shortened (see [`crate::fs::temp_name::temp_name`]), so there is no inverse:
/// the GC maps every known destination forward and compares.
fn part_of(rel: &RelPath) -> Option<RelPath> {
    let b = rel.as_bytes();
    let i = b.iter().rposition(|&c| c == b'/').map_or(0, |i| i + 1);
    let mut p = b[..i].to_vec();
    p.extend_from_slice(&crate::fs::temp_name::temp_name(&b[i..], b".part"));
    RelPath::new(&p).ok()
}

/// What [`remove_part`] did.
#[derive(Debug, PartialEq, Eq)]
enum Removal {
    Removed,
    Missing,
    /// Not ours to remove: wrong name, not a regular file, or touched since.
    Skipped,
}

/// Removes `root/rel` if its name is a part name, it is a regular file
/// and its mtime is not newer than `cutoff`. With `dry_run` only
/// classifies. No symlink is followed, the final name's or a directory's
/// on the way (one swapped in since the path was recorded): such a path
/// holds no part of ours, so it counts as missing.
fn remove_part(root: &Path, rel: &RelPath, cutoff: i64, dry_run: bool) -> io::Result<Removal> {
    use crate::fs::safe::{open_dir_beneath, split_last};
    use crate::platform::{cause, Cause, Kind as FileKind};
    if !rel.components().last().is_some_and(is_part_name) {
        return Ok(Removal::Skipped);
    }
    let (dir, name) = split_last(rel)?;
    let name = &name[..];
    let dfd = match open_dir_beneath(root, &dir) {
        Ok(fd) => fd,
        // A symlinked or non-directory component (`InvalidInput`), or gone.
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
            ) =>
        {
            return Ok(Removal::Missing);
        }
        Err(e) => return Err(e),
    };
    match dfd.stat(name) {
        Ok(st) if st.kind == FileKind::File && st.mtime.0 <= cutoff => {
            if !dry_run {
                dfd.remove_file(name)?;
            }
            Ok(Removal::Removed)
        }
        Ok(_) => Ok(Removal::Skipped),
        Err(e) if cause(&e) == Cause::NotThere => Ok(Removal::Missing),
        Err(e) => Err(e),
    }
}

/// The `others` (export or index roots) strictly inside `root`, relative
/// to it, sorted. An export's GC leaves those subtrees to their own index
/// (the home exported automatically holds every export below it).
pub fn nested_roots<'a>(root: &Path, others: impl IntoIterator<Item = &'a Path>) -> Vec<RelPath> {
    let outer = crate::resolve::parts_of(root);
    let mut out: Vec<RelPath> = others
        .into_iter()
        .filter_map(|p| {
            let inner = crate::resolve::parts_of(p);
            if inner.len() <= outer.len() || inner[..outer.len()] != outer[..] {
                return None;
            }
            RelPath::new(&inner[outer.len()..].join(&b'/')).ok()
        })
        .collect();
    out.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    out.dedup();
    out
}

/// `rel` is `top` or below it.
fn is_under(rel: &RelPath, top: &RelPath) -> bool {
    let (r, t) = (rel.as_bytes(), top.as_bytes());
    r == t || (r.len() > t.len() && r.starts_with(t) && r[t.len()] == b'/')
}

/// Garbage-collects resume state under `root`: abandoned `.part` files and
/// their `partials` rows, then dormant sessions, all older than `older_than`.
/// A part is only removed when no session plan references its path and its
/// record (or, for an orphan without a record, its mtime) is stale. With
/// `dry_run` nothing is changed. Parts are collected before sessions, so a
/// part freed by an expiring session goes on the next run.
pub async fn gc(
    index: IndexHandle,
    root: &Path,
    older_than: Duration,
    dry_run: bool,
) -> io::Result<GcReport> {
    gc_skipping(index, root, older_than, dry_run, &[]).await
}

/// [`gc`], leaving alone everything at or below the paths in `skip`
/// (relative to `root`): the roots of other exports ([`nested_roots`]).
/// No part, record or index entry there is removed.
pub async fn gc_skipping(
    index: IndexHandle,
    root: &Path,
    older_than: Duration,
    dry_run: bool,
    skip: &[RelPath],
) -> io::Result<GcReport> {
    let skip = skip.to_vec();
    let skipped = move |p: &RelPath| skip.iter().any(|s| is_under(p, s));
    let cutoff = crate::index::cutoff(older_than);

    // Orphan candidates: stale `.part` files on disk (the walk never follows symlinks).
    let (mut rx, summary) = crate::scan::walk(
        root.to_path_buf(),
        Arc::new(FilterSet::new(vec![])),
        WalkOpts {
            follow_root_symlink: false,
            threads: 2,
            max_depth: None,
        },
        None,
    );
    let mut on_disk = Vec::new();
    // Every regular file the walk saw, by inode and size.
    let mut seen = std::collections::HashMap::new();
    while let Some(ev) = rx.recv().await {
        // Gaps are in the summary's errors.
        let crate::scan::ScanEvent::Entry(item) = ev else {
            continue;
        };
        if skipped(&item.path) {
            continue;
        }
        let is_reg = item.entry.kind() == irsync_proto::v1::file_entry::Kind::Regular;
        let named = item.path.components().last().is_some_and(is_part_name);
        if is_reg && named && item.entry.mtime_sec <= cutoff {
            on_disk.push(item.path);
        } else if is_reg {
            seen.insert(item.path, (item.entry.inode, item.entry.size));
        }
    }
    let mut walk_errors = Vec::new();
    if let Ok(sum) = summary.await {
        walk_errors = sum.errors;
    }

    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || -> io::Result<GcReport> {
        let mut ix = index.lock().map_err(|_| io_err("index lock poisoned"))?;
        let mut referenced: Vec<Vec<u8>> = Vec::new();
        for row in ix.all_sessions().map_err(io_err)? {
            let plan = SessionPlan::decode(&row.plan[..]).map_err(io_err)?;
            referenced.extend(plan.entries.into_iter().map(|e| e.path));
        }
        // The `.part` of every referenced path, so a part on disk can be
        // matched without inverting its (possibly shortened) name.
        let referenced_parts: Vec<Vec<u8>> = referenced
            .iter()
            .filter_map(|p| RelPath::new(p).ok())
            .filter_map(|p| part_of(&p))
            .map(|p| p.as_bytes().to_vec())
            .collect();
        // Plan paths are relative to the session root, which may sit below the
        // index root: match on a whole-component suffix to stay conservative.
        let suffix_of = |rel: &RelPath, list: &[Vec<u8>]| {
            let b = rel.as_bytes();
            list.iter().any(|p| {
                !p.is_empty()
                    && (b == &p[..]
                        || (b.len() > p.len()
                            && b.ends_with(p)
                            && b[b.len() - p.len() - 1] == b'/'))
            })
        };
        let is_ref = |rel: &RelPath| suffix_of(rel, &referenced);
        let mut report = GcReport::default();
        let mut handled = std::collections::HashSet::new();

        // Stale records, with their part files. The index lock is held, so a
        // live receiver cannot refresh a record between check and removal.
        for dest in ix.partials_older_than(older_than).map_err(io_err)? {
            if skipped(&dest) {
                continue;
            }
            if is_ref(&dest) {
                report.parts_kept += 1;
                continue;
            }
            let Some(part) = part_of(&dest) else { continue };
            handled.insert(part.clone());
            match remove_part(&root, &part, cutoff, dry_run) {
                Ok(Removal::Removed) => report.parts_removed.push(part),
                Ok(Removal::Missing) => report.rows_without_file += 1,
                Ok(Removal::Skipped) => {
                    report.parts_kept += 1;
                    continue;
                }
                Err(e) => {
                    tracing::warn!(path = %part.to_os().to_string_lossy(), "gc: {e}");
                    report.errors.push((part, e.to_string()));
                    continue;
                }
            }
            if !dry_run {
                ix.remove_partial(&dest).map_err(io_err)?;
            }
        }
        // Orphans: stale files with no record. A part with a record of any
        // age is not an orphan (the stale ones were handled above).
        let recorded: std::collections::HashSet<RelPath> = ix
            .partial_paths()
            .map_err(io_err)?
            .iter()
            .filter_map(part_of)
            .collect();
        for part in on_disk {
            if handled.contains(&part) || recorded.contains(&part) {
                continue;
            }
            if is_ref(&part) || suffix_of(&part, &referenced_parts) {
                report.parts_kept += 1;
                continue;
            }
            match remove_part(&root, &part, cutoff, dry_run) {
                Ok(Removal::Removed) => report.parts_removed.push(part),
                Ok(Removal::Missing | Removal::Skipped) => {}
                Err(e) => {
                    tracing::warn!(path = %part.to_os().to_string_lossy(), "gc: {e}");
                    report.errors.push((part, e.to_string()));
                }
            }
        }
        report.parts_removed.sort();
        report.sessions_removed = if dry_run {
            ix.count_sessions_older_than(older_than).map_err(io_err)?
        } else {
            ix.delete_sessions_older_than(older_than)
                .map_err(io_err)?
                .len() as u32
        };
        report.entries_removed =
            prune_entries(&mut ix, &root, &seen, &walk_errors, &skipped, dry_run)?;
        report.errors.extend(walk_errors);
        if !dry_run {
            ix.set_last_gc_at(now_secs()).map_err(io_err)?;
        }
        Ok(report)
    })
    .await
    .map_err(io_err)?
}

/// Removes the index entries of regular files that are gone or were
/// replaced (another inode or size), unless `dry_run`; returns how many.
/// `seen` is what the walk found. A path under one of the walk's `errors`
/// may only be unreadable, so it stays; one the walk missed or saw
/// differently may have been placed since, so it is looked at once more
/// (the index lock is held: a receiver cannot index it meanwhile). A walk
/// that found no file at all prunes nothing: the root may be a mount point
/// whose filesystem is not mounted.
fn prune_entries(
    ix: &mut crate::index::Index,
    root: &Path,
    seen: &std::collections::HashMap<RelPath, (u64, u64)>,
    errors: &[(RelPath, String)],
    skipped: &dyn Fn(&RelPath) -> bool,
    dry_run: bool,
) -> io::Result<usize> {
    if seen.is_empty() {
        return Ok(0);
    }
    let regular = irsync_proto::v1::file_entry::Kind::Regular as i32;
    let unreadable = |p: &RelPath| {
        errors.iter().any(|(e, _)| {
            let (e, p) = (e.as_bytes(), p.as_bytes());
            e.is_empty() || p == e || (p.starts_with(e) && p.get(e.len()) == Some(&b'/'))
        })
    };
    let mut removed = 0;
    for (path, kind, inode, size) in ix.entry_identities().map_err(io_err)? {
        if kind != regular
            || seen.get(&path) == Some(&(inode, size))
            || unreadable(&path)
            || skipped(&path)
        {
            continue;
        }
        let here = crate::platform::symlink_metadata(&root.join(path.to_os()))
            .ok()
            .filter(|m| m.kind == crate::platform::Kind::File)
            .map(|m| (m.ino, m.size));
        if here == Some((inode, size)) {
            continue;
        }
        removed += 1;
        if !dry_run {
            ix.remove_entry(&path).map_err(io_err)?;
        }
    }
    Ok(removed)
}

/// A full GC walk is due at most this often, unless stale partials wait.
pub const GC_EVERY: Duration = Duration::from_secs(24 * 3600);

/// Whether a GC walk is due: never walked, the last walk is [`GC_EVERY`]
/// old (or in the future: the clock stepped back), or partial records have
/// gone stale since (Ruling 32).
pub fn gc_due(last_gc_at: Option<i64>, now: i64, stale_partials: bool) -> bool {
    stale_partials || last_gc_at.is_none_or(|t| now < t || now - t >= GC_EVERY.as_secs() as i64)
}

/// Runs [`gc`] only when [`gc_due`] says so; `stale_after` is the age at
/// which a partial record counts as stale. `None` when skipped.
pub async fn gc_if_due(
    index: IndexHandle,
    root: &Path,
    older_than: Duration,
    stale_after: Duration,
) -> io::Result<Option<GcReport>> {
    gc_if_due_skipping(index, root, older_than, stale_after, &[]).await
}

/// [`gc_if_due`] with [`gc_skipping`]'s `skip`.
pub async fn gc_if_due_skipping(
    index: IndexHandle,
    root: &Path,
    older_than: Duration,
    stale_after: Duration,
    skip: &[RelPath],
) -> io::Result<Option<GcReport>> {
    let ix = index.clone();
    let due = tokio::task::spawn_blocking(move || -> io::Result<bool> {
        let ix = ix.lock().map_err(|_| io_err("index lock poisoned"))?;
        let (last, now) = (ix.last_gc_at().map_err(io_err)?, now_secs());
        // Records that went stale since the last walk: one stale at that
        // walk already was looked at, and kept (younger than `older_than`,
        // or still referenced); it must not make every run walk again.
        let stale = match last {
            Some(t) if t <= now => {
                let since = Duration::from_secs((now - t) as u64);
                let stale_now = ix.partials_older_than(stale_after).map_err(io_err)?;
                let stale_then = ix
                    .partials_older_than(stale_after + since)
                    .map_err(io_err)?;
                stale_now.len() > stale_then.len()
            }
            // Due anyway.
            _ => false,
        };
        Ok(gc_due(last, now, stale))
    })
    .await
    .map_err(io_err)??;
    if !due {
        return Ok(None);
    }
    gc_skipping(index, root, older_than, false, skip)
        .await
        .map(Some)
}
