//! Tests start the `irsync` binary only through `tests/common/bin.rs`,
//! whose builder points every default location (the home, and on Windows
//! the profile folders) at a temporary directory and refuses the real
//! ones. This test holds every other test file to that, so no test can
//! reach the real home or profile by building its own command.
//!
//! Exempt: `tests/common/bin.rs` itself, this file, and files compiled only
//! on Unix (`#![cfg(unix)]`), whose fake-ssh fixtures set `HOME` in a
//! script.

use std::path::{Path, PathBuf};

/// The environment variables that hold a default location.
const LOCATION_VARS: [&str; 5] = [
    "\"HOME\"",
    "\"XDG_STATE_HOME\"",
    "\"USERPROFILE\"",
    "\"APPDATA\"",
    "\"LOCALAPPDATA\"",
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

/// What is wrong with `line`, if anything: the binary started (or its path
/// kept to start it) other than through the builder, or a location
/// variable set by hand.
fn offence(line: &str) -> Option<&'static str> {
    if line.contains("CARGO_BIN_EXE_") && (line.contains("Command::new") || line.contains("let ")) {
        return Some("starts the binary without tests/common/bin.rs");
    }
    if LOCATION_VARS.iter().any(|v| line.contains(v)) {
        return Some("sets a location variable by hand");
    }
    None
}

#[test]
fn the_binary_starts_only_with_a_temporary_home() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let exempt = [
        crate_dir.join("tests").join("common").join("bin.rs"),
        crate_dir.join("tests").join("home_rule.rs"),
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
        files.contains(&crate_dir.join("tests").join("common").join("mod.rs")),
        "found no tests in {}",
        crate_dir.display()
    );
    let mut offending = Vec::new();
    for file in files.iter().filter(|f| !exempt.contains(f)) {
        let text = std::fs::read_to_string(file).unwrap();
        if text.lines().any(|l| l.trim() == "#![cfg(unix)]") {
            continue;
        }
        for (i, line) in text.lines().enumerate() {
            if let Some(why) = offence(line) {
                offending.push(format!(
                    "{}:{}: {why}: {}",
                    file.strip_prefix(crate_dir).unwrap().display(),
                    i + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        offending.is_empty(),
        "use irsync_command (tests/common/bin.rs):\n{}",
        offending.join("\n")
    );
}

#[test]
fn the_rule_sees_a_direct_start_and_a_hand_set_home() {
    assert!(offence(r#"Command::new(env!("CARGO_BIN_EXE_irsync"))"#).is_some());
    assert!(offence(r#"let exe = env!("CARGO_BIN_EXE_irsync");"#).is_some());
    assert!(offence(r#".env("HOME", home)"#).is_some());
    assert!(offence(r#".env("APPDATA", dir)"#).is_some());
    assert!(offence(r#"PathBuf::from(env!("CARGO_BIN_EXE_irsync"))"#).is_none());
    assert!(offence("irsync_command(home)").is_none());
}
