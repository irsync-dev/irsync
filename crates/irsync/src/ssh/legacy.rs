//! The server's old ssh install, `~/.local/share/irsync/bin/irsync`, from
//! before irsync installed to `~/.local/bin` like the release installer:
//! the file-system checks `ssh-setup` needs to move a server off it.
//! `ssh-setup` decides what to do with them.

use std::io;
use std::path::{Path, PathBuf};

use super::client::OLD_REMOTE_BIN;

/// What a service file starting the old binary contains.
const OLD_DIR: &[u8] = b".local/share/irsync/bin";

/// The service files irsync printed for the user to save, relative to the
/// home directory.
const SERVICE_FILES: [&str; 2] = [
    ".config/systemd/user/irsync.service",
    "Library/LaunchAgents/org.irsync.daemon.plist",
];

/// The service files under `home` that mention ".local/share/irsync/bin",
/// or that exist and cannot be read.
pub fn stale_services(home: &Path) -> Vec<PathBuf> {
    SERVICE_FILES
        .iter()
        .map(|f| home.join(f))
        .filter(|path| match std::fs::read(path) {
            Ok(bytes) => bytes.windows(OLD_DIR.len()).any(|w| w == OLD_DIR),
            Err(e) => e.kind() != io::ErrorKind::NotFound,
        })
        .collect()
}

/// Whether `exe` is the old binary under `home`, comparing canonical paths
/// when both resolve, else the paths as written.
pub fn is_old_bin(home: &Path, exe: &Path) -> bool {
    let old = home.join(OLD_REMOTE_BIN);
    match (old.canonicalize(), exe.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => old == exe,
    }
}

/// Remove the old binary, then its `bin` directory if empty. Missing is fine.
pub fn remove_old_bin(home: &Path) -> io::Result<bool> {
    let old = home.join(OLD_REMOTE_BIN);
    let mut removed = match std::fs::remove_file(&old) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(e),
    };
    if let Some(bin) = old.parent() {
        // Fails when the directory holds anything else, or is gone: both fine.
        removed |= std::fs::remove_dir(bin).is_ok();
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn put(home: &Path, rel: &str, text: &str) -> PathBuf {
        let path = home.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn finds_the_systemd_unit() {
        let home = tempfile::tempdir().unwrap();
        let unit = put(
            home.path(),
            SERVICE_FILES[0],
            "[Service]\nExecStart=%h/.local/share/irsync/bin/irsync daemon -vv\n",
        );
        assert_eq!(stale_services(home.path()), vec![unit]);
    }

    #[test]
    fn finds_the_launchd_plist() {
        let home = tempfile::tempdir().unwrap();
        let plist = put(
            home.path(),
            SERVICE_FILES[1],
            "<array><string>/Users/x/.local/share/irsync/bin/irsync</string></array>",
        );
        assert_eq!(stale_services(home.path()), vec![plist]);
    }

    #[test]
    fn ignores_a_unit_with_the_new_path() {
        let home = tempfile::tempdir().unwrap();
        put(
            home.path(),
            SERVICE_FILES[0],
            "[Service]\nExecStart=%h/.local/bin/irsync daemon -vv\n",
        );
        assert!(stale_services(home.path()).is_empty());
    }

    #[test]
    fn unreadable_unit_counts_as_stale() {
        if nix::unistd::geteuid().is_root() {
            // root reads any file.
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let unit = put(home.path(), SERVICE_FILES[0], "ExecStart=/usr/bin/irsync\n");
        fs::set_permissions(&unit, fs::Permissions::from_mode(0o000)).unwrap();
        assert_eq!(stale_services(home.path()), vec![unit]);
    }

    #[test]
    fn no_files_no_findings() {
        let home = tempfile::tempdir().unwrap();
        assert!(stale_services(home.path()).is_empty());
    }

    #[test]
    fn old_bin_through_a_symlinked_home() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        put(&real, OLD_REMOTE_BIN, "old");
        let home = dir.path().join("home");
        std::os::unix::fs::symlink(&real, &home).unwrap();
        assert!(is_old_bin(&home, &real.join(OLD_REMOTE_BIN)));
    }

    #[test]
    fn another_exe_is_not_the_old_bin() {
        let home = tempfile::tempdir().unwrap();
        put(home.path(), OLD_REMOTE_BIN, "old");
        let new = put(home.path(), ".local/bin/irsync", "new");
        assert!(!is_old_bin(home.path(), &new));
        // Neither exists: the paths as written.
        let gone = tempfile::tempdir().unwrap();
        assert!(!is_old_bin(gone.path(), Path::new("/usr/bin/irsync")));
        assert!(is_old_bin(gone.path(), &gone.path().join(OLD_REMOTE_BIN)));
    }

    #[test]
    fn remove_takes_the_empty_bin_dir_but_not_a_full_one() {
        let home = tempfile::tempdir().unwrap();
        let old = put(home.path(), OLD_REMOTE_BIN, "old");
        let bin = old.parent().unwrap().to_path_buf();
        assert!(remove_old_bin(home.path()).unwrap());
        assert!(!old.exists());
        assert!(!bin.exists());
        assert!(bin.parent().unwrap().exists(), "only bin goes");
        // Missing is fine.
        assert!(!remove_old_bin(home.path()).unwrap());

        let old = put(home.path(), OLD_REMOTE_BIN, "old");
        let other = put(home.path(), ".local/share/irsync/bin/mine", "keep");
        assert!(remove_old_bin(home.path()).unwrap());
        assert!(!old.exists());
        assert!(other.exists());
    }
}
