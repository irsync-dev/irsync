//! `check`: walk the tree and report how it differs from the manifest.

use std::collections::BTreeSet;
use std::io::{ErrorKind, Write};
use std::ops::Bound;

use anyhow::Context;

use super::{
    cannot_open, canonical_root, store_parent, usable_here, walk_config, CommonOpts, Outcome,
    Report, Warnings,
};
use crate::compare::{diff, Field, Selection, Tier};
use crate::escape::escape;
use crate::model::{Flags, Kind};
use crate::pipeline::{self, Decision};
use crate::platform::{self, Platform};
use crate::progress::Progress;
use crate::report::{line_add, line_chg, line_del, line_entry, line_skp, CheckSummary, FieldDiff};
use crate::store::{self, OpenMode};
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
    let store = store::open(&loc.path, loc.backend, OpenMode::ReadOnly)
        .with_context(|| cannot_open(&loc))?;
    usable_here(store.meta()?.platform, Platform::CURRENT)?;
    let sel = Selection::new(opts.tier, &opts.ignore, &opts.require);

    let mut cfg = walk_config(common, &root, &loc);
    // The walk threads look every entry up themselves, in batches: a lookup of its own
    // for each entry costs three times as much on a manifest opened read-only. With a
    // reader the walk would take xattrs the manifest holds as current from the manifest;
    // a check reads them from the tree.
    cfg.force_xattr_reads = true;
    let reader = store.reader()?;
    let store_parent = store_parent(&cfg, &root);

    let mut warnings = Warnings::new(err);
    let mut report = Report::new(out, common.verbose);
    let mut summary = CheckSummary::default();
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

    let decide = |obs: &mut Observed| hash_or_skip(obs, &sel);
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
                report.verbose_line(|| line_skp(&path))?;
                excluded_below.insert(unobserved_prefix(&path));
                excluded.insert(path);
                return Ok(());
            }
            WalkEvent::Entry(obs) => obs,
        };
        if let Some(e) = &obs.lookup_error {
            anyhow::bail!("cannot look up {} in the manifest: {e}", escape(&obs.path));
        }
        if obs.entry.blake3.is_some() {
            progress.hashed(obs.entry.size, obs.hash_cpu);
        }
        progress.entry();
        let Some(stored) = &obs.stored else {
            report.line(&line_add(&obs.path, obs.entry.kind))?;
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
        let diffs = diff(mtime_exempt, stored, &obs.entry, &sel);
        if diffs.is_empty() {
            report.verbose_line(|| line_entry("EQL", &obs.path, &obs.entry))?;
            summary.unchanged += 1;
        } else {
            report.line(&line_chg(&obs.path, &diffs))?;
            summary.changed += 1;
        }
        Ok(())
    };
    pipeline::run(
        common.jobs,
        move |emit| walk::walk(&cfg, Some(&*reader), emit),
        decide,
        sink,
    )?;

    // Second pass: stored paths that are gone from the tree, or that the walk could not
    // observe. Stored content check cannot see is a difference, not just a warning.
    store.for_each(&mut |path, stored| {
        if path == b"." || excluded.contains(path) || is_under(&excluded_below, path) {
            return Ok(());
        }
        let unverified = || line_chg(path, &[unverified_diff(stored.kind)]);
        if is_under(&not_descended, path) {
            report.line(&unverified())?;
            summary.changed += 1;
            return Ok(());
        }
        if is_under(&replaced, path) {
            report.line(&line_del(path))?;
            summary.deleted += 1;
            return Ok(());
        }
        // A stored path this system cannot name is an error, never a deleted file.
        let Ok(abs) = platform::rel_to_path(&root, path) else {
            anyhow::bail!(
                "manifest path {} is not a valid name on this platform",
                escape(path)
            );
        };
        // The walk's own stat, so that the two agree on what can be seen: on Windows,
        // std's falls back to the directory listing where the walk's is refused.
        match platform::lstat(&abs) {
            Ok(_) => {}
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                report.line(&line_del(path))?;
                summary.deleted += 1;
            }
            Err(_) => {
                report.line(&unverified())?;
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
/// An entry the manifest could not be asked about is not hashed: the sink stops the run
/// when its turn comes.
fn hash_or_skip(obs: &Observed, sel: &Selection) -> Decision {
    let e = &obs.entry;
    if e.kind != Kind::File || e.flags.contains(Flags::STORE) || !sel.contains(Field::Blake3) {
        return Decision::Skip;
    }
    match obs.stored {
        Some(_) => Decision::Hash,
        None => Decision::Skip,
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
