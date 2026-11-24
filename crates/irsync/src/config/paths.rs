//! Default locations of the config and state files, from
//! [`crate::platform::default_locations`] and this process's environment.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::platform::Locations;

/// This process's environment, as the platform's lookups take it.
fn process_env(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

/// The default locations for this process's environment. `None` only on
/// Windows, when neither the environment nor the system names the config
/// or state folder.
fn locations() -> Option<Locations> {
    crate::platform::default_locations(&process_env)
}

/// The user's home directory (`HOME` on Unix, `%USERPROFILE%` on Windows):
/// what `~` in a configured path stands for.
pub fn home_dir() -> Option<PathBuf> {
    crate::platform::home_dir(&process_env)
}

/// The directory of the config file (`~/.config/irsync`, on Windows
/// `%APPDATA%\irsync`); the default keys live there. `.` without one.
pub fn config_dir() -> PathBuf {
    locations()
        .and_then(|l| l.config.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The key tried when the config's own client key does not exist yet:
/// `~/.ssh/id_ed25519`, the key ssh itself tries first; on Windows
/// `%USERPROFILE%\.irsync\id_ed25519`. `None` without a home.
pub fn ssh_identity() -> Option<PathBuf> {
    locations().and_then(|l| l.key)
}

/// `$XDG_STATE_HOME/irsync` when that is an absolute path, else
/// `~/.local/state/irsync`, on every Unix; `%LOCALAPPDATA%\irsync` on
/// Windows. `./irsync` without one.
pub fn state_dir() -> PathBuf {
    locations()
        .map(|l| l.state)
        .unwrap_or_else(|| PathBuf::from(".").join("irsync"))
}

/// The config file: the one file irsync is configured with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config: PathBuf,
}

impl Paths {
    /// `config`, or `irsync.toml` under [`config_dir`].
    pub fn resolve(config: Option<&Path>) -> Paths {
        let config = config
            .map(Path::to_path_buf)
            .unwrap_or_else(|| config_dir().join("irsync.toml"));
        Paths { config }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A lookup over `vars` only.
    fn env_of(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[cfg(windows)]
    #[test]
    fn windows_locations() {
        let all = [
            ("APPDATA", r"C:\t\roaming"),
            ("LOCALAPPDATA", r"C:\t\local"),
            ("USERPROFILE", r"C:\t\me"),
            ("HOME", r"C:\elsewhere"),
            ("XDG_STATE_HOME", r"C:\elsewhere\xdg"),
        ];
        let l = crate::platform::default_locations(&env_of(&all)).unwrap();
        assert_eq!(l.config, PathBuf::from(r"C:\t\roaming\irsync\irsync.toml"));
        assert_eq!(l.state, PathBuf::from(r"C:\t\local\irsync"));
        assert_eq!(l.key, Some(PathBuf::from(r"C:\t\me\.irsync\id_ed25519")));
        assert_eq!(
            crate::platform::home_dir(&env_of(&all)),
            Some(PathBuf::from(r"C:\t\me"))
        );
        // A missing or empty variable: that one location is the system's
        // folder (its path is read, nothing touched), never `.`; the other
        // two still come from the environment.
        let config = dirs::config_dir().unwrap().join(r"irsync\irsync.toml");
        let state = dirs::data_local_dir().unwrap().join("irsync");
        let key = dirs::home_dir().unwrap().join(r".irsync\id_ed25519");
        for missing in ["APPDATA", "LOCALAPPDATA", "USERPROFILE"] {
            let some: Vec<_> = all.iter().copied().filter(|(k, _)| *k != missing).collect();
            let empty: Vec<_> = all
                .iter()
                .map(|&(k, v)| (k, if k == missing { "" } else { v }))
                .collect();
            for (vars, how) in [(some, "missing"), (empty, "empty")] {
                let l = crate::platform::default_locations(&env_of(&vars)).unwrap();
                let want = |var: &str, from_env: PathBuf, system: &PathBuf| {
                    if var == missing {
                        system.clone()
                    } else {
                        from_env
                    }
                };
                assert_eq!(
                    l.config,
                    want(
                        "APPDATA",
                        PathBuf::from(r"C:\t\roaming\irsync\irsync.toml"),
                        &config
                    ),
                    "{missing} {how}"
                );
                assert_eq!(
                    l.state,
                    want("LOCALAPPDATA", PathBuf::from(r"C:\t\local\irsync"), &state),
                    "{missing} {how}"
                );
                assert_eq!(
                    l.key,
                    Some(want(
                        "USERPROFILE",
                        PathBuf::from(r"C:\t\me\.irsync\id_ed25519"),
                        &key
                    )),
                    "{missing} {how}"
                );
                for p in [&l.config, &l.state] {
                    assert!(p.is_absolute(), "{missing} {how}: {}", p.display());
                }
            }
        }
        assert_eq!(
            crate::platform::home_dir(&env_of(&[])),
            dirs::home_dir(),
            "the profile without USERPROFILE"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_locations_are_unchanged() {
        let l = crate::platform::default_locations(&env_of(&[("HOME", "/h")])).unwrap();
        assert_eq!(l.config, PathBuf::from("/h/.config/irsync/irsync.toml"));
        assert_eq!(l.state, PathBuf::from("/h/.local/state/irsync"));
        assert_eq!(l.key, Some(PathBuf::from("/h/.ssh/id_ed25519")));
        let with_xdg = [("HOME", "/h"), ("XDG_STATE_HOME", "/x")];
        let l = crate::platform::default_locations(&env_of(&with_xdg)).unwrap();
        assert_eq!(l.config, PathBuf::from("/h/.config/irsync/irsync.toml"));
        assert_eq!(l.state, PathBuf::from("/x/irsync"));
        // A relative XDG_STATE_HOME is ignored, as the spec says.
        let relative = [("HOME", "/h"), ("XDG_STATE_HOME", "x")];
        let l = crate::platform::default_locations(&env_of(&relative)).unwrap();
        assert_eq!(l.state, PathBuf::from("/h/.local/state/irsync"));
        assert_eq!(
            crate::platform::home_dir(&env_of(&[("HOME", "/h")])),
            Some(PathBuf::from("/h"))
        );
    }
}
