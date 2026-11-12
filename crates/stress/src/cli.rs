//! Command line of `irsync-stress`.

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

#[derive(Debug, Parser)]
#[command(name = "irsync-stress", version)]
pub struct Cli {
    /// Working directory root for all iterations.
    pub root: PathBuf,
    #[arg(long, value_enum)]
    pub direction: Direction,
    #[arg(long, default_value_t = 10)]
    pub iterations: u64,
    #[arg(long)]
    pub seed: Option<u64>,
    #[arg(long, default_value_t = 100)]
    pub steps: u64,
    /// Passed through to filegen verbatim.
    #[arg(long)]
    pub max_total_size: Option<String>,
    /// Passed through to filegen verbatim.
    #[arg(long)]
    pub max_file_size: Option<String>,
    /// Extra filegen `--set KEY=VALUE`; repeatable.
    #[arg(long, value_name = "KEY=VALUE")]
    pub filegen_set: Vec<String>,
    #[arg(long, default_value = "debug")]
    pub log_level: String,
    #[arg(long)]
    pub keep_going: bool,
    #[arg(long)]
    pub irsync: Option<PathBuf>,
    #[arg(long)]
    pub filegen: Option<PathBuf>,
    #[arg(long)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_passthrough() {
        let c = Cli::try_parse_from([
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
            Cli::try_parse_from(a)
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
        assert!(Cli::try_parse_from(["irsync-stress", "/r"]).is_err());
    }

    #[test]
    fn filegen_set_repeats_and_sizes_are_verbatim() {
        let c = Cli::try_parse_from([
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
