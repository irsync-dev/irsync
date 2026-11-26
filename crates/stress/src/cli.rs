//! Command line of `irsync-stress`, and the flags `irsync-netstress`
//! shares with it.

use std::ffi::OsString;
use std::path::PathBuf;

use ::filegen::pace::Pace;
use clap::{Parser, ValueEnum};

/// Which way the irsync client moves data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Direction {
    Push,
    Pull,
}

/// The flags of a stress run, shared by `irsync-stress` and
/// `irsync-netstress`. What is left out each tool fills in with
/// [`RunArgs::resolve`]: `irsync-stress` with [`STRESS_DEFAULTS`],
/// `irsync-netstress` with its profile's.
#[derive(Debug, Clone, clap::Args)]
pub struct RunArgs {
    /// Working directory root for all iterations; created if missing, and
    /// it must be empty if it exists.
    pub root: PathBuf,
    /// Which way the irsync client moves data: `push` syncs ROOT/SRC to
    /// the daemon's ROOT/DST, `pull` the other way.
    #[arg(long, value_enum)]
    pub direction: Direction,
    /// Sync-and-verify rounds (default: irsync-stress 10,
    /// irsync-netstress the profile's).
    #[arg(long, value_name = "N")]
    pub iterations: Option<u64>,
    /// Seed for filegen (in irsync-netstress also for the network's random
    /// choices), printed in the header (default: random).
    #[arg(long, value_name = "S")]
    pub seed: Option<u64>,
    /// filegen operations per fill or mutation (default: irsync-stress
    /// 100, irsync-netstress the profile's).
    #[arg(long, value_name = "N")]
    pub steps: Option<u64>,
    /// Passed through to filegen verbatim (default: filegen's, or the
    /// profile's for irsync-netstress).
    #[arg(long, value_name = "SIZE")]
    pub max_total_size: Option<String>,
    /// Passed through to filegen verbatim (default: filegen's, or the
    /// profile's for irsync-netstress).
    #[arg(long, value_name = "SIZE")]
    pub max_file_size: Option<String>,
    /// Extra filegen `--set KEY=VALUE`; repeatable.
    #[arg(long, value_name = "KEY=VALUE")]
    pub filegen_set: Vec<String>,
    /// Log level of the daemon and the client, as RUST_LOG takes it.
    #[arg(long, value_name = "LEVEL", default_value = "debug")]
    pub log_level: String,
    /// Carry on after a failed iteration instead of stopping.
    #[arg(long)]
    pub keep_going: bool,
    /// The irsync binary to run (default: the one beside this program,
    /// else on PATH).
    #[arg(long, value_name = "PATH")]
    pub irsync: Option<PathBuf>,
    /// The filegen binary to run (default: as for --irsync).
    #[arg(long, value_name = "PATH")]
    pub filegen: Option<PathBuf>,
    /// The fileset binary to run (default: as for --irsync).
    #[arg(long, value_name = "PATH")]
    pub fileset: Option<PathBuf>,
    /// Run filegen against the source while each sync runs, then judge
    /// the destination against every state the source held.
    #[arg(long)]
    pub concurrent: bool,
    /// filegen's pace for every round (fast, steady:N or bursts:ON:OFF);
    /// drawn from the seed per round when not given.
    #[arg(long, value_name = "PACE", requires = "concurrent")]
    pub pace: Option<Pace>,
    /// A run's `logs/rounds.tsv`: replay its paces and step counts.
    #[arg(long, value_name = "FILE", requires = "concurrent")]
    pub replay_rounds: Option<PathBuf>,
    /// Extra arguments for the irsync client, after `--`.
    #[arg(last = true, allow_hyphen_values = true)]
    pub client_args: Vec<OsString>,
}

/// What a run uses where [`RunArgs`] leaves a setting out.
#[derive(Debug, Clone, Copy)]
pub struct Defaults {
    pub iterations: u64,
    pub steps: u64,
    /// `None`: filegen's own.
    pub max_total_size: Option<&'static str>,
    pub max_file_size: Option<&'static str>,
}

/// `irsync-stress`'s defaults.
pub const STRESS_DEFAULTS: Defaults = Defaults {
    iterations: 10,
    steps: 100,
    max_total_size: None,
    max_file_size: None,
};

impl RunArgs {
    /// The run these flags ask for, with `d` where they leave a setting
    /// out.
    pub fn resolve(&self, d: Defaults) -> Cli {
        let or = |given: &Option<String>, default: Option<&str>| {
            given.clone().or_else(|| default.map(str::to_string))
        };
        Cli {
            root: self.root.clone(),
            direction: self.direction,
            iterations: self.iterations.unwrap_or(d.iterations),
            seed: self.seed,
            steps: self.steps.unwrap_or(d.steps),
            max_total_size: or(&self.max_total_size, d.max_total_size),
            max_file_size: or(&self.max_file_size, d.max_file_size),
            filegen_set: self.filegen_set.clone(),
            log_level: self.log_level.clone(),
            keep_going: self.keep_going,
            irsync: self.irsync.clone(),
            filegen: self.filegen.clone(),
            fileset: self.fileset.clone(),
            concurrent: self.concurrent,
            pace: self.pace,
            replay_rounds: self.replay_rounds.clone(),
            client_args: self.client_args.clone(),
        }
    }
}

// `irsync-stress`'s command line; the doc line below is its --help.
/// Fill a tree with filegen, sync it with irsync, check the copy with
/// fileset, change the tree and sync again, for many rounds.
#[derive(Debug, Parser)]
#[command(name = "irsync-stress", version)]
pub struct Args {
    #[command(flatten)]
    pub run: RunArgs,
}

impl Args {
    /// The run, with irsync-stress's defaults.
    pub fn into_cli(self) -> Cli {
        self.run.resolve(STRESS_DEFAULTS)
    }
}

/// One stress run with every setting decided: what the run code reads.
#[derive(Debug, Clone)]
pub struct Cli {
    pub root: PathBuf,
    pub direction: Direction,
    pub iterations: u64,
    pub seed: Option<u64>,
    pub steps: u64,
    pub max_total_size: Option<String>,
    pub max_file_size: Option<String>,
    pub filegen_set: Vec<String>,
    pub log_level: String,
    pub keep_going: bool,
    pub irsync: Option<PathBuf>,
    pub filegen: Option<PathBuf>,
    pub fileset: Option<PathBuf>,
    pub concurrent: bool,
    pub pace: Option<Pace>,
    pub replay_rounds: Option<PathBuf>,
    pub client_args: Vec<OsString>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse<I, T>(args: I) -> Result<Cli, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        Args::try_parse_from(args).map(Args::into_cli)
    }

    #[test]
    fn defaults_and_passthrough() {
        let c = parse([
            "irsync-stress",
            "--direction",
            "push",
            "/r",
            "--",
            "--parallel",
            "4",
            "--stats",
        ])
        .unwrap();
        assert_eq!(c.iterations, 10);
        assert_eq!(c.steps, 100);
        assert_eq!(c.log_level, "debug");
        assert!(c.seed.is_none() && c.max_total_size.is_none() && !c.keep_going);
        assert_eq!(c.client_args, ["--parallel", "4", "--stats"]);
    }

    #[test]
    fn concurrent_flags_need_concurrent() {
        let base = ["irsync-stress", "--direction", "push"];
        let parse = |extra: &[&str]| {
            let mut a = base.to_vec();
            a.extend(extra);
            a.push("/r");
            parse(a)
        };
        assert!(parse(&["--pace", "fast"]).is_err());
        assert!(parse(&["--replay-rounds", "/x/rounds.tsv"]).is_err());
        let c = parse(&["--concurrent"]).unwrap();
        assert!(c.concurrent && c.pace.is_none() && c.replay_rounds.is_none());
        let c = parse(&[
            "--concurrent",
            "--pace",
            "bursts:100:200",
            "--replay-rounds",
            "/x/rounds.tsv",
        ])
        .unwrap();
        assert_eq!(c.pace.unwrap().to_string(), "bursts:100:200");
        assert_eq!(c.replay_rounds.unwrap(), PathBuf::from("/x/rounds.tsv"));
        assert!(parse(&["--concurrent", "--pace", "slow"]).is_err());
        assert!(!parse(&[]).unwrap().concurrent);
    }

    #[test]
    fn direction_is_required() {
        assert!(parse(["irsync-stress", "/r"]).is_err());
    }

    #[test]
    fn filegen_set_repeats_and_sizes_are_verbatim() {
        let c = parse([
            "irsync-stress",
            "--direction",
            "pull",
            "--filegen-set",
            "a=1",
            "--filegen-set",
            "b=2",
            "--max-total-size",
            "8M",
            "/r",
        ])
        .unwrap();
        assert_eq!(c.filegen_set, ["a=1", "b=2"]);
        assert_eq!(c.max_total_size.as_deref(), Some("8M"));
        assert!(matches!(c.direction, Direction::Pull));
    }
}
