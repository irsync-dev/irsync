//! Engines behind the `update` and `check` verbs.

pub mod check;
pub mod update;

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::store::{self, Backend, Location};
use crate::walk::WalkConfig;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Clean,
    Differences,
    Failed,
}

impl Outcome {
    pub fn exit_code(self) -> i32 {
        match self {
            Outcome::Clean => 0,
            Outcome::Differences => 1,
            Outcome::Failed => 2,
        }
    }
}

/// Options shared by every verb.
pub struct CommonOpts {
    pub root: PathBuf,
    pub manifest: Option<PathBuf>,
    pub cross_mount_points: bool,
    pub sort: bool,
    pub jobs: usize,
    pub fail_on_warnings: bool,
    pub builtin_excludes: bool,
    pub verbose: bool,
}

/// Writes `WARN <text>` lines to `err` and counts them; with `verbose`, also uncounted
/// `INFO <text>` lines.
pub struct Warnings<'a> {
    err: &'a mut dyn Write,
    count: u64,
    verbose: bool,
}

impl<'a> Warnings<'a> {
    pub fn new(err: &'a mut dyn Write, verbose: bool) -> Self {
        Warnings {
            err,
            count: 0,
            verbose,
        }
    }

    pub fn info(&mut self, text: &str) -> io::Result<()> {
        if self.verbose {
            writeln!(self.err, "INFO {text}")?;
        }
        Ok(())
    }

    pub fn emit(&mut self, text: &str) -> io::Result<()> {
        self.count += 1;
        writeln!(self.err, "WARN {text}")
    }

    pub fn count(&self) -> u64 {
        self.count
    }
}

/// Canonicalizes `path` and requires it to be a directory.
pub fn canonical_root(path: &Path) -> anyhow::Result<PathBuf> {
    let root = std::fs::canonicalize(path)
        .map_err(|e| anyhow::anyhow!("cannot resolve {}: {e}", path.display()))?;
    if !root.is_dir() {
        anyhow::bail!("{} is not a directory", path.display());
    }
    Ok(root)
}

/// The walk of `root` for a run that uses the manifest at `loc`: the store is flagged when
/// it lies inside the tree, and a SQLite store's sidecars are skipped.
pub fn walk_config(common: &CommonOpts, root: &Path, loc: &Location) -> WalkConfig {
    WalkConfig {
        root: root.to_path_buf(),
        cross_mount_points: common.cross_mount_points,
        sort: common.sort,
        store_path: store::store_path_inside(root, loc),
        sqlite_store: loc.backend == Backend::Sqlite,
        builtin_excludes: common.builtin_excludes,
    }
}
