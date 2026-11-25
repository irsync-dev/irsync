//! Tests start the `irsync` binary only through `tests/common/bin.rs`,
//! whose builder points every default location (the home, the state, and
//! on Windows the profile folders) at a temporary directory and refuses the
//! real ones. This test holds every other test file to that, so no test can
//! reach the real home or profile by building its own command.
//!
//! Two rules, outside `tests/common/bin.rs` and this file:
//!
//! - The binary's path is not named at all (`CARGO_BIN_EXE_irsync`, the
//!   builder's private `IRSYNC_EXE`, `cargo_bin(`), in any test file, Unix
//!   only or not, except on the lines of [`ALLOWED`]: read-only uses
//!   (copying the binary, comparing a path with it), each with its reason.
//! - A location variable (`HOME`, `XDG_STATE_HOME`, `USERPROFILE`,
//!   `APPDATA`, `LOCALAPPDATA`) is not set by hand. Files compiled only on
//!   Unix (`#![cfg(unix)]`) are exempt from this one: their fake-ssh worlds
//!   set `HOME` for the remote side on purpose, and they never run on
//!   Windows.

use std::path::{Path, PathBuf};

/// What names the binary's path.
const BINARY_NAMES: [&str; 3] = ["CARGO_BIN_EXE_irsync", "IRSYNC_EXE", "cargo_bin("];

/// The environment variables that hold a default location.
const LOCATION_VARS: [&str; 5] = [
    "\"HOME\"",
    "\"XDG_STATE_HOME\"",
    "\"USERPROFILE\"",
    "\"APPDATA\"",
    "\"LOCALAPPDATA\"",
];

/// `(file under tests/, the line trimmed, why it may name the binary)`:
/// uses that never start it with the test's own environment.
const ALLOWED: [(&str, &str, &str); 10] = [
    (
        "daemon_lock.rs",
        r#"std::path::PathBuf::from(env!("CARGO_BIN_EXE_irsync"))"#,
        "compares the path the daemon lock records with the binary's",
    ),
    (
        "daemon_lock.rs",
        r#"env!("CARGO_BIN_EXE_irsync"),"#,
        "the binary's path in an expected error message",
    ),
    (
        "onboard_e2e.rs",
        r#"Path::new(env!("CARGO_BIN_EXE_irsync")),"#,
        "packs a copy of the binary into a fake release",
    ),
    (
        "onboard_e2e.rs",
        r#"std::fs::copy(env!("CARGO_BIN_EXE_irsync"), &other).unwrap();"#,
        "copies the binary to another install path (Unix only; that copy is \
         started with a temporary HOME the test sets)",
    ),
    (
        "ssh_client.rs",
        r#"symlink(Path::new(env!("CARGO_BIN_EXE_irsync")), &exe).unwrap();"#,
        "links the binary as the exe the installer copies (Unix only)",
    ),
    (
        "ssh_client.rs",
        r#"std::fs::read(env!("CARGO_BIN_EXE_irsync")).unwrap()"#,
        "reads the binary's bytes to compare with an installed copy",
    ),
    (
        "ssh_client.rs",
        r#"std::fs::copy(env!("CARGO_BIN_EXE_irsync"), dir.join("irsync")).unwrap();"#,
        "copies the binary beside its extras for the installer",
    ),
    (
        "ssh_client.rs",
        r#"let this = Path::new(env!("CARGO_BIN_EXE_irsync"));"#,
        "packs a copy of the binary into a fake release",
    ),
    (
        "ssh_provision.rs",
        r#"let mut helper = hold_lock(&home.state(), env!("CARGO_BIN_EXE_irsync"), "old");"#,
        "the path written into a fake daemon lock",
    ),
    (
        "ssh_release.rs",
        r#"Path::new(env!("CARGO_BIN_EXE_irsync"))"#,
        "packs a copy of the binary into a fake release",
    ),
];

/// The `.rs` files below `dir`, at any depth.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// What is wrong with `line` of `file` (its path below `tests/`, with `/`),
/// if anything. `unix_only`: the file is compiled only on Unix.
fn offence(file: &str, line: &str, unix_only: bool) -> Option<&'static str> {
    let trimmed = line.trim();
    if BINARY_NAMES.iter().any(|n| line.contains(n))
        && !ALLOWED.iter().any(|(f, l, _)| *f == file && *l == trimmed)
    {
        return Some("names the binary outside tests/common/bin.rs");
    }
    if !unix_only && LOCATION_VARS.iter().any(|v| line.contains(v)) {
        return Some("sets a location variable by hand");
    }
    None
}

#[test]
fn the_binary_starts_only_with_a_temporary_home() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tests = crate_dir.join("tests");
    let exempt = [
        tests.join("common").join("bin.rs"),
        tests.join("home_rule.rs"),
    ];
    let mut files = Vec::new();
    for dir in ["tests", "benches"] {
        let dir = crate_dir.join(dir);
        if dir.is_dir() {
            rust_files(&dir, &mut files);
        }
    }
    files.sort();
    assert!(
        files.contains(&tests.join("common").join("mod.rs")),
        "found no tests in {}",
        crate_dir.display()
    );
    let mut offending = Vec::new();
    let mut allowed_seen = vec![false; ALLOWED.len()];
    for file in files.iter().filter(|f| !exempt.contains(f)) {
        let text = std::fs::read_to_string(file).unwrap();
        let unix_only = text.lines().any(|l| l.trim() == "#![cfg(unix)]");
        let name = file
            .strip_prefix(&tests)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        for (i, line) in text.lines().enumerate() {
            if let Some(why) = offence(&name, line, unix_only) {
                offending.push(format!("tests/{name}:{}: {why}: {}", i + 1, line.trim()));
            }
            for (k, (f, l, _)) in ALLOWED.iter().enumerate() {
                allowed_seen[k] |= *f == name && *l == line.trim();
            }
        }
    }
    assert!(
        offending.is_empty(),
        "use irsync_command (tests/common/bin.rs):\n{}",
        offending.join("\n")
    );
    // An entry whose line is gone must go too, or it would allow it again
    // somewhere else in that file later.
    let stale: Vec<_> = ALLOWED
        .iter()
        .zip(&allowed_seen)
        .filter(|(_, seen)| !**seen)
        .map(|((f, l, _), _)| format!("{f}: {l}"))
        .collect();
    assert!(
        stale.is_empty(),
        "allow-list entries that match no line:\n{}",
        stale.join("\n")
    );
}

#[test]
fn the_rule_sees_every_way_to_the_binary() {
    let flagged = |line: &str| offence("x.rs", line, false).is_some();
    // Direct starts, and a hand-set home.
    assert!(flagged(r#"Command::new(env!("CARGO_BIN_EXE_irsync"))"#));
    assert!(flagged(r#".env("HOME", home)"#));
    assert!(flagged(r#".env("APPDATA", dir)"#));
    // The three bypasses of the first version of this rule.
    assert!(flagged("let c = Command::new(common::bin::IRSYNC_EXE);"));
    assert!(flagged(
        r#"fn exe() -> PathBuf { PathBuf::from(env!("CARGO_BIN_EXE_irsync")) }"#
    ));
    assert!(flagged(r#"const X: &str = env!("CARGO_BIN_EXE_irsync");"#));
    assert!(flagged(r#"static X: &str = env!("CARGO_BIN_EXE_irsync");"#));
    assert!(flagged(
        "let exe = assert_cmd::cargo::cargo_bin(\"irsync\");"
    ));
    // An allowed line is allowed only in its own file.
    let (file, line, _) = ALLOWED[0];
    assert!(offence(file, line, false).is_none());
    assert!(offence("other.rs", line, false).is_some());
    // Unix-only files may set HOME, never name the binary.
    assert!(offence("x.rs", r#".env("HOME", home)"#, true).is_none());
    assert!(offence(
        "x.rs",
        r#"Command::new(env!("CARGO_BIN_EXE_irsync"))"#,
        true
    )
    .is_some());
    assert!(!flagged("irsync_command(home)"));
}
