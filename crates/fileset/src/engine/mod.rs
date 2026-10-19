//! Engines behind the `update` and `check` verbs.

pub mod check;
pub mod update;

use std::io::{self, Write};
use std::path::{Path, PathBuf};

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
}

/// Writes `WARN <text>` lines to `err` and counts them.
pub struct Warnings<'a> {
    err: &'a mut dyn Write,
    count: u64,
}

impl<'a> Warnings<'a> {
    pub fn new(err: &'a mut dyn Write) -> Self {
        Warnings { err, count: 0 }
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
