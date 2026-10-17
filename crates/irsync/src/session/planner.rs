//! The receiver's planner (rsync-generator style): merge-joins the sender's
//! listing with the local one and acts on every source path in order.
//! Directories, symlinks, specials and metadata-only changes are applied
//! here; regular files that need data are requested with `PlanItem` and
//! handed to the receiver's main loop.

use std::collections::{HashMap, HashSet};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::StreamExt;
use irsync_proto::v1::{
    control::Msg, file_entry::Kind, plan_item::Action, FileEntry, PlanItem, SessionPlan,
};
use tokio::sync::mpsc;

use super::control::{Ev, Out};
use super::engine::{blocking, Ctx};
use super::receiver::Done;
use crate::fs::meta::{apply_metadata, create_special};
use crate::fs::paths::RelPath;
use crate::plan::{decide, merge_join, Decision, HashLookup, PlanOpts, Side};

/// A regular file the planner requested from the sender.
pub(super) struct Planned {
    pub file_id: u64,
    pub path: RelPath,
    /// The sender's listing entry (metadata applied at finalize).
    pub entry: FileEntry,
    pub action: Action,
    /// Rename detection found this local file with the same content (an
    /// index key, i.e. relative to the index root, not the session root).
    pub local_copy: Option<RelPath>,
}

pub(super) struct DirEntry {
    pub path: RelPath,
    pub entry: FileEntry,
    /// Created or metadata differs: apply even if no child changed.
    pub apply: bool,
}

#[derive(Default)]
pub(super) struct PlannerReport {
    pub skipped: u64,
    pub errors: Vec<(RelPath, String)>,
    /// Source directories in path order (applied in reverse at the end).
    pub dirs: Vec<DirEntry>,
    /// Directories whose contents this session changes (their mtime moves).
    pub touched: HashSet<RelPath>,
}

/// What an earlier connection of a resumed session already planned: its file
/// ids are kept (so acknowledgements stay meaningful) and files it placed
/// are not requested again.
#[derive(Default)]
pub(super) struct Prior {
    /// Planned path -> (file id, placed).
    by_path: HashMap<RelPath, (u64, bool)>,
    /// The highest file id used so far.
    max_id: u64,
}

impl Prior {
    pub fn new(plan: &SessionPlan, is_done: impl Fn(u64) -> bool) -> Prior {
        let mut p = Prior::default();
        for e in &plan.entries {
            if let Ok(path) = RelPath::new(&e.path) {
                p.by_path.insert(path, (e.file_id, is_done(e.file_id)));
            }
            p.max_id = p.max_id.max(e.file_id);
        }
        p
    }
}

fn parent_of(p: &RelPath) -> RelPath {
    let b = p.as_bytes();
    match b.iter().rposition(|&c| c == b'/') {
        Some(i) => RelPath::new(&b[..i]).unwrap_or_default(),
        None => RelPath::root(),
    }
}

/// `.irsync.<name>.tmp` next to `full`, for atomic replacement of links and specials.
fn temp_sibling(full: &Path) -> PathBuf {
    let mut name = b".irsync.".to_vec();
    name.extend_from_slice(full.file_name().map_or(&b""[..], |n| n.as_bytes()));
    name.extend_from_slice(b".tmp");
    full.with_file_name(std::ffi::OsStr::from_bytes(&name))
}

/// Creates `full` from `make`, applies metadata, then renames it into place.
fn place_via_temp(
    full: &Path,
    make: impl FnOnce(&Path) -> io::Result<()>,
    entry: &FileEntry,
    apply: &crate::fs::meta::ApplyOpts,
) -> io::Result<()> {
    let tmp = temp_sibling(full);
    match std::fs::remove_file(&tmp) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let res = make(&tmp)
        .and_then(|()| apply_metadata(&tmp, entry, apply))
        .and_then(|()| std::fs::rename(&tmp, full));
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// Makes `full` a directory, replacing a non-directory in the way.
pub(super) fn mkdir_replacing(full: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(full) {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => {
            std::fs::remove_file(full)?;
            std::fs::create_dir(full)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => std::fs::create_dir(full),
        Err(e) => Err(e),
    }
}

/// A precomputed answer for `decide`'s rename detection.
struct Found(Option<RelPath>);

impl HashLookup for Found {
    fn path_with_whole_hash(&self, _h: &[u8; 32]) -> Option<RelPath> {
        self.0.clone()
    }
}

fn stream_of<T: Send + 'static>(
    rx: mpsc::UnboundedReceiver<T>,
) -> impl futures::Stream<Item = T> + Send {
    futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|x| (x, rx)) })
}

struct Planner {
    ctx: Arc<Ctx>,
    out: Out,
    events: mpsc::UnboundedSender<Ev>,
    popts: PlanOpts,
    report: PlannerReport,
    ok_dirs: HashSet<RelPath>,
    failed_dirs: HashSet<RelPath>,
    next_id: u64,
    prior: Prior,
}

impl Planner {
    fn error(&mut self, path: &RelPath, msg: impl Into<String>) {
        let msg = msg.into();
        tracing::warn!(?path, error = %msg, "receiver");
        self.report.errors.push((path.clone(), msg));
    }

    fn touch_parent(&mut self, path: &RelPath) {
        if !path.as_bytes().is_empty() {
            self.report.touched.insert(parent_of(path));
        }
    }

    /// Rename detection: a local file already holding this content.
    async fn lookup(&self, path: &RelPath, src: &Side, dst: Option<&Side>) -> Found {
        let wanted = self.popts.rename_detection
            && src.entry.kind() == Kind::Regular
            && dst.is_none_or(|d| d.entry.kind() != Kind::Regular);
        let Some(d) = src.digest.as_ref().filter(|_| wanted) else {
            return Found(None);
        };
        // Lookups and results are index keys (relative to the index root).
        let (index, hash, key) = (self.ctx.index().clone(), d.hash, self.ctx.key(path));
        let found = blocking(move || {
            let ix = index
                .lock()
                .map_err(|_| io::Error::other("index poisoned"))?;
            let paths = ix
                .find_paths_by_whole_hash(&hash)
                .map_err(io::Error::other)?;
            Ok(paths.into_iter().find(|p| *p != key))
        })
        .await;
        Found(found.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "rename lookup failed");
            None
        }))
    }

    async fn one(&mut self, path: RelPath, src: Side, dst: Option<Side>) {
        let is_dir = src.entry.kind() == Kind::Directory;
        if path.as_bytes().is_empty() {
            if !is_dir {
                self.error(&path, "source root is not a directory");
                self.failed_dirs.insert(path);
                return;
            }
        } else {
            let parent = parent_of(&path);
            if self.failed_dirs.contains(&parent) {
                self.error(&path, "not created: its parent directory failed");
                if is_dir {
                    self.failed_dirs.insert(path);
                }
                return;
            }
            if !self.ok_dirs.contains(&parent) {
                self.error(&path, "parent is not a directory in the source listing");
                if is_dir {
                    self.failed_dirs.insert(path);
                }
                return;
            }
        }
        if src.entry.kind() == Kind::Symlink && !self.popts.preserve.links {
            tracing::debug!(?path, "skipping symlink (links not preserved)");
            return;
        }
        let found = self.lookup(&path, &src, dst.as_ref()).await;
        let decision = decide(Some(&src), dst.as_ref(), &self.popts, &found);
        let full = self.ctx.full(&path);
        let apply = self.ctx.apply_opts(&src.entry);
        let entry = src.entry.clone();
        let res = match decision {
            Decision::Skip | Decision::Delete => {
                if is_dir {
                    self.ok_dirs.insert(path.clone());
                    self.report.dirs.push(DirEntry {
                        path,
                        entry,
                        apply: false,
                    });
                } else if entry.kind() == Kind::Regular {
                    self.report.skipped += 1;
                }
                return;
            }
            Decision::Mkdir => {
                let res = blocking(move || mkdir_replacing(&full)).await;
                match res {
                    Ok(()) => {
                        self.touch_parent(&path);
                        self.ok_dirs.insert(path.clone());
                        self.report.dirs.push(DirEntry {
                            path,
                            entry,
                            apply: true,
                        });
                    }
                    Err(e) => {
                        self.error(&path, format!("mkdir: {e}"));
                        self.failed_dirs.insert(path);
                    }
                }
                return;
            }
            Decision::MetaOnly if is_dir => {
                // Applied after the children, like every directory.
                self.ok_dirs.insert(path.clone());
                self.report.dirs.push(DirEntry {
                    path,
                    entry,
                    apply: true,
                });
                return;
            }
            Decision::MetaOnly => blocking(move || apply_metadata(&full, &entry, &apply)).await,
            Decision::Symlink => {
                self.touch_parent(&path);
                blocking(move || {
                    let target = std::ffi::OsStr::from_bytes(&entry.link_target).to_owned();
                    place_via_temp(
                        &full,
                        |tmp| std::os::unix::fs::symlink(&target, tmp),
                        &entry,
                        &apply,
                    )
                })
                .await
            }
            Decision::Special => {
                self.touch_parent(&path);
                blocking(move || {
                    place_via_temp(&full, |tmp| create_special(tmp, &entry), &entry, &apply)
                })
                .await
            }
            Decision::Whole | Decision::Delta | Decision::LocalCopy { .. } => {
                // Placing it (now or on an earlier connection) moved the parent's mtime.
                self.touch_parent(&path);
                let file_id = match self.prior.by_path.get(&path) {
                    // Placed by an earlier connection of this session.
                    Some(&(_, true)) => {
                        self.report.skipped += 1;
                        return;
                    }
                    Some(&(id, false)) => id,
                    None => {
                        self.next_id += 1;
                        self.next_id
                    }
                };
                let (action, local_copy) = match decision {
                    Decision::Delta => (Action::Delta, None),
                    Decision::LocalCopy { from } => (Action::Whole, Some(from)),
                    _ => (Action::Whole, None),
                };
                // Registered with the main loop before the request can be answered.
                let _ = self.events.send(Ev::Recv(Done::Planned(Box::new(Planned {
                    file_id,
                    path: path.clone(),
                    entry,
                    action,
                    local_copy,
                }))));
                self.out.send(Msg::Plan(PlanItem {
                    file_id,
                    path: path.as_bytes().to_vec(),
                    action: action as i32,
                }));
                return;
            }
        };
        if let Err(e) = res {
            self.error(&path, e.to_string());
        }
    }
}

/// Merge-joins the listings and acts on every path; reports at the end.
pub(super) async fn plan(
    ctx: Arc<Ctx>,
    out: Out,
    events: mpsc::UnboundedSender<Ev>,
    peer: mpsc::UnboundedReceiver<Side>,
    local: mpsc::UnboundedReceiver<Side>,
    prior: Prior,
) {
    let popts = PlanOpts {
        checksum: ctx.opts.checksum,
        update_only: ctx.opts.update_only,
        // Deletions are a later pass (not in this engine yet).
        delete: false,
        rename_detection: ctx.opts.rename_detection,
        preserve: ctx.opts.preserve,
    };
    let mut p = Planner {
        ctx,
        out,
        events: events.clone(),
        popts,
        report: PlannerReport::default(),
        ok_dirs: HashSet::new(),
        failed_dirs: HashSet::new(),
        next_id: prior.max_id,
        prior,
    };
    let joined = merge_join(stream_of(peer), stream_of(local));
    futures::pin_mut!(joined);
    while let Some((path, src, dst)) = joined.next().await {
        if let Some(src) = src {
            p.one(path, src, dst).await;
        }
    }
    let _ = events.send(Ev::Recv(Done::PlannerFinished(Box::new(p.report))));
}
