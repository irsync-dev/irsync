//! The receiver's planner (rsync-generator style): merge-joins the sender's
//! listing with the local one and acts on every source path in order.
//! Directories, symlinks, specials and metadata-only changes are applied
//! here; regular files that need data are requested with `PlanItem` and
//! handed to the receiver's main loop.
//!
//! With a delete mode, destination-only paths become deletion candidates
//! (excluded ones only with `delete_excluded`; the receiver's own walk is
//! unfiltered, so its filters are applied here). `--delete-before` buffers the
//! merged listing until both listings have ended, has the main loop run the
//! deletion pass, and only then acts on the source paths. `--delete-during`
//! deletes as it goes: whenever the merge crosses a directory boundary, the
//! candidates it has left behind (neither the current path nor an ancestor
//! of it) are deleted in one pass before the planner goes on; what is still
//! above the current path waits for the final pass, which runs after the
//! transfers as for `--delete-after`. Either listing reports its gaps (a
//! directory it could not read, an entry it could not stat) in path order,
//! so a pass knows whether the listing it acts on is complete so far: a gap
//! stops every later pass unless `--ignore-errors`. On a dry run nothing is
//! created, changed or requested.

use std::collections::{HashMap, HashSet};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use irsync_proto::v1::{
    control::Msg, file_entry::Kind, plan_item::Action, FileEntry, Itemized, SessionPlan,
};
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tracing::Instrument;

use super::control::{is_own_temp, Ev, Listed, Out};
use super::engine::{blocking, Ctx};
use super::receiver::Done;
use super::recv_io::{index_old_file, OldFile};
use crate::cli::DeleteMode;
use crate::delete::DeletePass;
use crate::filter::Verdict;
use crate::fs::meta::{apply_metadata, create_special};
use crate::fs::paths::RelPath;
use crate::plan::{decide, itemize, merge_join, meta_differs, Decision, PlanOpts, Side};
use crate::telemetry::meter::Activity;
use crate::telemetry::PhaseKind;

/// A regular file the planner requested from the sender.
pub(super) struct Planned {
    pub file_id: u64,
    pub path: RelPath,
    /// The sender's listing entry (metadata applied at finalize).
    pub entry: FileEntry,
    pub action: Action,
    /// `APPEND`: the trusted prefix length.
    pub append_from: u64,
    pub deferred: Deferred,
    /// One of the session's in-flight slots, released when the file leaves
    /// the receiver (placed or failed).
    pub slot: OwnedSemaphorePermit,
}

/// What the planner started early or held back for a `DELTA` file; empty
/// for other actions and for files of a resumed plan.
#[derive(Default)]
pub(super) struct Deferred {
    /// Hashing of the old destination file, started at plan time so it
    /// runs alongside the sender's digest of the new one.
    pub prehash: Option<JoinHandle<Option<OldFile>>>,
    /// The itemize line, emitted by the receiver once the chunk list shows
    /// whether the content changed.
    pub itemize: Option<Itemize>,
}

/// A `DELTA` file's itemize line as planned (a transfer) and as it reads if
/// the content proves unchanged (attributes only; none if nothing differs).
pub(super) struct Itemize {
    pub planned: String,
    pub meta: Option<String>,
}

/// At most this many requested files are in flight (planned, not yet
/// acknowledged) per session; the planner waits for a slot before the next.
pub(super) const MAX_INFLIGHT: usize = 4096;

/// Descriptors a file in flight may hold: its `.part` and, for a `DELTA`
/// or `APPEND`, the old file.
const FDS_PER_INFLIGHT: usize = 2;

/// Open files the in-flight window leaves to everything else: the
/// connection, the index and its journal, logs, hasher workers, local
/// chunk copies.
const NOFILE_RESERVE: usize = 256;

/// The in-flight window the process's soft open-file limit allows
/// ([`MAX_INFLIGHT`] at most, one at least).
fn inflight_for_limit(soft: u64) -> usize {
    let spare = usize::try_from(soft)
        .unwrap_or(usize::MAX)
        .saturating_sub(NOFILE_RESERVE);
    (spare / FDS_PER_INFLIGHT).clamp(1, MAX_INFLIGHT)
}

/// [`MAX_INFLIGHT`] (or a test's smaller cap), reduced to what the
/// open-file limit allows.
pub(super) fn inflight_cap(ctx: &Ctx) -> usize {
    #[cfg(feature = "test-hooks")]
    let cap = ctx.hooks.max_inflight.unwrap_or(MAX_INFLIGHT);
    #[cfg(not(feature = "test-hooks"))]
    let cap = {
        let _ = ctx;
        MAX_INFLIGHT
    };
    cap.min(inflight_for_limit(crate::fs::rlimit::nofile_soft()))
        .max(1)
}

/// A hard link (`-H`) the planner decided on: placed by the receiver after
/// every regular file, since its target may still be in flight.
pub(super) struct Linked {
    pub file_id: u64,
    pub path: RelPath,
    /// The sender's listing entry (kind `HARDLINK`, `link_target` = target).
    pub entry: FileEntry,
    pub target: RelPath,
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
    /// Source entries by kind (`--stats`).
    pub listed: crate::scan::ListStats,
    /// Items planned that did not exist at the destination.
    pub created: u64,
    pub errors: Vec<(RelPath, String)>,
    /// Source directories in path order (applied in reverse at the end).
    pub dirs: Vec<DirEntry>,
    /// Directories whose contents this session changes (their mtime moves).
    pub touched: HashSet<RelPath>,
    /// The deletion candidates, for the receiver to delete after the
    /// transfers (`None` without a delete mode or once run before them).
    pub deletes: Option<DeletePass>,
    /// The highest file id the plan used (an earlier connection's included).
    pub max_id: u64,
    /// Source regular files planned under a verified parent: the only
    /// paths the sender's final re-check may plan again (`Replan`).
    pub regular: HashSet<RelPath>,
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

pub(super) fn parent_of(p: &RelPath) -> RelPath {
    let b = p.as_bytes();
    match b.iter().rposition(|&c| c == b'/') {
        Some(i) => RelPath::new(&b[..i]).unwrap_or_default(),
        None => RelPath::root(),
    }
}

/// Whether a destination entry is a regular file with other names, which a
/// change in place would reach too.
fn shares_inode(d: &Side) -> bool {
    d.entry.kind() == Kind::Regular && d.entry.nlink > 1
}

/// `.irsync.<name>.tmp` next to `full` (shortened to fit `NAME_MAX` when
/// needed), for atomic replacement of links and specials.
fn temp_sibling(full: &Path) -> PathBuf {
    let name = full.file_name().map_or(&b""[..], |n| n.as_bytes());
    let name = crate::recv::temp_name(name, b".tmp");
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

/// Places `root/path` as a hard link to the regular file `root/target`
/// (`link`), or, without `-H` on this side, as an independent copy of it
/// with `entry`'s metadata. Both parents are resolved from `root` without
/// following any symlink, and every operation is relative to those
/// directory descriptors, so neither name can lead out of `root`. Whatever
/// is at `path` is replaced (never followed); a directory in the way is an
/// error.
pub(super) fn place_hardlink(
    root: &Path,
    target: &RelPath,
    path: &RelPath,
    link: bool,
    entry: &FileEntry,
    apply: &crate::fs::meta::ApplyOpts,
) -> io::Result<()> {
    use crate::fs::safe::{errno, is_regular_mode, open_dir_beneath, split_last};
    use nix::fcntl::{renameat, AtFlags, OFlag};
    use nix::sys::stat::{fstatat, Mode};
    use nix::unistd::{linkat, unlinkat, UnlinkatFlags};
    use std::ffi::OsStr;

    let ctx = |what: &str, e: io::Error| io::Error::new(e.kind(), format!("{what}: {e}"));
    let (tdir, tname) = split_last(target)?;
    let (ldir, lname) = split_last(path)?;
    let (tname, lname) = (OsStr::from_bytes(&tname), OsStr::from_bytes(&lname));
    let tfd = open_dir_beneath(root, &tdir).map_err(|e| ctx("hard link target", e))?;
    let lfd = open_dir_beneath(root, &ldir)?;
    let tm = fstatat(&tfd, tname, AtFlags::AT_SYMLINK_NOFOLLOW)
        .map_err(|e| ctx("hard link target", errno(e)))?;
    if !is_regular_mode(tm.st_mode as u32) {
        return Err(io::Error::other("hard link target is not a regular file"));
    }
    let tmp = crate::recv::temp_name(lname.as_bytes(), b".tmp");
    let tmp = OsStr::from_bytes(&tmp);
    match unlinkat(&lfd, tmp, UnlinkatFlags::NoRemoveDir) {
        Ok(()) | Err(nix::Error::ENOENT) => {}
        Err(e) => return Err(errno(e)),
    }
    let made = if link {
        if let Ok(m) = fstatat(&lfd, lname, AtFlags::AT_SYMLINK_NOFOLLOW) {
            // Already linked (renaming a link over itself would leave the temp).
            if m.st_dev == tm.st_dev && m.st_ino == tm.st_ino {
                return Ok(());
            }
        }
        // The link shares the target's inode and with it the target's
        // metadata; no flags: a symlink at the target name is never followed.
        linkat(&tfd, tname, &lfd, tmp, AtFlags::empty()).map_err(errno)
    } else {
        (|| {
            let nofollow = OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC;
            let src = nix::fcntl::openat(&tfd, tname, OFlag::O_RDONLY | nofollow, Mode::empty())
                .map_err(errno)?;
            let mut src = std::fs::File::from(src);
            crate::fs::safe::require_regular(&src, Path::new(tname))?;
            let flags = OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | nofollow;
            let dst = nix::fcntl::openat(&lfd, tmp, flags, Mode::from_bits_truncate(0o600))
                .map_err(errno)?;
            let mut dst = std::fs::File::from(dst);
            io::copy(&mut src, &mut dst)?;
            crate::fs::meta::apply_metadata_fd(&dst, entry, apply)
        })()
    };
    let res = made.and_then(|()| renameat(&lfd, tmp, &lfd, lname).map_err(errno));
    if res.is_err() {
        let _ = unlinkat(&lfd, tmp, UnlinkatFlags::NoRemoveDir);
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

/// The entries of a listing; a gap records its path in `gap` (the earliest
/// of either listing) and is otherwise skipped.
fn sides(
    rx: mpsc::Receiver<Listed>,
    gap: Arc<Mutex<Option<RelPath>>>,
) -> impl futures::Stream<Item = Side> + Send {
    futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|x| (x, rx)) })
        .filter_map(move |item| {
            let gap = gap.clone();
            async move {
                match item {
                    Listed::Side(s) => Some(s),
                    Listed::Gap(path) => {
                        tracing::debug!(?path, "a listing has a gap: no deletions past it");
                        let mut first = gap.lock().unwrap_or_else(|e| e.into_inner());
                        if first.as_ref().is_none_or(|f| path < *f) {
                            *first = Some(path);
                        }
                        None
                    }
                }
            }
        })
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
    deletes: Option<DeletePass>,
    /// Directories the receiver's filters exclude (so is everything below).
    excluded_dirs: HashSet<RelPath>,
    /// `-H`: the destination inode of each multiply-linked regular file
    /// this session leaves in place, so links to it already there are kept.
    kept_inodes: HashMap<RelPath, (u64, u64)>,
    /// Regular files this session placed or kept, each under a parent the
    /// planner verified: the only paths a hard link may point at.
    regular_ok: HashSet<RelPath>,
    /// Slots for files in flight (see [`MAX_INFLIGHT`]).
    inflight: Arc<Semaphore>,
    /// `Done::Planned` events still on their way (test hook, see
    /// `SideHooks::delay_planned`); all arrive before `PlannerFinished`.
    #[cfg(feature = "test-hooks")]
    delayed: Vec<tokio::task::JoinHandle<()>>,
}

impl Planner {
    /// Posts a `Done::Planned` to the main loop.
    fn post_planned(&mut self, ev: Ev) {
        #[cfg(feature = "test-hooks")]
        if let Some(d) = self.ctx.hooks.delay_planned {
            let events = self.events.clone();
            self.delayed.push(tokio::spawn(async move {
                tokio::time::sleep(d).await;
                let _ = events.send(ev);
            }));
            return;
        }
        let _ = self.events.send(ev);
    }

    fn error(&mut self, path: &RelPath, msg: impl Into<String>) {
        let msg = msg.into();
        tracing::info!(?path, error = %msg, "receiver");
        self.ctx
            .hooks
            .bus
            .emit(crate::session::Event::Failed(path.clone(), msg.clone()));
        self.report.errors.push((path.clone(), msg));
    }

    /// A source file found up to date: counted, reported on the bus and
    /// added to the bytes the heartbeat tells the peer about. A file an
    /// earlier connection of this session placed is only counted: its
    /// `FileDone` already put its size in the progress percent.
    fn note_skipped(&mut self, path: &RelPath, size: u64) {
        self.report.skipped += 1;
        if matches!(self.prior.by_path.get(path), Some(&(_, true))) {
            return;
        }
        self.ctx
            .hooks
            .bus
            .emit(crate::session::Event::Skipped(path.clone(), size));
        self.ctx
            .busy
            .skipped_bytes
            .fetch_add(size, std::sync::atomic::Ordering::Relaxed);
    }

    fn touch_parent(&mut self, path: &RelPath) {
        if !path.as_bytes().is_empty() {
            self.report.touched.insert(parent_of(path));
        }
    }

    async fn one(&mut self, path: RelPath, src: Side, dst: Option<Side>) {
        #[cfg(feature = "test-hooks")]
        if let Some(d) = self.ctx.hooks.slow_plan {
            tokio::time::sleep(d).await;
        }
        let meter = self.ctx.meter.clone();
        let planning = meter.start(Activity::Plan);
        self.report.listed.count(&src.entry);
        let is_dir = src.entry.kind() == Kind::Directory;
        if is_own_temp(&path) {
            // Never a sender's real entry (listings leave them out): a peer
            // naming one tries to make us write through our own temp files.
            self.error(
                &path,
                "refusing a name reserved for irsync's temporary files",
            );
            if is_dir {
                self.failed_dirs.insert(path);
            }
            return;
        }
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
        if src.entry.kind() == Kind::Socket {
            // rsync's behaviour: sockets are not transferred.
            tracing::warn!(?path, "skipping socket");
            return;
        }
        let mut decision = decide(Some(&src), dst.as_ref(), &self.popts);
        if let Decision::Hardlink { target } = &decision {
            decision = self.link_decision(target, &src, dst.as_ref());
        }
        if decision == Decision::MetaOnly
            && src.entry.kind() == Kind::Regular
            && dst.as_ref().is_some_and(shares_inode)
        {
            // Applied in place, the metadata would reach every other name of
            // the destination's inode: rebuild it from its own chunks instead.
            decision = Decision::Delta;
        }
        if let Decision::Hardlink { target } = &decision {
            // Only to a file this session handles as a regular file under a
            // verified parent: never to a peer-chosen path elsewhere (through
            // a symlink, out of the root), nor to one that failed planning.
            if !self.regular_ok.contains(target) {
                let msg = format!(
                    "hard link: target {} is not a regular file received in this session",
                    String::from_utf8_lossy(target.as_bytes())
                );
                self.error(&path, msg);
                return;
            }
        }
        if src.entry.kind() == Kind::Regular {
            // Withdrawn again if applying its metadata fails below.
            self.regular_ok.insert(path.clone());
        }
        if src.entry.kind() == Kind::Regular && src.entry.nlink > 1 {
            if let (Decision::Skip | Decision::MetaOnly, Some(d)) = (&decision, &dst) {
                if d.entry.kind() == Kind::Regular {
                    self.kept_inodes
                        .insert(path.clone(), (d.entry.dev, d.entry.inode));
                }
            }
        }
        let mut deferred = None;
        if decision != Decision::Skip {
            if dst.is_none() {
                self.report.created += 1;
            }
            let p = &self.popts.preserve;
            let code = itemize(&decision, Some(&src), dst.as_ref(), '>', p);
            if decision == Decision::Delta && !self.ctx.opts.dry_run {
                // Itemized by the receiver once the chunk list shows whether
                // the content changed (a `touch` is an attribute update).
                let meta = dst
                    .as_ref()
                    .filter(|d| meta_differs(&src.entry, &d.entry, p))
                    .map(|_| itemize(&Decision::MetaOnly, Some(&src), dst.as_ref(), '>', p));
                deferred = Some(Itemize {
                    planned: code,
                    meta,
                });
            } else {
                // A receiving daemon tells the (sending) client what it plans.
                // Paced: nothing else bounds these (directories, links and
                // metadata-only changes take no in-flight slot), so a slow
                // link stalls the planner instead of the queue growing.
                if self.ctx.opts.itemize && self.ctx.remote_id.is_some() {
                    self.out
                        .send_paced(Msg::Itemized(Itemized {
                            path: path.as_bytes().to_vec(),
                            code: code.clone(),
                        }))
                        .await;
                }
                let bus = &self.ctx.hooks.bus;
                bus.emit(crate::session::Event::Itemized(path.clone(), code));
                // rsync -n -v lists what would be transferred.
                if self.ctx.opts.dry_run
                    && matches!(
                        decision,
                        Decision::Whole | Decision::Delta | Decision::Append { .. }
                    )
                {
                    bus.emit(crate::session::Event::WouldTransfer(path.clone()));
                }
            }
        }
        if self.ctx.opts.dry_run {
            // Decide only: nothing is created, changed or requested.
            if is_dir {
                self.ok_dirs.insert(path);
            } else if src.entry.kind() == Kind::Regular && decision == Decision::Skip {
                self.note_skipped(&path, src.entry.size);
            }
            return;
        }
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
                } else if matches!(entry.kind(), Kind::Regular | Kind::Hardlink) {
                    self.note_skipped(&path, entry.size);
                }
                return;
            }
            Decision::Hardlink { target } => {
                self.touch_parent(&path);
                let Some(file_id) = self.file_id(&path, entry.size) else {
                    return;
                };
                // Placed after every regular file (the target may be in flight).
                let _ = self.events.send(Ev::Recv(Done::Linked(Box::new(Linked {
                    file_id,
                    path,
                    entry,
                    target,
                }))));
                return;
            }
            Decision::Mkdir => {
                // Making an entry is placing it, not planning.
                drop(planning);
                let placing = meter.start(Activity::Place);
                let res = blocking(move || mkdir_replacing(&full)).await;
                drop(placing);
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
            Decision::MetaOnly => {
                drop(planning);
                let _placing = meter.start(Activity::Place);
                blocking(move || apply_metadata(&full, &entry, &apply)).await
            }
            Decision::Symlink => {
                self.touch_parent(&path);
                drop(planning);
                let _placing = meter.start(Activity::Place);
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
                drop(planning);
                let _placing = meter.start(Activity::Place);
                blocking(move || {
                    place_via_temp(&full, |tmp| create_special(tmp, &entry), &entry, &apply)
                })
                .await
            }
            Decision::Whole | Decision::Delta | Decision::Append { .. } => {
                // Placing it (now or on an earlier connection) moved the parent's mtime.
                self.touch_parent(&path);
                let Some(file_id) = self.file_id(&path, entry.size) else {
                    return;
                };
                let (action, append_from) = match decision {
                    Decision::Delta => (Action::Delta, 0),
                    Decision::Append { from } => (Action::Append, from),
                    _ => (Action::Whole, 0),
                };
                let prehash = matches!(action, Action::Delta | Action::Append).then(|| {
                    self.ctx.hooks.event("prehash", &path);
                    let (ctx, p) = (self.ctx.clone(), path.clone());
                    tokio::spawn(async move { index_old_file(&ctx, &p).await })
                });
                // Bounded in flight: wait until an earlier file is acknowledged
                // (the peer's doing, not planning time).
                drop(planning);
                let Ok(slot) = self.inflight.clone().acquire_owned().await else {
                    return;
                };
                let _planning = meter.start(Activity::Plan);
                // The main loop registers the file, then requests it: the
                // peer cannot answer a request for a file not yet known.
                self.post_planned(Ev::Recv(Done::Planned(Box::new(Planned {
                    file_id,
                    path: path.clone(),
                    entry,
                    action,
                    append_from,
                    deferred: Deferred {
                        prehash,
                        itemize: deferred,
                    },
                    slot,
                }))));
                return;
            }
        };
        if let Err(e) = res {
            self.regular_ok.remove(&path);
            self.error(&path, e.to_string());
        }
    }

    /// The id for a planned path: kept from an earlier connection of this
    /// session, or new. `None` (counted as skipped) if that connection
    /// already placed it.
    fn file_id(&mut self, path: &RelPath, size: u64) -> Option<u64> {
        match self.prior.by_path.get(path) {
            Some(&(_, true)) => {
                self.note_skipped(path, size);
                None
            }
            Some(&(id, false)) => Some(id),
            None => {
                self.next_id += 1;
                Some(self.next_id)
            }
        }
    }

    /// A hard link in the sender's listing. With `-H`, a destination that
    /// already shares the (unchanged) target's inode is kept. Without `-H`
    /// on this side it is an independent file, compared like one.
    fn link_decision(&self, target: &RelPath, src: &Side, dst: Option<&Side>) -> Decision {
        let linked = Decision::Hardlink {
            target: target.clone(),
        };
        let Some(d) = dst.filter(|d| d.entry.kind() == Kind::Regular) else {
            return linked;
        };
        if self.popts.preserve.hard_links {
            let same = self.kept_inodes.get(target) == Some(&(d.entry.dev, d.entry.inode));
            return if same { Decision::Skip } else { linked };
        }
        let as_file = Side {
            entry: FileEntry {
                kind: Kind::Regular as i32,
                ..src.entry.clone()
            },
            digest: None,
        };
        match decide(Some(&as_file), Some(d), &self.popts) {
            d @ (Decision::Skip | Decision::MetaOnly) => d,
            _ => linked,
        }
    }

    /// Whether the receiver's filters exclude `path` (or a directory above
    /// it). Called for every merged path, in order, while deleting.
    fn excluded(&mut self, path: &RelPath, is_dir: bool) -> bool {
        if path.as_bytes().is_empty() {
            return false;
        }
        let ex = self.excluded_dirs.contains(&parent_of(path))
            || self.ctx.filters.matches(path, is_dir) == Verdict::Exclude;
        if ex && is_dir {
            self.excluded_dirs.insert(path.clone());
        }
        ex
    }

    /// Deletion bookkeeping for one merged path, in merge order: a
    /// destination-only path becomes a candidate (or is protected).
    fn consider(&mut self, path: &RelPath, src: Option<&Side>, dst: Option<&Side>) {
        if self.deletes.is_none() {
            return;
        }
        let is_dir = dst
            .or(src)
            .is_some_and(|s| s.entry.kind() == Kind::Directory);
        let excluded = self.excluded(path, is_dir);
        if let (None, Some(d)) = (src, dst) {
            self.extraneous(path.clone(), d, excluded);
        }
    }

    fn extraneous(&mut self, path: RelPath, dst: &Side, excluded: bool) {
        let Some(pass) = self.deletes.as_mut() else {
            return;
        };
        if decide(None, Some(dst), &self.popts) != Decision::Delete {
            return;
        }
        // Without -r a directory's contents were never compared: it stays.
        if !self.popts.preserve.recursive && dst.entry.kind() == Kind::Directory {
            return;
        }
        if excluded && !pass.delete_excluded() {
            pass.protect(&path);
        } else {
            pass.plan(path, dst.entry.kind());
        }
    }

    /// `--delete-during`, at a directory boundary: hands the candidates the
    /// merge has left behind to the receiver's main loop and waits for them
    /// to be deleted. The pass is incomplete when a listing reported a gap
    /// at or before its last candidate: the merge may have pulled a gap
    /// past the current path already, which explains nothing before it.
    async fn delete_during(&mut self, current: &RelPath, gap: &Mutex<Option<RelPath>>) {
        let Some(ready) = self.deletes.as_mut().and_then(|d| d.split_ready(current)) else {
            return;
        };
        let first_gap = gap.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let incomplete = match (first_gap, ready.last_candidate()) {
            (Some(g), Some(last)) => g <= *last,
            _ => false,
        };
        let (tx, rx) = oneshot::channel();
        let _ = self.events.send(Ev::Recv(Done::DeleteDuring {
            pass: Box::new(ready),
            incomplete,
            reply: tx,
        }));
        if let Ok(deleted) = rx.await {
            for p in &deleted {
                self.touch_parent(p);
            }
        }
    }

    /// `--delete-before`: hands the candidates to the receiver's main loop,
    /// which deletes once both listings have ended, and waits for it.
    async fn delete_before(&mut self) {
        let Some(pass) = self.deletes.take() else {
            return;
        };
        let (tx, rx) = oneshot::channel();
        let _ = self
            .events
            .send(Ev::Recv(Done::DeleteBefore(Box::new(pass), tx)));
        // A dropped reply means the session failed (this task is aborted).
        if let Ok(deleted) = rx.await {
            for p in &deleted {
                self.touch_parent(p);
            }
        }
    }
}

/// What the receiver does with a file the sender's final re-check found
/// changed (`Replan`), decided by [`replan`].
pub(super) struct Replanned {
    /// Request it again: none if nothing (or only metadata) needs sending.
    pub request: Option<(Action, u64)>,
    /// Nothing was at the path at the destination.
    pub created: bool,
    /// The itemize line, if anything is done.
    pub itemize: Option<String>,
}

/// Plans a file again that the sender found changed at its final re-check
/// (`--paranoid`), with `entry` as it is now, against what the destination
/// holds now: as the planner plans a listed file, except that size and
/// mtime prove nothing (the sender saw the file change, perhaps with its
/// mtime set back), so the contents are compared as with `--checksum`.
/// `path` was planned as a regular file under a verified parent already.
/// A metadata-only change is applied here.
pub(super) async fn replan(
    ctx: &Arc<Ctx>,
    path: &RelPath,
    entry: &FileEntry,
) -> io::Result<Replanned> {
    let (root, rel) = (ctx.root.clone(), path.clone());
    let dst = blocking(
        move || match crate::fs::meta::lstat_entry_with_ctime(&root, &rel) {
            Ok((entry, _)) => Ok(Some(entry)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        },
    )
    .await?;
    let mut dst = dst.map(|entry| Side {
        entry,
        digest: None,
    });
    if let Some(d) = &mut dst {
        crate::fs::ids::IdNames::new().attach(&mut d.entry);
    }
    let src = Side {
        entry: entry.clone(),
        digest: None,
    };
    let o = &ctx.opts;
    let popts = PlanOpts {
        checksum: true,
        update_only: o.update_only,
        delete: false,
        preserve: o.preserve,
        append: o.append,
    };
    let decision = decide(Some(&src), dst.as_ref(), &popts);
    let itemize = (decision != Decision::Skip)
        .then(|| itemize(&decision, Some(&src), dst.as_ref(), '>', &popts.preserve));
    let created = dst.is_none();
    let request = match decision {
        Decision::Skip => None,
        Decision::MetaOnly => {
            let (full, entry, apply) = (ctx.full(path), entry.clone(), ctx.apply_opts(entry));
            blocking(move || apply_metadata(&full, &entry, &apply)).await?;
            None
        }
        Decision::Delta => Some((Action::Delta, 0)),
        Decision::Append { from } => Some((Action::Append, from)),
        _ => Some((Action::Whole, 0)),
    };
    Ok(Replanned {
        request,
        created,
        itemize,
    })
}

/// Merge-joins the listings and acts on every path; reports at the end.
pub(super) async fn plan(
    ctx: Arc<Ctx>,
    out: Out,
    events: mpsc::UnboundedSender<Ev>,
    peer: mpsc::Receiver<Listed>,
    local: mpsc::Receiver<Listed>,
    prior: Prior,
) {
    let phase = ctx.phase(PhaseKind::Plan);
    plan_listings(ctx, out, events, peer, local, prior)
        .instrument(phase.span().clone())
        .await;
}

async fn plan_listings(
    ctx: Arc<Ctx>,
    out: Out,
    events: mpsc::UnboundedSender<Ev>,
    peer: mpsc::Receiver<Listed>,
    local: mpsc::Receiver<Listed>,
    prior: Prior,
) {
    let inflight = Arc::new(Semaphore::new(inflight_cap(&ctx)));
    let o = &ctx.opts;
    let deletes = (o.delete != DeleteMode::Off).then(|| {
        DeletePass::new(
            o.delete,
            o.max_delete,
            o.delete_excluded,
            o.dry_run,
            o.ignore_errors,
        )
    });
    let popts = PlanOpts {
        checksum: ctx.opts.checksum,
        update_only: ctx.opts.update_only,
        delete: deletes.is_some(),
        preserve: ctx.opts.preserve,
        append: ctx.opts.append,
    };
    let meter = ctx.meter.clone();
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
        deletes,
        excluded_dirs: HashSet::new(),
        kept_inodes: HashMap::new(),
        regular_ok: HashSet::new(),
        inflight,
        #[cfg(feature = "test-hooks")]
        delayed: Vec::new(),
    };
    let mode = p.deletes.as_ref().map(|d| d.mode());
    // The first gap either listing reported, known before the merge yields
    // any path after it (it may be known one path early).
    let gap = Arc::new(Mutex::new(None));
    // Both listings are in order: the peer's listing reader refuses one
    // that is not (a protocol error), and this side's comes from its walk.
    let joined = merge_join(sides(peer, gap.clone()), sides(local, gap.clone()));
    futures::pin_mut!(joined);
    if mode == Some(DeleteMode::Before) {
        // Every candidate is known only once both listings have ended.
        let mut sources = Vec::new();
        while let Some((path, src, dst)) = joined.next().await {
            {
                let _planning = meter.start(Activity::Plan);
                p.consider(&path, src.as_ref(), dst.as_ref());
            }
            if let Some(src) = src {
                sources.push((path, src, dst));
            }
        }
        p.delete_before().await;
        for (path, src, dst) in sources {
            p.one(path, src, dst).await;
        }
    } else {
        let during = mode == Some(DeleteMode::During);
        let mut dir: Option<RelPath> = None;
        while let Some((path, src, dst)) = joined.next().await {
            if during {
                let parent = path.parent();
                if dir.as_ref() != Some(&parent) {
                    dir = Some(parent);
                    p.delete_during(&path, &gap).await;
                }
            }
            {
                let _planning = meter.start(Activity::Plan);
                p.consider(&path, src.as_ref(), dst.as_ref());
            }
            if let Some(src) = src {
                p.one(path, src, dst).await;
            }
        }
    }
    p.report.deletes = p.deletes.take();
    p.report.max_id = p.next_id;
    p.report.regular = std::mem::take(&mut p.regular_ok);
    #[cfg(feature = "test-hooks")]
    for t in std::mem::take(&mut p.delayed) {
        let _ = t.await;
    }
    let _ = events.send(Ev::Recv(Done::PlannerFinished(Box::new(p.report))));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::meta::ApplyOpts;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn inflight_cap_fits_the_open_file_limit() {
        // A generous limit allows the whole window.
        assert_eq!(inflight_for_limit(524_288), MAX_INFLIGHT);
        // The common soft limit of 1024: two descriptors per file in flight
        // (the `.part` and the old file) plus the reserve must fit, and
        // most of the limit should still go to files.
        let cap = inflight_for_limit(1024);
        assert!(cap * FDS_PER_INFLIGHT + NOFILE_RESERVE <= 1024, "{cap}");
        assert!(cap >= 256, "{cap}");
        // Never zero, however small the limit.
        assert_eq!(inflight_for_limit(0), 1);
    }

    #[test]
    fn place_hardlink_never_follows_symlinked_components() {
        let root = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        std::fs::write(out.path().join("secret"), b"s").unwrap();
        std::fs::write(root.path().join("t"), b"t").unwrap();
        std::os::unix::fs::symlink(out.path(), root.path().join("esc")).unwrap();
        let r = |s: &[u8]| RelPath::new(s).unwrap();
        let e = FileEntry::default();
        let none = ApplyOpts::default();
        for link in [true, false] {
            // Target through a symlinked directory.
            let err = place_hardlink(root.path(), &r(b"esc/secret"), &r(b"l"), link, &e, &none)
                .unwrap_err();
            assert!(err.to_string().contains("symlink"), "{err}");
            assert!(!root.path().join("l").exists());
            // Link name through a symlinked directory.
            assert!(place_hardlink(root.path(), &r(b"t"), &r(b"esc/l"), link, &e, &none).is_err());
            assert!(!out.path().join("l").exists());
        }
        // A symlink as the target itself is not a regular file.
        std::os::unix::fs::symlink(out.path().join("secret"), root.path().join("s")).unwrap();
        assert!(place_hardlink(root.path(), &r(b"s"), &r(b"l"), true, &e, &none).is_err());
        assert_eq!(
            std::fs::metadata(out.path().join("secret"))
                .unwrap()
                .nlink(),
            1
        );
        // The normal case links, and a second run keeps the link.
        place_hardlink(root.path(), &r(b"t"), &r(b"l"), true, &e, &none).unwrap();
        place_hardlink(root.path(), &r(b"t"), &r(b"l"), true, &e, &none).unwrap();
        let (a, b) = (
            std::fs::metadata(root.path().join("t")).unwrap(),
            std::fs::metadata(root.path().join("l")).unwrap(),
        );
        assert_eq!(a.ino(), b.ino());
        place_hardlink(root.path(), &r(b"t"), &r(b"c"), false, &e, &none).unwrap();
        assert_eq!(std::fs::read(root.path().join("c")).unwrap(), b"t");
        assert_ne!(
            std::fs::metadata(root.path().join("c")).unwrap().ino(),
            a.ino()
        );
    }
}
