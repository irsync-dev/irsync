//! Command line of `irsync-netstress`.

use clap::Parser;
use stress::cli::{Defaults, RunArgs};

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
    #[command(flatten)]
    pub run: RunArgs,
    /// Network profile to run under; it also gives the defaults of
    /// --iterations, --steps, --max-total-size and --max-file-size.
    #[arg(long, value_enum)]
    pub profile: Profile,
    /// QUIC congestion controller for both sides (written to the client's
    /// `[defaults]` and the daemon's `[daemon]` config). Unset: irsync's
    /// default.
    #[arg(long, value_enum)]
    pub congestion: Option<Congestion>,
    /// The ACKs both sides ask of each other (passed to the fault layer).
    /// Unset: irsync's own (immediate).
    #[arg(long, value_enum)]
    pub acks: Option<Acks>,
}

impl Cli {
    /// The equivalent `irsync-stress` run, with whatever was not given
    /// filled in from the profile.
    pub fn to_stress(&self) -> stress::cli::Cli {
        let d = self.profile.defaults();
        self.run.resolve(Defaults {
            iterations: d.iterations,
            steps: d.steps,
            max_total_size: Some(d.max_total_size),
            max_file_size: Some(d.max_file_size),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
