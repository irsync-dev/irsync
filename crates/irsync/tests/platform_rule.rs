//! Every call into the operating system goes through `src/platform/`, so a
//! new platform is one new file there. This test holds the rest of the crate
//! to that.

use std::path::{Path, PathBuf};

const BANNED: [&str; 7] = [
    "nix::",
    "libc::",
    "std::os::unix",
    "std::os::windows",
    "std::os::fd",
    "windows_sys",
    "winapi_util",
];

/// Files not moved yet. Each task of this pull request removes the files it
/// converts. Task 5 empties the list and deletes this constant.
const PENDING: &[&str] = &[
    "tests/autoexport.rs",
    "tests/backpressure.rs",
    "tests/change_detection.rs",
    "tests/chunk_list_parts.rs",
    "tests/commands_e2e.rs",
    "tests/config_files.rs",
    "tests/containment.rs",
    "tests/content_equal.rs",
    "tests/daemon_auth.rs",
    "tests/daemon_root.rs",
    "tests/delete.rs",
    "tests/delta.rs",
    "tests/file_report.rs",
    "tests/gc.rs",
    "tests/hasher.rs",
    "tests/identity.rs",
    "tests/ids_e2e.rs",
    "tests/index.rs",
    "tests/inplace.rs",
    "tests/isolated_e2e.rs",
    "tests/manage_doc.rs",
    "tests/meta.rs",
    "tests/paranoid_clone.rs",
    "tests/posix_extras.rs",
    "tests/progress_e2e.rs",
    "tests/push_options.rs",
    "tests/recursion_e2e.rs",
    "tests/recv.rs",
    "tests/rsync_compare.rs",
    "tests/scan.rs",
    "tests/send.rs",
    "tests/single_file.rs",
    "tests/single_file_e2e.rs",
    "tests/single_place.rs",
    "tests/ssh_client.rs",
    "tests/ssh_release.rs",
    "tests/ssh_setup.rs",
    "tests/sub_chunk_delta.rs",
    "tests/sync_e2e.rs",
    "tests/telemetry_debug.rs",
    "tests/telemetry_usage.rs",
    "tests/transient.rs",
    "tests/warm.rs",
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
/// longer one: `unix::` does not name `nix::`.
fn names(line: &str, word: &str) -> bool {
    line.match_indices(word).any(|(i, _)| {
        !line[..i]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// `file` relative to `crate_dir`, with `/` between components on every
/// system, as [`PENDING`] spells it.
fn relative(crate_dir: &Path, file: &Path) -> String {
    let rel = file.strip_prefix(crate_dir).unwrap();
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.join("/")
}

#[test]
fn only_the_platform_module_names_a_platform_api() {
    // Every .rs file under src/, tests/ and benches/ (if there is one) of
    // this crate, except src/platform/, tests/common/ and this file.
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let exempt = [
        crate_dir.join("src").join("platform"),
        crate_dir.join("tests").join("common"),
        crate_dir.join("tests").join("platform_rule.rs"),
    ];
    let mut files = Vec::new();
    for dir in ["src", "tests", "benches"] {
        let dir = crate_dir.join(dir);
        // src/ and tests/ always exist; benches/ may not.
        if dir.is_dir() || dir.file_name().unwrap() != "benches" {
            rust_files(&dir, &mut files);
        }
    }
    files.sort();
    assert!(
        files.contains(&crate_dir.join("src").join("lib.rs")),
        "found no sources in {}",
        crate_dir.display()
    );
    let mut offending = Vec::new();
    let mut seen_pending = Vec::new();
    let mut moved = Vec::new();
    for file in files
        .iter()
        .filter(|f| !exempt.iter().any(|e| f.starts_with(e)))
    {
        let rel = relative(crate_dir, file);
        let text = std::fs::read_to_string(file).unwrap();
        let lines: Vec<String> = text
            .lines()
            .enumerate()
            .filter(|(_, line)| BANNED.iter().any(|word| names(line, word)))
            .map(|(i, line)| format!("{rel}:{}: {}", i + 1, line.trim()))
            .collect();
        if PENDING.contains(&rel.as_str()) {
            seen_pending.push(rel.clone());
            if lines.is_empty() {
                moved.push(rel);
            }
        } else {
            offending.extend(lines);
        }
    }
    assert!(
        offending.is_empty(),
        "platform APIs named outside src/platform/:\n{}",
        offending.join("\n")
    );
    // The list cannot rot: a file that no longer names a platform API, or
    // that is gone, leaves it.
    moved.extend(
        PENDING
            .iter()
            .filter(|p| !seen_pending.iter().any(|s| s == *p))
            .map(|p| format!("{p} (no such file)")),
    );
    assert!(
        moved.is_empty(),
        "in PENDING but naming no platform API; take them off the list:\n{}",
        moved.join("\n")
    );
}
