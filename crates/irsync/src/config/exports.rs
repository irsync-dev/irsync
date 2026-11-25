//! The `[exports]` table: what the daemon shares and who may use it, in
//! `irsync.toml`.

use super::{expand_tilde, file_error, ConfigError};
use irsync_proto::config as pb;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Names that cannot be export names: `all` is the word `grant remove all
/// WHO` uses for every export, and `all_exports`, `all_paths` and `admin` are
/// the rights `grant list` lists by those names.
pub const RESERVED: [&str; 4] = ["all", "all_exports", "all_paths", "admin"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    pub path: PathBuf,
    pub read_only: bool,
    /// Hash the export's files into its index when the daemon starts.
    pub index_at_start: bool,
    /// Flush every file a push writes into the export before placing it,
    /// as if the client had passed `--fsync`.
    pub fsync: bool,
    /// Created by the daemon for a path a client with `all_paths` reached,
    /// not written by hand.
    pub auto: bool,
    pub rw: Vec<String>,
    pub ro: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exports {
    pub path: PathBuf,
    pub exports: BTreeMap<String, Export>,
}

/// The message for a reserved export name.
pub fn reserved(name: &str) -> String {
    let why = match name {
        "all_exports" | "all_paths" | "admin" => {
            format!("`irsync grant list {name}` lists the clients with that right")
        }
        _ => "it is the word `grant remove all WHO` and `index --export all` use for every export"
            .to_string(),
    };
    format!("{name:?} is reserved: {why}")
}

/// The message for a name that cannot be an export's, or `None`.
pub(crate) fn name_problem(name: &str) -> Option<String> {
    if RESERVED.contains(&name) {
        return Some(reserved(name));
    }
    // The name is the first segment of a HOST:PATH.
    (name.is_empty() || name.contains('/')).then(|| {
        format!("{name:?} is not a valid export name: it must not be empty or contain '/'")
    })
}

/// The message for an export path that is not absolute once `~` is
/// expanded.
pub(crate) fn not_absolute(name: &str, expanded: &Path) -> String {
    format!(
        "[exports.{name}]: path {:?} must be absolute",
        expanded.to_string_lossy()
    )
}

/// The values of a string array of the message.
fn values(entries: &[pb::Entry]) -> Vec<String> {
    entries.iter().map(|e| e.value.clone()).collect()
}

impl Exports {
    /// The exports of the message, checked as
    /// the reader checks the file: names, absolute paths (`~` expanded),
    /// and no name twice. `path` is the file named in errors (and
    /// recorded as `Exports.path`).
    pub fn from_message(msg: &pb::Config, path: &Path) -> Result<Exports, ConfigError> {
        Self::read(msg, path, |name, written| {
            let expanded = expand_tilde(written);
            if !expanded.is_absolute() {
                return Err(file_error(path, not_absolute(name, &expanded)));
            }
            // As this system means it (on Unix the path as written).
            Ok(crate::platform::local_path(&expanded))
        })
    }

    /// The exports of a message another machine sent (what `irsync remote`
    /// shows of a server's): names checked as [`Exports::from_message`]
    /// checks them, but each path kept as the server wrote it. It is a
    /// path of the server's system, which may be another kind than this
    /// one (`C:\srv` is not absolute on Unix, nor `/srv` on Windows), so
    /// this system's rules neither check nor change it; it is only shown.
    pub fn as_sent(msg: &pb::Config, path: &Path) -> Result<Exports, ConfigError> {
        Self::read(msg, path, |_, written| Ok(PathBuf::from(written)))
    }

    /// The exports of `msg`, with each export's path as `path_of` (given
    /// the name and the path as written) makes it.
    fn read(
        msg: &pb::Config,
        path: &Path,
        path_of: impl Fn(&str, &str) -> Result<PathBuf, ConfigError>,
    ) -> Result<Exports, ConfigError> {
        let mut exports = BTreeMap::new();
        for e in &msg.exports {
            let name = &e.name;
            if let Some(problem) = name_problem(name) {
                return Err(file_error(path, problem));
            }
            let export = Export {
                path: path_of(name, &e.path)?,
                read_only: e.read_only.unwrap_or(false),
                index_at_start: e.index_at_start.unwrap_or(false),
                fsync: e.fsync.unwrap_or(false),
                auto: e.auto.unwrap_or(false),
                rw: values(&e.rw),
                ro: values(&e.ro),
            };
            if exports.insert(name.clone(), export).is_some() {
                return Err(file_error(path, format!("duplicate export {name:?}")));
            }
        }
        Ok(Exports {
            path: path.to_path_buf(),
            exports,
        })
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    /// An export root written with a trailing dot or space is the
    /// directory Windows names by it (`x.` is `x`), and that is what the
    /// daemon opens.
    #[test]
    fn a_written_root_is_read_as_windows_reads_it() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("x")).unwrap();
        std::fs::write(t.path().join("x").join("f"), b"f").unwrap();
        for tail in [".", " "] {
            let written = format!("{}{tail}", t.path().join("x").display());
            let msg = pb::Config {
                exports: vec![pb::Export {
                    name: "m".into(),
                    path: written.clone(),
                    ..Default::default()
                }],
                ..Default::default()
            };
            let e = Exports::from_message(&msg, Path::new("irsync.toml")).unwrap();
            let path = &e.exports["m"].path;
            assert_eq!(path, &t.path().join("x"), "{written:?}");
            let d = crate::platform::Dir::open_root_nofollow(path).unwrap();
            assert_eq!(d.stat(b"f").unwrap().size, 1);
        }
    }
}
