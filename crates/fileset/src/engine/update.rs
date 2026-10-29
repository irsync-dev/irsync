//! `init` and `update`: walk the tree, hash what changed, record what differs from the
//! manifest, sweep what disappeared. The two verbs share this engine and differ only in whether the manifest
//! must already exist.

use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use super::{canonical_root, walk_config, CommonOpts, Outcome, Warnings};
use crate::escape::escape;
use crate::model::{Entry, Flags, Kind};
use crate::pipeline::{self, Decision};
use crate::progress::Progress;
use crate::report::UpdateSummary;
use crate::store::{self, Backend, Meta, OpenMode};
use crate::walk::{self, Observed, WalkEvent};

/// Entries buffered before each `put_batch`.
const BATCH_SIZE: usize = 2000;

/// Which verb is running. `Init` creates the manifest and refuses an existing one;
/// `Update` requires an existing manifest and refuses to create one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Init {
        backend: Option<Backend>,
    },
    Update {
        rehash: bool,
        /// Read xattrs even where the manifest's are known to be current.
        force_xattr_reads: bool,
    },
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
    let (requested, rehash, force_xattr_reads) = match opts.mode {
        Mode::Init { backend } => (backend, false, false),
        Mode::Update {
            rehash,
            force_xattr_reads,
        } => (None, rehash, force_xattr_reads),
    };
    let loc = store::locate(&root, common.manifest.as_deref(), requested, true)?;
    let mut warnings = Warnings::new(err, common.verbose);
    let mode = match (opts.mode, loc.exists) {
        (Mode::Init { .. }, true) if unfinished_manifest(&loc) => {
            warnings.emit(&format!(
                "replacing the unfinished manifest an interrupted init left at {}",
                loc.path.display()
            ))?;
            remove_created(&loc).map_err(|e| {
                anyhow::anyhow!(
                    "cannot remove the unfinished manifest at {}: {e}",
                    loc.path.display()
                )
            })?;
            OpenMode::Create
        }
        (Mode::Init { .. }, true) => anyhow::bail!(
            "manifest already exists at {}; run 'fileset update' to bring it up to date",
            loc.path.display()
        ),
        (Mode::Update { .. }, true) if unfinished_manifest(&loc) => anyhow::bail!(
            "the manifest at {} was left by an interrupted init and holds nothing yet; run \
             'fileset init' to create it again",
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
    if matches!(opts.mode, Mode::Init { .. }) {
        if let Some(m) = store::enclosing_manifest(&root) {
            anyhow::bail!(
                "refusing to init: {} is inside the fileset with manifest {}; nested filesets \
                 are not supported",
                root.display(),
                m.display()
            );
        }
    }
    let store = RefCell::new(store::open(&loc.path, loc.backend, mode)?);
    let old_meta = store.borrow().meta()?;
    let generation = old_meta.generation + 1;
    // Claim the generation before writing any entry with it, so no two runs, finished or
    // not, write entries under the same number.
    store.borrow_mut().set_meta(&Meta {
        generation,
        ..old_meta.clone()
    })?;

    let mut cfg = walk_config(common, &root, &loc);
    cfg.force_xattr_reads = force_xattr_reads;
    // The walk threads look every entry up themselves; a new manifest has nothing to find.
    let reader = match opts.mode {
        Mode::Init { .. } => None,
        Mode::Update { .. } => Some(store.borrow().reader()?),
    };

    let mut summary = UpdateSummary {
        entries: 0,
        hashed: 0,
        reused: 0,
        warnings: 0,
    };
    // Per entry, in walk order: whether `decide` sent it to be hashed, and what the
    // manifest held for it.
    let looked_up: RefCell<VecDeque<(bool, Option<Entry>)>> = RefCell::new(VecDeque::new());
    let mut batch: Vec<(Vec<u8>, Entry)> = Vec::with_capacity(BATCH_SIZE);
    let mut seen = Seen::default();

    let decide = |obs: &mut Observed| {
        // Not worth hashing: the sink stops the run when this entry's turn comes.
        if obs.lookup_error.is_some() {
            looked_up.borrow_mut().push_back((false, None));
            return Decision::Skip;
        }
        let stored = obs.stored.take();
        let decision = reuse_or_hash(stored.as_ref(), obs, rehash);
        if matches!(decision, Decision::Skip) && obs.entry.blake3.is_some() {
            summary.reused += 1;
        }
        looked_up
            .borrow_mut()
            .push_back((matches!(decision, Decision::Hash), stored));
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
            // The sweep also drops what an older manifest recorded at or below it.
            WalkEvent::Excluded(path) => return Ok(warnings.info(&walk::excluded_info(&path))?),
            WalkEvent::Entry(obs) => obs,
        };
        let (sent, stored) = looked_up
            .borrow_mut()
            .pop_front()
            .expect("one lookup per entry, in walk order");
        // Without the manifest's view every file would look new and be hashed: stop here.
        if let Some(e) = &obs.lookup_error {
            anyhow::bail!("cannot look up {} in the manifest: {e}", escape(&obs.path));
        }
        match &obs.hash_error {
            // Attempted but not hashed: a warning, not a count.
            Some(e) => warnings.emit(&format!("cannot hash {}: {e}", escape(&obs.path)))?,
            None if sent => {
                summary.hashed += 1;
                progress.hashed_bytes(obs.entry.size);
            }
            None => {}
        }
        // A row equal but for its generation stays as it is: rewriting it would only
        // churn the manifest.
        let unchanged = stored.is_some_and(|s| {
            obs.entry.generation = s.generation;
            obs.entry == s
        });
        obs.entry.generation = generation;
        summary.entries += 1;
        progress.entry();
        seen.insert(&obs.path);
        if unchanged {
            return Ok(());
        }
        batch.push((obs.path, obs.entry));
        if batch.len() >= BATCH_SIZE {
            store.borrow_mut().put_batch(&batch)?;
            batch.clear();
        }
        Ok(())
    };
    let run = pipeline::run(
        common.jobs,
        move |emit| walk::walk(&cfg, reader.as_deref(), emit),
        decide,
        sink,
    );
    if let Err(e) = run {
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
    store.sweep(&|path| seen.contains(path))?;
    store.set_meta(&Meta {
        generation,
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        updated_at: unix_now()?,
        unfinished: false,
        ..old_meta
    })?;
    if let Some(note) = store.close()? {
        warnings.emit(&note)?;
    }
    progress.finish();

    summary.warnings = warnings.count();
    writeln!(err, "{summary}")?;
    Ok(if common.fail_on_warnings && summary.warnings > 0 {
        Outcome::Differences
    } else {
        Outcome::Clean
    })
}

/// The paths a walk saw, kept as 128-bit hashes: 16 bytes an entry, whatever the path's
/// length. An unseen path would survive the sweep only by sharing a seen one's hash.
#[derive(Default)]
struct Seen(HashSet<u128>);

impl Seen {
    fn key(path: &[u8]) -> u128 {
        let h = blake3::hash(path);
        u128::from_le_bytes(h.as_bytes()[..16].try_into().expect("16 bytes"))
    }

    fn insert(&mut self, path: &[u8]) {
        self.0.insert(Self::key(path));
    }

    fn contains(&self, path: &[u8]) -> bool {
        self.0.contains(&Self::key(path))
    }
}

/// Reuses the stored hash of an unchanged regular file; hashes other regular files.
fn reuse_or_hash(stored: Option<&Entry>, obs: &mut Observed, rehash: bool) -> Decision {
    let e = &mut obs.entry;
    if e.kind != Kind::File || e.flags.contains(Flags::STORE) {
        return Decision::Skip;
    }
    if rehash {
        return Decision::Hash;
    }
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

/// Whether the manifest at `loc` was left by an init that never finished. A manifest
/// that cannot be opened or read is not one: the caller's own error names it.
fn unfinished_manifest(loc: &store::Location) -> bool {
    store::open(&loc.path, loc.backend, OpenMode::ReadOnly)
        .and_then(|s| {
            let unfinished = s.meta()?.unfinished;
            s.close()?;
            Ok(unfinished)
        })
        .unwrap_or(false)
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

fn unix_now() -> anyhow::Result<i64> {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| anyhow::anyhow!("the system clock is set before 1970"))?;
    Ok(d.as_secs() as i64)
}
