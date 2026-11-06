//! The build info baked into the binary: package version, git commit and
//! whether the tree was dirty when it was built.

use std::process::{Command, Stdio};

use irsync::build::{BuildInfo, LONG_VERSION};

fn irsync(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_irsync"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn git(args: &[&str]) -> String {
    let o = Command::new("git")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

#[test]
fn current_build_reports_the_package_version_and_checked_out_commit() {
    let b = BuildInfo::current();
    assert_eq!(b.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(b.commit, git(&["rev-parse", "--short", "HEAD"]));
    // Known one way or the other inside a checkout; the value itself can
    // lag an uncommitted edit, since cargo only rebuilds on HEAD changes.
    assert!(b.dirty.is_some());
}

#[test]
fn display_is_version_commit_and_a_dirty_marker() {
    let clean = BuildInfo::from_parts("0.1.0", Some("abc1234"), Some("false"));
    assert_eq!(clean.to_string(), "0.1.0 (abc1234)");
    let dirty = BuildInfo::from_parts("0.1.0", Some("abc1234"), Some("true"));
    assert_eq!(dirty.to_string(), "0.1.0 (abc1234, dirty)");
    assert_eq!(dirty.commit, "abc1234");
    assert_eq!(dirty.dirty, Some(true));
}

#[test]
fn a_build_outside_a_checkout_reports_unknown() {
    let b = BuildInfo::from_parts("0.1.0", None, None);
    assert_eq!(b.commit, "unknown");
    assert_eq!(b.dirty, None);
    assert_eq!(b.to_string(), "0.1.0 (unknown)");
    // vergen's idempotent placeholder counts as unknown too.
    let b = BuildInfo::from_parts(
        "0.1.0",
        Some("VERGEN_IDEMPOTENT_OUTPUT"),
        Some("VERGEN_IDEMPOTENT_OUTPUT"),
    );
    assert_eq!(b.commit, "unknown");
    assert_eq!(b.dirty, None);
}

#[test]
fn version_flag_prints_the_build_info() {
    let o = irsync(&["--version"]);
    assert!(o.status.success());
    assert_eq!(
        String::from_utf8(o.stdout).unwrap(),
        format!("irsync {}\n", *LONG_VERSION)
    );
}

#[test]
fn help_opens_with_the_build_info() {
    let o = irsync(&["--help"]);
    assert!(o.status.success());
    let out = String::from_utf8(o.stdout).unwrap();
    assert_eq!(
        out.lines().next().unwrap(),
        format!("irsync {}", *LONG_VERSION)
    );
    assert!(out.contains("rsync-like directory sync over iroh"), "{out}");
    // Subcommand help carries it as well.
    let o = irsync(&["daemon", "--help"]);
    assert!(o.status.success());
    let out = String::from_utf8(o.stdout).unwrap();
    assert!(out.contains(LONG_VERSION.as_str()), "{out}");
}

#[test]
fn build_info_survives_the_wire_and_an_absent_one_is_unknown() {
    let b = BuildInfo::current();
    assert_eq!(BuildInfo::from_wire(Some(&b.to_wire())), b);
    let dirty = BuildInfo::from_parts("0.2.0", Some("def5678"), Some("true"));
    assert_eq!(BuildInfo::from_wire(Some(&dirty.to_wire())), dirty);
    // An older peer sends no build info at all.
    let none = BuildInfo::from_wire(None);
    assert_eq!(
        (none.version.as_str(), none.commit.as_str()),
        ("unknown", "unknown")
    );
    assert_eq!(none.dirty, None);
    assert_eq!(none.to_string(), "unknown (unknown)");
}
