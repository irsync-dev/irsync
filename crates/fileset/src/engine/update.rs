//! `update`: walk the tree, hash what changed, record it, sweep what disappeared.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use super::{canonical_root, CommonOpts, Outcome, Warnings};
use crate::escape::escape;
use crate::model::{Entry, Flags, Kind};
use crate::pipeline::{self, Decision};
use crate::progress::Progress;
use crate::report::UpdateSummary;
use crate::store::{self, Backend, Meta, OpenMode, Store};
use crate::walk::{self, Observed, WalkConfig, WalkEvent};

/// Entries buffered before each `put_batch`.
const BATCH_SIZE: usize = 2000;

pub struct UpdateOpts {
    pub common: CommonOpts,
    pub backend: Option<Backend>,
    pub rehash: bool,
}

pub fn update(
    opts: &UpdateOpts,
    _out: &mut dyn Write,
    err: &mut dyn Write,
    progress: &dyn Progress,
) -> anyhow::Result<Outcome> {
    let common = &opts.common;
    let root = canonical_root(&common.root)?;
    let loc = store::locate(&root, common.manifest.as_deref(), opts.backend, true)?;
    let mode = if loc.exists {
        OpenMode::ReadWrite
    } else {
        OpenMode::Create
    };
    let store = RefCell::new(store::open(&loc.path, loc.backend, mode)?);
    let old_meta = store.borrow().meta()?;
    let generation = old_meta.generation + 1;

    let cfg = WalkConfig {
        root: root.clone(),
        cross_mount_points: common.cross_mount_points,
        sort: common.sort,
        store_path: store::store_path_inside(&root, &loc),
    };

    let mut warnings = Warnings::new(err);
    let mut summary = UpdateSummary {
        entries: 0,
        hashed: 0,
        reused: 0,
        warnings: 0,
    };
    let mut lookup_error: Option<anyhow::Error> = None;
    // One flag per entry, in walk order: whether `decide` sent it to be hashed.
    let sent_to_hash: RefCell<VecDeque<bool>> = RefCell::new(VecDeque::new());
    let mut batch: Vec<(Vec<u8>, Entry)> = Vec::with_capacity(BATCH_SIZE);

    let decide = |obs: &mut Observed| {
        let decision = reuse_or_hash(&**store.borrow(), obs, opts.rehash, &mut lookup_error);
        if matches!(decision, Decision::Skip) && obs.entry.blake3.is_some() {
            summary.reused += 1;
        }
        sent_to_hash
            .borrow_mut()
            .push_back(matches!(decision, Decision::Hash));
        decision
    };
    let sink = |ev: WalkEvent| -> anyhow::Result<()> {
        let mut obs = match ev {
            WalkEvent::Warning(text) => return Ok(warnings.emit(&text)?),
            WalkEvent::Entry(obs) => obs,
        };
        if sent_to_hash.borrow_mut().pop_front().unwrap_or(false) {
            summary.hashed += 1;
            progress.hashed_bytes(obs.entry.size);
        }
        if let Some(e) = &obs.hash_error {
            warnings.emit(&format!("cannot hash {}: {e}", escape(&obs.path)))?;
        }
        obs.entry.generation = generation;
        summary.entries += 1;
        progress.entry();
        batch.push((obs.path, obs.entry));
        if batch.len() >= BATCH_SIZE {
            store.borrow_mut().put_batch(&batch)?;
            batch.clear();
        }
        Ok(())
    };
    pipeline::run(
        common.jobs,
        move |emit| walk::walk(&cfg, emit),
        decide,
        sink,
    )?;
    if let Some(e) = lookup_error {
        return Err(e);
    }

    let mut store = store.into_inner();
    if !batch.is_empty() {
        store.put_batch(&batch)?;
    }
    store.sweep(generation)?;
    store.set_meta(&Meta {
        generation,
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        updated_at: unix_now(),
        ..old_meta
    })?;
    store.close()?;
    progress.finish();

    summary.warnings = warnings.count();
    writeln!(err, "{summary}")?;
    Ok(if common.fail_on_warnings && summary.warnings > 0 {
        Outcome::Differences
    } else {
        Outcome::Clean
    })
}

/// Reuses the stored hash of an unchanged regular file; hashes other regular files.
/// The first store lookup failure is kept in `lookup_error` and the file is hashed.
fn reuse_or_hash(
    store: &dyn Store,
    obs: &mut Observed,
    rehash: bool,
    lookup_error: &mut Option<anyhow::Error>,
) -> Decision {
    let e = &mut obs.entry;
    if e.kind != Kind::File || e.flags.contains(Flags::STORE) {
        return Decision::Skip;
    }
    if rehash {
        return Decision::Hash;
    }
    let stored = match store.get(&obs.path) {
        Ok(s) => s,
        Err(err) => {
            lookup_error.get_or_insert(err);
            return Decision::Hash;
        }
    };
    match stored {
        Some(s)
            if s.kind == Kind::File
                && s.blake3.is_some()
                && s.size == e.size
                && s.mtime == e.mtime
                && s.ino == e.ino =>
        {
            e.blake3 = s.blake3;
            Decision::Skip
        }
        _ => Decision::Hash,
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
