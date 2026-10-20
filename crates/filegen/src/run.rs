//! The run loop: start or resume, step the chain, checkpoint, finish.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Instant;

use indicatif::{HumanBytes, ProgressBar, ProgressDrawTarget, ProgressStyle};
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;

use crate::chain::{cap_pressure, Chain};
use crate::cli::{self, RunArgs};
use crate::config::{self, Config, ConfigError};
use crate::escape::{escape, unescape};
use crate::kind::OpKind;
use crate::log::{Footer, Header, LogWriter, Stats, Timing};
use crate::model::{Model, NodeId};
use crate::ops::file::create_file;
use crate::ops::{dispatch, Applied, OpCtx, OpError, OpOutcome};
use crate::pick::anchor_of;
use crate::signals;
use crate::state::{self, ChainState, LastRun, State, StateError, TreeInfo, FORMAT_VERSION};
use crate::walk;

/// Where a run starts from.
pub enum Start {
    Seed(u64),
    Resume(PathBuf),
}

/// Everything `run` needs, resolved from the command line.
pub struct RunOpts {
    pub root: PathBuf,
    pub steps: u64,
    pub start: Start,
    pub config_file: Option<PathBuf>,
    pub sets: Vec<(String, String)>,
    pub flags: RunArgs,
    pub dry_run: bool,
    pub force: bool,
    pub quiet: bool,
    pub verbose: bool,
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Failed(String),
    Interrupted(i32),
}

/// Why a run could not start (or finish writing its state).
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("{0}")]
    Usage(String),
    #[error(transparent)]
    Io(io::Error),
    #[error(transparent)]
    Config(ConfigError),
    #[error("state file {}: {source}", path.display())]
    State { path: PathBuf, source: StateError },
}

/// What one call to [`Runner::step`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepResult {
    Done { step: u64, kind: OpKind },
    Retry,
}

/// How many times a step redraws from the same row when the drawn kind
/// has no target, before falling back to `create_file` in the root.
const REDRAWS: usize = 16;

/// The kind a fresh run's chain starts from.
const FIRST_KIND: OpKind = OpKind::Mkdir;

/// A run in progress: the model, the generator, and the output files.
pub struct Runner {
    root: PathBuf,
    cfg: Config,
    chain: Chain,
    model: Model,
    rng: ChaCha8Rng,
    seed: u64,
    last_kind: OpKind,
    anchor: NodeId,
    steps_done: u64,
    start_step: u64,
    dry: bool,
    log: LogWriter,
    log_path: PathBuf,
    state_path: PathBuf,
    stats: Stats,
    started: jiff::Timestamp,
    clock: Instant,
    bytes_written: u64,
    /// Steps of this invocation that found nothing to do.
    skipped: u64,
}

impl Runner {
    /// Resolves the configuration and output files, walks the tree, and
    /// opens the log with this run's header. See the module tests for the
    /// refusals.
    pub fn new(opts: RunOpts) -> Result<Runner, RunError> {
        let root = canonical_root(&opts.root)?;
        let resumed = match &opts.start {
            Start::Seed(_) => None,
            Start::Resume(path) => {
                let s = state::read(path).map_err(|source| RunError::State {
                    path: path.clone(),
                    source,
                })?;
                if s.dry_run != opts.dry_run {
                    return Err(RunError::Usage(if s.dry_run {
                        format!(
                            "{} was written by a dry run; pass --dry-run to continue it",
                            path.display()
                        )
                    } else {
                        format!(
                            "{} was written by a real run; resume it without --dry-run",
                            path.display()
                        )
                    }));
                }
                Some(s)
            }
        };
        // Where a resume writes is decided below from its state file's path,
        // so the saved out_dir and prefix do not carry over; only this
        // invocation's config file, --set or flags can redirect it.
        let base = resumed.as_ref().map(|s| Config {
            out_dir: None,
            prefix: None,
            ..s.config.clone()
        });
        let mut cfg = config::load(opts.config_file.as_deref(), base.as_ref(), &opts.sets)
            .map_err(RunError::Config)?;
        config::apply_flags(&mut cfg, &opts.flags);
        cfg.validate().map_err(RunError::Config)?;
        let chain = Chain::from_weights(&cfg.weights).map_err(RunError::Config)?;

        let seed = match (&opts.start, &resumed) {
            (_, Some(s)) => s.seed,
            (Start::Seed(seed), None) => *seed,
            (Start::Resume(_), None) => unreachable!("a resume reads its state"),
        };
        // A resume without an out dir or prefix given on this invocation
        // keeps appending to the history its state file belongs to,
        // whatever the environment or the current directory says now.
        let resume_file = match &opts.start {
            Start::Resume(path) => Some(path.as_path()),
            Start::Seed(_) => None,
        };
        let out_dir = match resume_file {
            Some(path) if opts.flags.out_dir.is_none() && cfg.out_dir.is_none() => {
                match path.parent() {
                    Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
                    _ => PathBuf::from("."),
                }
            }
            _ => cli::resolve_out_dir(
                opts.flags.out_dir.as_deref(),
                cfg.out_dir.as_deref(),
                &|k| std::env::var_os(k),
            ),
        };
        let out_dir = lenient_canonical(&out_dir).map_err(RunError::Io)?;
        // The state records the directory actually used, so its config
        // means the same from any working directory.
        cfg.out_dir = Some(out_dir.clone());
        if out_dir.starts_with(&root) {
            return Err(RunError::Usage(format!(
                "output directory {} is inside the root {}; pass --out-dir elsewhere",
                out_dir.display(),
                root.display()
            )));
        }
        let path_max = libc::PATH_MAX as usize;
        if root.as_os_str().len() + 1 + cfg.max_path_len as usize >= path_max {
            return Err(RunError::Usage(format!(
                "root {} ({} bytes) plus max_path_len {} reaches PATH_MAX ({path_max}); \
                 lower max_path_len",
                root.display(),
                root.as_os_str().len(),
                cfg.max_path_len
            )));
        }
        let prefix = opts
            .flags
            .prefix
            .clone()
            .or_else(|| cfg.prefix.clone())
            .or_else(|| resume_file.and_then(prefix_of))
            .unwrap_or_else(|| format!("filegen-{seed}"));
        if prefix.is_empty() || prefix == "." || prefix == ".." || prefix.contains('/') {
            return Err(RunError::Usage(format!(
                "invalid prefix {prefix:?}: it names files in the output directory, \
                 so it must not be empty, `.`, `..` or contain `/`"
            )));
        }
        let dry = if opts.dry_run { ".dry" } else { "" };
        let log_path = out_dir.join(format!("{prefix}{dry}.log"));
        let state_path = out_dir.join(format!("{prefix}{dry}.state.yaml"));
        let truncate = resumed.is_none() && (log_path.exists() || state_path.exists());
        if truncate && !opts.force {
            return Err(RunError::Usage(format!(
                "{} or {} already exists; pass --resume {} to continue that run, \
                 --prefix to start a separate one, or --force to truncate both",
                log_path.display(),
                state_path.display(),
                state_path.display()
            )));
        }
        fs::create_dir_all(&out_dir).map_err(RunError::Io)?;

        let model = walk::from_disk(&root).map_err(RunError::Io)?;
        // Only now that the walk succeeded: a failed start must not
        // destroy the previous history.
        if truncate {
            for p in [&log_path, &state_path] {
                File::create(p).map_err(RunError::Io)?;
            }
        }
        let (rng, last_kind, anchor, steps_done) = match resumed {
            Some(s) => {
                let anchor = unescape(&s.chain_state.anchor)
                    .ok()
                    .and_then(|rel| model.resolve(&rel))
                    .map_or(model.root(), |id| anchor_of(&model, id));
                (s.rng, s.chain_state.last_kind, anchor, s.steps_done)
            }
            None => (ChaCha8Rng::seed_from_u64(seed), FIRST_KIND, model.root(), 0),
        };
        let started = jiff::Timestamp::now();
        let mut log = LogWriter::open_append(&log_path, opts.verbose).map_err(RunError::Io)?;
        log.header(&Header {
            seed,
            start_step: steps_done,
            steps: opts.steps,
            log_path: log_path.clone(),
            state_path: state_path.clone(),
            config_file: opts.config_file.clone(),
            dry_run: opts.dry_run,
            started,
        })
        .map_err(RunError::Io)?;
        if !opts.quiet {
            eprintln!("log: {}", log_path.display());
            eprintln!("state: {}", state_path.display());
        }
        Ok(Runner {
            root,
            cfg,
            chain,
            model,
            rng,
            seed,
            last_kind,
            anchor,
            steps_done,
            start_step: steps_done,
            dry: opts.dry_run,
            log,
            log_path,
            state_path,
            stats: Stats::default(),
            started,
            clock: Instant::now(),
            bytes_written: 0,
            skipped: 0,
        })
    }

    /// Performs one step: draws a kind from the chain (redrawing from the
    /// same row while it has no target, then falling back to
    /// `create_file` in the root), applies it, and logs it. Every
    /// `checkpoint_steps` steps the state is written and the log flushed.
    /// A failed operation is logged and returned, with the generator
    /// rewound to where it was before the step, so a state written
    /// afterwards is at the last logged step. (`last_kind`, `anchor` and
    /// the model change only once an operation succeeds.) A step where
    /// not even the fallback found a target returns
    /// [`StepResult::Retry`] and counts as skipped: its draws are kept, but
    /// it logs nothing and `steps_done` does not advance.
    pub fn step(&mut self) -> Result<StepResult, io::Error> {
        let rng_before = self.rng.clone();
        match self.apply() {
            Ok(None) => {
                self.skipped += 1;
                Ok(StepResult::Retry)
            }
            Ok(Some((pressure, kind, a))) => self.commit(pressure, kind, a),
            Err((kind, e)) => {
                self.rng = rng_before;
                let (path, to) = OpError::of(&e)
                    .map_or((&[][..], None), |o| (o.path.as_slice(), o.to.as_deref()));
                let _ = self.log.failure(self.steps_done + 1, kind, path, to, &e);
                let _ = self.log.flush();
                Err(e)
            }
        }
    }

    /// Draws and applies one operation; `None` if not even the fallback
    /// found a target. On failure, the kind that failed.
    #[allow(clippy::type_complexity)]
    fn apply(&mut self) -> Result<Option<(f64, OpKind, Applied)>, (OpKind, io::Error)> {
        let pressure = cap_pressure(self.model.total_bytes(), self.cfg.max_total_size.0);
        let mut ctx = OpCtx {
            root: &self.root,
            cfg: &self.cfg,
            model: &mut self.model,
            rng: &mut self.rng,
            anchor: self.anchor,
            dry: self.dry,
            fsync: self.cfg.fsync,
        };
        for _ in 0..REDRAWS {
            let kind = self.chain.next(ctx.rng, self.last_kind, pressure);
            match dispatch(kind)(&mut ctx) {
                Ok(OpOutcome::Done(a)) => return Ok(Some((pressure, kind, a))),
                Ok(OpOutcome::NoTarget) => {}
                Err(e) => return Err((kind, e)),
            }
        }
        ctx.anchor = ctx.model.root();
        match create_file(&mut ctx) {
            Ok(OpOutcome::Done(a)) => Ok(Some((pressure, OpKind::CreateFile, a))),
            Ok(OpOutcome::NoTarget) => Ok(None),
            Err(e) => Err((OpKind::CreateFile, e)),
        }
    }

    /// Counts, logs and records an applied operation; checkpoints when
    /// due. The step has happened on disk, so an error here (writing the
    /// log or the checkpoint) does not rewind anything.
    fn commit(&mut self, pressure: f64, kind: OpKind, a: Applied) -> io::Result<StepResult> {
        self.steps_done += 1;
        let step = self.steps_done;
        self.log.step(
            step,
            jiff::Timestamp::now(),
            &a.rec,
            Timing {
                dur: a.dur,
                gen: a.gen,
            },
            self.dry,
            pressure,
        )?;
        self.stats.record(kind, a.rec.bytes, a.dur, a.gen);
        self.bytes_written += a.rec.bytes;
        self.last_kind = kind;
        self.anchor = a.anchor;
        if step.is_multiple_of(self.cfg.checkpoint_steps) {
            state::write_atomic(&self.state_path, &self.state("running"))?;
            self.log.flush()?;
        }
        Ok(StepResult::Done { step, kind })
    }

    /// Steps completed over every run of this seed.
    pub fn steps_done(&self) -> u64 {
        self.steps_done
    }

    pub fn model(&self) -> &Model {
        &self.model
    }

    /// Steps of this invocation that found nothing to do.
    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    /// The log and state file paths.
    pub fn paths(&self) -> (&Path, &Path) {
        (&self.log_path, &self.state_path)
    }

    /// Writes the log footer and the state file for `outcome`.
    pub fn finish(mut self, outcome: Outcome) -> Result<(), RunError> {
        let outcome = match outcome {
            Outcome::Ok => "ok".to_string(),
            Outcome::Failed(msg) => format!("failed: {msg}"),
            Outcome::Interrupted(sig) => format!("interrupted: {}", signals::name(sig)),
        };
        let footer = Footer {
            steps_done: self.steps_done,
            outcome: outcome.clone(),
            elapsed: self.clock.elapsed(),
            skipped: self.skipped,
        };
        // Write the state even if the log cannot take its footer.
        let logged = self.log.footer(&footer, &self.stats.summary());
        state::write_atomic(&self.state_path, &self.state(&outcome)).map_err(RunError::Io)?;
        logged.and_then(|()| self.log.flush()).map_err(RunError::Io)
    }

    /// Bytes written by this run's steps.
    fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// The state as of now, with `outcome` for `last_run`.
    fn state(&self, outcome: &str) -> State {
        State {
            format_version: FORMAT_VERSION,
            seed: self.seed,
            steps_done: self.steps_done,
            rng: self.rng.clone(),
            chain_state: ChainState {
                last_kind: self.last_kind,
                anchor: escape(&self.model.rel_path(self.anchor)),
            },
            dry_run: self.dry,
            config: self.cfg.clone(),
            tree: TreeInfo {
                entries: self.model.entries() as u64,
                total_bytes: self.model.total_bytes(),
            },
            last_run: LastRun {
                started: self.started,
                finished: jiff::Timestamp::now(),
                steps: self.steps_done - self.start_step,
                outcome: outcome.to_string(),
                summary: self.stats.summary(),
            },
        }
    }
}

/// The prefix a state file's name implies: its name without
/// `.dry.state.yaml` or `.state.yaml`.
fn prefix_of(state_file: &Path) -> Option<String> {
    let name = state_file.file_name()?.to_str()?;
    name.strip_suffix(".dry.state.yaml")
        .or_else(|| name.strip_suffix(".state.yaml"))
        .map(str::to_string)
}

/// The canonical root; a usage error unless it exists and is a directory.
fn canonical_root(root: &Path) -> Result<PathBuf, RunError> {
    let canon = fs::canonicalize(root)
        .map_err(|e| RunError::Usage(format!("root {}: {e}", root.display())))?;
    if !canon.is_dir() {
        return Err(RunError::Usage(format!(
            "root {} is not a directory",
            root.display()
        )));
    }
    Ok(canon)
}

/// `p` made absolute with every symlink resolved, where the part that
/// does not exist yet is appended as given (so a missing directory can be
/// checked before it is created).
fn lenient_canonical(p: &Path) -> io::Result<PathBuf> {
    let abs = std::path::absolute(p)?;
    let mut missing = Vec::new();
    let mut cur = abs.as_path();
    loop {
        match fs::canonicalize(cur) {
            Ok(c) => return Ok(missing.iter().rev().fold(c, |acc, n| acc.join(n))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                missing.push(cur.file_name().unwrap_or_default().to_os_string());
                cur = match cur.parent() {
                    Some(parent) => parent,
                    None => return Err(e),
                };
            }
            Err(e) => return Err(e),
        }
    }
}

/// Runs `opts.steps` steps (a step that could do nothing counts too),
/// checking for SIGINT and SIGTERM between steps, with a progress line on
/// stderr unless quiet (or verbose, whose echoed log lines would garble
/// it). A failed step ends the run with [`Outcome::Failed`], a signal with
/// [`Outcome::Interrupted`]; both still write the footer and state. If any
/// step found nothing to do, one warning says so on stderr unless quiet.
pub fn run(opts: RunOpts) -> Result<Outcome, RunError> {
    let steps = opts.steps;
    let quiet = opts.quiet;
    let bar = if opts.quiet || opts.verbose {
        ProgressBar::hidden()
    } else {
        let bar = ProgressBar::with_draw_target(Some(steps), ProgressDrawTarget::stderr());
        bar.set_style(
            ProgressStyle::with_template("{pos}/{len} steps, {msg} written, {per_sec}")
                .expect("a valid template"),
        );
        bar
    };
    let mut runner = Runner::new(opts)?;
    let signal = signals::install();
    let mut done = 0;
    let outcome = loop {
        let sig = signal.load(Ordering::SeqCst);
        if sig != 0 {
            break Outcome::Interrupted(sig);
        }
        if done == steps {
            break Outcome::Ok;
        }
        match runner.step() {
            Ok(_) => {
                done += 1;
                bar.inc(1);
                bar.set_message(HumanBytes(runner.bytes_written()).to_string());
            }
            // Handlers use SA_RESTART, so a signal never cuts a step
            // short; it is only seen between steps, above.
            Err(e) => break Outcome::Failed(e.to_string()),
        }
    };
    bar.finish_and_clear();
    let skipped = runner.skipped();
    runner.finish(outcome.clone())?;
    if skipped > 0 && !quiet {
        eprintln!(
            "filegen: warning: {skipped} of {done} steps found nothing to do, not even \
             create_file in the root, and were skipped (skipped= in the log's # end line)"
        );
    }
    Ok(outcome)
}

#[cfg(test)]
#[path = "../tests/common/mod.rs"]
mod test_common;

#[cfg(test)]
mod tests {
    use super::test_common::snapshot;
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    use tempfile::TempDir;

    use crate::ops::testutil::triples;
    use crate::state;
    use crate::walk::from_disk;

    const CAPS: [(&str, &str); 2] = [("max_file_size", "64K"), ("max_total_size", "4M")];

    fn args(root: &Path, out: &Path) -> RunArgs {
        RunArgs {
            path: root.to_path_buf(),
            steps: 0,
            seed: None,
            resume: None,
            config: None,
            out_dir: Some(out.to_path_buf()),
            prefix: None,
            set: Vec::new(),
            dry_run: false,
            force: false,
            quiet: true,
            verbose: false,
            max_total_size: None,
            max_file_size: None,
            max_depth: None,
            max_path_len: None,
            remove_tree_max_entries: None,
            fsync: None,
            checkpoint_steps: None,
        }
    }

    /// Options for a quiet run with small caps plus `extra` settings.
    fn opts_with(
        root: &Path,
        out: &Path,
        steps: u64,
        start: Start,
        extra: &[(&str, &str)],
    ) -> RunOpts {
        let sets = CAPS
            .iter()
            .chain(extra)
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        RunOpts {
            root: root.to_path_buf(),
            steps,
            start,
            config_file: None,
            sets,
            flags: args(root, out),
            dry_run: false,
            force: false,
            quiet: true,
            verbose: false,
        }
    }

    fn opts(root: &Path, out: &Path, steps: u64, start: Start) -> RunOpts {
        opts_with(root, out, steps, start, &[])
    }

    fn run_ok(o: RunOpts) {
        assert_eq!(run(o).unwrap(), Outcome::Ok);
    }

    fn usage(r: Result<Outcome, RunError>) -> String {
        match r {
            Err(RunError::Usage(m)) => m,
            other => panic!("expected a usage error, got {other:?}"),
        }
    }

    /// A temp dir holding `root/` and the out dirs.
    fn base() -> (TempDir, PathBuf) {
        let t = TempDir::new().unwrap();
        let root = t.path().join("root");
        fs::create_dir(&root).unwrap();
        (t, root)
    }

    /// Empties `root` so a second run starts from the same path.
    fn reset(root: &Path) {
        fs::remove_dir_all(root).unwrap();
        fs::create_dir(root).unwrap();
    }

    fn text(p: &Path) -> String {
        fs::read_to_string(p).unwrap()
    }

    /// The value of `key="..."` in a log line, escapes kept.
    fn quoted_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
        let start = line.find(&format!(" {key}=\""))? + key.len() + 3;
        let rest = &line[start..];
        let mut escaped = false;
        for (i, c) in rest.char_indices() {
            match c {
                '\\' if !escaped => escaped = true,
                '"' if !escaped => return Some(&rest[..i]),
                _ => escaped = false,
            }
        }
        panic!("unterminated {key} value in {line:?}");
    }

    /// The step lines of a log without their timestamps and durations.
    fn step_lines(log: &str) -> Vec<String> {
        log.lines()
            .filter(|l| !l.starts_with('#'))
            .map(|l| {
                let (head, rest) = l.split_once(" ts=").expect("a step line has ts=");
                let rest = rest.split_once(' ').map_or("", |(_, r)| r);
                let line = format!("{head} {rest}");
                match line.rsplit_once(" dur_us=") {
                    Some((keep, _)) => keep.to_string(),
                    None => line,
                }
            })
            .collect()
    }

    #[test]
    fn run_two_seeds_same_tree_and_log() {
        let (t, root) = base();
        let (out_a, out_b) = (t.path().join("a"), t.path().join("b"));
        run_ok(opts(&root, &out_a, 400, Start::Seed(11)));
        let snap = snapshot(&root);
        assert!(snap.len() > 20, "only {} entries", snap.len());
        assert!(
            snap.iter().any(|e| e.6.is_some()),
            "no drawn mtime survived to be compared"
        );
        reset(&root);
        run_ok(opts(&root, &out_b, 400, Start::Seed(11)));
        assert_eq!(snapshot(&root), snap);
        let a = step_lines(&text(&out_a.join("filegen-11.log")));
        let b = step_lines(&text(&out_b.join("filegen-11.log")));
        assert_eq!(a.len(), 400);
        assert_eq!(a, b);
        let s = state::read(&out_a.join("filegen-11.state.yaml")).unwrap();
        assert_eq!(s.steps_done, 400);
        assert_eq!(s.seed, 11);
        assert_eq!(s.last_run.outcome, "ok");
        assert_eq!(s, {
            let mut sb = state::read(&out_b.join("filegen-11.state.yaml")).unwrap();
            sb.last_run = s.last_run.clone();
            sb.config.out_dir = s.config.out_dir.clone();
            sb
        });
    }

    #[test]
    fn run_resume_matches_uninterrupted() {
        let (t, root) = base();
        let (out_a, out_b) = (t.path().join("a"), t.path().join("b"));
        run_ok(opts(&root, &out_a, 400, Start::Seed(12)));
        let snap = snapshot(&root);
        reset(&root);
        run_ok(opts(&root, &out_b, 150, Start::Seed(12)));
        let state_b = out_b.join("filegen-12.state.yaml");
        assert_eq!(state::read(&state_b).unwrap().steps_done, 150);
        run_ok(opts(&root, &out_b, 250, Start::Resume(state_b.clone())));
        assert_eq!(snapshot(&root), snap);
        let log_b = text(&out_b.join("filegen-12.log"));
        assert_eq!(log_b.lines().filter(|l| l.starts_with("# run")).count(), 2);
        assert!(log_b.contains("start_step=150"));
        assert_eq!(
            step_lines(&text(&out_a.join("filegen-12.log"))),
            step_lines(&log_b)
        );
        let s = state::read(&state_b).unwrap();
        assert_eq!(s.steps_done, 400);
        assert_eq!(s.last_run.steps, 250);
        assert_eq!(
            s.rng,
            state::read(&out_a.join("filegen-12.state.yaml"))
                .unwrap()
                .rng
        );
    }

    #[test]
    fn run_every_default_kind_appears() {
        let (t, root) = base();
        let out = t.path().join("out");
        let mut r = Runner::new(opts(&root, &out, 5000, Start::Seed(13))).unwrap();
        for _ in 0..5000 {
            r.step().unwrap();
        }
        // After every kind has run, the model still matches the disk.
        let disk = from_disk(&root).unwrap();
        assert_eq!(triples(&disk), triples(r.model()));
        assert_eq!(disk.total_bytes(), r.model().total_bytes());
        r.finish(Outcome::Ok).unwrap();
        let log = text(&out.join("filegen-13.log"));
        for kind in OpKind::ALL {
            let seen = log.contains(&format!(" op={} ", kind.name()));
            assert_eq!(seen, kind.default_enabled(), "{}", kind.name());
        }
        assert!(log.contains("# stat op=create_file"));
    }

    /// The sizes of every regular file under `dir`.
    fn file_sizes(dir: &Path) -> Vec<u64> {
        let mut out = Vec::new();
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let ft = e.file_type().unwrap();
            if ft.is_dir() {
                out.extend(file_sizes(&e.path()));
            } else if ft.is_file() {
                out.push(e.metadata().unwrap().len());
            }
        }
        out
    }

    #[test]
    fn run_caps_hold_every_step() {
        let (t, root) = base();
        let out = t.path().join("out");
        let mut o = opts(&root, &out, 2000, Start::Seed(16));
        o.sets = vec![
            ("max_total_size".into(), "200K".into()),
            ("max_file_size".into(), "50K".into()),
        ];
        let mut r = Runner::new(o).unwrap();
        let mut pressured = false;
        for _ in 0..2000 {
            r.step().unwrap();
            let sizes = file_sizes(&root);
            assert!(sizes.iter().sum::<u64>() <= 200 << 10);
            assert!(sizes.iter().all(|&s| s <= 50 << 10));
            assert_eq!(r.model().total_bytes(), sizes.iter().sum::<u64>());
            pressured |= r.model().total_bytes() > 160 << 10;
        }
        assert!(pressured, "never came near the cap");
        r.finish(Outcome::Ok).unwrap();
        assert!(text(&out.join("filegen-16.log")).contains(" pressure="));
    }

    #[test]
    fn run_root_symlink_and_trailing_slash() {
        let t = TempDir::new().unwrap();
        let real = t.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = t.path().join("link");
        symlink(&real, &link).unwrap();
        let mut root = link.into_os_string();
        root.push("/");
        let root = PathBuf::from(root);
        let out = t.path().join("out");
        run_ok(opts(&root, &out, 50, Start::Seed(17)));
        assert!(fs::symlink_metadata(t.path().join("link"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(!snapshot(&real).is_empty());
        let log = text(&out.join("filegen-17.log"));
        let out_canon = fs::canonicalize(&out).unwrap();
        assert!(log.contains(&format!("log=\"{}", out_canon.display())));
        // Absolute symlink targets name the real root, never the link.
        assert!(!log.contains("/link/"), "the root is used through the link");
        // An out dir inside the real root, named through either path.
        let msg = usage(run(opts(&root, &real.join("o"), 1, Start::Seed(18))));
        assert!(msg.contains("inside"), "{msg}");
        let msg = usage(run(opts(&root, &root.join("o"), 1, Start::Seed(18))));
        assert!(msg.contains("inside"), "{msg}");
        assert!(!real.join("o").exists());
        // A missing root, and a root that is a file.
        let missing = t.path().join("missing");
        usage(run(opts(&missing, &out, 1, Start::Seed(18))));
        let file = t.path().join("file");
        fs::write(&file, b"").unwrap();
        usage(run(opts(&file, &out, 1, Start::Seed(18))));
    }

    #[test]
    fn run_refuses_out_dir_inside_root_and_existing_files() {
        let (t, root) = base();
        let msg = usage(run(opts(&root, &root.join("sub"), 10, Start::Seed(19))));
        assert!(msg.contains("inside"), "{msg}");
        assert!(!root.join("sub").exists());
        let msg = usage(run(opts(&root, &root, 10, Start::Seed(19))));
        assert!(msg.contains("inside"), "{msg}");

        let out = t.path().join("out");
        run_ok(opts(&root, &out, 10, Start::Seed(19)));
        let msg = usage(run(opts(&root, &out, 10, Start::Seed(19))));
        for want in ["--resume", "--prefix", "--force"] {
            assert!(msg.contains(want), "{msg}");
        }
        // Only the state file left: still refused.
        fs::remove_file(out.join("filegen-19.log")).unwrap();
        usage(run(opts(&root, &out, 10, Start::Seed(19))));

        let mut o = opts(&root, &out, 10, Start::Seed(19));
        o.force = true;
        run_ok(o);
        let log = text(&out.join("filegen-19.log"));
        assert_eq!(log.lines().filter(|l| l.starts_with("# run")).count(), 1);
        assert_eq!(
            state::read(&out.join("filegen-19.state.yaml"))
                .unwrap()
                .steps_done,
            10
        );

        // A prefix starts a separate history.
        let mut o = opts(&root, &out, 10, Start::Seed(19));
        o.flags.prefix = Some("mine".into());
        run_ok(o);
        assert!(out.join("mine.log").exists());
        assert!(out.join("mine.state.yaml").exists());
    }

    #[test]
    fn run_dry_run_files_and_state() {
        let (t, root) = base();
        let out = t.path().join("out");
        let mut o = opts(&root, &out, 300, Start::Seed(14));
        o.dry_run = true;
        run_ok(o);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        let dry_state = out.join("filegen-14.dry.state.yaml");
        let dry_log = text(&out.join("filegen-14.dry.log"));
        assert!(!out.join("filegen-14.log").exists());
        assert!(!out.join("filegen-14.state.yaml").exists());
        let lines = step_lines(&dry_log);
        assert_eq!(lines.len(), 300);
        assert!(lines.iter().all(|l| l.contains(" dry=1")));
        let dry = state::read(&dry_state).unwrap();
        assert!(dry.dry_run);

        run_ok(opts(&root, &out, 300, Start::Seed(14)));
        let real_state = out.join("filegen-14.state.yaml");
        let real = state::read(&real_state).unwrap();
        assert!(!real.dry_run);
        assert_eq!(real.rng, dry.rng);
        assert_eq!(real.steps_done, dry.steps_done);
        assert_eq!(real.chain_state, dry.chain_state);
        assert_eq!(real.tree, dry.tree);

        let msg = usage(run(opts(&root, &out, 1, Start::Resume(dry_state.clone()))));
        assert!(msg.contains("--dry-run"), "{msg}");
        let mut o = opts(&root, &out, 1, Start::Resume(real_state));
        o.dry_run = true;
        let msg = usage(run(o));
        assert!(msg.contains("--dry-run"), "{msg}");
    }

    #[test]
    fn run_failure_writes_state_and_footer() {
        let (t, root) = base();
        let out = t.path().join("out");
        let mut r = Runner::new(opts(&root, &out, 100, Start::Seed(20))).unwrap();
        for i in 0..20 {
            assert!(matches!(r.step().unwrap(), StepResult::Done { step, .. } if step == i + 1));
        }
        assert_eq!(r.steps_done(), 20);
        // Every path the model knows, escaped as in the log; "" is the root.
        let known: std::collections::BTreeSet<String> = triples(r.model())
            .into_iter()
            .map(|(p, _, _)| escape(&p))
            .chain([String::new()])
            .collect();
        fs::remove_dir_all(&root).unwrap();
        let e = r.step().unwrap_err();
        assert_eq!(r.steps_done(), 20);
        let (log_path, state_path) = (r.paths().0.to_path_buf(), r.paths().1.to_path_buf());
        r.finish(Outcome::Failed(e.to_string())).unwrap();
        let s = state::read(&state_path).unwrap();
        assert!(
            s.last_run.outcome.starts_with("failed:"),
            "{}",
            s.last_run.outcome
        );
        assert_eq!(s.steps_done, 20);
        let log = text(&log_path);
        assert_eq!(log.lines().filter(|l| l.contains(" error=")).count(), 1);
        let err = log.lines().find(|l| l.contains(" error=")).unwrap();
        assert!(err.starts_with("step=21 "), "{err}");
        // The failure names the path it acted on: one the model held, or a
        // new name in a directory it held.
        let path = quoted_value(err, "path").expect("a path on the failure line");
        assert!(!path.is_empty(), "{err}");
        let parent = path.rsplit_once('/').map_or("", |(dir, _)| dir);
        assert!(
            known.contains(path) || known.contains(parent),
            "{path:?} is not a path in the tree: {err}"
        );
        if let Some(to) = quoted_value(err, "to") {
            let to_parent = to.rsplit_once('/').map_or("", |(dir, _)| dir);
            assert!(known.contains(to) || known.contains(to_parent), "{err}");
        }
        let end = log.lines().find(|l| l.starts_with("# end")).unwrap();
        assert!(end.contains("steps_done=20 outcome=\"failed: "), "{end}");
        assert!(log.lines().last().unwrap().starts_with('#'));

        // The failed step's draws were rewound: the state is exactly that
        // of a run that stopped after 20 steps.
        let (t2, root2) = base();
        let out2 = t2.path().join("out");
        let mut clean = Runner::new(opts(&root2, &out2, 20, Start::Seed(20))).unwrap();
        for _ in 0..20 {
            clean.step().unwrap();
        }
        let clean_state = clean.paths().1.to_path_buf();
        clean.finish(Outcome::Ok).unwrap();
        let c = state::read(&clean_state).unwrap();
        assert_eq!(s.rng, c.rng);
        assert_eq!(s.chain_state, c.chain_state);
    }

    #[test]
    fn run_force_keeps_history_when_the_walk_fails() {
        use std::os::unix::fs::PermissionsExt;
        if nix::unistd::geteuid().is_root() {
            eprintln!("skipping: root can list an unreadable dir");
            return;
        }
        let (t, root) = base();
        let out = t.path().join("out");
        run_ok(opts(&root, &out, 10, Start::Seed(26)));
        let log_before = text(&out.join("filegen-26.log"));
        fs::set_permissions(&root, fs::Permissions::from_mode(0o000)).unwrap();
        let mut o = opts(&root, &out, 10, Start::Seed(26));
        o.force = true;
        let r = run(o);
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(r, Err(RunError::Io(_))), "{r:?}");
        assert_eq!(text(&out.join("filegen-26.log")), log_before);
        assert_eq!(
            state::read(&out.join("filegen-26.state.yaml"))
                .unwrap()
                .steps_done,
            10
        );
    }

    #[test]
    fn run_rejects_prefixes_naming_other_dirs() {
        let (t, root) = base();
        let out = t.path().join("out");
        for bad in ["../root/x", "a/b", "", ".", ".."] {
            let mut o = opts(&root, &out, 1, Start::Seed(24));
            o.flags.prefix = Some(bad.into());
            let msg = usage(run(o));
            assert!(msg.contains("prefix"), "{bad:?}: {msg}");
        }
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    }

    #[test]
    fn run_resume_keeps_the_state_files_history() {
        // Without --out-dir or --prefix, a resume writes next to its state
        // file under the state file's prefix, wherever the defaults point.
        let (t, root) = base();
        let out = t.path().join("out");
        let mut o = opts(&root, &out, 30, Start::Seed(25));
        o.flags.prefix = Some("hist".into());
        let mut r = Runner::new(o).unwrap();
        let state_path = r.paths().1.to_path_buf();
        for _ in 0..30 {
            r.step().unwrap();
        }
        r.finish(Outcome::Ok).unwrap();
        // The saved config names the out dir used and the prefix; a resume
        // ignores both and goes by the state file's path.
        let s = state::read(&state_path).unwrap();
        assert_eq!(s.config.out_dir, Some(fs::canonicalize(&out).unwrap()));
        assert_eq!(s.config.prefix.as_deref(), Some("hist"));
        let mut o = opts(&root, &out, 20, Start::Resume(state_path.clone()));
        o.flags.out_dir = None;
        run_ok(o);
        assert_eq!(state::read(&state_path).unwrap().steps_done, 50);
        let log = text(&out.join("hist.log"));
        assert_eq!(log.lines().filter(|l| l.starts_with("# run")).count(), 2);
        assert_eq!(fs::read_dir(&out).unwrap().count(), 2);
    }

    #[test]
    fn run_resume_after_external_change() {
        let (t, root) = base();
        let out = t.path().join("out");
        run_ok(opts(&root, &out, 200, Start::Seed(21)));
        let mut names: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        names.sort();
        assert!(names.len() >= 2);
        for p in names.iter().step_by(2) {
            if fs::symlink_metadata(p).unwrap().is_dir() {
                fs::remove_dir_all(p).unwrap();
            } else {
                fs::remove_file(p).unwrap();
            }
        }
        fs::create_dir_all(root.join("stray/inner")).unwrap();
        let state_path = out.join("filegen-21.state.yaml");
        run_ok(opts(&root, &out, 200, Start::Resume(state_path.clone())));
        let s = state::read(&state_path).unwrap();
        assert_eq!(s.steps_done, 400);
        let disk = from_disk(&root).unwrap();
        assert_eq!(s.tree.entries, disk.entries() as u64);
        assert_eq!(s.tree.total_bytes, disk.total_bytes());
    }

    #[test]
    fn run_checkpoints_state() {
        let (t, root) = base();
        let out = t.path().join("out");
        let o = opts_with(
            &root,
            &out,
            100,
            Start::Seed(22),
            &[("checkpoint_steps", "10")],
        );
        let mut r = Runner::new(o).unwrap();
        let state_path = r.paths().1.to_path_buf();
        let log_path = r.paths().0.to_path_buf();
        for _ in 0..25 {
            r.step().unwrap();
            assert_eq!(state_path.exists(), r.steps_done() >= 10);
        }
        // The log is flushed with each checkpoint.
        assert!(step_lines(&text(&log_path)).len() >= 20);
        let s = state::read(&state_path).unwrap();
        assert_eq!(s.steps_done, 20);
        assert_eq!(s.last_run.outcome, "running");
        // Dropped without finish, as by a kill no handler catches.
        drop(r);
        assert_eq!(state::read(&state_path).unwrap().steps_done, 20);

        // The checkpoint is exactly the state of a run that stopped after
        // 2 x checkpoint_steps steps.
        let (t2, root2) = base();
        let o = opts_with(
            &root2,
            &t2.path().join("out"),
            20,
            Start::Seed(22),
            &[("checkpoint_steps", "10")],
        );
        let mut fresh = Runner::new(o).unwrap();
        for _ in 0..20 {
            fresh.step().unwrap();
        }
        let f = fresh.state("ok");
        assert_eq!(s.rng, f.rng);
        assert_eq!(s.chain_state, f.chain_state);
        drop(fresh);

        // Resuming from a checkpoint (killed right after it, so the tree is
        // as the checkpoint left it) for the remaining steps reproduces the
        // uninterrupted run. Both at one root path: absolute symlink
        // targets name it.
        let cp = [("checkpoint_steps", "10")];
        let whole = t.path().join("whole");
        reset(&root);
        run_ok(opts_with(&root, &whole, 100, Start::Seed(22), &cp));
        let want = snapshot(&root);
        reset(&root);
        let killed = t.path().join("killed");
        let mut r = Runner::new(opts_with(&root, &killed, 100, Start::Seed(22), &cp)).unwrap();
        let state_path = r.paths().1.to_path_buf();
        for _ in 0..30 {
            r.step().unwrap();
        }
        drop(r);
        assert_eq!(state::read(&state_path).unwrap().steps_done, 30);
        run_ok(opts_with(
            &root,
            &killed,
            70,
            Start::Resume(state_path),
            &cp,
        ));
        assert_eq!(snapshot(&root), want);
        assert_eq!(
            step_lines(&text(&whole.join("filegen-22.log"))),
            step_lines(&text(&killed.join("filegen-22.log")))
        );
    }

    #[test]
    fn run_counts_skipped_steps() {
        // No name fits a max_path_len of 0: every step, the create_file
        // fallback included, finds nothing to do.
        let (t, root) = base();
        let out = t.path().join("out");
        let o = opts_with(&root, &out, 5, Start::Seed(27), &[("max_path_len", "0")]);
        let mut r = Runner::new(o).unwrap();
        for _ in 0..5 {
            assert_eq!(r.step().unwrap(), StepResult::Retry);
        }
        assert_eq!(r.skipped(), 5);
        let (log_path, state_path) = (r.paths().0.to_path_buf(), r.paths().1.to_path_buf());
        r.finish(Outcome::Ok).unwrap();
        let end = text(&log_path)
            .lines()
            .find(|l| l.starts_with("# end"))
            .unwrap()
            .to_string();
        assert!(end.starts_with("# end steps_done=0 outcome=ok "), "{end}");
        assert!(end.ends_with(" skipped=5"), "{end}");
        let s = state::read(&state_path).unwrap();
        assert_eq!(s.last_run.outcome, "ok");
        assert_eq!(s.steps_done, 0);

        // A run that skipped nothing has no skipped= at all.
        let out2 = t.path().join("out2");
        run_ok(opts(&root, &out2, 5, Start::Seed(27)));
        let log = text(&out2.join("filegen-27.log"));
        assert!(!log.contains("skipped="), "{log}");
    }

    #[test]
    fn run_path_max_check() {
        let (t, root) = base();
        let out = t.path().join("out");
        let o = opts_with(&root, &out, 1, Start::Seed(23), &[("max_path_len", "4096")]);
        let msg = usage(run(o));
        assert!(msg.contains("max_path_len"), "{msg}");
    }
}
