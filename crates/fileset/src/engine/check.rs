//! `check`: walk the tree and report how it differs from the manifest.

use std::cell::RefCell;
use std::ffi::OsStr;
use std::io::{ErrorKind, Write};
use std::os::unix::ffi::OsStrExt;

use super::{canonical_root, CommonOpts, Outcome, Warnings};
use crate::compare::{diff, Field, Selection, Tier};
use crate::escape::escape;
use crate::model::{Flags, Kind};
use crate::pipeline::{self, Decision};
use crate::progress::Progress;
use crate::report::{line_add, line_chg, line_del, CheckSummary};
use crate::store::{self, OpenMode, Store};
use crate::walk::{self, Observed, WalkConfig, WalkEvent};

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
    let store = RefCell::new(store::open(&loc.path, loc.backend, OpenMode::ReadOnly)?);
    let sel = Selection::new(opts.tier, &opts.ignore, &opts.require);

    let cfg = WalkConfig {
        root: root.clone(),
        cross_mount_points: common.cross_mount_points,
        sort: common.sort,
        store_path: store::store_path_inside(&root, &loc),
    };

    let mut warnings = Warnings::new(err);
    let mut summary = CheckSummary::default();
    let mut lookup_error: Option<anyhow::Error> = None;

    let decide = |obs: &mut Observed| {
        hash_or_skip(&**store.borrow(), obs, &sel, &mut lookup_error)
    };
    let sink = |ev: WalkEvent| -> anyhow::Result<()> {
        let obs = match ev {
            WalkEvent::Warning(text) => return Ok(warnings.emit(&text)?),
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
        if stored.kind == Kind::File
            && stored.blake3.is_none()
            && !stored.flags.contains(Flags::STORE)
        {
            warnings.emit(&format!("no stored checksum for {}", escape(&obs.path)))?;
        }
        let diffs = diff(&obs.path, &stored, &obs.entry, &sel);
        if diffs.is_empty() {
            summary.unchanged += 1;
        } else {
            writeln!(out, "{}", line_chg(&obs.path, &diffs))?;
            summary.changed += 1;
        }
        Ok(())
    };
    pipeline::run(common.jobs, move |emit| walk::walk(&cfg, emit), decide, sink)?;
    if let Some(e) = lookup_error {
        return Err(e);
    }

    // Second pass: stored paths that are gone from the tree.
    store.borrow().for_each(&mut |path, _| {
        if path == b"." {
            return Ok(());
        }
        match root.join(OsStr::from_bytes(path)).symlink_metadata() {
            Ok(_) => {}
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                writeln!(out, "{}", line_del(path))?;
                summary.deleted += 1;
            }
            Err(e) => warnings.emit(&format!("cannot stat {}: {e}", escape(path)))?,
        }
        Ok(())
    })?;
    progress.finish();

    summary.warnings = warnings.count();
    writeln!(err, "{summary}")?;
    let differs = summary.added + summary.deleted + summary.changed > 0;
    Ok(if differs || (common.fail_on_warnings && summary.warnings > 0) {
        Outcome::Differences
    } else {
        Outcome::Clean
    })
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
