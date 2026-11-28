//! Directories that hold irsync's own state.

use std::io;
use std::path::Path;

pub use crate::platform::open_file_problem;

/// Like [`std::fs::create_dir_all`], but every directory it makes is for
/// the owner only (0700). One that exists keeps its mode.
pub fn create_dir_all(dir: &Path) -> io::Result<()> {
    crate::platform::create_private_dir_all(dir)
}

/// Writes `contents` to `path`, replacing what is there. A file it makes is
/// for the owner only (0600); one that exists keeps its mode.
pub fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
    crate::platform::write_private(path.as_ref(), contents.as_ref())
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
pub(crate) fn read_with_problem(path: &Path) -> io::Result<(String, Option<String>)> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let problem = open_file_problem(&file.metadata()?);
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok((text, problem))
}

/// Both tests are about permission modes, which Windows does not have.
#[cfg(all(test, unix))]
mod tests {
    use crate::platform::{mode_of, set_mode_path_following};

    #[test]
    fn a_file_is_private_only_when_group_and_others_have_nothing() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("f");
        std::fs::write(&f, b"x").unwrap();
        let problem = |mode: u32| {
            set_mode_path_following(&f, mode).unwrap();
            super::open_file_problem(&std::fs::metadata(&f).unwrap())
        };
        for ok in [0o600, 0o400, 0o700] {
            assert_eq!(problem(ok), None, "{ok:o}");
        }
        for open in [0o640, 0o604, 0o620, 0o602, 0o610, 0o601, 0o644, 0o666] {
            let p = problem(open).unwrap_or_else(|| panic!("{open:o}"));
            assert!(p.contains(&format!("{open:04o}")), "{open:o}");
        }
        // A directory, like a pipe or a device, is not a stored secret.
        set_mode_path_following(t.path(), 0o755).unwrap();
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
        set_mode_path_following(&old, 0o755).unwrap();
        super::create_dir_all(&old.join("a/b")).unwrap();
        let mode = |p: &std::path::Path| mode_of(&std::fs::metadata(p).unwrap()) & 0o777;
        assert_eq!(mode(&old), 0o755);
        assert_eq!(mode(&old.join("a")), 0o700);
        assert_eq!(mode(&old.join("a/b")), 0o700);
    }
}
