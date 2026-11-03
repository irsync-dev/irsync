//! Index warm-up for destinations irsync did not create.
//!
//! Cross-file reuse and rename detection read the receiver's index, and
//! the index only knows files irsync has hashed: those it placed, `DELTA`
//! targets, and `-c` runs. A destination filled by anything else (a prior
//! rsync, a restore, a manual copy) contributes nothing until its files
//! are hashed. Two ways in:
//!
//! - [`Cold`]: the receiver's own walk notes every regular file without a
//!   fresh index row, by size. When a whole file is about to be fetched,
//!   up to [`CANDIDATES`] destination files of exactly its size are hashed
//!   first, through the ordinary hasher so the rows persist, and the chunk
//!   lookup that follows finds a renamed or copied file. Sizes below
//!   [`MIN_SIZE`] are left out: for a file smaller than a chunk the
//!   bookkeeping costs about what the fetch does.
//! - [`index_root`]: `irsync index`, or an export's `warm_index` setting:
//!   walk a root and hash every regular file whose row is not fresh.
//!
//! Nothing here can make a wrong decision: a candidate's chunks are
//! verified when copied like every other local source, and a stale row
//! degrades to a network fetch.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use irsync_proto::chunk::CHUNK_PARAMS;
use irsync_proto::v1::file_entry::Kind;

use crate::filter::FilterSet;
use crate::fs::paths::RelPath;
use crate::hasher::Hasher;
use crate::index::IndexHandle;
use crate::scan::{walk, ScanSummary, WalkOpts};

/// Destination files hashed, at most, before one whole file is fetched.
pub const CANDIDATES: usize = 4;
/// Files smaller than this are never candidates.
pub const MIN_SIZE: u64 = CHUNK_PARAMS.min as u64;
const WALK_THREADS: usize = 4;

/// Regular files a receiver's walk listed without a fresh index row, by
/// size: index keys, in walk order.
#[derive(Debug, Default)]
pub struct Cold {
    by_size: Mutex<HashMap<u64, Vec<RelPath>>>,
}

impl Cold {
    /// Notes `key`, a regular file of `size` bytes with no fresh row.
    pub fn record(&self, size: u64, key: RelPath) {
        if size < MIN_SIZE {
            return;
        }
        self.by_size
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(size)
            .or_default()
            .push(key);
    }

    /// Up to [`CANDIDATES`] files of exactly `size`, each handed out once.
    pub fn take(&self, size: u64) -> Vec<RelPath> {
        let mut m = self.by_size.lock().unwrap_or_else(|e| e.into_inner());
        let Some(v) = m.get_mut(&size) else {
            return Vec::new();
        };
        let n = v.len().min(CANDIDATES);
        let out: Vec<RelPath> = v.drain(..n).collect();
        if v.is_empty() {
            m.remove(&size);
        }
        out
    }
}

/// What [`index_root`] did (or, on a dry run, would do).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WarmReport {
    /// Regular files seen.
    pub examined: u64,
    /// Files read and recorded.
    pub hashed: u64,
    /// Their bytes.
    pub bytes: u64,
    /// Files that could not be read or listed; the pass carries on.
    pub errors: Vec<(RelPath, String)>,
    /// The walk could not list everything.
    pub incomplete: bool,
}

/// Walks `root` and hashes every regular file whose index row is not fresh
/// (size, mtime, ctime and inode unchanged), `workers` at a time, storing
/// the digests in `index`. irsync's own temporary files are left out. With
/// `dry_run` nothing is read; the report counts what would be.
pub async fn index_root(
    index: IndexHandle,
    root: &Path,
    workers: usize,
    dry_run: bool,
) -> io::Result<WarmReport> {
    let workers = workers.max(1);
    let hasher = Hasher::new(index, root.to_path_buf(), workers);
    let (rx, summary) = walk(
        root.to_path_buf(),
        Arc::new(FilterSet::new(vec![])),
        WalkOpts {
            follow_root_symlink: true,
            threads: WALK_THREADS,
            max_depth: None,
        },
        None,
    );
    let report = Mutex::new(WarmReport::default());
    let (report_ref, hasher_ref) = (&report, &hasher);
    let items =
        futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|x| (x, rx)) })
            .filter_map(|ev| async move {
                match ev {
                    crate::scan::ScanEvent::Entry(item) => Some(item),
                    // Recorded in the summary at the end.
                    crate::scan::ScanEvent::Gap { .. } => None,
                }
            });
    items
        .for_each_concurrent(workers, |item| async move {
            let (report, hasher) = (report_ref, hasher_ref);
            let own_temp = item
                .path
                .components()
                .last()
                .is_some_and(crate::recv::is_temp_name);
            if item.entry.kind() != Kind::Regular || own_temp {
                return;
            }
            // No guard is held across the await below.
            let lock = || report.lock().unwrap_or_else(|e| e.into_inner());
            let stale = {
                let mut r = lock();
                r.examined += 1;
                match hasher.cached(&item) {
                    Ok(Some(_)) => false,
                    Ok(None) => true,
                    Err(e) => {
                        r.errors.push((item.path.clone(), e.to_string()));
                        false
                    }
                }
            };
            if !stale {
                return;
            }
            if dry_run {
                let mut r = lock();
                r.hashed += 1;
                r.bytes += item.entry.size;
                return;
            }
            let res = hasher.digest(&item).await;
            let mut r = lock();
            match res {
                Ok(d) => {
                    r.hashed += 1;
                    r.bytes += d.size;
                }
                Err(e) => r.errors.push((item.path.clone(), e.to_string())),
            }
        })
        .await;
    let summary = summary.await.unwrap_or_else(|_| ScanSummary {
        incomplete: true,
        ..ScanSummary::default()
    });
    // The digests are in the index once this returns.
    tokio::task::spawn_blocking(move || hasher.flush())
        .await
        .map_err(io::Error::other)?;
    let mut r = report.into_inner().unwrap_or_else(|e| e.into_inner());
    r.incomplete = summary.incomplete;
    r.errors.extend(summary.errors);
    Ok(r)
}
