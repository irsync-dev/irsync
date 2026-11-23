//! Every call into the operating system goes through `src/platform/`, so a
//! new platform is one new file there. This test holds the rest of the crate
//! to that.

use std::path::{Path, PathBuf};

const BANNED: [&str; 10] = [
    "nix::",
    "libc::",
    "xattr::",
    "std::os::unix",
    "std::os::windows",
    "std::os::fd",
    "windows_sys",
    "winapi_util",
    "cpu_time",
    "ctrlc",
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

/// Whether `line` names `word` at the start of a name, not as the tail of a
/// longer one: `thread_cpu_time` does not name the crate `cpu_time`, and
/// `unix::` does not name `nix::`.
fn names(line: &str, word: &str) -> bool {
    line.match_indices(word).any(|(i, _)| {
        !line[..i]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

#[test]
fn only_the_platform_module_names_a_platform_api() {
    // Every .rs file under src/, tests/ and benches/ of this crate, except
    // src/platform/, tests/common/ and this file.
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let exempt = [
        crate_dir.join("src").join("platform"),
        crate_dir.join("tests").join("common"),
        crate_dir.join("tests").join("platform_rule.rs"),
    ];
    let mut files = Vec::new();
    for dir in ["src", "tests", "benches"] {
        rust_files(&crate_dir.join(dir), &mut files);
    }
    files.sort();
    assert!(
        files.contains(&crate_dir.join("src").join("lib.rs")),
        "found no sources in {}",
        crate_dir.display()
    );
    let mut offending = Vec::new();
    for file in files
        .iter()
        .filter(|f| !exempt.iter().any(|e| f.starts_with(e)))
    {
        let text = std::fs::read_to_string(file).unwrap();
        for (i, line) in text.lines().enumerate() {
            if BANNED.iter().any(|word| names(line, word)) {
                offending.push(format!(
                    "{}:{}: {}",
                    file.strip_prefix(crate_dir).unwrap().display(),
                    i + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        offending.is_empty(),
        "platform APIs named outside src/platform/:\n{}",
        offending.join("\n")
    );
}
