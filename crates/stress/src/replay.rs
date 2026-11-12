//! Replays a round's filegen steps on a copy of the round-start tree, to
//! rebuild every state each source path held, and judges the destination
//! against that history (see [`crate::judge`]).
//!
//! Content (hashes, bytes) comes from the replay tree, as do kinds and
//! symlink targets (an absolute target names the replay's root, so it is
//! moved to the live root). Sizes, modes and mtimes come from the live log's post
//! stats: replay mtimes are replay-time and wrong. Round-start metadata
//! comes from `start_meta`.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use ::filegen::cli::{Cli, Command};
use ::filegen::escape::escape;
use ::filegen::kind::OpKind;
use ::filegen::log::{PostStat, StepLine};
use ::filegen::model::Model;
use ::filegen::pace::Pace;
use ::filegen::run::{opts_from_args, Outcome, Runner, Start, StepResult};
use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;

use crate::judge::{
    in_flight_content_ok, judge, write_of, InFlight, State, StateEntry, Verdict, Write,
};
use crate::snapshot::{clone_or_copy_file, copy_tree, walk_meta, EntryKind, Meta};

/// One round, as the harness kept it.
pub struct RoundInput<'a> {
    /// The source's entries at round start (`walk_meta`).
    pub start_meta: &'a BTreeMap<Vec<u8>, Meta>,
    /// The copy of the source taken at round start.
    pub start_copy: &'a Path,
    /// Where the replay runs: a missing or empty directory; anything else
    /// is an error.
    pub work: &'a Path,
    /// Where the replay's filegen writes its log and state, and keeps its
    /// pre-step clone: a missing or empty directory outside `work`, never
    /// the live run's out dir; anything else is an error.
    pub replay_dir: &'a Path,
    /// filegen's state file as of round start. The replay only reads it.
    pub state: &'a Path,
    /// The live run's argv without `--pace`: `"filegen"`, `"run"`, then the
    /// mutate flags. It is parsed with `opts_from_args`, then the replay
    /// overrides the root (`work`), the start (`Start::Resume(state)`),
    /// the step count (`live_steps.len()`), the out dir, and runs quiet.
    pub filegen_args: &'a [OsString],
    /// The live run's step lines, in order.
    pub live_steps: &'a [StepLine],
    /// The live source after the round; the replay must end equal to it.
    pub live_src: &'a Path,
    /// The destination to judge.
    pub dest: &'a Path,
    /// Owners are recorded and compared only when true.
    pub is_root: bool,
}

#[derive(Debug, Default)]
pub struct RoundJudgement {
    /// Paths judged: the destination's, plus every path the source held
    /// at round start or during it.
    pub judged: u64,
    /// Each path that failed, with the rule it broke.
    pub failures: Vec<(Vec<u8>, String)>,
    /// Paths whose state changed during the round.
    pub touched: BTreeSet<Vec<u8>>,
    /// Touched paths whose destination is held and differs from its
    /// round-start state.
    pub changed_and_held: u64,
}

/// Entries with these names belong to the harness or to irsync, not to
/// the source's history; they are skipped wherever they appear.
fn ignored(rel: &[u8]) -> bool {
    rel.split(|&b| b == b'/').any(|n| {
        n == b".irsync-stress-ping"
            || n.starts_with(b".irsync-clones-")
            || (n.starts_with(b".irsync.") && n.ends_with(b".part"))
    })
}

/// The most retries (steps that found nothing to do) in a row before the
/// replay counts as stuck.
const MAX_RETRIES: u32 = 100_000;

/// Replays the round and judges every destination entry. `Err` is a
/// harness error, including a replay that does not do what the live run
/// did ("replay diverged ..."); a judged failure is in `failures`.
pub fn replay_and_judge(input: &RoundInput) -> Result<RoundJudgement> {
    let replay_dir = input.replay_dir;
    must_be_empty(input.work, "the replay's work dir")?;
    must_be_empty(replay_dir, "the replay dir")?;
    copy_tree(input.start_copy, input.work)
        .with_context(|| format!("copying the round start to {}", input.work.display()))?;
    fs::create_dir_all(replay_dir).with_context(|| format!("creating {}", replay_dir.display()))?;
    let mut runner = runner_for(input, replay_dir)?;

    let canon = |p: &Path| {
        fs::canonicalize(p)
            .map(|c| c.into_os_string().into_encoded_bytes())
            .with_context(|| format!("resolving {}", p.display()))
    };
    let (work_root, live_root) = (canon(input.work)?, canon(input.live_src)?);
    let reroot = Reroot {
        from: &work_root,
        to: &live_root,
    };
    let dest = read_tree(input.dest, input.is_root, None)?;
    let start = start_entries(input)?;
    let mut h = History {
        cur: start.clone(),
        start,
        states: BTreeMap::new(),
        flights: BTreeMap::new(),
    };
    let ctx = StepCtx {
        work: input.work,
        dest_root: input.dest,
        dest: &dest,
        clone: replay_dir.join("pre-step"),
        is_root: input.is_root,
        reroot,
    };
    for (k, live) in input.live_steps.iter().enumerate() {
        h.replay_step(&ctx, &mut runner, live, k as u64 + 1)?;
    }
    let _ = fs::remove_file(&ctx.clone);
    runner
        .finish(Outcome::Ok)
        .map_err(|e| anyhow!("replay: filegen could not finish: {e}"))?;
    same_trees(input.work, input.live_src, reroot)?;
    Ok(h.judge_all(&dest, input.is_root))
}

/// `Err` unless `dir` is missing or an empty directory.
fn must_be_empty(dir: &Path, what: &str) -> Result<()> {
    match fs::read_dir(dir) {
        Ok(mut it) => {
            if it.next().is_some() {
                bail!("{what} {} is not empty", dir.display());
            }
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("reading {what} {}", dir.display())),
    }
}

/// A `Runner` that replays the live run on `work`, writing only to
/// `replay_dir`.
fn runner_for(input: &RoundInput, replay_dir: &Path) -> Result<Runner> {
    let cli = Cli::try_parse_from(input.filegen_args)
        .map_err(|e| anyhow!("parsing the filegen args: {e}"))?;
    let Command::Run(args) = cli.command;
    let mut opts = opts_from_args(args).map_err(|e| anyhow!("{e}"))?;
    opts.root = input.work.to_path_buf();
    opts.start = Start::Resume(input.state.to_path_buf());
    opts.steps = input.live_steps.len() as u64;
    opts.flags.out_dir = Some(replay_dir.to_path_buf());
    opts.quiet = true;
    opts.verbose = false;
    opts.force = false;
    opts.pace = Pace::Fast;
    Runner::new(opts).map_err(|e| anyhow!("replay: starting filegen: {e}"))
}

fn rel_path(root: &Path, rel: &[u8]) -> PathBuf {
    root.join(OsStr::from_bytes(rel))
}

fn hash_file(p: &Path) -> Result<blake3::Hash> {
    let mut f = fs::File::open(p).with_context(|| format!("opening {}", p.display()))?;
    let mut h = blake3::Hasher::new();
    io::copy(&mut f, &mut h).with_context(|| format!("reading {}", p.display()))?;
    Ok(h.finalize())
}

fn kind_of(ft: fs::FileType) -> EntryKind {
    use std::os::unix::fs::FileTypeExt;
    if ft.is_file() {
        EntryKind::File
    } else if ft.is_dir() {
        EntryKind::Dir
    } else if ft.is_symlink() {
        EntryKind::Symlink
    } else if ft.is_fifo() {
        EntryKind::Fifo
    } else {
        EntryKind::Other
    }
}

/// Absolute symlink targets under `from` name `to` instead: filegen
/// writes them as its root joined with a path.
#[derive(Clone, Copy)]
struct Reroot<'a> {
    from: &'a [u8],
    to: &'a [u8],
}

impl Reroot<'_> {
    fn apply(&self, target: Vec<u8>) -> Vec<u8> {
        match target.strip_prefix(self.from) {
            Some(rest) if rest.is_empty() || rest[0] == b'/' => [self.to, rest].concat(),
            _ => target,
        }
    }
}

/// The entry at `root/rel` as the disk has it; `None` if absent. With
/// `reroot`, a symlink's target (and size) is moved to another root.
fn entry_on_disk(
    root: &Path,
    rel: &[u8],
    is_root: bool,
    reroot: Option<Reroot>,
) -> Result<Option<StateEntry>> {
    let p = rel_path(root, rel);
    let m = match fs::symlink_metadata(&p) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("stat {}", p.display())),
    };
    let kind = kind_of(m.file_type());
    let target = match kind {
        EntryKind::Symlink => {
            let t = fs::read_link(&p)
                .with_context(|| format!("readlink {}", p.display()))?
                .into_os_string()
                .into_encoded_bytes();
            Some(match reroot {
                Some(r) => r.apply(t),
                None => t,
            })
        }
        _ => None,
    };
    let size = match (&target, kind) {
        (Some(t), _) => t.len() as u64,
        (None, EntryKind::Dir) => 0,
        (None, _) => m.size(),
    };
    Ok(Some(StateEntry {
        kind,
        size,
        mode: m.mode() & 0o7777,
        mtime_ns: i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec()),
        hash: match kind {
            EntryKind::File => Some(hash_file(&p)?),
            _ => None,
        },
        target,
        owner: is_root.then(|| (m.uid(), m.gid())),
    }))
}

/// Every entry under `root` but the ignored ones.
fn read_tree(
    root: &Path,
    is_root: bool,
    reroot: Option<Reroot>,
) -> Result<BTreeMap<Vec<u8>, StateEntry>> {
    let mut out = BTreeMap::new();
    for rel in walk_meta(root)?.into_keys() {
        if ignored(&rel) {
            continue;
        }
        if let Some(e) = entry_on_disk(root, &rel, is_root, reroot)? {
            out.insert(rel, e);
        }
    }
    Ok(out)
}

/// The round-start states: metadata from `start_meta`, content and owner
/// from the start copy.
fn start_entries(input: &RoundInput) -> Result<BTreeMap<Vec<u8>, StateEntry>> {
    let mut out = BTreeMap::new();
    for (rel, m) in input.start_meta {
        let p = rel_path(input.start_copy, rel);
        let hash = match m.kind {
            EntryKind::File => Some(hash_file(&p)?),
            _ => None,
        };
        let owner = if input.is_root {
            let sm = fs::symlink_metadata(&p).with_context(|| format!("stat {}", p.display()))?;
            Some((sm.uid(), sm.gid()))
        } else {
            None
        };
        out.insert(
            rel.clone(),
            StateEntry {
                kind: m.kind,
                size: m.size,
                mode: m.mode,
                mtime_ns: m.mtime_ns,
                hash,
                target: m.target.clone(),
                owner,
            },
        );
    }
    Ok(out)
}

/// `rel`'s hard-link group in the model (`rel` alone for a non-file);
/// empty if the model has no such path.
fn group(model: &Model, rel: &[u8]) -> Vec<Vec<u8>> {
    match model.resolve(rel) {
        Some(id) if !rel.is_empty() => model
            .linked(id)
            .into_iter()
            .map(|i| model.rel_path(i))
            .collect(),
        _ => Vec::new(),
    }
}

/// Keys of `m` strictly below the directory `dir`.
fn below<'a, V>(m: &'a BTreeMap<Vec<u8>, V>, dir: &[u8]) -> impl Iterator<Item = &'a Vec<u8>> {
    let mut lo = dir.to_vec();
    lo.push(b'/');
    let mut hi = dir.to_vec();
    hi.push(b'/' + 1);
    m.range(lo..hi).map(|(k, _)| k)
}

fn kind_char_ok(kind: EntryKind, c: char) -> bool {
    match kind {
        EntryKind::File => c == 'f',
        EntryKind::Dir => c == 'd',
        EntryKind::Symlink => c == 'l',
        EntryKind::Fifo => c == 'p',
        EntryKind::Other => matches!(c, 'c' | 'b' | 's' | '?'),
    }
}

fn show(rel: &[u8]) -> String {
    escape(rel)
}

fn show_post(p: &Option<PostStat>) -> String {
    match p {
        None => "absent".into(),
        Some(p) => format!("{}:{}:{:o}", p.kind, p.size, p.mode),
    }
}

fn show_entry(e: &Option<StateEntry>) -> String {
    match e {
        None => "absent".into(),
        Some(e) => format!("{:?}:{}:{:o}", e.kind, e.size, e.mode),
    }
}

/// What every step of a replay shares.
struct StepCtx<'a> {
    work: &'a Path,
    dest_root: &'a Path,
    dest: &'a BTreeMap<Vec<u8>, StateEntry>,
    /// Where the pre-step clone is kept.
    clone: PathBuf,
    is_root: bool,
    /// From the replay's root to the live one.
    reroot: Reroot<'a>,
}

/// The source's history so far.
struct History {
    start: BTreeMap<Vec<u8>, StateEntry>,
    /// Each path's state after the last replayed step.
    cur: BTreeMap<Vec<u8>, StateEntry>,
    /// Paths that changed: their round-start state, then each change.
    states: BTreeMap<Vec<u8>, Vec<State>>,
    flights: BTreeMap<Vec<u8>, Vec<InFlight>>,
}

impl History {
    /// Replays live step `n` (1-based within the round) and records what
    /// it changed.
    fn replay_step(
        &mut self,
        ctx: &StepCtx,
        runner: &mut Runner,
        live: &StepLine,
        n: u64,
    ) -> Result<()> {
        let path = live.path.as_slice();
        let to = live.to.as_deref();
        let diverged = |what: String| anyhow!("replay diverged at step {}: {what}", live.step);

        // Before the step: the bytes the file's link group holds now, kept
        // when the destination has a file at one of its paths.
        let pre_group = group(runner.model(), path);
        let path_is_file = fs::symlink_metadata(rel_path(ctx.work, path))
            .map(|m| m.is_file())
            .unwrap_or(false);
        let wanted = pre_group
            .iter()
            .any(|m| ctx.dest.get(m).is_some_and(|d| d.kind == EntryKind::File));
        let clone = if path_is_file && wanted {
            clone_or_copy_file(&rel_path(ctx.work, path), &ctx.clone)?;
            Some(ctx.clone.as_path())
        } else {
            None
        };
        let mut touched: BTreeSet<Vec<u8>> = pre_group.iter().cloned().collect();
        for p in std::iter::once(path).chain(to) {
            if !p.is_empty() {
                touched.insert(p.to_vec());
            }
        }
        let path_was_dir =
            !path.is_empty() && self.cur.get(path).is_some_and(|e| e.kind == EntryKind::Dir);

        // The step.
        let mut retries = 0;
        let step = loop {
            match runner.step() {
                Ok(StepResult::Done { step, .. }) => break step,
                Ok(StepResult::Retry) => {
                    retries += 1;
                    if retries > MAX_RETRIES {
                        return Err(diverged(format!(
                            "{MAX_RETRIES} steps in a row found nothing to do"
                        )));
                    }
                }
                Err(e) => return Err(diverged(format!("filegen failed: {e}"))),
            }
        };
        let rec = runner.last_step().expect("a completed step");
        // Whether a hole was punched or written as zeros depends on the
        // filesystem, and so does `bytes`; the bytes are zeros either way.
        let bytes_differ = rec.bytes != live.bytes && live.op != OpKind::PunchHole;
        if step != live.step
            || rec.kind != live.op
            || rec.path != live.path
            || rec.to != live.to
            || bytes_differ
        {
            let fmt = |step: u64, op: OpKind, p: &[u8], to: Option<&[u8]>, bytes: u64| {
                let to = to.map_or(String::new(), |t| format!(" to={:?}", show(t)));
                format!(
                    "step={step} op={} path={:?}{to} bytes={bytes}",
                    op.name(),
                    show(p)
                )
            };
            return Err(diverged(format!(
                "the replay did {}, the live run {}",
                fmt(step, rec.kind, &rec.path, rec.to.as_deref(), rec.bytes),
                fmt(live.step, live.op, path, to, live.bytes)
            )));
        }

        // After the step: which live stat each touched path takes.
        let mut post_of: BTreeMap<Vec<u8>, Option<Option<PostStat>>> = BTreeMap::new();
        let post_group = group(runner.model(), path);
        for m in &post_group {
            post_of.insert(m.clone(), live.post);
        }
        if let Some(to) = to {
            for m in group(runner.model(), to) {
                post_of.insert(m, live.post_to);
            }
        }
        post_of.insert(path.to_vec(), live.post);
        if let Some(to) = to {
            post_of.insert(to.to_vec(), live.post_to);
        }
        touched.extend(post_of.keys().cloned());
        // A directory removed or moved away takes its entries with it; no
        // other step changes what lies below a directory.
        let dir_left = path_was_dir && !matches!(live.post, Some(Some(ps)) if ps.kind == 'd');
        if dir_left {
            touched.extend(below(&self.cur, path).cloned());
        }
        // Entries moved with a renamed directory.
        let mut carried: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        if let Some(to) = to.filter(|t| !t.is_empty() && dir_left) {
            let dir = rel_path(ctx.work, to);
            if fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
                for rest in walk_meta(&dir)?.into_keys() {
                    let mut new = to.to_vec();
                    new.push(b'/');
                    new.extend_from_slice(&rest);
                    let mut old = path.to_vec();
                    old.push(b'/');
                    old.extend_from_slice(&rest);
                    touched.insert(new.clone());
                    carried.insert(new, old);
                }
            }
        }

        let mut changes = Vec::new();
        for p in &touched {
            let exists = || fs::symlink_metadata(rel_path(ctx.work, p)).is_ok();
            let new = if let Some(post) = post_of.get(p) {
                let disk = entry_on_disk(ctx.work, p, ctx.is_root, Some(ctx.reroot))?;
                match (disk, post) {
                    (_, None) => bail!("live step {} has no post stat (a dry run?)", live.step),
                    (None, Some(None)) => None,
                    (Some(d), Some(Some(ps)))
                        if kind_char_ok(d.kind, ps.kind)
                            && (d.kind == EntryKind::Dir || d.size == ps.size) =>
                    {
                        Some(StateEntry {
                            mode: ps.mode,
                            mtime_ns: ps.mtime_ns,
                            ..d
                        })
                    }
                    (d, Some(ps)) => {
                        return Err(diverged(format!(
                            "{:?} is {} in the replay but {} live",
                            show(p),
                            show_entry(&d),
                            show_post(ps)
                        )))
                    }
                }
            } else if let Some(old) = carried.get(p) {
                // Moved, not changed: the previous entry, hash included.
                match (exists(), self.cur.get(old)) {
                    (true, Some(prev)) => Some(prev.clone()),
                    _ => {
                        return Err(diverged(format!(
                            "{:?} moved to {:?}, but one of them is missing",
                            show(old),
                            show(p)
                        )))
                    }
                }
            } else if !exists() {
                // Below a directory that was removed or moved away.
                None
            } else if let Some(prev) = self.cur.get(p) {
                // A link member the step left alone.
                Some(prev.clone())
            } else {
                return Err(diverged(format!(
                    "{:?} appeared in the replay, but the step does not account for it",
                    show(p)
                )));
            };
            if new.as_ref() == self.cur.get(p) {
                continue;
            }
            // Only a step that writes bytes in sequence has states in
            // between; any other step goes from one complete state to the
            // next.
            let pre_size = self
                .cur
                .get(p)
                .filter(|e| e.kind == EntryKind::File)
                .map(|e| e.size);
            let flight = match write_of(live.op, &live.details, pre_size) {
                Some(w) => Some(self.content_ok(ctx, p, &new, clone, &pre_group, &w)?),
                None => None,
            };
            changes.push((p.clone(), new, flight));
        }
        for (p, new, flight) in changes {
            let pre = self.cur.get(&p).cloned();
            let states = self.states.entry(p.clone()).or_insert_with(|| {
                vec![State {
                    step: 0,
                    entry: self.start.get(&p).cloned(),
                }]
            });
            states.push(State {
                step: n,
                entry: new.clone(),
            });
            if let Some(content_ok) = flight {
                self.flights.entry(p.clone()).or_default().push(InFlight {
                    step: n,
                    op: live.op,
                    pre: pre.clone(),
                    post: new.clone(),
                    content_ok,
                });
            }
            match new {
                Some(e) => self.cur.insert(p, e),
                None => self.cur.remove(&p),
            };
        }
        Ok(())
    }

    /// Whether the destination's bytes at `p` are a state the step passes
    /// through on its way to `new`. Old bytes come from the pre-step clone
    /// for the paths that shared `path`'s inode before the step.
    fn content_ok(
        &self,
        ctx: &StepCtx,
        p: &[u8],
        new: &Option<StateEntry>,
        clone: Option<&Path>,
        pre_group: &[Vec<u8>],
        w: &Write,
    ) -> Result<bool> {
        if !ctx.dest.get(p).is_some_and(|d| d.kind == EntryKind::File) {
            return Ok(false);
        }
        if new.as_ref().is_none_or(|n| n.kind != EntryKind::File) {
            return Ok(false);
        }
        let old = clone.filter(|_| pre_group.iter().any(|m| m == p));
        in_flight_content_ok(
            &rel_path(ctx.dest_root, p),
            old,
            &rel_path(ctx.work, p),
            Some(w),
        )
        .with_context(|| format!("comparing {:?} with the replay", show(p)))
    }

    fn judge_all(self, dest: &BTreeMap<Vec<u8>, StateEntry>, is_root: bool) -> RoundJudgement {
        let paths: BTreeSet<&Vec<u8>> = dest
            .keys()
            .chain(self.start.keys())
            .chain(self.states.keys())
            .filter(|p| !ignored(p))
            .collect();
        let mut j = RoundJudgement {
            touched: self
                .states
                .keys()
                .filter(|p| !ignored(p))
                .cloned()
                .collect(),
            ..RoundJudgement::default()
        };
        for p in paths {
            let start = self.start.get(p);
            let only_start;
            let states = match self.states.get(p) {
                Some(s) => s.as_slice(),
                None => {
                    only_start = [State {
                        step: 0,
                        entry: start.cloned(),
                    }];
                    &only_start[..]
                }
            };
            let flights = self.flights.get(p).map_or(&[][..], Vec::as_slice);
            let d = dest.get(p);
            j.judged += 1;
            match judge(d, states, flights, is_root) {
                Verdict::Held(_) => {
                    if j.touched.contains(p) && !same_as_start(d, start) {
                        j.changed_and_held += 1;
                    }
                }
                Verdict::NotHeld(why) => j.failures.push((p.clone(), why)),
            }
        }
        j
    }
}

/// `d` is the round-start state (directories by kind and mode only).
fn same_as_start(d: Option<&StateEntry>, start: Option<&StateEntry>) -> bool {
    match (d, start) {
        (None, None) => true,
        (Some(d), Some(s)) if d.kind == EntryKind::Dir => s.kind == d.kind && s.mode == d.mode,
        (Some(d), Some(s)) => {
            d.kind == s.kind
                && d.size == s.size
                && d.mode == s.mode
                && d.mtime_ns == s.mtime_ns
                && d.hash == s.hash
                && d.target == s.target
        }
        _ => false,
    }
}

type Shape = (EntryKind, u64, u32, Option<Vec<u8>>, Option<blake3::Hash>);

fn shape(root: &Path, reroot: Option<Reroot>) -> Result<BTreeMap<Vec<u8>, Shape>> {
    Ok(read_tree(root, false, reroot)?
        .into_iter()
        .map(|(k, e)| (k, (e.kind, e.size, e.mode, e.target, e.hash)))
        .collect())
}

/// The replay ended where the live source did: kinds, sizes, modes,
/// symlink targets and content (mtimes cannot match).
fn same_trees(work: &Path, live: &Path, reroot: Reroot) -> Result<()> {
    let (a, b) = (shape(work, Some(reroot))?, shape(live, None)?);
    let first = a
        .keys()
        .chain(b.keys())
        .filter(|k| a.get(*k) != b.get(*k))
        .min();
    if let Some(k) = first {
        bail!("replay diverged from the live source: {:?}", show(k));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::time::Duration;

    use ::filegen::cli::{Cli, Command};
    use ::filegen::kind::OpKind;
    use ::filegen::log::parse_step_line;
    use ::filegen::run::{opts_from_args, Outcome, Runner, StepResult};
    use clap::Parser;
    use nix::fcntl::AT_FDCWD;
    use nix::sys::stat::{utimensat, UtimensatFlags};
    use nix::sys::time::TimeSpec;
    use tempfile::TempDir;

    use super::*;
    use crate::snapshot::{copy_tree, walk_meta, EntryKind};

    /// What the live run saw around one completed step.
    struct Cap {
        /// The whole tree just before the step.
        pre: BTreeMap<Vec<u8>, Meta>,
        /// The step's `path` right after it, if a regular file.
        content: Option<Vec<u8>>,
        /// `path`'s hard-link group right after the step.
        group: Vec<Vec<u8>>,
        /// The step's `path` just before it, if a regular file; recorded
        /// only when `drive` is asked to.
        pre_content: Option<Vec<u8>>,
    }

    /// Runs `filegen <argv>` in-process, step by step, pausing a little
    /// between steps so mtimes differ, and records each completed step.
    /// With `keep_pre`, each step's `pre_content` too (which reads every
    /// file before every step).
    fn drive(argv: &[OsString], root: &Path, keep_pre: bool) -> Vec<Cap> {
        let Command::Run(a) = Cli::try_parse_from(argv).unwrap().command;
        let o = opts_from_args(a).unwrap();
        let steps = o.steps;
        let mut r = Runner::new(o).unwrap();
        let mut caps = Vec::new();
        for _ in 0..steps {
            let pre = walk_meta(root).unwrap();
            let pre_files: BTreeMap<Vec<u8>, Vec<u8>> = if keep_pre {
                pre.iter()
                    .filter(|(_, m)| m.kind == EntryKind::File)
                    .map(|(rel, _)| (rel.clone(), fs::read(root.join(p(rel))).unwrap()))
                    .collect()
            } else {
                BTreeMap::new()
            };
            if let StepResult::Done { .. } = r.step().unwrap() {
                let l = r.last_step().unwrap();
                let pre_content = pre_files.get(&l.path).cloned();
                let p = root.join(OsStr::from_bytes(&l.path));
                let content = fs::symlink_metadata(&p)
                    .ok()
                    .filter(|m| m.is_file())
                    .map(|_| fs::read(&p).unwrap());
                let m = r.model();
                let group = m
                    .resolve(&l.path)
                    .map(|id| m.linked(id).into_iter().map(|i| m.rel_path(i)).collect())
                    .unwrap_or_default();
                caps.push(Cap {
                    pre,
                    content,
                    group,
                    pre_content,
                });
            }
            std::thread::sleep(Duration::from_millis(3));
        }
        r.finish(Outcome::Ok).unwrap();
        caps
    }

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    /// `weights.<every kind>.<column>=<w>`.
    fn every_row(column: &str, w: u32) -> Vec<String> {
        OpKind::ALL
            .iter()
            .map(|k| format!("weights.{}.{column}={w}", k.name()))
            .collect()
    }

    struct Fixture {
        _t: TempDir,
        base: PathBuf,
        src: PathBuf,
        start_copy: PathBuf,
        start_meta: BTreeMap<Vec<u8>, Meta>,
        round: PathBuf,
        runs: Cell<u32>,
        state: PathBuf,
        args: Vec<OsString>,
        live: Vec<StepLine>,
        caps: Vec<Cap>,
    }

    /// A filled tree (`fill` steps), its round-start copy and state, then
    /// a live run of `steps` steps on it; `sets` apply to both runs.
    fn fixture(fill: u64, steps: u64, sets: &[String]) -> Fixture {
        fixture_with(fill, steps, sets, false)
    }

    /// [`fixture`], with each live step's `pre_content` when `keep_pre`.
    fn fixture_with(fill: u64, steps: u64, sets: &[String], keep_pre: bool) -> Fixture {
        fixture_seeded(7, fill, steps, sets, keep_pre)
    }

    /// [`fixture_with`], with filegen's seed.
    fn fixture_seeded(
        seed: u64,
        fill: u64,
        steps: u64,
        sets: &[String],
        keep_pre: bool,
    ) -> Fixture {
        let t = TempDir::new().unwrap();
        let base = t.path().to_path_buf();
        let src = base.join("src");
        fs::create_dir(&src).unwrap();
        let mut flags = os(&["-q", "--max-total-size", "2M", "--max-file-size", "64K"]);
        for s in sets {
            flags.extend(os(&["--set", s]));
        }
        let state = format!("filegen-{seed}.state.yaml");
        let seed_arg = seed.to_string();
        let mut argv = os(&["filegen", "run", "--seed", &seed_arg, "--steps"]);
        argv.push(fill.to_string().into());
        argv.extend(os(&["--out-dir"]));
        argv.push(base.join("fill").into());
        argv.extend(flags.iter().cloned());
        argv.push(src.clone().into());
        drive(&argv, &src, false);

        let start_meta = walk_meta(&src).unwrap();
        let start_copy = base.join("start");
        copy_tree(&src, &start_copy).unwrap();
        let round = base.join("round");
        fs::create_dir(&round).unwrap();
        fs::copy(base.join("fill").join(&state), round.join(&state)).unwrap();
        let live_dir = base.join("live");
        fs::create_dir(&live_dir).unwrap();
        fs::copy(base.join("fill").join(&state), live_dir.join(&state)).unwrap();

        let mut args = os(&["filegen", "run", "--resume"]);
        args.push(live_dir.join(&state).into());
        args.push("--steps".into());
        args.push(steps.to_string().into());
        args.extend(flags);
        args.push(src.clone().into());
        let caps = drive(&args, &src, keep_pre);
        let live: Vec<StepLine> = fs::read_to_string(live_dir.join(format!("filegen-{seed}.log")))
            .unwrap()
            .lines()
            .filter_map(parse_step_line)
            .collect();
        assert_eq!(live.len(), caps.len());
        Fixture {
            round,
            runs: Cell::new(0),
            state: base.join("round").join(&state),
            _t: t,
            base,
            src,
            start_copy,
            start_meta,
            args,
            live,
            caps,
        }
    }

    impl Fixture {
        /// A fresh work dir and replay dir for one replay.
        fn fresh(&self) -> (PathBuf, PathBuf) {
            let n = self.runs.get() + 1;
            self.runs.set(n);
            (
                self.round.join(format!("work-{n}")),
                self.round.join(format!("replay-{n}")),
            )
        }

        fn input<'a>(
            &'a self,
            dest: &'a Path,
            live: &'a [StepLine],
            dirs: &'a (PathBuf, PathBuf),
        ) -> RoundInput<'a> {
            RoundInput {
                start_meta: &self.start_meta,
                start_copy: &self.start_copy,
                work: &dirs.0,
                replay_dir: &dirs.1,
                state: &self.state,
                filegen_args: &self.args,
                live_steps: live,
                live_src: &self.src,
                dest,
                is_root: false,
            }
        }

        /// A destination holding a copy of `from`.
        fn dest(&self, name: &str, from: &Path) -> PathBuf {
            let d = self.base.join(name);
            copy_tree(from, &d).unwrap();
            d
        }

        /// Judges `dest`, replaying into fresh dirs each time.
        fn judge(&self, dest: &Path) -> RoundJudgement {
            let dirs = self.fresh();
            replay_and_judge(&self.input(dest, &self.live, &dirs)).unwrap()
        }
    }

    fn p(rel: &[u8]) -> &Path {
        Path::new(OsStr::from_bytes(rel))
    }

    /// Replaces `rel` in `dest` (breaking any hard link) with `bytes`,
    /// `mode` and `mtime_ns`.
    fn plant(dest: &Path, rel: &[u8], bytes: &[u8], mode: u32, mtime_ns: i128) {
        let f = dest.join(p(rel));
        let _ = fs::remove_file(&f);
        fs::write(&f, bytes).unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(mode)).unwrap();
        let ts = TimeSpec::new(
            (mtime_ns / 1_000_000_000) as i64,
            (mtime_ns % 1_000_000_000) as i64,
        );
        utimensat(AT_FDCWD, &f, &ts, &ts, UtimensatFlags::NoFollowSymlink).unwrap();
    }

    fn assert_clean(j: &RoundJudgement) {
        let shown: Vec<_> = j
            .failures
            .iter()
            .map(|(p, why)| format!("{}: {why}", String::from_utf8_lossy(p)))
            .collect();
        assert!(shown.is_empty(), "{shown:#?}");
    }

    #[test]
    fn replay_accepts_the_end_state() {
        let f = fixture(100, 30, &[]);
        let dest = f.dest("dest", &f.src);
        let j = f.judge(&dest);
        assert_clean(&j);
        assert!(!j.touched.is_empty());
        assert!(j.changed_and_held > 0);
        let end = walk_meta(&f.src).unwrap();
        let all: BTreeSet<&Vec<u8>> = f
            .start_meta
            .keys()
            .chain(end.keys())
            .chain(j.touched.iter())
            .collect();
        assert_eq!(j.judged, all.len() as u64);
    }

    #[test]
    fn replay_accepts_the_start_state() {
        let f = fixture(100, 30, &[]);
        let dest = f.dest("dest", &f.start_copy);
        let j = f.judge(&dest);
        assert_clean(&j);
        assert_eq!(j.changed_and_held, 0);
    }

    #[test]
    fn replay_rejects_a_torn_file() {
        let f = fixture(60, 60, &every_row("rewrite", 30));
        let end = walk_meta(&f.src).unwrap();
        // Two rewrites of one path, the later one's state still a file in
        // the end tree.
        let mut found = None;
        'outer: for (b, lb) in f.live.iter().enumerate() {
            if lb.op != OpKind::Rewrite || lb.bytes < 64 {
                continue;
            }
            if end.get(&lb.path).is_none_or(|m| m.kind != EntryKind::File) {
                continue;
            }
            for (a, la) in f.live[..b].iter().enumerate() {
                if la.op == OpKind::Rewrite && la.path == lb.path && la.bytes >= 64 {
                    let (ca, cb) = (
                        f.caps[a].content.as_ref().unwrap(),
                        f.caps[b].content.as_ref().unwrap(),
                    );
                    let h = cb.len().min(ca.len()) / 2;
                    if ca[..h] != cb[..h] {
                        let mut torn = ca[..h].to_vec();
                        torn.extend_from_slice(&cb[h..]);
                        found = Some((lb.clone(), torn));
                        break 'outer;
                    }
                }
            }
        }
        let (lb, torn) = found.expect("no path rewritten twice");
        let post = lb.post.unwrap().unwrap();
        let dest = f.dest("dest", &f.src);
        plant(&dest, &lb.path, &torn, post.mode, post.mtime_ns);
        let j = f.judge(&dest);
        assert_eq!(j.failures.len(), 1, "{:?}", j.failures);
        assert_eq!(j.failures[0].0, lb.path);
    }

    /// A file caught part way through one real write step: the bytes of
    /// the step's write pattern with the first `k` new bytes written.
    struct MidStep {
        path: Vec<u8>,
        step: u64,
        op: OpKind,
        k: u64,
        bytes: Vec<u8>,
        mode: u32,
        mtime_ns: i128,
    }

    /// `base` with `new[offset, offset + k)` written over it at `offset`
    /// (zeros fill any gap past `base`'s end).
    fn written(base: &[u8], new: &[u8], offset: u64, k: u64) -> Vec<u8> {
        let (o, k) = (offset as usize, k as usize);
        let mut out = base.to_vec();
        if k > 0 && out.len() < o + k {
            out.resize(o + k, 0);
        }
        out[o..o + k].copy_from_slice(&new[o..o + k]);
        out
    }

    /// An mtime in (`pre`, `post`] (at most `post` with no `pre`), near
    /// the middle, that no recorded state has: on a coarse clock another
    /// state of the path can share a tick inside the window, and a file
    /// with that state's size and mtime but other bytes is rule 5's
    /// `content and times disagree` (the documented coarse-timestamp
    /// limit), not a mid-step state. `None` if every candidate is taken.
    fn free_mtime(pre: Option<i128>, post: i128, taken: &BTreeSet<i128>) -> Option<i128> {
        let lo = pre.map_or(i128::MIN, |p| p + 1);
        if lo > post {
            return None;
        }
        let mid = match pre {
            Some(pre) => pre + ((post - pre) / 2).max(1),
            None => post - 1,
        };
        (0..1000i128)
            .flat_map(|d| [mid + d, mid - d])
            .find(|m| (lo..=post).contains(m) && !taken.contains(m))
    }

    /// Every mid-step state of `f`'s live append, rewrite,
    /// overwrite_range and create_file steps on files that are not hard
    /// linked: `k` = 0, len/3 and len-1 new bytes written; with the pre-
    /// and the post-step mode each (only the post one for a create); an
    /// mtime from [`free_mtime`] (a step with no such mtime is skipped, as
    /// is a path whose parent the end tree lacks or which is a directory
    /// there).
    fn mid_step_states(f: &Fixture) -> Vec<MidStep> {
        let end = walk_meta(&f.src).unwrap();
        // Every mtime a state of the round had.
        let taken: BTreeSet<i128> = f
            .start_meta
            .values()
            .map(|m| m.mtime_ns)
            .chain(
                f.live
                    .iter()
                    .flat_map(|l| [l.post, l.post_to])
                    .flatten()
                    .flatten()
                    .map(|ps| ps.mtime_ns),
            )
            .collect();
        let writes = [
            OpKind::Append,
            OpKind::Rewrite,
            OpKind::OverwriteRange,
            OpKind::CreateFile,
        ];
        let mut out = Vec::new();
        for (l, cap) in f.live.iter().zip(&f.caps) {
            if !writes.contains(&l.op) || cap.group.len() > 1 {
                continue;
            }
            let parent = match l.path.iter().rposition(|&b| b == b'/') {
                Some(i) => end.get(&l.path[..i]).map(|m| m.kind),
                None => Some(EntryKind::Dir),
            };
            if parent != Some(EntryKind::Dir)
                || end.get(&l.path).is_some_and(|m| m.kind == EntryKind::Dir)
            {
                continue;
            }
            let post = l.post.unwrap().unwrap();
            let pre = cap.pre.get(&l.path);
            let new = cap.content.as_ref().unwrap();
            let w = write_of(l.op, &l.details, pre.map(|m| m.size)).unwrap();
            let Some(mtime_ns) = free_mtime(pre.map(|m| m.mtime_ns), post.mtime_ns, &taken) else {
                continue;
            };
            let old = cap.pre_content.as_deref().unwrap_or_default();
            let base: &[u8] = if w.truncate_first { &[] } else { old };
            let mut ks = vec![0, w.len / 3, w.len.saturating_sub(1)];
            ks.dedup();
            let modes: Vec<u32> = match pre {
                Some(pre) if pre.mode != post.mode => vec![pre.mode, post.mode],
                _ => vec![post.mode],
            };
            for &k in &ks {
                for &mode in &modes {
                    out.push(MidStep {
                        path: l.path.clone(),
                        step: l.step,
                        op: l.op,
                        k,
                        bytes: written(base, new, w.offset, k),
                        mode,
                        mtime_ns,
                    });
                }
            }
        }
        out
    }

    #[test]
    fn replay_accepts_real_mid_step_states() {
        let mut sets = every_row("append", 30);
        sets.extend(every_row("rewrite", 30));
        sets.extend(every_row("overwrite_range", 30));
        sets.extend(every_row("create_file", 15));
        let f = fixture_with(80, 120, &sets, true);
        let states = mid_step_states(&f);
        for op in [
            OpKind::Append,
            OpKind::Rewrite,
            OpKind::OverwriteRange,
            OpKind::CreateFile,
        ] {
            let n = states.iter().filter(|s| s.op == op).count();
            assert!(n >= 6, "only {n} mid-step states for {}", op.name());
        }
        // Waves: each destination holds at most one planted state per
        // path, every other path at its end state.
        let mut by_path: BTreeMap<&[u8], Vec<&MidStep>> = BTreeMap::new();
        for s in &states {
            by_path.entry(&s.path).or_default().push(s);
        }
        let waves = by_path.values().map(Vec::len).max().unwrap();
        for wave in 0..waves {
            let dest = f.dest(&format!("dest-{wave}"), &f.src);
            let planted: Vec<&MidStep> = by_path
                .values()
                .filter_map(|v| v.get(wave).copied())
                .collect();
            for s in &planted {
                plant(&dest, &s.path, &s.bytes, s.mode, s.mtime_ns);
            }
            let j = f.judge(&dest);
            let shown: Vec<String> = j
                .failures
                .iter()
                .map(|(path, why)| {
                    let s = planted.iter().find(|s| &s.path == path);
                    format!(
                        "{}: {why}; planted: {}",
                        String::from_utf8_lossy(path),
                        s.map_or("nothing".into(), |s| format!(
                            "step {} {} k={} len={} mode={:o} mtime={}",
                            s.step,
                            s.op.name(),
                            s.k,
                            s.bytes.len(),
                            s.mode,
                            s.mtime_ns
                        ))
                    )
                })
                .collect();
            assert!(shown.is_empty(), "wave {wave}: {shown:#?}");
        }
    }

    #[test]
    fn replay_detects_divergence() {
        let f = fixture(100, 30, &[]);
        let dest = f.dest("dest", &f.src);
        let mut live = f.live.clone();
        live[10].path = b"no-such-path".to_vec();
        let dirs = f.fresh();
        let e = replay_and_judge(&f.input(&dest, &live, &dirs))
            .unwrap_err()
            .to_string();
        assert!(e.contains("replay diverged at step"), "{e}");
        assert!(e.contains(&live[10].step.to_string()), "{e}");
    }

    #[test]
    fn replay_tracks_hard_link_groups() {
        let mut sets = every_row("hardlink", 10);
        sets.extend(every_row("append", 10));
        let f = fixture(200, 200, &sets);
        let dest = f.dest("dest", &f.src);
        assert_clean(&f.judge(&dest));

        // An append to a linked file, seen at another member that still
        // exists at the end: its size and times say "after the append",
        // but the appended bytes are not the ones appended.
        let end = walk_meta(&f.src).unwrap();
        let (k, member) = f
            .live
            .iter()
            .enumerate()
            .filter(|(_, l)| l.op == OpKind::Append && l.bytes > 0)
            .find_map(|(k, l)| {
                let m = f.caps[k].group.iter().find(|m| {
                    **m != l.path && end.get(*m).is_some_and(|e| e.kind == EntryKind::File)
                })?;
                Some((k, m.clone()))
            })
            .expect("no append to a linked file");
        let l = &f.live[k];
        let post = l.post.unwrap().unwrap();
        let mut garbled = f.caps[k].content.clone().unwrap();
        let old_len = garbled.len() - l.bytes as usize;
        for b in &mut garbled[old_len..] {
            *b ^= 0xa5;
        }
        let dest = f.dest("dest2", &f.src);
        plant(&dest, &member, &garbled, post.mode, post.mtime_ns);
        let j = f.judge(&dest);
        assert_eq!(j.failures.len(), 1, "{:?}", j.failures);
        assert_eq!(j.failures[0].0, member);
    }

    #[test]
    fn replay_tracks_renamed_directories() {
        let f = fixture(200, 200, &every_row("rename", 15));
        // A directory with entries below it was renamed.
        let moved = f.live.iter().enumerate().any(|(k, l)| {
            let mut below = l.path.clone();
            below.push(b'/');
            l.op == OpKind::Rename
                && f.caps[k].pre.get(&l.path).map(|m| m.kind) == Some(EntryKind::Dir)
                && f.caps[k].pre.keys().any(|key| key.starts_with(&below))
        });
        assert!(moved, "no non-empty directory was renamed");
        let start = f.dest("dest-start", &f.start_copy);
        assert_clean(&f.judge(&start));
        let end = f.dest("dest-end", &f.src);
        assert_clean(&f.judge(&end));
    }

    #[test]
    fn replay_rejects_old_times_after_a_move() {
        let mut sets = every_row("rename", 15);
        sets.extend(every_row("rewrite", 10));
        // A file written, then moved with its directory and left alone:
        // its end path, with its mtime from before the write. Whether a run
        // holds one depends on filegen's random stream, so try a few seeds.
        let mut found = None;
        'seeds: for seed in 7..27 {
            let f = fixture_seeded(seed, 300, 300, &sets, false);
            let end = walk_meta(&f.src).unwrap();
            for (b, lb) in f.live.iter().enumerate() {
                let Some(to) = &lb.to else { continue };
                if lb.op != OpKind::Rename
                    || f.caps[b].pre.get(&lb.path).map(|m| m.kind) != Some(EntryKind::Dir)
                {
                    continue;
                }
                for (a, la) in f.live[..b].iter().enumerate() {
                    let Some(rest) = la.path.strip_prefix(lb.path.as_slice()) else {
                        continue;
                    };
                    let writes = [OpKind::Rewrite, OpKind::Append, OpKind::OverwriteRange];
                    if !writes.contains(&la.op) || !rest.starts_with(b"/") {
                        continue;
                    }
                    let Some(old) = f.caps[a].pre.get(&la.path) else {
                        continue;
                    };
                    let moved = f.caps[b].pre.get(&la.path);
                    let at_end = [to.as_slice(), rest].concat();
                    let Some(e) = end.get(&at_end) else { continue };
                    // Unchanged since the move, and the old mtime predates every
                    // state the end path held.
                    let unchanged = moved.is_some_and(|m| m.mtime_ns == e.mtime_ns);
                    if e.kind == EntryKind::File && unchanged && old.mtime_ns < e.mtime_ns {
                        let (old_mtime, mode) = (old.mtime_ns, e.mode);
                        found = Some((f, at_end, old_mtime, mode));
                        break 'seeds;
                    }
                }
            }
        }
        let (f, at_end, old_mtime, mode) = found.expect("no file written, then moved");
        let dest = f.dest("dest", &f.src);
        let bytes = fs::read(f.src.join(p(&at_end))).unwrap();
        plant(&dest, &at_end, &bytes, mode, old_mtime);
        let j = f.judge(&dest);
        assert_eq!(j.failures.len(), 1, "{:?}", j.failures);
        assert_eq!(j.failures[0].0, at_end);
    }

    #[test]
    fn replay_refuses_dirs_in_use() {
        let f = fixture(50, 10, &[]);
        let dest = f.dest("dest", &f.src);
        let dirs = f.fresh();
        fs::create_dir_all(dirs.0.join("stale")).unwrap();
        let e = replay_and_judge(&f.input(&dest, &f.live, &dirs)).unwrap_err();
        assert!(e.to_string().contains("not empty"), "{e}");
        let dirs = f.fresh();
        fs::create_dir_all(&dirs.1).unwrap();
        fs::write(dirs.1.join("filegen-7.log"), b"live log\n").unwrap();
        let e = replay_and_judge(&f.input(&dest, &f.live, &dirs)).unwrap_err();
        assert!(e.to_string().contains("not empty"), "{e}");
        assert_eq!(
            fs::read(dirs.1.join("filegen-7.log")).unwrap(),
            b"live log\n"
        );
    }
}
