//! `check`: walk the tree and report how it differs from the manifest.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io::{ErrorKind, Write};
use std::ops::Bound;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use anyhow::Context;

use super::{canonical_root, walk_config, CommonOpts, Outcome, Warnings};
use crate::compare::{diff, Field, Selection, Tier};
use crate::escape::escape;
use crate::model::{Flags, Kind};
use crate::pipeline::{self, Decision};
use crate::progress::Progress;
use crate::report::{line_add, line_chg, line_del, CheckSummary, FieldDiff};
use crate::store::{self, OpenMode, Store};
use crate::walk::{self, Observed, WalkEvent};

pub struct CheckOpts {
    pub common: CommonOpts,
    pub tier: Tier,
    pub ignore: Vec<Field>,
    pub require: Vec<Field>,
}

pub fn check(
    opts: &CheckOpts,
    out: &mut dyn Write,
    err: &mut dyn Write,
    progress: &dyn Progress,
) -> anyhow::Result<Outcome> {
    let common = &opts.common;
    let root = canonical_root(&common.root)?;
    let loc = store::locate(&root, common.manifest.as_deref(), None, false)?;
    let store = store::open(&loc.path, loc.backend, OpenMode::ReadOnly).with_context(|| {
        format!(
            "cannot open manifest at {}; if it is on read-only media, copy it elsewhere \
             and pass --manifest",
            loc.path.display()
        )
    })?;
    let store = RefCell::new(store);
    let sel = Selection::new(opts.tier, &opts.ignore, &opts.require);

    let cfg = walk_config(common, &root, &loc);
    // The root's mtime is not compared, nor that of the directory holding an in-tree store:
    // writing the manifest (and its sidecars) changes both.
    let store_parent: Option<Vec<u8>> = cfg
        .store_path
        .as_deref()
        .and_then(Path::parent)
        .and_then(|p| p.strip_prefix(&root).ok())
        .map(|rel| match rel.as_os_str().as_bytes() {
            b"" => b".".to_vec(),
            r => r.to_vec(),
        });

    let mut warnings = Warnings::new(err, common.verbose);
    let mut summary = CheckSummary::default();
    let mut lookup_error: Option<anyhow::Error> = None;
    // Prefixes (see `unobserved_prefix`) of what the walk could not observe; one per
    // directory not descended or object not stat'ed, never one per tree entry.
    let mut not_descended: BTreeSet<Vec<u8>> = BTreeSet::new();
    // Prefixes of stored directories observed as something else (a symlink, a file): the
    // walk does not look through them, and nothing stored below them is there any more.
    let mut replaced: BTreeSet<Vec<u8>> = BTreeSet::new();
    // Built-in excludes the walk skipped, and their prefixes. An older manifest may hold
    // them; they are not differences.
    let mut excluded: BTreeSet<Vec<u8>> = BTreeSet::new();
    let mut excluded_below: BTreeSet<Vec<u8>> = BTreeSet::new();

    let decide = |obs: &mut Observed| hash_or_skip(&**store.borrow(), obs, &sel, &mut lookup_error);
    let sink = |ev: WalkEvent| -> anyhow::Result<()> {
        let obs = match ev {
            WalkEvent::Warning(text) => return Ok(warnings.emit(&text)?),
            WalkEvent::NestedManifest(path) => {
                return Ok(warnings.emit(&walk::nested_manifest_warning(&path))?)
            }
            WalkEvent::NotDescended(path) => {
                not_descended.insert(unobserved_prefix(&path));
                return Ok(());
            }
            WalkEvent::Excluded(path) => {
                warnings.info(&walk::excluded_info(&path))?;
                excluded_below.insert(unobserved_prefix(&path));
                excluded.insert(path);
                return Ok(());
            }
            WalkEvent::Entry(obs) => obs,
        };
        if obs.entry.blake3.is_some() {
            progress.hashed_bytes(obs.entry.size);
        }
        progress.entry();
        let Some(stored) = store.borrow().get(&obs.path)? else {
            writeln!(out, "{}", line_add(&obs.path, obs.entry.kind))?;
            summary.added += 1;
            return Ok(());
        };
        if sel.contains(Field::Blake3)
            && stored.kind == Kind::File
            && obs.entry.kind == Kind::File
            && stored.blake3.is_none()
            && !stored.flags.contains(Flags::STORE)
        {
            warnings.emit(&format!("no stored checksum for {}", escape(&obs.path)))?;
        }
        if stored.kind == Kind::Dir && obs.entry.kind != Kind::Dir {
            replaced.insert(unobserved_prefix(&obs.path));
        }
        let mtime_exempt = obs.path == b"." || store_parent.as_deref() == Some(&obs.path[..]);
        let diffs = diff(mtime_exempt, &stored, &obs.entry, &sel);
        if diffs.is_empty() {
            summary.unchanged += 1;
        } else {
            writeln!(out, "{}", line_chg(&obs.path, &diffs))?;
            summary.changed += 1;
        }
        Ok(())
    };
    pipeline::run(
        common.jobs,
        move |emit| walk::walk(&cfg, None, emit),
        decide,
        sink,
    )?;
    if let Some(e) = lookup_error {
        return Err(e);
    }

    // Second pass: stored paths that are gone from the tree, or that the walk could not
    // observe. Stored content check cannot see is a difference, not just a warning.
    store.borrow().for_each(&mut |path, stored| {
        if path == b"." || excluded.contains(path) || is_under(&excluded_below, path) {
            return Ok(());
        }
        let unverified = || line_chg(path, &[unverified_diff(stored.kind)]);
        if is_under(&not_descended, path) {
            writeln!(out, "{}", unverified())?;
            summary.changed += 1;
            return Ok(());
        }
        if is_under(&replaced, path) {
            writeln!(out, "{}", line_del(path))?;
            summary.deleted += 1;
            return Ok(());
        }
        match root.join(OsStr::from_bytes(path)).symlink_metadata() {
            Ok(_) => {}
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                writeln!(out, "{}", line_del(path))?;
                summary.deleted += 1;
            }
            Err(_) => {
                writeln!(out, "{}", unverified())?;
                summary.changed += 1;
            }
        }
        Ok(())
    })?;
    progress.finish();

    summary.warnings = warnings.count();
    writeln!(err, "{summary}")?;
    let differs = summary.added + summary.deleted + summary.changed > 0;
    Ok(
        if differs || (common.fail_on_warnings && summary.warnings > 0) {
            Outcome::Differences
        } else {
            Outcome::Clean
        },
    )
}

/// The key prefix of everything stored strictly below `path`: `path/`, or empty for the root.
fn unobserved_prefix(path: &[u8]) -> Vec<u8> {
    if path == b"." {
        return Vec::new();
    }
    let mut p = path.to_vec();
    p.push(b'/');
    p
}

/// Whether `path` lies strictly below one of `prefixes`. Prefixes never nest (the walk does
/// not descend below one), so the greatest prefix not after `path` is the only candidate.
fn is_under(prefixes: &BTreeSet<Vec<u8>>, path: &[u8]) -> bool {
    prefixes
        .range::<[u8], _>((Bound::Unbounded, Bound::Included(path)))
        .next_back()
        .is_some_and(|p| path.starts_with(p))
}

fn unverified_diff(kind: Kind) -> FieldDiff {
    FieldDiff {
        name: "kind".into(),
        old: kind.name().into(),
        new: "unverified".into(),
    }
}

/// Hashes a regular file only when there is a stored entry to compare its checksum with.
/// The first store lookup failure is kept in `lookup_error` and the file is skipped.
fn hash_or_skip(
    store: &dyn Store,
    obs: &Observed,
    sel: &Selection,
    lookup_error: &mut Option<anyhow::Error>,
) -> Decision {
    let e = &obs.entry;
    if e.kind != Kind::File || e.flags.contains(Flags::STORE) || !sel.contains(Field::Blake3) {
        return Decision::Skip;
    }
    match store.get(&obs.path) {
        Ok(Some(_)) => Decision::Hash,
        Ok(None) => Decision::Skip,
        Err(err) => {
            lookup_error.get_or_insert(err);
            Decision::Skip
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_under_matches_only_strict_descendants() {
        let set: BTreeSet<Vec<u8>> = [b"d".as_slice(), b"x/y"]
            .into_iter()
            .map(unobserved_prefix)
            .collect();
        assert!(is_under(&set, b"d/f"));
        assert!(is_under(&set, b"d/e/f"));
        assert!(is_under(&set, b"x/y/z"));
        assert!(!is_under(&set, b"d"));
        assert!(!is_under(&set, b"d-x"));
        assert!(!is_under(&set, b"da/f"));
        assert!(!is_under(&set, b"x/yz"));
        assert!(!is_under(&set, b"c"));
        let root: BTreeSet<Vec<u8>> = [unobserved_prefix(b".")].into_iter().collect();
        assert!(is_under(&root, b"a"));
        assert!(is_under(&root, b"a/b"));
    }
}
