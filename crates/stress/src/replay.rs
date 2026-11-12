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

use crate::judge::{in_flight_content_ok, judge, write_of, InFlight, State, StateEntry, Verdict};
use crate::snapshot::{clone_or_copy_file, copy_tree, walk_meta, EntryKind, Meta};

/// One round, as the harness kept it.
pub struct RoundInput<'a> {
    /// The source's entries at round start (`walk_meta`).
    pub start_meta: &'a BTreeMap<Vec<u8>, Meta>,
    /// The copy of the source taken at round start.
    pub start_copy: &'a Path,
    /// Where the replay runs: a missing or empty directory, outside the
    /// round dir's `replay/`.
    pub work: &'a Path,
    /// filegen's state file as of round start. Its directory is the round
    /// dir: the replay reads this file and writes its own log and state to
    /// `<round dir>/replay/` (emptied first), never next to the live ones.
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
    copy_tree(input.start_copy, input.work)
        .with_context(|| format!("copying the round start to {}", input.work.display()))?;
    let round_dir = match input.state.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let replay_dir = round_dir.join("replay");
    if replay_dir.exists() {
        fs::remove_dir_all(&replay_dir)
            .with_context(|| format!("removing {}", replay_dir.display()))?;
    }
    fs::create_dir_all(&replay_dir)
        .with_context(|| format!("creating {}", replay_dir.display()))?;
    let mut runner = runner_for(input, &replay_dir)?;

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
                touched.extend(below(&self.cur, p).cloned());
            }
        }

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
        // Entries moved with a renamed directory.
        let mut carried: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        if let Some(to) = to.filter(|t| !t.is_empty()) {
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
            let disk = entry_on_disk(ctx.work, p, ctx.is_root, Some(ctx.reroot))?;
            let new = if let Some(post) = post_of.get(p) {
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
                match (&disk, self.cur.get(old)) {
                    (Some(_), Some(prev)) => Some(prev.clone()),
                    _ => {
                        return Err(diverged(format!(
                            "{:?} moved to {:?}, but one of them is missing",
                            show(old),
                            show(p)
                        )))
                    }
                }
            } else {
                // Untouched by the step itself (a link member that stayed
                // behind, or a path under a removed or moved directory).
                disk.and_then(|d| self.cur.get(p).cloned().or(Some(d)))
            };
            if new.as_ref() == self.cur.get(p) {
                continue;
            }
            let content_ok = self.content_ok(ctx, live, p, &new, clone, &pre_group)?;
            changes.push((p.clone(), new, content_ok));
        }
        for (p, new, content_ok) in changes {
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
            self.flights.entry(p.clone()).or_default().push(InFlight {
                step: n,
                op: live.op,
                pre: pre.clone(),
                post: new.clone(),
                content_ok,
            });
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
        live: &StepLine,
        p: &[u8],
        new: &Option<StateEntry>,
        clone: Option<&Path>,
        pre_group: &[Vec<u8>],
    ) -> Result<bool> {
        let Some(d) = ctx.dest.get(p).filter(|d| d.kind == EntryKind::File) else {
            return Ok(false);
        };
        if new.as_ref().is_none_or(|n| n.kind != EntryKind::File) {
            return Ok(false);
        }
        let pre_size = self
            .cur
            .get(p)
            .filter(|e| e.kind == EntryKind::File)
            .map(|e| e.size);
        // Every in-between state of an append holds at least one new byte:
        // the one with none is the complete state before, with its own
        // mtime, and rule 5 judges that.
        if live.op == OpKind::Append && pre_size == Some(d.size) {
            return Ok(false);
        }
        let old = clone.filter(|_| pre_group.iter().any(|m| m == p));
        let w = write_of(live.op, &live.details, pre_size);
        in_flight_content_ok(
            &rel_path(ctx.dest_root, p),
            old,
            &rel_path(ctx.work, p),
            w.as_ref(),
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

    const STATE: &str = "filegen-7.state.yaml";

    /// What the live run saw around one completed step.
    struct Cap {
        /// The whole tree just before the step.
        pre: BTreeMap<Vec<u8>, Meta>,
        /// The step's `path` right after it, if a regular file.
        content: Option<Vec<u8>>,
        /// `path`'s hard-link group right after the step.
        group: Vec<Vec<u8>>,
    }

    /// Runs `filegen <argv>` in-process, step by step, pausing a little
    /// between steps so mtimes differ, and records each completed step.
    fn drive(argv: &[OsString], root: &Path) -> Vec<Cap> {
        let Command::Run(a) = Cli::try_parse_from(argv).unwrap().command;
        let o = opts_from_args(a).unwrap();
        let steps = o.steps;
        let mut r = Runner::new(o).unwrap();
        let mut caps = Vec::new();
        for _ in 0..steps {
            let pre = walk_meta(root).unwrap();
            if let StepResult::Done { .. } = r.step().unwrap() {
                let l = r.last_step().unwrap();
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
        state: PathBuf,
        work: PathBuf,
        args: Vec<OsString>,
        live: Vec<StepLine>,
        caps: Vec<Cap>,
    }

    /// A filled tree (`fill` steps), its round-start copy and state, then
    /// a live run of `steps` steps on it; `sets` apply to both runs.
    fn fixture(fill: u64, steps: u64, sets: &[String]) -> Fixture {
        let t = TempDir::new().unwrap();
        let base = t.path().to_path_buf();
        let src = base.join("src");
        fs::create_dir(&src).unwrap();
        let mut flags = os(&["-q", "--max-total-size", "2M", "--max-file-size", "64K"]);
        for s in sets {
            flags.extend(os(&["--set", s]));
        }
        let mut argv = os(&["filegen", "run", "--seed", "7", "--steps"]);
        argv.push(fill.to_string().into());
        argv.extend(os(&["--out-dir"]));
        argv.push(base.join("fill").into());
        argv.extend(flags.iter().cloned());
        argv.push(src.clone().into());
        drive(&argv, &src);

        let start_meta = walk_meta(&src).unwrap();
        let start_copy = base.join("start");
        copy_tree(&src, &start_copy).unwrap();
        let round = base.join("round");
        fs::create_dir(&round).unwrap();
        fs::copy(base.join("fill").join(STATE), round.join(STATE)).unwrap();
        let live_dir = base.join("live");
        fs::create_dir(&live_dir).unwrap();
        fs::copy(base.join("fill").join(STATE), live_dir.join(STATE)).unwrap();

        let mut args = os(&["filegen", "run", "--resume"]);
        args.push(live_dir.join(STATE).into());
        args.push("--steps".into());
        args.push(steps.to_string().into());
        args.extend(flags);
        args.push(src.clone().into());
        let caps = drive(&args, &src);
        let live: Vec<StepLine> = fs::read_to_string(live_dir.join("filegen-7.log"))
            .unwrap()
            .lines()
            .filter_map(parse_step_line)
            .collect();
        assert_eq!(live.len(), caps.len());
        Fixture {
            work: round.join("work"),
            state: round.join(STATE),
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
        fn input<'a>(&'a self, dest: &'a Path, live: &'a [StepLine]) -> RoundInput<'a> {
            RoundInput {
                start_meta: &self.start_meta,
                start_copy: &self.start_copy,
                work: &self.work,
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

        /// Judges `dest`, replaying into a fresh work dir each time.
        fn judge(&self, dest: &Path) -> RoundJudgement {
            if self.work.exists() {
                fs::remove_dir_all(&self.work).unwrap();
            }
            replay_and_judge(&self.input(dest, &self.live)).unwrap()
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

    #[test]
    fn replay_detects_divergence() {
        let f = fixture(100, 30, &[]);
        let dest = f.dest("dest", &f.src);
        let mut live = f.live.clone();
        live[10].path = b"no-such-path".to_vec();
        let e = replay_and_judge(&f.input(&dest, &live))
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
        // exists at the end, whose times moved with the append.
        let end = walk_meta(&f.src).unwrap();
        let (k, member) = f
            .live
            .iter()
            .enumerate()
            .filter(|(_, l)| l.op == OpKind::Append && l.bytes > 0)
            .find_map(|(k, l)| {
                let post = l.post.unwrap().unwrap();
                let pre = f.caps[k].pre.get(&l.path)?;
                if pre.mtime_ns == post.mtime_ns {
                    return None;
                }
                let m = f.caps[k].group.iter().find(|m| {
                    **m != l.path && end.get(*m).is_some_and(|e| e.kind == EntryKind::File)
                })?;
                Some((k, m.clone()))
            })
            .expect("no append to a linked file");
        let l = &f.live[k];
        let post = l.post.unwrap().unwrap();
        let after = f.caps[k].content.as_ref().unwrap();
        let before = &after[..after.len() - l.bytes as usize];
        let dest = f.dest("dest2", &f.src);
        plant(&dest, &member, before, post.mode, post.mtime_ns);
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
}
