//! `init` and `update`: walk the tree, hash what changed, record it, sweep what
//! disappeared. The two verbs share this engine and differ only in whether the manifest
//! must already exist.

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

/// Which verb is running. `Init` creates the manifest and refuses an existing one;
/// `Update` requires an existing manifest and refuses to create one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Init { backend: Option<Backend> },
    Update { rehash: bool },
}

pub struct UpdateOpts {
    pub common: CommonOpts,
    pub mode: Mode,
}

pub fn update(
    opts: &UpdateOpts,
    _out: &mut dyn Write,
    err: &mut dyn Write,
    progress: &dyn Progress,
) -> anyhow::Result<Outcome> {
    let common = &opts.common;
    let root = canonical_root(&common.root)?;
    let (requested, rehash) = match opts.mode {
        Mode::Init { backend } => (backend, false),
        Mode::Update { rehash } => (None, rehash),
    };
    let loc = store::locate(&root, common.manifest.as_deref(), requested, true)?;
    let mode = match (opts.mode, loc.exists) {
        (Mode::Init { .. }, true) => anyhow::bail!(
            "manifest already exists at {}; run 'fileset update' to bring it up to date",
            loc.path.display()
        ),
        (Mode::Update { .. }, false) => match common.manifest.as_deref() {
            Some(m) => anyhow::bail!(
                "no manifest at {}; run 'fileset init' to create one",
                m.display()
            ),
            None => anyhow::bail!(
                "no manifest found in {}: neither {} nor {} exists; run 'fileset init' to \
                 create one",
                root.display(),
                store::SQLITE_NAME,
                store::FJALL_NAME
            ),
        },
        (_, true) => OpenMode::ReadWrite,
        (_, false) => OpenMode::Create,
    };
    let store = RefCell::new(store::open(&loc.path, loc.backend, mode)?);
    let old_meta = store.borrow().meta()?;
    let generation = old_meta.generation + 1;
    // Claim the generation before writing any entry with it, so entries committed by a
    // run that dies midway carry a generation the next run does not reuse and sweeps.
    store.borrow_mut().set_meta(&Meta {
        generation,
        ..old_meta.clone()
    })?;

    let store_path = store::store_path_inside(&root, &loc);
    let cfg = WalkConfig {
        root: root.clone(),
        cross_mount_points: common.cross_mount_points,
        sort: common.sort,
        store_path,
        sqlite_store: loc.backend == Backend::Sqlite,
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
        let decision = reuse_or_hash(&**store.borrow(), obs, rehash, &mut lookup_error);
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
            WalkEvent::NestedManifest(path) => {
                let text = walk::nested_manifest_warning(&path);
                return match opts.mode {
                    // Nested filesets are unsupported, and a tree is only ever adopted here.
                    Mode::Init { .. } => Err(anyhow::anyhow!(
                        "refusing to init: {text}; nested filesets are not supported"
                    )),
                    Mode::Update { .. } => Ok(warnings.emit(&text)?),
                };
            }
            // Already warned; the sweep drops what the walk did not see, as for any absence.
            WalkEvent::NotDescended(_) => return Ok(()),
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
    let run = pipeline::run(
        common.jobs,
        move |emit| walk::walk(&cfg, emit),
        decide,
        sink,
    );
    if let Err(e) = run.and_then(|()| lookup_error.map_or(Ok(()), Err)) {
        if mode == OpenMode::Create {
            // Nothing in a half-built manifest is worth keeping, and leaving it would make
            // the next `init` refuse with "already exists".
            drop(store);
            if let Err(rm) = remove_created(&loc) {
                let _ = warnings.emit(&format!(
                    "cannot remove the unfinished manifest at {}: {rm}",
                    loc.path.display()
                ));
            }
        }
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

/// Deletes the manifest this run created, with any SQLite sidecars.
fn remove_created(loc: &store::Location) -> std::io::Result<()> {
    match loc.backend {
        Backend::Sqlite => {
            for suffix in ["-wal", "-shm", "-journal"] {
                let mut sidecar = loc.path.as_os_str().to_owned();
                sidecar.push(suffix);
                match std::fs::remove_file(&sidecar) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                    _ => {}
                }
            }
            std::fs::remove_file(&loc.path)
        }
        Backend::Fjall => std::fs::remove_dir_all(&loc.path),
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
