//! Engines behind the `update`, `check` and `list` verbs.

pub mod check;
pub mod list;
pub mod update;

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::platform::Platform;
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
    pub walk_threads: usize,
    pub fail_on_warnings: bool,
    pub builtin_excludes: bool,
    pub verbose: bool,
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

/// The tree-relative path of the directory holding an in-tree store. Its mtime, like the
/// root's, is not compared: writing the manifest (and its sidecars) changes both.
pub fn store_parent(cfg: &WalkConfig, root: &Path) -> Option<Vec<u8>> {
    cfg.store_path
        .as_deref()
        .and_then(Path::parent)
        .and_then(|p| p.strip_prefix(root).ok())
        .map(|rel| match crate::platform::path_to_rel(rel) {
            r if r.is_empty() => b".".to_vec(),
            r => r,
        })
}

/// Report lines on stdout. With `verbose`, every line is flushed as it is written, so the
/// lines show up while the run is going.
pub struct Report<'a> {
    out: &'a mut dyn Write,
    verbose: bool,
}

impl<'a> Report<'a> {
    pub fn new(out: &'a mut dyn Write, verbose: bool) -> Self {
        Report { out, verbose }
    }

    /// A line printed with or without `-v`.
    pub fn line(&mut self, text: &str) -> io::Result<()> {
        writeln!(self.out, "{text}")?;
        if self.verbose {
            self.out.flush()?;
        }
        Ok(())
    }

    /// A line printed only with `-v`; `text` is not called otherwise.
    pub fn verbose_line(&mut self, text: impl FnOnce() -> String) -> io::Result<()> {
        if self.verbose {
            self.line(&text())?;
        }
        Ok(())
    }
}

/// What `check` and `list` say when the manifest at `loc` does not open. SQLite and fjall
/// write to a manifest even to read it, so read-only media are a likely cause there.
pub fn cannot_open(loc: &Location) -> String {
    let hint = match loc.backend {
        Backend::Sqlite | Backend::Fjall => {
            "; if it is on read-only media, copy it elsewhere and pass --manifest"
        }
        Backend::Redb => "",
    };
    format!("cannot open manifest at {}{hint}", loc.path.display())
}

/// Whether a manifest made on `manifest` can be used on `running`.
/// The one place that decides; step 3 of the Windows work replaces the refusal.
pub fn usable_here(manifest: Platform, running: Platform) -> anyhow::Result<()> {
    if manifest == running {
        return Ok(());
    }
    anyhow::bail!(
        "this manifest was made on {}, and this is {}; fileset cannot use a manifest across the \
         two yet",
        manifest.as_str(),
        running.as_str()
    )
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
        threads: common.walk_threads,
        force_xattr_reads: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usable_here_allows_the_same_family_only() {
        usable_here(Platform::Unix, Platform::Unix).unwrap();
        usable_here(Platform::Windows, Platform::Windows).unwrap();
        let e = usable_here(Platform::Unix, Platform::Windows).unwrap_err();
        assert_eq!(
            e.to_string(),
            "this manifest was made on unix, and this is windows; fileset cannot use a manifest \
             across the two yet"
        );
        let e = usable_here(Platform::Windows, Platform::Unix).unwrap_err();
        assert_eq!(
            e.to_string(),
            "this manifest was made on windows, and this is unix; fileset cannot use a manifest \
             across the two yet"
        );
    }
}
