//! The per-iteration result, its one-line rendering, and the run's header,
//! footer and exit code.

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use ::filegen::pace::Pace;

use crate::cli::Direction;
use crate::client::{SyncExit, SyncResult};
use crate::fileset::CheckOutcome;
use crate::replay::RoundJudgement;

pub struct Iteration {
    pub number: u64,
    pub total: u64,
    pub sync: SyncResult,
    pub manifest_ok: bool,
    pub check: CheckOutcome,
}

impl Iteration {
    /// Sync exited 0, the manifests match and the fileset check is clean.
    pub fn passed(&self) -> bool {
        self.sync.exit == SyncExit::Ok
            && self.manifest_ok
            && matches!(self.check, CheckOutcome::Clean(_))
    }

    pub fn line(&self) -> String {
        format!(
            "iter {:04}/{:04}  sync {:.1}s  exit {:<3}  manifest {}  check {}",
            self.number,
            self.total,
            self.sync.elapsed.as_secs_f64(),
            exit_text(self.sync.exit),
            manifest_text(self.manifest_ok),
            check_text(&self.check)
        )
    }
}

fn exit_text(e: SyncExit) -> String {
    match e.code() {
        -1 => "sig".to_string(),
        code => code.to_string(),
    }
}

fn manifest_text(ok: bool) -> &'static str {
    if ok {
        "ok"
    } else {
        "MISMATCH"
    }
}

fn check_text(c: &CheckOutcome) -> String {
    match c {
        CheckOutcome::Clean(_) => "ok".to_string(),
        CheckOutcome::Differences(c) => format!(
            "FAILED (added={} deleted={} changed={})",
            c.added, c.deleted, c.changed
        ),
    }
}

/// 0 if every iteration passed, else 1.
pub fn exit_code(iterations: &[Iteration]) -> i32 {
    if iterations.iter().all(Iteration::passed) {
        0
    } else {
        1
    }
}

/// One round of the concurrent mode.
pub struct ConcurrentRound {
    pub number: u64,
    pub total: u64,
    pub pace: Pace,
    /// filegen's steps during the sync.
    pub steps: u64,
    /// The concurrent sync.
    pub sync: SyncResult,
    /// `retried` lines in the client's and the daemon's reports.
    pub retried: u64,
    /// Every path the client gave up on, in report order.
    pub gave_up: Vec<Vec<u8>>,
    /// Those of `gave_up` filegen did not touch during the round (the
    /// root path `""` never counts as touched).
    pub gave_up_untouched: Vec<Vec<u8>>,
    pub judgement: RoundJudgement,
    /// The quiet sync after the round, with today's exact-match checks.
    pub quiet: Iteration,
}

impl ConcurrentRound {
    /// The concurrent sync exited 0 or 24, or 23 having given up only on
    /// paths filegen touched (and on at least one: a 23 with no path is a
    /// failed session); the judge found nothing; the quiet sync passed.
    pub fn passed(&self) -> bool {
        let sync_ok = match self.sync.exit {
            SyncExit::Ok | SyncExit::Other(24) => true,
            SyncExit::Partial => !self.gave_up.is_empty() && self.gave_up_untouched.is_empty(),
            SyncExit::Other(_) => false,
        };
        sync_ok && self.judgement.failures.is_empty() && self.quiet.passed()
    }

    pub fn line(&self) -> String {
        let judge = match self.judgement.failures.len() {
            0 => "ok".to_string(),
            n => format!("FAILED ({n})"),
        };
        let q = &self.quiet;
        let quiet = format!(
            "{:.1}s exit {}  manifest {}  check {}",
            q.sync.elapsed.as_secs_f64(),
            exit_text(q.sync.exit),
            manifest_text(q.manifest_ok),
            check_text(&q.check)
        );
        format!(
            "iter {:04}/{:04}  pace {}  steps {}  sync {:.1}s  exit {:<3}  retried {}  gave-up {}/{}  touched {}  changed+held {}  judged {}  judge {}  quiet {}",
            self.number,
            self.total,
            self.pace,
            self.steps,
            self.sync.elapsed.as_secs_f64(),
            exit_text(self.sync.exit),
            self.retried,
            self.gave_up.len(),
            self.gave_up_untouched.len(),
            self.judgement.touched.len(),
            self.judgement.changed_and_held,
            self.judgement.judged,
            judge,
            quiet,
        )
    }
}

/// 1 if any round failed; else 4 if no file changed during a sync was
/// placed in any round (the concurrent writes did not bite); else 0.
pub fn concurrent_exit_code(rounds: &[ConcurrentRound]) -> i32 {
    if !rounds.iter().all(ConcurrentRound::passed) {
        1
    } else if rounds
        .iter()
        .map(|r| r.judgement.changed_and_held)
        .sum::<u64>()
        == 0
    {
        4
    } else {
        0
    }
}

/// The concurrent run's footer: passed and failed rounds, the counts
/// summed over them, and the elapsed time.
pub fn concurrent_footer(rounds: &[ConcurrentRound], total: Duration) -> String {
    let passed = rounds.iter().filter(|r| r.passed()).count();
    concurrent_footer_of(rounds.iter(), passed, total)
}

/// [`concurrent_footer`] for rounds that may be held by something bigger,
/// with the number of `passed` ones given by the caller.
pub fn concurrent_footer_of<'a>(
    rounds: impl ExactSizeIterator<Item = &'a ConcurrentRound> + Clone,
    passed: usize,
    total: Duration,
) -> String {
    let count = rounds.len();
    let sum = |f: fn(&ConcurrentRound) -> u64| rounds.clone().map(f).sum::<u64>();
    format!(
        "passed={} failed={} steps={} retried={} gave_up={} touched={} changed_and_held={} judged={} elapsed={:.1}s",
        passed,
        count - passed,
        sum(|r| r.steps),
        sum(|r| r.retried),
        sum(|r| r.gave_up.len() as u64),
        sum(|r| r.judgement.touched.len() as u64),
        sum(|r| r.judgement.changed_and_held),
        sum(|r| r.judgement.judged),
        total.as_secs_f64()
    )
}

pub fn header(
    root: &Path,
    seed: u64,
    direction: Direction,
    endpoint_id: &str,
    address: &SocketAddr,
) -> String {
    let direction = match direction {
        Direction::Push => "push",
        Direction::Pull => "pull",
    };
    format!(
        "irsync-stress\n  root:      {}\n  seed:      {}\n  direction: {}\n  endpoint:  {}\n  address:   {}",
        root.display(),
        seed,
        direction,
        endpoint_id,
        address
    )
}

pub fn footer(iterations: &[Iteration], total: Duration) -> String {
    let passed = iterations.iter().filter(|i| i.passed()).count();
    format!(
        "passed={} failed={} elapsed={:.1}s",
        passed,
        iterations.len() - passed,
        total.as_secs_f64()
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::client::{SyncExit, SyncResult};
    use crate::fileset::{CheckCounts, CheckOutcome};

    use super::*;

    fn zero() -> CheckCounts {
        CheckCounts::default()
    }

    fn sync(exit: SyncExit, ms: u64) -> SyncResult {
        SyncResult {
            exit,
            elapsed: Duration::from_millis(ms),
        }
    }

    fn iter(
        n: u64,
        total: u64,
        exit: SyncExit,
        manifest_ok: bool,
        check: CheckOutcome,
    ) -> Iteration {
        Iteration {
            number: n,
            total,
            sync: sync(exit, 0),
            manifest_ok,
            check,
        }
    }

    #[test]
    fn line_format_matches_spec() {
        let mut ok = iter(1, 100, SyncExit::Ok, true, CheckOutcome::Clean(zero()));
        ok.sync = sync(SyncExit::Ok, 12_340);
        assert_eq!(
            ok.line(),
            "iter 0001/0100  sync 12.3s  exit 0    manifest ok  check ok"
        );
        let counts = CheckCounts {
            added: 0,
            deleted: 1,
            changed: 2,
            unchanged: 9,
            warnings: 0,
        };
        let mut bad = iter(
            2,
            100,
            SyncExit::Partial,
            true,
            CheckOutcome::Differences(counts),
        );
        bad.sync = sync(SyncExit::Partial, 11_800);
        assert_eq!(
            bad.line(),
            "iter 0002/0100  sync 11.8s  exit 23   manifest ok  check FAILED (added=0 deleted=1 changed=2)"
        );
    }

    #[test]
    fn signal_exit_renders_sig() {
        let it = iter(3, 5, SyncExit::Other(-1), true, CheckOutcome::Clean(zero()));
        assert!(it.line().contains("exit sig  manifest"), "{}", it.line());
        let it = iter(3, 5, SyncExit::Other(1), true, CheckOutcome::Clean(zero()));
        assert!(it.line().contains("exit 1    manifest"), "{}", it.line());
    }

    #[test]
    fn exit_23_still_runs_verification() {
        let it = iter(1, 1, SyncExit::Partial, true, CheckOutcome::Clean(zero()));
        assert!(!it.passed());
        assert_eq!(exit_code(&[it]), 1);
    }

    #[test]
    fn manifest_mismatch_alone_fails() {
        let it = iter(1, 1, SyncExit::Ok, false, CheckOutcome::Clean(zero()));
        assert!(!it.passed());
        assert!(it.line().contains("manifest MISMATCH"));
    }

    #[test]
    fn all_passed_is_zero_and_footer_counts() {
        let mk = |n| iter(n, 2, SyncExit::Ok, true, CheckOutcome::Clean(zero()));
        let its = [mk(1), mk(2)];
        assert_eq!(exit_code(&its), 0);
        assert_eq!(
            footer(&its, Duration::from_millis(1_250)),
            "passed=2 failed=0 elapsed=1.2s"
        );
    }

    fn concurrent(exit: SyncExit, gave_up: &[&[u8]], untouched: &[&[u8]]) -> ConcurrentRound {
        ConcurrentRound {
            number: 1,
            total: 3,
            pace: filegen::pace::Pace::Steady { per_sec: 40 },
            steps: 12,
            sync: sync(exit, 2_500),
            retried: 2,
            gave_up: gave_up.iter().map(|p| p.to_vec()).collect(),
            gave_up_untouched: untouched.iter().map(|p| p.to_vec()).collect(),
            judgement: RoundJudgement {
                judged: 40,
                changed_and_held: 3,
                ..RoundJudgement::default()
            },
            quiet: iter(1, 3, SyncExit::Ok, true, CheckOutcome::Clean(zero())),
        }
    }

    #[test]
    fn partial_ok_only_for_touched_paths() {
        assert!(!concurrent(SyncExit::Partial, &[b"x"], &[b"x"]).passed());
        assert!(concurrent(SyncExit::Partial, &[b"x"], &[]).passed());
        // 23 with nothing given up on is a failed session, not a race.
        assert!(!concurrent(SyncExit::Partial, &[], &[]).passed());
        assert!(concurrent(SyncExit::Ok, &[], &[]).passed());
        assert!(concurrent(SyncExit::Other(24), &[], &[]).passed());
        assert!(!concurrent(SyncExit::Other(1), &[], &[]).passed());
    }

    #[test]
    fn a_judged_failure_or_a_failed_quiet_sync_fails_the_round() {
        let mut r = concurrent(SyncExit::Ok, &[], &[]);
        r.judgement.failures.push((b"a".to_vec(), "torn".into()));
        assert!(!r.passed());
        assert!(r.line().contains("judge FAILED (1)"), "{}", r.line());
        let mut r = concurrent(SyncExit::Ok, &[], &[]);
        r.quiet.manifest_ok = false;
        assert!(!r.passed());
    }

    #[test]
    fn concurrent_line_has_the_counts() {
        assert_eq!(
            concurrent(SyncExit::Partial, &[b"x"], &[]).line(),
            "iter 0001/0003  pace steady:40  steps 12  sync 2.5s  exit 23   retried 2  \
             gave-up 1/0  touched 0  changed+held 3  judged 40  judge ok  \
             quiet 0.0s exit 0  manifest ok  check ok"
        );
    }

    #[test]
    fn concurrent_exit_codes() {
        let ok = || concurrent(SyncExit::Ok, &[], &[]);
        assert_eq!(concurrent_exit_code(&[ok(), ok()]), 0);
        let mut bad = ok();
        bad.quiet.manifest_ok = false;
        assert_eq!(concurrent_exit_code(&[ok(), bad]), 1);
        let mut idle = ok();
        idle.judgement.changed_and_held = 0;
        assert_eq!(concurrent_exit_code(&[idle]), 4);
        let mut idle = ok();
        idle.judgement.changed_and_held = 0;
        let mut bad = ok();
        bad.judgement.changed_and_held = 0;
        bad.quiet.manifest_ok = false;
        assert_eq!(concurrent_exit_code(&[idle, bad]), 1);
    }

    #[test]
    fn header_names_everything() {
        let addr: std::net::SocketAddr = "127.0.0.1:4000".parse().unwrap();
        let h = header(
            std::path::Path::new("/tmp/r"),
            42,
            crate::cli::Direction::Push,
            "ep1",
            &addr,
        );
        for want in ["/tmp/r", "42", "push", "ep1", "127.0.0.1:4000"] {
            assert!(h.contains(want), "{want} missing in {h}");
        }
        assert!(h.lines().count() > 1);
    }
}
