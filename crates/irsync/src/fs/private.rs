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

/// Writes `contents` to `path`, replacing what is there. A file it makes is
/// for the owner only (0600); one that exists keeps its mode.
pub fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?
        .write_all(contents.as_ref())
}

/// What is wrong with the owner or mode of a file that only its owner
/// should reach, as sshd asks of a key: `None` when it belongs to this
/// user (or root) and neither its group nor anyone else may read, write or
/// run it. Root may use a file of any owner: it manages the daemon
/// account's files. Only a regular file is judged: a pipe or a device
/// stores nothing.
pub fn open_file_problem(meta: &std::fs::Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    if !meta.is_file() {
        return None;
    }
    let me = nix::unistd::geteuid().as_raw();
    if me != 0 && meta.uid() != me && meta.uid() != 0 {
        return Some(format!("is owned by another user (uid {})", meta.uid()));
    }
    let mode = meta.mode() & 0o7777;
    (mode & 0o077 != 0).then(|| format!("has mode {mode:04o}, which is open to other users"))
}

/// The whole refusal for `path`: what is wrong, and the two ways out.
pub fn refusal(path: &Path, problem: &str) -> String {
    format!(
        "{p} {problem}; run: chmod 600 {p} (or set unsafe_config_allowed = true in the config file)",
        p = path.display()
    )
}

/// Reads `path` as text and judges the file that was opened, not the name:
/// the text, and what [`open_file_problem`] says of it.
pub fn read_with_problem(path: &Path) -> io::Result<(String, Option<String>)> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let problem = open_file_problem(&file.metadata()?);
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok((text, problem))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_file_is_private_only_when_group_and_others_have_nothing() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("f");
        std::fs::write(&f, b"x").unwrap();
        let problem = |mode: u32| {
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(mode)).unwrap();
            super::open_file_problem(&std::fs::metadata(&f).unwrap())
        };
        for ok in [0o600, 0o400, 0o700] {
            assert_eq!(problem(ok), None, "{ok:o}");
        }
        for open in [0o640, 0o604, 0o620, 0o602, 0o610, 0o601, 0o644, 0o666] {
            let p = problem(open).unwrap_or_else(|| panic!("{open:o}"));
            assert!(p.contains(&format!("{open:04o}")), "{p}");
        }
        // A directory, like a pipe or a device, is not a stored secret.
        std::fs::set_permissions(t.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            super::open_file_problem(&std::fs::metadata(t.path()).unwrap()),
            None
        );
        let (text, p) = super::read_with_problem(&f).unwrap();
        assert_eq!(text, "x");
        assert!(p.is_some());
    }

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
