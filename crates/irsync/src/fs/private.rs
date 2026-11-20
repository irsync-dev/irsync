//! Directories that hold irsync's own state.

use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;

/// Like [`std::fs::create_dir_all`], but every directory it makes is for
/// the owner only (0700). One that exists keeps its mode.
pub fn create_dir_all(dir: &Path) -> io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn new_directories_are_private_and_old_ones_keep_their_mode() {
        let t = tempfile::tempdir().unwrap();
        let old = t.path().join("old");
        std::fs::create_dir(&old).unwrap();
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o755)).unwrap();
        super::create_dir_all(&old.join("a/b")).unwrap();
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&old), 0o755);
        assert_eq!(mode(&old.join("a")), 0o700);
        assert_eq!(mode(&old.join("a/b")), 0o700);
    }
}
