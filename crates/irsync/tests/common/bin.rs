//! The built `irsync` for tests, never with the real home or profile.
//!
//! Every test starts the binary through [`irsync_command`], which points
//! every default location at a temporary home: `HOME`, `XDG_CONFIG_HOME`
//! and `XDG_STATE_HOME` (Unix), and
//! `USERPROFILE`, `APPDATA` and `LOCALAPPDATA` (Windows, where `HOME` means
//! nothing to irsync). The Windows ones are laid out as the Unix ones are,
//! so a test sees the same files on both: `APPDATA` is `<home>/.config`,
//! `LOCALAPPDATA` is `<home>/.local/state`, or the state directory a test
//! names with [`StateHome::state_home`]. Each path is checked first: it
//! must lie in the temporary directory, and must not hold the real home or
//! profile, or the test fails before anything runs. `tests/home_rule.rs`
//! fails if a test starts the binary any other way.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command};

/// The built `irsync` binary. Private: a test starts it only through
/// [`irsync_command`] (`tests/home_rule.rs`).
const IRSYNC_EXE: &str = env!("CARGO_BIN_EXE_irsync");

/// `irsync` with every default location under `home` (see the module doc).
/// Panics if `home` is not in the temporary directory or holds the real
/// home or profile.
pub fn irsync_command(home: impl AsRef<Path>) -> Command {
    let home = isolated(home.as_ref());
    let state = home.join(".local").join("state");
    let mut cmd = Command::new(IRSYNC_EXE);
    // The XDG variables too, where the defaults without them would be, so
    // a developer's own ones never reach the child.
    cmd.env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_STATE_HOME", &state)
        .env("USERPROFILE", &home)
        .env("APPDATA", home.join(".config"))
        .env("LOCALAPPDATA", &state);
    cmd
}

/// The default locations a command from [`irsync_command`] with `home`
/// sees, as the product computes them from the environment it is given:
/// the config file, the state directory and the key it falls back to
/// (`~/.ssh/id_ed25519` on Unix; `%USERPROFILE%\.irsync\id_ed25519` on
/// Windows, which does not look in `.ssh`).
pub fn locations(home: impl AsRef<Path>) -> irsync::platform::Locations {
    let home = home.as_ref().to_path_buf();
    let env = |k: &str| -> Option<std::ffi::OsString> {
        match k {
            "HOME" | "USERPROFILE" => Some(home.clone().into()),
            "XDG_STATE_HOME" | "LOCALAPPDATA" => Some(home.join(".local").join("state").into()),
            "XDG_CONFIG_HOME" | "APPDATA" => Some(home.join(".config").into()),
            _ => None,
        }
    };
    irsync::platform::default_locations(&env).expect("a home gives every location")
}

/// The state directory of a command from [`irsync_command`].
pub trait StateHome {
    /// Keeps irsync's state in `dir/irsync`: `XDG_STATE_HOME` (Unix) and
    /// `LOCALAPPDATA` (Windows). Checked as `irsync_command` checks a home.
    fn state_home(&mut self, dir: impl AsRef<Path>) -> &mut Self;
}

impl StateHome for Command {
    fn state_home(&mut self, dir: impl AsRef<Path>) -> &mut Self {
        let dir = isolated(dir.as_ref());
        self.env("XDG_STATE_HOME", &dir).env("LOCALAPPDATA", &dir)
    }
}

/// `p`, made absolute, if it lies inside the temporary directory and holds
/// neither the real home nor the real profile folders; else a panic that
/// says why. The real ones come from the system (`dirs`: the Known Folder
/// API on Windows), not from this process's environment. A `..` anywhere is
/// refused: on Unix `absolute` keeps it, so it could climb out unseen.
fn isolated(p: &Path) -> PathBuf {
    assert!(
        !p.components().any(|c| c == std::path::Component::ParentDir),
        "refusing to run irsync with {} as a home or state directory: it holds `..`",
        p.display()
    );
    let p = std::path::absolute(p).unwrap();
    let tmp = std::path::absolute(std::env::temp_dir()).unwrap();
    assert!(
        p.starts_with(&tmp) && p != tmp,
        "refusing to run irsync with {} as a home or state directory: not \
         inside the temporary directory {}; a test must never use the real \
         home or profile",
        p.display(),
        tmp.display()
    );
    for real in [dirs::home_dir(), dirs::config_dir(), dirs::data_local_dir()]
        .into_iter()
        .flatten()
    {
        assert!(
            p != real && !real.starts_with(&p),
            "refusing to run irsync with {} as a home or state directory: it \
             is or holds the real {}",
            p.display(),
            real.display()
        );
    }
    p
}

/// Stops `child`, a daemon a test started, without waiting for it: SIGTERM
/// on Unix (`kill -TERM`), `TerminateProcess` on Windows, which has no
/// signal to ask with.
pub fn stop_child(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id().to_string();
        Command::new("kill").args(["-TERM", &pid]).status().unwrap();
    }
    #[cfg(windows)]
    child.kill().unwrap();
}
