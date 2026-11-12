//! The concurrent mode: filegen writes to the source while irsync syncs
//! it; then the destination is judged against every state the source held
//! (`replay`), and a quiet sync must leave an exact copy.
//!
//! A run: fill, `fileset init`, round 0 (a quiet sync with today's
//! checks), then rounds 1..=N, each as [`round`] describes.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Instant;

use ::filegen::escape::escape;
use ::filegen::log::{parse_step_line, StepLine};
use ::filegen::pace::Pace;
use anyhow::{anyhow, bail, Context, Result};
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::Deserialize;

use crate::cli::Cli;
use crate::client::{SyncExit, SyncResult};
use crate::filegen::{self, FilegenOpts};
use crate::fileset;
use crate::layout::Layout;
use crate::replay::{replay_and_judge, RoundInput, RoundJudgement};
use crate::report::{self, ConcurrentRound, Iteration};
use crate::run::{
    direction_name, new_run_file, ping, run_filegen, start_daemon, RunningDaemon, Syncer,
};
use crate::signals;
use crate::snapshot::{clone_or_copy_file, copy_tree, walk_meta};
use crate::tools::Tools;
use crate::verify;

/// The variable that asks irsync for its file report.
pub const FILE_REPORT_ENV: &str = "IRSYNC_FILE_REPORT";

/// How many failing paths a failed round prints before the count.
const SHOWN_FAILURES: usize = 20;

/// The pace of round `round`, drawn from the run's seed alone: fast,
/// steady or bursts with probability 1/3 each; a steady rate log-uniform
/// in [5, 500] steps a second; burst and pause lengths each uniform in
/// [50, 2000] ms.
pub fn draw_pace(seed: u64, round: u64) -> Pace {
    let mut rng = ChaCha8Rng::seed_from_u64(seed ^ 0x636f_6e63_7572_7265);
    rng.set_stream(round);
    match rng.random_range(0..3u32) {
        0 => Pace::Fast,
        1 => {
            let ln = rng.random_range(5f64.ln()..=500f64.ln());
            Pace::Steady {
                per_sec: (ln.exp().round() as u32).clamp(5, 500),
            }
        }
        _ => Pace::Bursts {
            on_ms: rng.random_range(50..=2000),
            off_ms: rng.random_range(50..=2000),
        },
    }
}

/// What a round needs to be run again: its pace and how many steps
/// filegen took during the sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoundRecord {
    pub round: u64,
    pub pace: Pace,
    pub steps: u64,
}

/// Appends `round\tpace\tsteps` to `path`.
pub fn append_round(path: &Path, r: &RoundRecord) -> Result<()> {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(f, "{}\t{}\t{}", r.round, r.pace, r.steps)
        .with_context(|| format!("writing {}", path.display()))
}

/// Every record in a rounds file; a missing file or a bad line is an error.
pub fn read_rounds(path: &Path) -> Result<Vec<RoundRecord>> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let bad = || anyhow!("{}:{}: bad round line {line:?}", path.display(), n + 1);
        let fields: Vec<&str> = line.split('\t').collect();
        let [round, pace, steps] = fields.as_slice() else {
            return Err(bad());
        };
        out.push(RoundRecord {
            round: round.parse().map_err(|_| bad())?,
            pace: pace.parse().map_err(|_| bad())?,
            steps: steps.parse().map_err(|_| bad())?,
        });
    }
    Ok(out)
}

/// One line of irsync's file report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportLine {
    /// "retried", "gave_up" or "vanished".
    pub event: String,
    /// Relative to the transfer root; empty for the root itself.
    pub path: Vec<u8>,
    pub reason: String,
}

#[derive(Deserialize)]
struct RawLine {
    event: String,
    path_hex: String,
    reason: String,
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// The complete lines of the report at `path` from byte `from` on, and the
/// offset just past the last of them (a line still being written is left
/// for the next read). A missing file means no lines.
pub fn read_report(path: &Path, from: u64) -> Result<(Vec<ReportLine>, u64)> {
    let mut f = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((vec![], from)),
        Err(e) => return Err(e).with_context(|| format!("opening {}", path.display())),
    };
    f.seek(SeekFrom::Start(from))
        .with_context(|| format!("reading {}", path.display()))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)
        .with_context(|| format!("reading {}", path.display()))?;
    let complete = buf.iter().rposition(|&b| b == b'\n').map_or(0, |p| p + 1);
    let mut out = Vec::new();
    for line in buf[..complete].split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let bad = || {
            anyhow!(
                "{}: bad report line {:?}",
                path.display(),
                String::from_utf8_lossy(line)
            )
        };
        let raw: RawLine = serde_json::from_slice(line).map_err(|_| bad())?;
        out.push(ReportLine {
            event: raw.event,
            path: unhex(&raw.path_hex).ok_or_else(bad)?,
            reason: raw.reason,
        });
    }
    Ok((out, from + complete as u64))
}

/// The exact reasons irsync gives on the root path when another path's
/// failure (a source directory that vanished while it was walked, say)
/// left the source listing incomplete: the listing itself, and the
/// `--delete` the receiver then skipped (`delete::DeleteRefused`). The
/// client reports its own receiver's errors bare and the daemon
/// receiver's with "receiver: " in front.
const LISTING_CONSEQUENCES: [&str; 4] = [
    "deletions skipped: a file listing was incomplete (--ignore-errors deletes anyway)",
    "receiver: deletions skipped: a file listing was incomplete (--ignore-errors deletes anyway)",
    "the sender's listing is incomplete",
    "receiver: the sender's listing is incomplete",
];

/// The `gave_up` paths of `lines`, and those of them not in `touched`. The
/// root path `""` is never touched, with one exception: an incomplete
/// listing, or the deletions skipped because of one, is a consequence of
/// another path's failure, so it passes when some other path was given up
/// on too (that path must then be touched itself).
pub fn gave_up_split(
    lines: &[ReportLine],
    touched: &BTreeSet<Vec<u8>>,
) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let given_up: Vec<&ReportLine> = lines.iter().filter(|l| l.event == "gave_up").collect();
    let another_path = given_up.iter().any(|l| !l.path.is_empty());
    let untouched = given_up
        .iter()
        .filter(|l| {
            if l.path.is_empty() {
                !(another_path && LISTING_CONSEQUENCES.contains(&l.reason.as_str()))
            } else {
                !touched.contains(&l.path)
            }
        })
        .map(|l| l.path.clone())
        .collect();
    (given_up.iter().map(|l| l.path.clone()).collect(), untouched)
}

/// What every round of a run shares.
pub struct RoundEnv<'a> {
    pub cli: &'a Cli,
    pub layout: &'a Layout,
    pub tools: &'a Tools,
    pub seed: u64,
    /// The run's filegen options; a round replaces the step count.
    pub filegen_opts: FilegenOpts<'a>,
    /// The command that reruns the run, for what a failure prints.
    pub rerun: &'a dyn Fn() -> String,
}

/// One concurrent round, `i`:
///
/// 1. the pace: `fixed`'s, else `--pace`, else [`draw_pace`];
/// 2. a fresh `round/`: the source's metadata, a copy of it in `start/`,
///    filegen's state in `state.yaml`;
/// 3. filegen in the background (`fixed.steps` steps, else unbounded),
///    while `sync` runs (its client must write its file report to
///    `client_report(i)`);
/// 4. filegen stopped (SIGTERM, or waited for when `fixed`), and the
///    round's record appended to `rounds.tsv`;
/// 5. both file reports read (the daemon's from `daemon_report_offset`,
///    which is moved past what was read);
/// 6. the replay and judge, the failures written to `judge_report(i)`;
/// 7. `fileset update`, then `quiet` and today's exact-match checks.
///
/// A concurrent sync that exits other than 0, 23 or 24 is a harness
/// error, as is anything `sync` or `quiet` returns as `Err` (a dead
/// daemon, say). On `Err` the round's directory is left as it is.
pub fn round(
    env: &RoundEnv,
    i: u64,
    fixed: Option<&RoundRecord>,
    sync: &dyn Fn(&Path) -> Result<SyncResult>,
    quiet: &dyn Fn(&Path) -> Result<SyncResult>,
    daemon_report_offset: &mut u64,
) -> Result<ConcurrentRound> {
    let (cli, layout, seed) = (env.cli, env.layout, env.seed);
    let pace = match fixed {
        Some(r) => r.pace,
        None => cli.pace.unwrap_or_else(|| draw_pace(seed, i)),
    };

    let dir = layout.round_dir();
    fresh_round_dir(&dir)?;
    let (start, work, replay, state) = (
        dir.join("start"),
        dir.join("work"),
        dir.join("replay"),
        dir.join("state.yaml"),
    );
    let start_meta = walk_meta(&layout.src_data)?;
    copy_tree(&layout.src_data, &start)
        .with_context(|| format!("copying the source to {}", start.display()))?;
    let live_state = layout.filegen_state(seed);
    fs::copy(&live_state, &state)
        .with_context(|| format!("copying {} to {}", live_state.display(), state.display()))?;

    let filegen_log = layout.filegen_log(seed);
    let log_from = file_len(&filegen_log)?;
    *daemon_report_offset = file_len(&layout.daemon_report())?;

    let o = &env.filegen_opts;
    let opts = FilegenOpts {
        steps: fixed.map_or(u64::MAX, |r| r.steps),
        max_total_size: o.max_total_size,
        max_file_size: o.max_file_size,
        sets: o.sets,
    };
    let mutate = filegen::mutate_args(&live_state, &layout.src_data, &opts);
    let running = filegen::spawn(
        &env.tools.filegen.path,
        &with_pace(&mutate, pace),
        &layout.filegen_stderr_log(),
    )?;
    let client_log = layout.client_log(i);
    let synced = sync(&client_log);
    // Stopped before any error is returned, so its log is complete.
    let stopped = filegen::stop(running, fixed.is_some());
    let result = (|| -> Result<ConcurrentRound> {
        // Recorded before anything can fail, so a harness error's rerun
        // command replays this round too.
        let ran = filegen_ran(&filegen_log, log_from);
        if let Ok(ran) = &ran {
            let record = RoundRecord {
                round: i,
                pace,
                steps: ran.steps,
            };
            if let Some(f) = fixed {
                if f.steps != record.steps {
                    println!(
                        "  note: filegen took {} steps where the replayed round took {}",
                        record.steps, f.steps
                    );
                }
            }
            append_round(&layout.rounds_file(), &record)?;
        }
        let r = synced?;
        stopped.with_context(|| format!("filegen's own log is {}", filegen_log.display()))?;
        if matches!(r.exit, SyncExit::Other(c) if c != 24) {
            bail!(
                "irsync client exited {} in iteration {i}; see {} and {}",
                r.exit.code(),
                client_log.display(),
                layout.daemon_log().display()
            );
        }
        let ran = ran?;
        let steps = ran.lines;

        let (client_lines, _) = read_report(&layout.client_report(i), 0)?;
        let (daemon_lines, offset) = read_report(&layout.daemon_report(), *daemon_report_offset)?;
        *daemon_report_offset = offset;

        let mut filegen_args: Vec<OsString> = vec!["filegen".into()];
        filegen_args.extend(mutate);
        let judgement = replay_and_judge(&RoundInput {
            start_meta: &start_meta,
            start_copy: &start,
            work: &work,
            replay_dir: &replay,
            state: &state,
            filegen_args: &filegen_args,
            live_steps: &steps,
            live_src: &layout.src_data,
            dest: &layout.dst_data,
            is_root: nix::unistd::geteuid().is_root(),
        })
        .with_context(|| {
            format!(
                "judging iteration {i}; the round is kept in {}",
                dir.display()
            )
        })?;
        write_judge_report(&layout.judge_report(i), i, &judgement)?;
        if !judgement.failures.is_empty() {
            // The quiet sync repairs the destination: keep it as judged.
            let kept = dir.join("dest");
            copy_tree(&layout.dst_data, &kept)
                .with_context(|| format!("keeping the judged destination in {}", kept.display()))?;
        }
        let (gave_up, gave_up_untouched) = gave_up_split(&client_lines, &judgement.touched);
        let retried = client_lines
            .iter()
            .chain(&daemon_lines)
            .filter(|l| l.event == "retried")
            .count() as u64;

        let orphans = remove_orphan_parts(&layout.src_data, &layout.dst_data, &gave_up)?;
        if !orphans.is_empty() {
            let mut f = OpenOptions::new()
                .append(true)
                .open(layout.judge_report(i))
                .with_context(|| format!("opening {}", layout.judge_report(i).display()))?;
            for p in &orphans {
                writeln!(
                    f,
                    "# removed the partial file of a given-up path: {}",
                    escape(p)
                )
                .with_context(|| format!("writing {}", layout.judge_report(i).display()))?;
            }
        }

        fileset::update(&env.tools.fileset.path, &layout.src_data)?;
        let quiet = quiet_check(env, i, quiet)?;
        Ok(ConcurrentRound {
            number: i,
            total: cli.iterations,
            pace,
            steps: ran.steps,
            sync: r,
            retried,
            gave_up,
            gave_up_untouched,
            judgement,
            quiet,
        })
    })();
    if result.is_err() {
        print_kept(env, &dir);
    }
    result
}

/// Removes the destination's `.irsync.<name>.part` of each given-up path
/// the source no longer holds, and returns those parts' paths. irsync
/// keeps a partial file for a later run to resume; when the source file
/// is gone no run ever will, and garbage collection removes it only days
/// later, so the quiet sync's exact-match check would count it as an
/// added file.
fn remove_orphan_parts(src: &Path, dest: &Path, gave_up: &[Vec<u8>]) -> Result<Vec<Vec<u8>>> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut removed = Vec::new();
    for rel in gave_up {
        let (dir, name) = match rel.iter().rposition(|&b| b == b'/') {
            Some(k) => (&rel[..k + 1], &rel[k + 1..]),
            None => (&rel[..0], &rel[..]),
        };
        if name.is_empty() {
            continue;
        }
        let in_src = src.join(std::ffi::OsStr::from_bytes(rel));
        match fs::symlink_metadata(&in_src) {
            Err(e)
                if e.kind() == io::ErrorKind::NotFound
                    || e.raw_os_error() == Some(libc::ENOTDIR) => {}
            Ok(_) => continue,
            Err(e) => return Err(e).with_context(|| format!("stat {}", in_src.display())),
        }
        let part_rel = [dir, b".irsync.", name, b".part"].concat();
        let part = dest.join(std::ffi::OsStr::from_bytes(&part_rel));
        match fs::remove_file(&part) {
            Ok(()) => removed.push(part_rel),
            // A name too long for its part name has a shortened one
            // (not handled here); a parent may be gone or a file now.
            Err(e)
                if e.kind() == io::ErrorKind::NotFound
                    || matches!(e.raw_os_error(), Some(libc::ENAMETOOLONG | libc::ENOTDIR)) => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", part.display())),
        }
    }
    Ok(removed)
}

/// Removes the previous round's `start/`, `work/`, `replay/`, `dest/`
/// and `state.yaml`, and makes sure `dir` exists.
fn fresh_round_dir(dir: &Path) -> Result<()> {
    for d in ["start", "work", "replay", "dest"] {
        let p = dir.join(d);
        match fs::remove_dir_all(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", p.display())),
        }
    }
    let state = dir.join("state.yaml");
    match fs::remove_file(&state) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("removing {}", state.display())),
    }
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))
}

/// The length of `p`; 0 if it does not exist.
fn file_len(p: &Path) -> Result<u64> {
    match fs::metadata(p) {
        Ok(m) => Ok(m.len()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e).with_context(|| format!("stat {}", p.display())),
    }
}

/// `args` with `--pace <pace>` before the last argument (the tree).
fn with_pace(args: &[OsString], pace: Pace) -> Vec<OsString> {
    let (tree, flags) = args.split_last().expect("filegen args end with the tree");
    let mut out = flags.to_vec();
    out.push("--pace".into());
    out.push(pace.to_string().into());
    out.push(tree.clone());
    out
}

/// What one background filegen run did, from its log.
struct FilegenRan {
    /// Its step lines.
    lines: Vec<StepLine>,
    /// The steps it took, as `--steps` counts them: the step lines plus
    /// the steps that found nothing to do (`skipped=`), which log nothing.
    steps: u64,
}

/// The value of `key=` among the space-separated fields of `line`.
fn log_field(line: &str, key: &str) -> Option<u64> {
    line.split(' ')
        .find_map(|f| f.strip_prefix(key)?.strip_prefix('='))
        .and_then(|v| v.parse().ok())
}

/// What the run whose log starts at byte `from` of `log` did. Its `# run`
/// line gives `start_step`, its `# end` line `steps_done` and `skipped`. A
/// run with neither line and no step (killed before it began) took 0
/// steps.
fn filegen_ran(log: &Path, from: u64) -> Result<FilegenRan> {
    let mut f = fs::File::open(log).with_context(|| format!("opening {}", log.display()))?;
    f.seek(SeekFrom::Start(from))
        .with_context(|| format!("reading {}", log.display()))?;
    let mut text = String::new();
    f.read_to_string(&mut text)
        .with_context(|| format!("reading {}", log.display()))?;
    let lines: Vec<StepLine> = text.lines().filter_map(parse_step_line).collect();
    let start = text
        .lines()
        .find(|l| l.starts_with("# run "))
        .and_then(|l| log_field(l, "start_step"));
    let end = text.lines().find(|l| l.starts_with("# end "));
    let steps = match (start, end) {
        (Some(start), Some(end)) => {
            let done = log_field(end, "steps_done")
                .ok_or_else(|| anyhow!("{}: no steps_done in {end:?}", log.display()))?;
            done.checked_sub(start)
                .ok_or_else(|| anyhow!("{}: steps_done below start_step", log.display()))?
                + log_field(end, "skipped").unwrap_or(0)
        }
        (_, None) if lines.is_empty() => 0,
        _ => bail!(
            "{}: the background filegen run has no complete # run and # end lines",
            log.display()
        ),
    };
    Ok(FilegenRan { lines, steps })
}

/// The judge's counts, then one `path<TAB>rule` line per failure (the
/// path escaped as in filegen's log).
fn write_judge_report(path: &Path, i: u64, j: &RoundJudgement) -> Result<()> {
    let mut text = format!(
        "# iteration {i}: judged={} touched={} changed_and_held={} failures={}\n",
        j.judged,
        j.touched.len(),
        j.changed_and_held,
        j.failures.len()
    );
    for (p, rule) in &j.failures {
        text.push_str(&format!("{}\t{rule}\n", escape(p)));
    }
    fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

/// The quiet sync of iteration `i` and today's checks: manifest
/// comparison and `fileset check`. An exit other than 0 or 23 is a harness
/// error, as in today's loop.
pub fn quiet_check(
    env: &RoundEnv,
    i: u64,
    quiet: &dyn Fn(&Path) -> Result<SyncResult>,
) -> Result<Iteration> {
    let layout = env.layout;
    let log = layout.quiet_client_log(i);
    let r = quiet(&log)?;
    if let SyncExit::Other(c) = r.exit {
        bail!(
            "the quiet irsync client exited {c} in iteration {i}; see {} and {}",
            log.display(),
            layout.daemon_log().display()
        );
    }
    let hashes = verify::compare_manifests(
        &layout.src_data,
        &layout.dst_data,
        &layout.manifest_hashes(i),
    )?;
    let check = fileset::check(
        &env.tools.fileset.path,
        &layout.dst_data,
        &layout.check_report(i),
    )?;
    Ok(Iteration {
        number: i,
        total: env.cli.iterations,
        sync: r,
        manifest_ok: hashes.matches(),
        check,
    })
}

/// Whether files on the run's filesystem can be cloned: a probe file in
/// `round/` cloned next to itself.
pub fn clones_here(layout: &Layout) -> Result<bool> {
    let dir = layout.round_dir();
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let (a, b) = (dir.join(".clone-probe"), dir.join(".clone-probe.copy"));
    fs::write(&a, b"irsync-stress clone probe\n")
        .with_context(|| format!("writing {}", a.display()))?;
    let cloned = clone_or_copy_file(&a, &b);
    let _ = fs::remove_file(&a);
    let _ = fs::remove_file(&b);
    cloned
}

/// `s` quoted for a POSIX shell when it needs it.
fn shell_quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-./,:=+@%".contains(&b));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// The command that runs this run again on a fresh root (the root with
/// `.rerun` appended), with the same seed and the round records in
/// `rounds_file`. `tool` is the command's name and `lead` the arguments
/// that come right after it.
pub fn rerun_command(
    tool: &str,
    lead: &[String],
    cli: &Cli,
    seed: u64,
    rounds_file: &Path,
) -> String {
    let lossy = |p: &Path| p.to_string_lossy().into_owned();
    let mut a: Vec<String> = vec![tool.into()];
    a.extend(lead.iter().cloned());
    a.extend([
        "--direction".into(),
        direction_name(cli.direction).into(),
        "--iterations".into(),
        cli.iterations.to_string(),
        "--seed".into(),
        seed.to_string(),
        "--steps".into(),
        cli.steps.to_string(),
    ]);
    if let Some(s) = &cli.max_total_size {
        a.extend(["--max-total-size".into(), s.clone()]);
    }
    if let Some(s) = &cli.max_file_size {
        a.extend(["--max-file-size".into(), s.clone()]);
    }
    for kv in &cli.filegen_set {
        a.extend(["--filegen-set".into(), kv.clone()]);
    }
    a.extend(["--log-level".into(), cli.log_level.clone()]);
    if cli.keep_going {
        a.push("--keep-going".into());
    }
    for (flag, p) in [
        ("--irsync", &cli.irsync),
        ("--filegen", &cli.filegen),
        ("--fileset", &cli.fileset),
    ] {
        if let Some(p) = p {
            a.extend([flag.into(), lossy(p)]);
        }
    }
    a.push("--concurrent".into());
    if let Some(p) = cli.pace {
        a.extend(["--pace".into(), p.to_string()]);
    }
    a.extend(["--replay-rounds".into(), lossy(rounds_file)]);
    let mut root = cli.root.as_os_str().to_owned();
    root.push(".rerun");
    a.push(lossy(Path::new(&root)));
    if !cli.client_args.is_empty() {
        a.push("--".into());
        a.extend(
            cli.client_args
                .iter()
                .map(|s| s.to_string_lossy().into_owned()),
        );
    }
    a.iter()
        .map(|s| shell_quote(s))
        .collect::<Vec<_>>()
        .join(" ")
}

/// One sync through the shared daemon, marked in its log as `marker`;
/// a daemon that is gone before or after it is an error. `run` starts the
/// client, with its output going to `log`.
pub fn synced_with(
    daemon: &RefCell<RunningDaemon>,
    log: &Path,
    marker: &str,
    run: &dyn Fn() -> Result<SyncResult>,
) -> Result<SyncResult> {
    {
        let mut d = daemon.borrow_mut();
        d.check_alive()?;
        d.marker(marker)?;
    }
    let r = run()?;
    let mut d = daemon.borrow_mut();
    let daemon_log = d.log_path().to_path_buf();
    if let Err(e) = d.check_alive() {
        return Err(e.context(format!(
            "daemon died during the sync logged in {}; see {}",
            log.display(),
            daemon_log.display()
        )));
    }
    Ok(r)
}

/// [`synced_with`] through the plain `Syncer`.
fn synced(
    daemon: &RefCell<RunningDaemon>,
    syncer: &Syncer,
    log: &Path,
    env: &[(OsString, OsString)],
    marker: &str,
) -> Result<SyncResult> {
    synced_with(daemon, log, marker, &|| syncer.run_with_env(log, env))
}

/// After a harness error in round `round_dir`: where the round is kept
/// and the command that reruns the run.
fn print_kept(env: &RoundEnv, round_dir: &Path) {
    println!("  round kept in {}", round_dir.display());
    println!("  rerun: {}", (env.rerun)());
}

/// Moves a failed round's `round/` to `round-NNNN/`, out of the next
/// round's way, and returns the new path.
pub fn keep_failed_round(layout: &Layout, i: u64) -> Result<std::path::PathBuf> {
    let (from, to) = (layout.round_dir(), layout.failed_round_dir(i));
    fs::rename(&from, &to)
        .with_context(|| format!("renaming {} to {}", from.display(), to.display()))?;
    Ok(to)
}

/// Prints why round `r` failed: its failing paths with their rule (the
/// first 20, then the count), the judge report and the rerun command.
pub fn print_failure(r: &ConcurrentRound, layout: &Layout, rerun: &str, kept: &Path) {
    let i = r.number;
    if r.sync.exit == SyncExit::Partial && r.gave_up.is_empty() {
        println!(
            "  exit 23 without a path given up on: a failed session; see {}",
            layout.client_log(i).display()
        );
    }
    let mut failing: Vec<(&[u8], &str)> = r
        .judgement
        .failures
        .iter()
        .map(|(p, rule)| (p.as_slice(), rule.as_str()))
        .collect();
    failing.extend(r.gave_up_untouched.iter().map(|p| {
        (
            p.as_slice(),
            "irsync gave up on a path filegen did not touch",
        )
    }));
    for (p, rule) in failing.iter().take(SHOWN_FAILURES) {
        println!("  {}: {rule}", escape(p));
    }
    if failing.len() > SHOWN_FAILURES {
        println!("  ... {} failing paths in all", failing.len());
    }
    println!("  judge report: {}", layout.judge_report(i).display());
    println!("  round kept in {}", kept.display());
    if !r.quiet.passed() {
        println!("  quiet sync check: {}", layout.check_report(i).display());
    }
    println!("  rerun: {rerun}");
}

/// The whole concurrent run. Returns the process exit code: 0, 1, or 4
/// when no file changed during a sync was ever placed; `Err` means exit 2.
/// The daemon is stopped on every path; the root is never deleted.
pub fn run(cli: &Cli) -> Result<i32> {
    let started_at = Instant::now();
    // Tools and the rounds file first: a mistake there leaves no
    // directories behind.
    let tools = Tools::locate(
        cli.irsync.as_deref(),
        cli.filegen.as_deref(),
        cli.fileset.as_deref(),
    )?;
    let fixed = match &cli.replay_rounds {
        Some(p) => read_rounds(p)?,
        None => Vec::new(),
    };
    let layout = Layout::create(&cli.root)?;
    let seed = cli.seed.unwrap_or_else(rand::random);
    let started = jiff::Timestamp::now().to_string();
    let mut run_file = new_run_file(cli, &tools, seed, &started);
    run_file.write(&layout.run_file)?;
    let clones = clones_here(&layout)?;

    let pid_slot = signals::install();
    let daemon_env = [(
        OsString::from(FILE_REPORT_ENV),
        layout.daemon_report().into_os_string(),
    )];
    let (daemon, syncer) =
        start_daemon(cli, &layout, &tools, pid_slot, &mut run_file, &daemon_env)?;
    println!(
        "{}\n  clones:    {}",
        report::header(
            &layout.root,
            seed,
            cli.direction,
            &daemon.startup.endpoint_id,
            &daemon.startup.address,
        ),
        if clones { "yes" } else { "no" }
    );
    ping(&daemon, &layout, seed, &started, |log| {
        syncer.run_with_env(log, &[])
    })?;
    // filegen takes over whatever the tree holds, the ping file too, and
    // the judge ignores that name: take it out before the fill. Round 0's
    // sync deletes it from the destination.
    let ping_file = layout.ping_file();
    fs::remove_file(&ping_file).with_context(|| format!("removing {}", ping_file.display()))?;

    let env = RoundEnv {
        cli,
        layout: &layout,
        tools: &tools,
        seed,
        filegen_opts: FilegenOpts {
            steps: cli.steps,
            max_total_size: cli.max_total_size.as_deref(),
            max_file_size: cli.max_file_size.as_deref(),
            sets: &cli.filegen_set,
        },
        rerun: &|| rerun_command("irsync-stress", &[], cli, seed, &layout.rounds_file()),
    };
    run_filegen(
        &tools,
        &layout,
        seed,
        &filegen::fill_args(
            seed,
            &layout.filegen_dir,
            &layout.src_data,
            &env.filegen_opts,
        ),
    )?;
    fileset::init(&tools.fileset.path, &layout.src_data)?;

    let daemon = RefCell::new(daemon);
    let quiet0 = |log: &Path| synced(&daemon, &syncer, log, &[], "iteration 0 quiet");
    let first = quiet_check(&env, 0, &quiet0)?;
    println!("{}", first.line());
    if !first.passed() {
        bail!(
            "the quiet sync before the first round failed; see {} and {}",
            layout.quiet_client_log(0).display(),
            layout.check_report(0).display()
        );
    }

    let mut offset = 0;
    let mut results = Vec::new();
    for i in 1..=cli.iterations {
        let report_env = [(
            OsString::from(FILE_REPORT_ENV),
            layout.client_report(i).into_os_string(),
        )];
        let marker = format!("iteration {i}");
        let quiet_marker = format!("iteration {i} quiet");
        let sync = |log: &Path| synced(&daemon, &syncer, log, &report_env, &marker);
        let quiet = |log: &Path| synced(&daemon, &syncer, log, &[], &quiet_marker);
        let fixed_round = fixed.iter().find(|r| r.round == i);
        let r = round(&env, i, fixed_round, &sync, &quiet, &mut offset)?;
        println!("{}", r.line());
        let passed = r.passed();
        if !passed {
            let kept = keep_failed_round(&layout, i)?;
            print_failure(&r, &layout, &(env.rerun)(), &kept);
        }
        results.push(r);
        if !passed && !cli.keep_going {
            break;
        }
    }

    drop(daemon); // stops it, then clears the signal handler's pid slot
    println!(
        "{}",
        report::concurrent_footer(&results, started_at.elapsed())
    );
    let code = report::concurrent_exit_code(&results);
    if code == 4 {
        println!("concurrent writes did not bite: no file changed during a sync was placed");
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn draw_pace_is_repeatable() {
        let mut kinds = BTreeSet::new();
        for round in 0..300 {
            let p = draw_pace(42, round);
            assert_eq!(p, draw_pace(42, round));
            kinds.insert(match p {
                Pace::Fast => "fast",
                Pace::Steady { per_sec } => {
                    assert!((5..=500).contains(&per_sec), "{p}");
                    "steady"
                }
                Pace::Bursts { on_ms, off_ms } => {
                    assert!((50..=2000).contains(&on_ms), "{p}");
                    assert!((50..=2000).contains(&off_ms), "{p}");
                    "bursts"
                }
            });
        }
        assert_eq!(kinds.len(), 3);
        // Another seed gives another sequence.
        let a: Vec<Pace> = (0..20).map(|r| draw_pace(42, r)).collect();
        let b: Vec<Pace> = (0..20).map(|r| draw_pace(43, r)).collect();
        assert_ne!(a, b);
    }

    #[test]
    fn rounds_file_round_trips() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("rounds.tsv");
        assert!(read_rounds(&p).is_err(), "a missing file is an error");
        let recs = [
            RoundRecord {
                round: 1,
                pace: Pace::Fast,
                steps: 0,
            },
            RoundRecord {
                round: 2,
                pace: Pace::Steady { per_sec: 17 },
                steps: 1234,
            },
            RoundRecord {
                round: 3,
                pace: Pace::Bursts {
                    on_ms: 50,
                    off_ms: 2000,
                },
                steps: 7,
            },
        ];
        for r in &recs {
            append_round(&p, r).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "1\tfast\t0\n2\tsteady:17\t1234\n3\tbursts:50:2000\t7\n"
        );
        assert_eq!(read_rounds(&p).unwrap(), recs);
        std::fs::write(&p, "1\tfast\n").unwrap();
        assert!(read_rounds(&p).is_err());
    }

    #[test]
    fn read_report_from_offset() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("report.jsonl");
        assert_eq!(read_report(&p, 0).unwrap(), (vec![], 0));
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&p)
            .unwrap();
        f.write_all(
            b"{\"event\":\"retried\",\"path_hex\":\"612f62\",\"path\":\"a/b\",\"reason\":\"changed\"}\n",
        )
        .unwrap();
        let (first, off) = read_report(&p, 0).unwrap();
        assert_eq!(
            first,
            [ReportLine {
                event: "retried".into(),
                path: b"a/b".to_vec(),
                reason: "changed".into(),
            }]
        );
        assert_eq!(off, std::fs::metadata(&p).unwrap().len());
        f.write_all(
            b"{\"event\":\"gave_up\",\"path_hex\":\"ff00\",\"path\":\"\\ufffd\\u0000\",\"reason\":\"x \\\"y\\\"\"}\n\
              {\"event\":\"gave_up\",\"path_hex\":\"\",\"path\":\"\",\"reason\":\"session\"}\n",
        )
        .unwrap();
        let (next, off2) = read_report(&p, off).unwrap();
        assert_eq!(
            next,
            [
                ReportLine {
                    event: "gave_up".into(),
                    path: vec![0xff, 0x00],
                    reason: "x \"y\"".into(),
                },
                ReportLine {
                    event: "gave_up".into(),
                    path: vec![],
                    reason: "session".into(),
                }
            ]
        );
        assert_eq!(off2, std::fs::metadata(&p).unwrap().len());
        assert_eq!(read_report(&p, off2).unwrap(), (vec![], off2));
        std::fs::write(&p, "not json\n").unwrap();
        assert!(read_report(&p, 0).is_err());
    }

    fn gave_up(path: &[u8], reason: &str) -> ReportLine {
        ReportLine {
            event: "gave_up".into(),
            path: path.to_vec(),
            reason: reason.into(),
        }
    }

    #[test]
    fn deletions_skipped_on_the_root_passes_only_beside_another_path() {
        let touched: BTreeSet<Vec<u8>> = [b"d/f".to_vec()].into();
        let skipped = "receiver: deletions skipped: a file listing was incomplete \
                       (--ignore-errors deletes anyway)";
        let lines = [gave_up(b"d/f", "walk: gone"), gave_up(b"", skipped)];
        let (all, untouched) = gave_up_split(&lines, &touched);
        assert_eq!(all, [b"d/f".to_vec(), vec![]]);
        assert!(untouched.is_empty());
        // Alone it is a failed session.
        let (_, untouched) = gave_up_split(&lines[1..], &touched);
        assert_eq!(untouched, [Vec::<u8>::new()]);
        let lines = [
            gave_up(b"d/f", "walk: gone"),
            gave_up(b"", "the sender's listing is incomplete"),
        ];
        assert!(gave_up_split(&lines, &touched).1.is_empty());
        // Only irsync's exact words count.
        let lines = [
            gave_up(b"d/f", "walk: gone"),
            gave_up(b"", "deletions skipped: a file listing was incomplete"),
        ];
        assert_eq!(gave_up_split(&lines, &touched).1, [Vec::<u8>::new()]);
        // Any other root error fails.
        let lines = [gave_up(b"d/f", "walk: gone"), gave_up(b"", "session lost")];
        let (_, untouched) = gave_up_split(&lines, &touched);
        assert_eq!(untouched, [Vec::<u8>::new()]);
    }

    #[test]
    fn only_the_parts_of_given_up_paths_the_source_lost_are_removed() {
        let t = tempfile::tempdir().unwrap();
        let (src, dest) = (t.path().join("src"), t.path().join("dest"));
        std::fs::create_dir_all(src.join("d")).unwrap();
        std::fs::create_dir_all(dest.join("d")).unwrap();
        std::fs::write(src.join("d/kept"), b"x").unwrap();
        for p in [
            "d/.irsync.gone.part",
            "d/.irsync.kept.part",
            ".irsync.other.part",
        ] {
            std::fs::write(dest.join(p), b"partial").unwrap();
        }
        let gave_up = [
            b"d/gone".to_vec(),
            b"d/kept".to_vec(),
            b"d/never-staged".to_vec(),
            vec![],
        ];
        let removed = remove_orphan_parts(&src, &dest, &gave_up).unwrap();
        assert_eq!(removed, [b"d/.irsync.gone.part".to_vec()]);
        assert!(!dest.join("d/.irsync.gone.part").exists());
        assert!(dest.join("d/.irsync.kept.part").exists());
        assert!(dest.join(".irsync.other.part").exists());
    }

    #[test]
    fn a_failed_round_moves_out_of_the_next_rounds_way() {
        let t = tempfile::tempdir().unwrap();
        let layout = Layout::create(&t.path().join("r")).unwrap();
        let round = layout.round_dir();
        std::fs::create_dir_all(round.join("start")).unwrap();
        std::fs::write(round.join("state.yaml"), b"state").unwrap();
        let kept = keep_failed_round(&layout, 3).unwrap();
        assert_eq!(kept, layout.root.join("round-0003"));
        assert!(kept.join("start").is_dir());
        assert_eq!(std::fs::read(kept.join("state.yaml")).unwrap(), b"state");
        assert!(!round.exists());
        // The next round starts afresh and leaves the kept one alone.
        fresh_round_dir(&round).unwrap();
        assert!(round.is_dir() && kept.join("state.yaml").exists());
    }

    #[test]
    fn filegen_ran_counts_the_steps_that_did_nothing() {
        let t = tempfile::tempdir().unwrap();
        let log = t.path().join("filegen-7.log");
        let earlier = "# end steps_done=40 outcome=ok elapsed_ms=3\n";
        std::fs::write(
            &log,
            format!(
                "{earlier}# run seed=7 start_step=40 steps=18446744073709551615 log=\"x\"\n\
                 step=41 ts=2026-11-12T00:00:00Z op=create_file path=\"a\" bytes=1 size=1 content=random post=f:1:664:1\n\
                 step=42 ts=2026-11-12T00:00:00Z op=create_file path=\"b\" bytes=1 size=1 content=random post=f:1:664:1\n\
                 # end steps_done=42 outcome=\"interrupted: SIGTERM\" elapsed_ms=9 skipped=3\n\
                 # stat op=mkdir count=2\n"
            ),
        )
        .unwrap();
        let ran = filegen_ran(&log, earlier.len() as u64).unwrap();
        assert_eq!(ran.lines.len(), 2);
        assert_eq!(ran.steps, 5);
        // Killed before it began: nothing logged, no step.
        let empty = std::fs::metadata(&log).unwrap().len();
        let ran = filegen_ran(&log, empty).unwrap();
        assert_eq!((ran.lines.len(), ran.steps), (0, 0));
        // Steps without an end line: the count is unknown.
        std::fs::write(
            &log,
            "# run seed=7 start_step=0 steps=5\n\
             step=1 ts=2026-11-12T00:00:00Z op=create_file path=\"a\" bytes=1 size=1 content=random post=f:1:664:1\n",
        )
        .unwrap();
        assert!(filegen_ran(&log, 0).is_err());
    }

    #[test]
    fn gave_up_paths_are_split_by_touched() {
        let touched: BTreeSet<Vec<u8>> = [b"a".to_vec(), b"b/c".to_vec()].into();
        let lines = [
            ("gave_up", &b"a"[..]),
            ("retried", b"x"),
            ("gave_up", b"y"),
            ("vanished", b"z"),
            ("gave_up", b""),
        ]
        .map(|(e, p)| ReportLine {
            event: e.into(),
            path: p.to_vec(),
            reason: String::new(),
        });
        let (gave_up, untouched) = gave_up_split(&lines, &touched);
        assert_eq!(gave_up, [b"a".to_vec(), b"y".to_vec(), vec![]]);
        assert_eq!(untouched, [b"y".to_vec(), vec![]]);
    }

    #[test]
    fn rerun_command_has_a_fresh_root_and_the_rounds_file() {
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "irsync-stress",
            "--direction",
            "pull",
            "--iterations",
            "5",
            "--max-total-size",
            "8M",
            "--filegen-set",
            "a=1 b",
            "--concurrent",
            "--pace",
            "fast",
            "/r/run",
            "--",
            "--paranoid",
        ])
        .unwrap();
        let c = rerun_command(
            "irsync-stress",
            &[],
            &cli,
            99,
            Path::new("/r/run/logs/rounds.tsv"),
        );
        assert_eq!(
            c,
            "irsync-stress --direction pull --iterations 5 --seed 99 --steps 100 \
             --max-total-size 8M --filegen-set 'a=1 b' --log-level debug --concurrent \
             --pace fast --replay-rounds /r/run/logs/rounds.tsv /r/run.rerun -- --paranoid"
        );
    }
}
