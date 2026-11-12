//! Command line of `irsync-netstress`.

use std::ffi::OsString;
use std::path::PathBuf;

use ::filegen::pace::Pace;
use clap::Parser;
use stress::cli::Direction;

use crate::profile::Profile;

/// irsync's `congestion` setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Congestion {
    Bbr,
    Cubic,
}

impl Congestion {
    /// The name irsync's config files use.
    pub fn name(self) -> &'static str {
        match self {
            Congestion::Bbr => "bbr",
            Congestion::Cubic => "cubic",
        }
    }
}

/// The ACKs each side asks of the other; fault build only, for measuring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Acks {
    /// irsync's own: every packet, within 1 ms.
    Immediate,
    /// Every second packet, within 1 ms.
    Gentle,
    /// Nothing asked: the peer's delayed ACKs (every second packet or 25 ms).
    PeerDefault,
}

impl Acks {
    /// The name the fault layer's `acks=` takes.
    pub fn name(self) -> &'static str {
        match self {
            Acks::Immediate => "immediate",
            Acks::Gentle => "gentle",
            Acks::PeerDefault => "peer-default",
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "irsync-netstress", version)]
pub struct Cli {
    /// Working directory root for all iterations.
    pub root: PathBuf,
    #[arg(long, value_enum)]
    pub direction: Direction,
    /// Network profile to run under.
    #[arg(long, value_enum)]
    pub profile: Profile,
    /// Defaults to the profile's.
    #[arg(long)]
    pub iterations: Option<u64>,
    #[arg(long)]
    pub seed: Option<u64>,
    /// Defaults to the profile's.
    #[arg(long)]
    pub steps: Option<u64>,
    /// Passed through to filegen verbatim. Defaults to the profile's.
    #[arg(long)]
    pub max_total_size: Option<String>,
    /// Passed through to filegen verbatim. Defaults to the profile's.
    #[arg(long)]
    pub max_file_size: Option<String>,
    /// Extra filegen `--set KEY=VALUE`; repeatable.
    #[arg(long, value_name = "KEY=VALUE")]
    pub filegen_set: Vec<String>,
    #[arg(long, default_value = "debug")]
    pub log_level: String,
    #[arg(long)]
    pub keep_going: bool,
    /// QUIC congestion controller for both sides (written to the client's
    /// `[client]` and the daemon's `[daemon]` config). Unset: irsync's
    /// default.
    #[arg(long, value_enum)]
    pub congestion: Option<Congestion>,
    /// The ACKs both sides ask of each other (passed to the fault layer).
    /// Unset: irsync's own (immediate).
    #[arg(long, value_enum)]
    pub acks: Option<Acks>,
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
    #[arg(long)]
    pub irsync: Option<PathBuf>,
    #[arg(long)]
    pub filegen: Option<PathBuf>,
    #[arg(long)]
    pub fileset: Option<PathBuf>,
    /// Extra arguments for the irsync client, after `--`.
    #[arg(last = true, allow_hyphen_values = true)]
    pub client_args: Vec<OsString>,
}

impl Cli {
    /// The equivalent `irsync-stress` command line, with whatever was not
    /// given filled in from the profile.
    pub fn to_stress(&self) -> stress::cli::Cli {
        let d = self.profile.defaults();
        stress::cli::Cli {
            root: self.root.clone(),
            direction: self.direction,
            iterations: self.iterations.unwrap_or(d.iterations),
            seed: self.seed,
            steps: self.steps.unwrap_or(d.steps),
            max_total_size: Some(
                self.max_total_size
                    .clone()
                    .unwrap_or_else(|| d.max_total_size.to_string()),
            ),
            max_file_size: Some(
                self.max_file_size
                    .clone()
                    .unwrap_or_else(|| d.max_file_size.to_string()),
            ),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_defaults_fill_what_was_not_given() {
        let c = Cli::try_parse_from([
            "irsync-netstress",
            "--profile",
            "dsl-video",
            "--direction",
            "push",
            "/r",
        ])
        .unwrap();
        let s = c.to_stress();
        assert_eq!((s.iterations, s.steps), (10, 100));
        assert_eq!(
            (s.max_total_size.as_deref(), s.max_file_size.as_deref()),
            (Some("32M"), Some("4M"))
        );
    }

    #[test]
    fn congestion_is_unset_unless_given() {
        let base = [
            "irsync-netstress",
            "--profile",
            "dsl-video",
            "--direction",
            "push",
        ];
        let c = Cli::try_parse_from(base.iter().chain(&["/r"])).unwrap();
        assert_eq!(c.congestion, None);
        let c = Cli::try_parse_from(base.iter().chain(&["--congestion", "cubic", "/r"])).unwrap();
        assert_eq!(c.congestion, Some(Congestion::Cubic));
    }

    #[test]
    fn concurrent_flags_map_and_need_concurrent() {
        let parse = |extra: &[&str]| {
            let mut a = vec![
                "irsync-netstress",
                "--profile",
                "test-blackout",
                "--direction",
                "push",
            ];
            a.extend(extra);
            a.push("/r");
            Cli::try_parse_from(a)
        };
        assert!(parse(&["--pace", "fast"]).is_err());
        assert!(parse(&["--replay-rounds", "/x/rounds.tsv"]).is_err());
        let s = parse(&[
            "--concurrent",
            "--pace",
            "steady:9",
            "--replay-rounds",
            "/x",
        ])
        .unwrap()
        .to_stress();
        assert!(s.concurrent);
        assert_eq!(s.pace.unwrap().to_string(), "steady:9");
        assert_eq!(s.replay_rounds.unwrap(), PathBuf::from("/x"));
        assert!(!parse(&[]).unwrap().to_stress().concurrent);
    }

    #[test]
    fn explicit_sizes_win() {
        let c = Cli::try_parse_from([
            "irsync-netstress",
            "--profile",
            "4g-vehicle",
            "--direction",
            "pull",
            "--iterations",
            "2",
            "--max-total-size",
            "1G",
            "/r",
        ])
        .unwrap();
        let s = c.to_stress();
        assert_eq!(s.iterations, 2);
        assert_eq!(s.max_total_size.as_deref(), Some("1G"));
        assert_eq!(s.max_file_size.as_deref(), Some("32M"));
    }

    #[test]
    fn profile_is_required_and_passthrough_works() {
        assert!(Cli::try_parse_from(["irsync-netstress", "--direction", "push", "/r"]).is_err());
        let c = Cli::try_parse_from([
            "irsync-netstress",
            "--profile",
            "test-blackout",
            "--direction",
            "push",
            "/r",
            "--",
            "--stats",
        ])
        .unwrap();
        assert_eq!(c.to_stress().client_args, ["--stats"]);
    }
}
