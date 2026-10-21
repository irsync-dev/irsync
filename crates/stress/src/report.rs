//! The per-iteration result, its one-line rendering, and the run's header,
//! footer and exit code.

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use crate::cli::Direction;
use crate::client::{SyncExit, SyncResult};
use crate::fileset::CheckOutcome;

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
        let exit = match self.sync.exit.code() {
            -1 => "sig".to_string(),
            code => code.to_string(),
        };
        let manifest = if self.manifest_ok { "ok" } else { "MISMATCH" };
        let check = match &self.check {
            CheckOutcome::Clean(_) => "ok".to_string(),
            CheckOutcome::Differences(c) => format!(
                "FAILED (added={} deleted={} changed={})",
                c.added, c.deleted, c.changed
            ),
        };
        format!(
            "iter {:04}/{:04}  sync {:.1}s  exit {:<3}  manifest {}  check {}",
            self.number,
            self.total,
            self.sync.elapsed.as_secs_f64(),
            exit,
            manifest,
            check
        )
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
