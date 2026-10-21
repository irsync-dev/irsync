//! Wrappers for the `fileset` binary's `init`, `update` and `check` verbs.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::process;

fn run_quiet(fileset: &Path, verb: &str, tree: &Path) -> Result<()> {
    let out = process::output(
        Command::new(fileset)
            .args([verb, "-q"])
            .arg(tree)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    )
    .with_context(|| format!("running {}", fileset.display()))?;
    if !out.status.success() {
        bail!(
            "fileset {verb} {} failed ({}): {}",
            tree.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// `fileset init -q <tree>`.
pub fn init(fileset: &Path, tree: &Path) -> Result<()> {
    run_quiet(fileset, "init", tree)
}

/// `fileset update -q <tree>`.
pub fn update(fileset: &Path, tree: &Path) -> Result<()> {
    run_quiet(fileset, "update", tree)
}

#[derive(Debug, Default, PartialEq)]
pub struct CheckCounts {
    pub added: u64,
    pub deleted: u64,
    pub changed: u64,
    pub unchanged: u64,
    pub warnings: u64,
}

#[derive(Debug, PartialEq)]
pub enum CheckOutcome {
    Clean(CheckCounts),
    Differences(CheckCounts),
}

/// Parses the last line of the form
/// `added=N deleted=N changed=N unchanged=N warnings=N`.
pub fn parse_summary(stderr: &str) -> Option<CheckCounts> {
    stderr.lines().rev().find_map(|line| {
        let mut it = line.split_whitespace();
        let mut field = |name: &str| -> Option<u64> {
            it.next()?
                .strip_prefix(name)?
                .strip_prefix('=')?
                .parse()
                .ok()
        };
        Some(CheckCounts {
            added: field("added")?,
            deleted: field("deleted")?,
            changed: field("changed")?,
            unchanged: field("unchanged")?,
            warnings: field("warnings")?,
        })
    })
}

/// `fileset check -q <tree>`; stdout then stderr are written to `report`.
/// Exit 0 is `Clean`, 1 is `Differences`, anything else is an error.
pub fn check(fileset: &Path, tree: &Path, report: &Path) -> Result<CheckOutcome> {
    let out = process::output(
        Command::new(fileset)
            .args(["check", "-q"])
            .arg(tree)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .with_context(|| format!("running {}", fileset.display()))?;
    let mut all = out.stdout.clone();
    all.extend_from_slice(&out.stderr);
    std::fs::write(report, &all).with_context(|| format!("writing {}", report.display()))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    match out.status.code() {
        Some(0) => Ok(CheckOutcome::Clean(
            parse_summary(&stderr).unwrap_or_default(),
        )),
        Some(1) => Ok(CheckOutcome::Differences(
            parse_summary(&stderr).unwrap_or_default(),
        )),
        _ => bail!(
            "fileset check {} failed ({}): {}",
            tree.display(),
            out.status,
            stderr.trim()
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn fake(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn summary_line_parses() {
        assert_eq!(
            parse_summary("WARN x\nadded=1 deleted=2 changed=3 unchanged=4 warnings=5\n"),
            Some(CheckCounts {
                added: 1,
                deleted: 2,
                changed: 3,
                unchanged: 4,
                warnings: 5
            })
        );
        assert_eq!(parse_summary("nothing here"), None);
    }

    #[test]
    fn check_exit_codes_map() {
        let t = tempfile::tempdir().unwrap();
        let clean = fake(
            t.path(),
            "clean.sh",
            "echo 'added=0 deleted=0 changed=0 unchanged=7 warnings=0' >&2\nexit 0",
        );
        assert_eq!(
            check(&clean, t.path(), &t.path().join("clean.txt")).unwrap(),
            CheckOutcome::Clean(CheckCounts {
                unchanged: 7,
                ..Default::default()
            })
        );

        let diff = fake(
            t.path(),
            "diff.sh",
            "echo 'ADD x kind=file'\necho 'added=1 deleted=0 changed=0 unchanged=2 warnings=0' >&2\nexit 1",
        );
        let report = t.path().join("diff.txt");
        assert_eq!(
            check(&diff, t.path(), &report).unwrap(),
            CheckOutcome::Differences(CheckCounts {
                added: 1,
                unchanged: 2,
                ..Default::default()
            })
        );
        assert_eq!(
            std::fs::read_to_string(&report).unwrap(),
            "ADD x kind=file\nadded=1 deleted=0 changed=0 unchanged=2 warnings=0\n"
        );

        let nosum = fake(t.path(), "nosum.sh", "exit 1");
        assert_eq!(
            check(&nosum, t.path(), &t.path().join("n.txt")).unwrap(),
            CheckOutcome::Differences(CheckCounts::default())
        );
    }

    #[test]
    fn check_exit_2_is_a_harness_error() {
        let t = tempfile::tempdir().unwrap();
        let bad = fake(t.path(), "bad.sh", "echo boom >&2\nexit 2");
        let e = check(&bad, t.path(), &t.path().join("r.txt")).unwrap_err();
        assert!(e.to_string().contains("fileset check"), "{e}");
        assert!(e.to_string().contains("boom"), "{e}");
    }

    #[test]
    fn init_and_update_report_failures() {
        let t = tempfile::tempdir().unwrap();
        let ok = fake(t.path(), "ok.sh", "exit 0");
        init(&ok, t.path()).unwrap();
        update(&ok, t.path()).unwrap();
        let bad = fake(t.path(), "bad.sh", "echo exists >&2\nexit 2");
        let e = init(&bad, t.path()).unwrap_err();
        assert!(e.to_string().contains("exists"), "{e}");
    }
}
