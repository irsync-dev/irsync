//! The release installer script, built into irsync, for the ssh install.
//!
//! The server runs it with `--from DIR` on the files the client sends, so
//! it never downloads anything. The script still carries its download
//! mode, so its checksum placeholders are filled with a value that is not
//! hex: no real digest equals it, and download mode can never pass the
//! check. (Only the release's own copy of the script has real checksums.)
//!
//! The template lives outside this crate's directory. That is fine for git
//! and dist builds; it would have to move before a `cargo publish`.

/// Stands in for the three checksums. Not hex, so no digest equals it.
const NO_CHECKSUM: &str = "built-in-copy-has-no-checksum";

/// The script for this version, with every placeholder filled.
pub fn script() -> String {
    let version = env!("CARGO_PKG_VERSION");
    include_str!("../../../../packaging/installer/irsync-installer.sh.in")
        .replace("@VERSION@", version)
        .replace(
            "@BASE_URL@",
            &format!("https://github.com/irsync-dev/irsync/releases/download/v{version}"),
        )
        .replace("@SHA256_AARCH64_APPLE_DARWIN@", NO_CHECKSUM)
        .replace("@SHA256_AARCH64_UNKNOWN_LINUX_MUSL@", NO_CHECKSUM)
        .replace("@SHA256_X86_64_UNKNOWN_LINUX_MUSL@", NO_CHECKSUM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_placeholder_is_left() {
        let s = script();
        assert!(!s.contains("@VERSION@") && !s.contains("@BASE_URL@") && !s.contains("@SHA256_"));
    }

    #[test]
    fn names_this_version() {
        assert!(script().contains(&format!("version='{}'", env!("CARGO_PKG_VERSION"))));
    }

    #[test]
    fn download_mode_can_never_pass_the_checksum() {
        // Not hex, so no real digest equals it.
        assert_eq!(
            script()
                .matches("sha=built-in-copy-has-no-checksum")
                .count(),
            3
        );
    }

    #[test]
    fn is_valid_sh() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("installer.sh");
        std::fs::write(&file, script()).unwrap();
        let status = std::process::Command::new("sh")
            .arg("-n")
            .arg(&file)
            .status()
            .unwrap();
        assert!(status.success());
    }
}
