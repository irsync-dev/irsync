//! The receiver's deletion pass (`--delete*`): removes destination paths the
//! source no longer has. It is the one operation that can destroy an
//! archive, so it is guarded:
//!
//! - it runs only when a delete mode was asked for (the planner collects no
//!   candidates otherwise);
//! - it refuses when either listing was incomplete, unless `ignore_errors`;
//! - it never follows symlinks: a symlink is removed with `remove_file`, and a
//!   candidate is skipped when any of its parent components is no longer a
//!   real directory;
//! - directories are removed with `remove_dir` only, after their contents
//!   (deepest first); a directory that still holds something is an error for
//!   that path, never a recursive removal;
//! - it stops at `max_delete` (rsync exit code 25);
//! - on a dry run it only lists what it would delete.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use irsync_proto::v1::file_entry::Kind;

use crate::cli::DeleteMode;
use crate::fs::paths::RelPath;

/// The deletion pass was not run because a listing was incomplete: a path
/// missing from the source listing may only be unreadable, not gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteRefused;

impl std::fmt::Display for DeleteRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "deletions skipped: a file listing was incomplete (--ignore-errors deletes anyway)",
        )
    }
}

impl std::error::Error for DeleteRefused {}

/// What a deletion pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeleteReport {
    /// Removed, in the order removed (deepest first).
    pub deleted: Vec<RelPath>,
    /// Dry run: what would have been removed, in the same order.
    pub would_delete: Vec<RelPath>,
    /// `max_delete` was reached with candidates left.
    pub stopped_at_max: bool,
    /// Paths that could not be removed.
    pub errors: Vec<(RelPath, String)>,
}

/// Collects deletion candidates while the planner walks the merged listings,
/// then removes them in one [`DeletePass::execute`].
#[derive(Debug)]
pub struct DeletePass {
    mode: DeleteMode,
    max_delete: Option<u64>,
    delete_excluded: bool,
    dry_run: bool,
    ignore_errors: bool,
    /// Deletions an earlier connection of the same session already made
    /// (they count against `max_delete`).
    spent: u64,
    candidates: Vec<(RelPath, Kind)>,
    /// The candidates cannot be trusted (a listing was malformed): refuse
    /// even with `ignore_errors`.
    unreliable: bool,
    /// Directories that hold a protected (excluded) path: never removed.
    kept: HashSet<RelPath>,
}

impl DeletePass {
    pub fn new(
        mode: DeleteMode,
        max_delete: Option<u64>,
        delete_excluded: bool,
        dry_run: bool,
        ignore_errors: bool,
    ) -> DeletePass {
        DeletePass {
            mode,
            max_delete,
            delete_excluded,
            dry_run,
            ignore_errors,
            spent: 0,
            candidates: Vec::new(),
            unreliable: false,
            kept: HashSet::new(),
        }
    }

    pub fn mode(&self) -> DeleteMode {
        self.mode
    }

    /// Whether destination paths the receiver's filters exclude are deleted too.
    pub fn delete_excluded(&self) -> bool {
        self.delete_excluded
    }

    /// Deletions already made by earlier connections of this session.
    pub fn set_spent(&mut self, n: u64) {
        self.spent = n;
    }

    /// A destination path the planner decided to delete
    /// ([`crate::plan::Decision::Delete`]).
    pub fn plan(&mut self, path: RelPath, kind: Kind) {
        if path.as_bytes().is_empty() {
            // Never the session root.
            return;
        }
        self.candidates.push((path, kind));
    }

    /// A destination path that must survive (excluded, without
    /// `delete_excluded`): its parent directories are not removed either.
    pub fn protect(&mut self, path: &RelPath) {
        let mut b = path.as_bytes();
        while let Some(i) = b.iter().rposition(|&c| c == b'/') {
            b = &b[..i];
            let Ok(p) = RelPath::new(b) else { break };
            // Already kept: so are its parents.
            if !self.kept.insert(p) {
                break;
            }
        }
    }

    /// A listing was malformed (out of order): the pass will refuse,
    /// `ignore_errors` or not.
    pub fn mark_unreliable(&mut self) {
        self.unreliable = true;
    }

    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    /// Runs the pass under `root` (blocking work off the async threads).
    /// Refuses when `listing_incomplete` unless `ignore_errors`.
    pub async fn execute(
        self,
        root: PathBuf,
        listing_incomplete: bool,
    ) -> Result<DeleteReport, DeleteRefused> {
        if self.unreliable || (listing_incomplete && !self.ignore_errors) {
            tracing::warn!(
                candidates = self.candidates.len(),
                "a listing is incomplete: skipping deletions"
            );
            return Err(DeleteRefused);
        }
        let res = tokio::task::spawn_blocking(move || self.run(&root)).await;
        Ok(res.unwrap_or_else(|e| DeleteReport {
            errors: vec![(RelPath::root(), format!("deletion pass failed: {e}"))],
            ..DeleteReport::default()
        }))
    }

    fn run(mut self, root: &Path) -> DeleteReport {
        // Deepest first; among equals, reverse path order (children before
        // their directory however the names sort).
        self.candidates.sort_by(|(a, _), (b, _)| {
            let depth = |p: &RelPath| p.components().count();
            depth(b).cmp(&depth(a)).then_with(|| b.cmp(a))
        });
        let mut rep = DeleteReport::default();
        let mut count = self.spent;
        let mut real_dirs: HashSet<PathBuf> = HashSet::new();
        for (path, _) in std::mem::take(&mut self.candidates) {
            if self.kept.contains(&path) {
                tracing::info!(?path, "not deleting a directory that holds excluded files");
                continue;
            }
            if self.max_delete.is_some_and(|max| count >= max) {
                rep.stopped_at_max = true;
                break;
            }
            if self.dry_run {
                rep.would_delete.push(path);
                count += 1;
                continue;
            }
            match remove_one(root, &path, &mut real_dirs) {
                Ok(true) => {
                    tracing::info!(?path, "deleted");
                    rep.deleted.push(path);
                    count += 1;
                }
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(?path, error = %e, "delete failed");
                    rep.errors.push((path, format!("delete: {e}")));
                }
            }
        }
        if rep.stopped_at_max {
            tracing::warn!(
                max = self.max_delete,
                "deletions stopped at the --max-delete limit"
            );
        }
        rep
    }
}

/// Whether every component of `path` below `root` up to its parent is a real
/// directory (not a symlink), so removing `root/path` stays inside the tree.
fn parents_are_dirs(root: &Path, path: &RelPath, real_dirs: &mut HashSet<PathBuf>) -> bool {
    let comps: Vec<&[u8]> = path.components().collect();
    let mut p = root.to_path_buf();
    for c in &comps[..comps.len().saturating_sub(1)] {
        p.push(<std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(c));
        if real_dirs.contains(&p) {
            continue;
        }
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_dir() => {
                real_dirs.insert(p.clone());
            }
            _ => return false,
        }
    }
    true
}

/// Removes one path: `remove_dir` for a directory (fails unless empty),
/// `remove_file` for anything else, symlinks included (never followed).
/// `Ok(false)` when it is already gone or no longer inside the tree.
fn remove_one(root: &Path, path: &RelPath, real_dirs: &mut HashSet<PathBuf>) -> io::Result<bool> {
    if !parents_are_dirs(root, path, real_dirs) {
        tracing::warn!(?path, "not deleted: a parent is no longer a directory");
        return Ok(false);
    }
    let full = root.join(path.to_os());
    let md = match std::fs::symlink_metadata(&full) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let res = if md.file_type().is_dir() {
        std::fs::remove_dir(&full)
    } else {
        std::fs::remove_file(&full)
    };
    match res {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(s: &str) -> RelPath {
        RelPath::new(s.as_bytes()).unwrap()
    }

    fn pass(max: Option<u64>, dry_run: bool, ignore_errors: bool) -> DeletePass {
        DeletePass::new(DeleteMode::After, max, false, dry_run, ignore_errors)
    }

    fn tree() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("d/e")).unwrap();
        std::fs::write(t.path().join("d/e/f"), b"f").unwrap();
        std::fs::write(t.path().join("d/g"), b"g").unwrap();
        std::fs::write(t.path().join("h"), b"h").unwrap();
        t
    }

    fn plan_all(p: &mut DeletePass) {
        for (s, k) in [
            ("d", Kind::Directory),
            ("d/e", Kind::Directory),
            ("d/e/f", Kind::Regular),
            ("d/g", Kind::Regular),
            ("h", Kind::Regular),
        ] {
            p.plan(rel(s), k);
        }
    }

    #[tokio::test]
    async fn deletes_deepest_first() {
        let t = tree();
        let mut p = pass(None, false, false);
        plan_all(&mut p);
        let rep = p.execute(t.path().to_path_buf(), false).await.unwrap();
        assert!(rep.errors.is_empty(), "{:?}", rep.errors);
        let order: Vec<&[u8]> = rep.deleted.iter().map(|p| p.as_bytes()).collect();
        assert_eq!(order, [&b"d/e/f"[..], b"d/g", b"d/e", b"h", b"d"]);
        assert_eq!(std::fs::read_dir(t.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn refuses_when_incomplete_unless_ignore_errors() {
        let t = tree();
        let mut p = pass(None, false, false);
        plan_all(&mut p);
        assert_eq!(
            p.execute(t.path().to_path_buf(), true).await,
            Err(DeleteRefused)
        );
        assert!(t.path().join("d/e/f").exists());
        let mut p = pass(None, false, true);
        plan_all(&mut p);
        let rep = p.execute(t.path().to_path_buf(), true).await.unwrap();
        assert_eq!(rep.deleted.len(), 5);
        // A malformed listing is refused even with ignore_errors.
        let t = tree();
        let mut p = pass(None, false, true);
        plan_all(&mut p);
        p.mark_unreliable();
        assert_eq!(
            p.execute(t.path().to_path_buf(), false).await,
            Err(DeleteRefused)
        );
        assert!(t.path().join("h").exists());
    }

    #[tokio::test]
    async fn dry_run_and_max_delete() {
        let t = tree();
        let mut p = pass(Some(2), true, false);
        plan_all(&mut p);
        let rep = p.execute(t.path().to_path_buf(), false).await.unwrap();
        assert!(rep.deleted.is_empty());
        assert_eq!(rep.would_delete, [rel("d/e/f"), rel("d/g")]);
        assert!(rep.stopped_at_max);
        assert!(t.path().join("d/e/f").exists());

        let mut p = pass(Some(3), false, false);
        p.set_spent(1);
        plan_all(&mut p);
        let rep = p.execute(t.path().to_path_buf(), false).await.unwrap();
        assert_eq!(rep.deleted, [rel("d/e/f"), rel("d/g")]);
        assert!(rep.stopped_at_max);
    }

    #[tokio::test]
    async fn symlinks_are_removed_not_followed() {
        let t = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("precious"), b"p").unwrap();
        std::os::unix::fs::symlink(outside.path(), t.path().join("link")).unwrap();
        let mut p = pass(None, false, false);
        // Even a planned path through the link stays untouched.
        p.plan(rel("link/precious"), Kind::Regular);
        p.plan(rel("link"), Kind::Symlink);
        let rep = p.execute(t.path().to_path_buf(), false).await.unwrap();
        assert_eq!(rep.deleted, [rel("link")]);
        assert!(outside.path().join("precious").exists());
    }

    #[tokio::test]
    async fn non_empty_directory_is_an_error_not_a_recursive_removal() {
        let t = tree();
        let mut p = pass(None, false, false);
        // `d/g` and `d/e/f` are not candidates: `d` cannot go.
        p.plan(rel("d"), Kind::Directory);
        let rep = p.execute(t.path().to_path_buf(), false).await.unwrap();
        assert!(rep.deleted.is_empty());
        assert_eq!(rep.errors.len(), 1);
        assert_eq!(rep.errors[0].0, rel("d"));
        assert!(t.path().join("d/e/f").exists());
    }

    #[tokio::test]
    async fn protected_paths_keep_their_directories() {
        let t = tree();
        let mut p = pass(None, false, false);
        p.plan(rel("d"), Kind::Directory);
        p.plan(rel("d/e"), Kind::Directory);
        p.plan(rel("d/e/f"), Kind::Regular);
        p.protect(&rel("d/g"));
        let rep = p.execute(t.path().to_path_buf(), false).await.unwrap();
        assert!(rep.errors.is_empty(), "{:?}", rep.errors);
        assert_eq!(rep.deleted, [rel("d/e/f"), rel("d/e")]);
        assert!(t.path().join("d/g").exists());
    }
}
