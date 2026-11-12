//! irsync-stress's report with the network added: the profile in the
//! header, the outages per iteration and in total, and exit code 3.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use stress::cli::Direction;
use stress::report as base;

use crate::netlog::NetEvents;

pub struct NetIteration {
    pub base: base::Iteration,
    pub events: NetEvents,
    /// Why the client log fails the iteration, if it does.
    pub verdict: Option<String>,
}

impl NetIteration {
    /// The stress checks passed and the client log is fine.
    pub fn passed(&self) -> bool {
        self.base.passed() && self.verdict.is_none()
    }

    pub fn line(&self) -> String {
        format!("{}  outages {}", self.base.line(), self.events.dead_zones)
    }
}

/// 1 if an iteration failed or a partial file was left behind; else 3 if
/// the profile has outages but none happened; else 0.
pub fn exit_code(
    iterations: &[NetIteration],
    leftover_parts: &[PathBuf],
    has_outages: bool,
) -> i32 {
    if !iterations.iter().all(NetIteration::passed) || !leftover_parts.is_empty() {
        1
    } else if has_outages && iterations.iter().all(|i| i.events.dead_zones == 0) {
        3
    } else {
        0
    }
}

/// A concurrent round with what the network did during its concurrent
/// sync.
pub struct NetConcurrentRound {
    pub base: base::ConcurrentRound,
    pub events: NetEvents,
    /// Why the concurrent sync's client log fails the round, if it does.
    pub verdict: Option<String>,
}

impl NetConcurrentRound {
    /// The concurrent checks passed and the client log is fine.
    pub fn passed(&self) -> bool {
        self.base.passed() && self.verdict.is_none()
    }

    pub fn line(&self) -> String {
        format!("{}  outages {}", self.base.line(), self.events.dead_zones)
    }
}

/// 1 if a round failed; else 3 if the profile has outages but none
/// happened; else 4 if no changed file was placed; else 0.
pub fn concurrent_exit_code(rounds: &[NetConcurrentRound], has_outages: bool) -> i32 {
    if !rounds.iter().all(NetConcurrentRound::passed) {
        1
    } else if has_outages && rounds.iter().all(|r| r.events.dead_zones == 0) {
        3
    } else if rounds
        .iter()
        .all(|r| r.base.judgement.changed_and_held == 0)
    {
        4
    } else {
        0
    }
}

/// The concurrent footer, counting the network verdict too, plus the
/// total number of outages.
pub fn concurrent_footer(rounds: &[NetConcurrentRound], total: Duration) -> String {
    let passed = rounds.iter().filter(|r| r.passed()).count();
    let outages: u32 = rounds.iter().map(|r| r.events.dead_zones).sum();
    format!(
        "{} outages={outages}",
        base::concurrent_footer_of(rounds.iter().map(|r| &r.base), passed, total)
    )
}

pub fn header(
    root: &Path,
    seed: u64,
    direction: Direction,
    endpoint_id: &str,
    address: &SocketAddr,
    profile: &str,
) -> String {
    let h = base::header(root, seed, direction, endpoint_id, address);
    format!(
        "{}\n  network:   {profile}",
        h.replacen("irsync-stress", "irsync-netstress", 1)
    )
}

/// irsync-stress's footer, counting the network verdict too, plus the
/// total number of outages.
pub fn footer(iterations: &[NetIteration], total: Duration) -> String {
    let passed = iterations.iter().filter(|i| i.passed()).count();
    let outages: u32 = iterations.iter().map(|i| i.events.dead_zones).sum();
    format!(
        "passed={} failed={} elapsed={:.1}s outages={}",
        passed,
        iterations.len() - passed,
        total.as_secs_f64(),
        outages
    )
}

#[cfg(test)]
mod tests {
    use stress::client::{SyncExit, SyncResult};
    use stress::fileset::{CheckCounts, CheckOutcome};

    use super::*;

    fn it(n: u64, dead_zones: u32, verdict: Option<&str>) -> NetIteration {
        NetIteration {
            base: base::Iteration {
                number: n,
                total: 2,
                sync: SyncResult {
                    exit: SyncExit::Ok,
                    elapsed: Duration::from_millis(1_500),
                },
                manifest_ok: true,
                check: CheckOutcome::Clean(CheckCounts::default()),
            },
            events: NetEvents {
                active: true,
                dead_zones,
                resumed: dead_zones,
                stray_reconnects: 0,
            },
            verdict: verdict.map(str::to_owned),
        }
    }

    #[test]
    fn line_adds_the_outages() {
        assert_eq!(
            it(1, 1, None).line(),
            "iter 0001/0002  sync 1.5s  exit 0    manifest ok  check ok  outages 1"
        );
    }

    #[test]
    fn a_verdict_fails_the_iteration_and_counts_in_the_footer() {
        let its = [it(1, 1, None), it(2, 1, Some("bad"))];
        assert!(!its[1].passed());
        assert_eq!(
            footer(&its, Duration::from_millis(1_250)),
            "passed=1 failed=1 elapsed=1.2s outages=2"
        );
        assert_eq!(exit_code(&its, &[], true), 1);
    }

    #[test]
    fn exit_codes() {
        assert_eq!(exit_code(&[it(1, 1, None), it(2, 0, None)], &[], true), 0);
        assert_eq!(exit_code(&[it(1, 0, None)], &[], true), 3);
        assert_eq!(exit_code(&[it(1, 0, None)], &[], false), 0);
        let part = [PathBuf::from("/d/.irsync.x.part")];
        assert_eq!(exit_code(&[it(1, 0, None)], &part, true), 1);
    }

    #[test]
    fn header_names_the_network() {
        let addr: SocketAddr = "127.0.0.1:4000".parse().unwrap();
        let h = header(
            Path::new("/r"),
            42,
            Direction::Pull,
            "ep1",
            &addr,
            "dsl-video",
        );
        assert!(h.starts_with("irsync-netstress\n"), "{h}");
        assert!(h.ends_with("\n  network:   dsl-video"), "{h}");
        assert!(h.contains("127.0.0.1:4000"), "{h}");
    }

    fn round(n: u64, dead_zones: u32, held: u64, verdict: Option<&str>) -> NetConcurrentRound {
        let mut base = stress::report::ConcurrentRound {
            number: n,
            total: 2,
            pace: filegen::pace::Pace::Fast,
            steps: 1,
            sync: SyncResult {
                exit: SyncExit::Ok,
                elapsed: Duration::from_millis(1_500),
            },
            retried: 0,
            gave_up: vec![],
            gave_up_untouched: vec![],
            judgement: Default::default(),
            quiet: it(n, 0, None).base,
        };
        base.judgement.changed_and_held = held;
        NetConcurrentRound {
            base,
            events: it(n, dead_zones, None).events,
            verdict: verdict.map(str::to_owned),
        }
    }

    #[test]
    fn concurrent_exit_codes() {
        assert_eq!(concurrent_exit_code(&[round(1, 1, 2, None)], true), 0);
        assert_eq!(concurrent_exit_code(&[round(1, 1, 2, Some("x"))], true), 1);
        assert_eq!(concurrent_exit_code(&[round(1, 0, 2, None)], true), 3);
        assert_eq!(concurrent_exit_code(&[round(1, 0, 0, None)], false), 4);
        assert_eq!(concurrent_exit_code(&[round(1, 1, 0, None)], true), 4);
        assert_eq!(concurrent_exit_code(&[round(1, 0, 2, None)], false), 0);
        let rs = [round(1, 1, 1, None), round(2, 2, 1, Some("bad"))];
        assert!(!rs[1].passed());
        let f = concurrent_footer(&rs, Duration::from_millis(1_250));
        assert!(f.starts_with("passed=1 failed=1 "), "{f}");
        assert!(f.ends_with("elapsed=1.2s outages=3"), "{f}");
    }
}
