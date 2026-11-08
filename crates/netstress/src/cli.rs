//! Command line of `irsync-netstress`.

use std::ffi::OsString;
use std::path::PathBuf;

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
