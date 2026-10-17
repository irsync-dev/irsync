//! Parallel directory walk that streams entries in `RelPath` order.

use crate::filter::{FilterSet, Verdict};
use crate::fs::meta::lstat_entry_with_ctime;
use crate::fs::paths::RelPath;
use irsync_proto::v1::FileEntry;
use jwalk::{Parallelism, WalkDirGeneric};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone)]
pub struct ScanItem {
    pub path: RelPath,
    pub entry: FileEntry,
    pub ctime: (i64, u32),
}

/// Entries seen by kind, for `--stats`. Hard links count as regular files.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ListStats {
    pub reg: u64,
    pub dir: u64,
    pub link: u64,
    pub special: u64,
    /// Sum of the sizes of the regular files.
    pub total_size: u64,
}

impl ListStats {
    pub fn count(&mut self, e: &irsync_proto::v1::FileEntry) {
        use irsync_proto::v1::file_entry::Kind;
        match e.kind() {
            Kind::Regular | Kind::Hardlink => {
                self.reg += 1;
                self.total_size += e.size;
            }
            Kind::Directory => self.dir += 1,
            Kind::Symlink => self.link += 1,
            _ => self.special += 1,
        }
    }

    pub fn add(&mut self, o: &ListStats) {
        self.reg += o.reg;
        self.dir += o.dir;
        self.link += o.link;
        self.special += o.special;
        self.total_size += o.total_size;
    }

    pub fn files(&self) -> u64 {
        self.reg + self.dir + self.link + self.special
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScanSummary {
    /// Filled by the session's listing (not by the walk itself).
    pub listed: ListStats,
    pub entries: u64,
    pub incomplete: bool,
    pub errors: Vec<(RelPath, String)>,
}

#[derive(Debug, Clone, Copy)]
pub struct WalkOpts {
    /// Walk the target if the root itself is a symlink to a directory.
    pub follow_root_symlink: bool,
    pub threads: usize,
}

fn rel_of(root: &Path, full: &Path) -> Option<RelPath> {
    let rest = full.strip_prefix(root).ok()?;
    RelPath::new(rest.as_os_str().as_bytes()).ok()
}

/// Walk `root`, sending every non-filtered entry (root first) in increasing `RelPath` order.
pub fn walk(
    root: PathBuf,
    filters: Arc<FilterSet>,
    opts: WalkOpts,
) -> (mpsc::Receiver<ScanItem>, oneshot::Receiver<ScanSummary>) {
    let (tx, rx) = mpsc::channel(1024);
    let (stx, srx) = oneshot::channel();
    tokio::task::spawn_blocking(move || {
        let summary = run_walk(root, filters, opts, tx);
        let _ = stx.send(summary);
    });
    (rx, srx)
}

fn run_walk(
    mut root: PathBuf,
    filters: Arc<FilterSet>,
    opts: WalkOpts,
    tx: mpsc::Sender<ScanItem>,
) -> ScanSummary {
    let mut sum = ScanSummary::default();
    if opts.follow_root_symlink && std::fs::metadata(&root).is_ok_and(|m| m.is_dir()) {
        if let Ok(real) = std::fs::canonicalize(&root) {
            root = real;
        }
    }
    let send = |sum: &mut ScanSummary, rel: RelPath| -> bool {
        match lstat_entry_with_ctime(&root, &rel) {
            Ok((entry, ctime)) => {
                if tx
                    .blocking_send(ScanItem {
                        path: rel,
                        entry,
                        ctime,
                    })
                    .is_err()
                {
                    return false;
                }
                sum.entries += 1;
            }
            Err(e) => {
                sum.incomplete = true;
                sum.errors.push((rel, e.to_string()));
            }
        }
        true
    };
    if !send(&mut sum, RelPath::root()) {
        return sum;
    }

    let cb_root = root.clone();
    let walker = WalkDirGeneric::<((), ())>::new(&root)
        .sort(true)
        .skip_hidden(false)
        .follow_links(false)
        .parallelism(Parallelism::RayonNewPool(opts.threads.max(1)))
        .process_read_dir(move |_, dir, _, children| {
            let Some(dir_rel) = rel_of(&cb_root, dir) else {
                return;
            };
            children.retain_mut(|c| {
                let Ok(e) = c else { return true };
                let Ok(rel) = dir_rel.join(e.file_name().as_bytes()) else {
                    return false;
                };
                if filters.matches(&rel, e.file_type().is_dir()) == Verdict::Exclude {
                    return false;
                }
                true
            });
        });

    for item in walker {
        match item {
            Ok(e) => {
                let Some(rel) = rel_of(&root, &e.path()) else {
                    continue;
                };
                if let Some(err) = e.read_children.as_ref().and_then(|r| r.error()) {
                    sum.incomplete = true;
                    sum.errors.push((rel.clone(), err.to_string()));
                }
                if e.depth() == 0 {
                    continue;
                }
                if !send(&mut sum, rel) {
                    return sum;
                }
            }
            Err(err) => {
                sum.incomplete = true;
                let rel = err
                    .path()
                    .and_then(|p| rel_of(&root, p))
                    .unwrap_or_default();
                sum.errors.push((rel, err.to_string()));
            }
        }
    }
    sum
}
