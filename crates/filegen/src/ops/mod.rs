//! Operations: the framework shared by every kind, and the kinds.
//!
//! Every operation follows one pattern: draw its target and parameters
//! from `ctx.rng` and `ctx.model` (returning [`OpOutcome::NoTarget`] if
//! there is nothing valid to act on), perform the filesystem syscalls
//! inside [`timed`] unless `ctx.dry`, update the model, and describe what
//! it did in an [`OpRecord`]. A dry run makes the same draws and the same
//! model updates, so its chain follows the real one.

/// An [`OpCtx`] over a temp dir with the root as anchor, for tests.
#[cfg(test)]
macro_rules! ctx {
    ($tmp:expr, $cfg:expr, $model:expr, $rng:expr) => {
        $crate::ops::OpCtx {
            root: $tmp.path(),
            cfg: &$cfg,
            model: &mut $model,
            rng: &mut $rng,
            anchor: 0,
            dry: false,
            fsync: false,
        }
    };
}

pub mod file;
pub mod link;
pub mod meta;
pub mod tree;

use std::fs::File;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::{Duration, Instant};

use rand::Rng;
use rand_chacha::ChaCha8Rng;

use crate::config::Config;
use crate::kind::OpKind;
use crate::log::OpRecord;
use crate::model::{Model, NodeId, NodeKind};
use crate::names::draw_name;
use crate::pick::{anchor_of, pick};

/// Everything one operation needs.
pub struct OpCtx<'a> {
    pub root: &'a Path,
    pub cfg: &'a Config,
    pub model: &'a mut Model,
    pub rng: &'a mut ChaCha8Rng,
    /// The directory the previous operation touched.
    pub anchor: NodeId,
    pub dry: bool,
    pub fsync: bool,
}

/// What an operation did.
pub struct Applied {
    pub rec: OpRecord,
    /// The directory this operation touched; the next one's anchor.
    pub anchor: NodeId,
    /// Time spent in the filesystem syscalls; zero in a dry run.
    pub dur: Duration,
}

pub enum OpOutcome {
    Done(Applied),
    /// Nothing valid to act on; model and disk are unchanged.
    NoTarget,
}

pub type OpFn = fn(&mut OpCtx) -> io::Result<OpOutcome>;

/// The function performing `kind`.
pub fn dispatch(kind: OpKind) -> OpFn {
    match kind {
        OpKind::CreateFile => file::create_file,
        OpKind::Append => file::append,
        OpKind::Truncate => file::truncate,
        OpKind::OverwriteRange => file::overwrite_range,
        OpKind::Rewrite => file::rewrite,
        OpKind::PunchHole => file::punch_hole,
        OpKind::DeleteFile => file::delete_file,
        OpKind::Mkdir => tree::mkdir,
        OpKind::Rmdir => tree::rmdir,
        OpKind::RemoveTree => tree::remove_tree,
        OpKind::Rename => tree::rename,
        OpKind::RenameOver => tree::rename_over,
        OpKind::Hardlink => tree::hardlink,
        OpKind::Mkfifo => tree::mkfifo,
        OpKind::Symlink => link::symlink,
        OpKind::RepointSymlink => link::repoint_symlink,
        OpKind::DeleteSymlink => link::delete_symlink,
        OpKind::Chmod => meta::chmod,
        OpKind::SetMtime => meta::set_mtime,
        OpKind::SetXattr => meta::set_xattr,
        OpKind::Chown => meta::chown,
        OpKind::Mknod => meta::mknod,
    }
}

/// Runs `f`, returning its result and how long it took. Everything inside
/// `f` is timed, so for content writes this includes generating the bytes
/// in `Content::write_to`, not only the syscalls.
pub fn timed<T>(f: impl FnOnce() -> io::Result<T>) -> io::Result<(T, Duration)> {
    let start = Instant::now();
    let v = f()?;
    Ok((v, start.elapsed()))
}

/// Flushes `file` to stable storage when `ctx.fsync` is set.
pub fn maybe_fsync(ctx: &OpCtx, file: &File) -> io::Result<()> {
    if ctx.fsync {
        file.sync_all()?;
    }
    Ok(())
}

/// A fresh name for a new entry in `dir` that keeps the entry's relative
/// path within `max_path_len`. Callers creating directories also check
/// `depth(dir) < max_depth`.
pub fn new_name(ctx: &mut OpCtx, dir: NodeId) -> Option<String> {
    let sep = usize::from(dir != ctx.model.root());
    let max_bytes = (ctx.cfg.max_path_len as usize)
        .saturating_sub(ctx.model.rel_path_len(dir))
        .saturating_sub(sep);
    if max_bytes == 0 {
        return None;
    }
    draw_name(ctx.rng, &ctx.cfg.names, ctx.model, dir, max_bytes).map(|(name, _)| name)
}

/// Bytes that may still be added to the tree.
pub fn budget(ctx: &OpCtx) -> u64 {
    ctx.cfg
        .max_total_size
        .0
        .saturating_sub(ctx.model.total_bytes())
}

/// The paths a failed operation acted on, carried inside the
/// [`io::Error`] it returns (see [`OpError::wrap`]) so the failure line can
/// name them. Displays as the OS error alone.
#[derive(Debug)]
pub struct OpError {
    /// The primary path, relative to the root.
    pub path: Vec<u8>,
    /// The second path of a rename or hard link, relative to the root.
    pub to: Option<Vec<u8>>,
    pub source: io::Error,
}

impl OpError {
    /// `source` with the paths attached: an [`io::Error`] of the same kind
    /// and message holding an `OpError`. `path` and `to` are absolute
    /// paths under `root`.
    pub fn wrap(root: &Path, path: &Path, to: Option<&Path>, source: io::Error) -> io::Error {
        let rel = |p: &Path| {
            p.strip_prefix(root)
                .unwrap_or(p)
                .as_os_str()
                .as_bytes()
                .to_vec()
        };
        let kind = source.kind();
        io::Error::new(
            kind,
            OpError {
                path: rel(path),
                to: to.map(rel),
                source,
            },
        )
    }

    /// The `OpError` inside `e`, if an operation attached one.
    pub fn of(e: &io::Error) -> Option<&OpError> {
        e.get_ref().and_then(|r| r.downcast_ref::<OpError>())
    }
}

impl std::fmt::Display for OpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(f)
    }
}

impl std::error::Error for OpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Runs the syscalls `f` on `path` (and `to`, for a rename or link) unless
/// in a dry run; returns its value (the default in a dry run) and the time
/// it took (zero in a dry run). An error carries both paths as an
/// [`OpError`].
pub(crate) fn syscalls<T: Default>(
    ctx: &OpCtx,
    path: &Path,
    to: Option<&Path>,
    f: impl FnOnce(&OpCtx) -> io::Result<T>,
) -> io::Result<(T, Duration)> {
    if ctx.dry {
        Ok((T::default(), Duration::ZERO))
    } else {
        timed(|| f(ctx)).map_err(|e| OpError::wrap(ctx.root, path, to, e))
    }
}

/// The outcome of an operation on the live node `id`, anchored at the
/// directory it touched (see [`anchor_of`]).
pub(crate) fn applied(
    ctx: &OpCtx,
    kind: OpKind,
    id: NodeId,
    bytes: u64,
    details: Vec<(&'static str, String)>,
    dur: Duration,
) -> OpOutcome {
    OpOutcome::Done(Applied {
        rec: OpRecord {
            kind,
            path: ctx.model.rel_path(id),
            to: None,
            bytes,
            details,
        },
        anchor: anchor_of(ctx.model, id),
        dur,
    })
}

/// Removes the live node `id` and its subtree from the model and
/// describes the removal, anchored at the parent directory. The record is
/// built first: `id` is dead afterwards.
pub(crate) fn removed(
    ctx: &mut OpCtx,
    kind: OpKind,
    id: NodeId,
    details: Vec<(&'static str, String)>,
    dur: Duration,
) -> OpOutcome {
    let parent = ctx.model.node(id).parent.expect("a live node has a parent");
    let rec = OpRecord {
        kind,
        path: ctx.model.rel_path(id),
        to: None,
        bytes: 0,
        details,
    };
    ctx.model.remove_subtree(id);
    OpOutcome::Done(Applied {
        rec,
        anchor: anchor_of(ctx.model, parent),
        dur,
    })
}

/// A node of `kind` near the anchor.
pub(crate) fn pick_kind(ctx: &mut OpCtx, kind: NodeKind) -> Option<NodeId> {
    pick(ctx.rng, ctx.model, &ctx.cfg.locality, ctx.anchor, kind)
}

/// One of `kinds`, drawn by how many of each the tree holds (one draw);
/// `None`, without drawing, if it holds none of them.
pub(crate) fn draw_kind(ctx: &mut OpCtx, kinds: &[NodeKind]) -> Option<NodeKind> {
    let counts = ctx.model.node(ctx.model.root()).counts;
    let weights: Vec<u64> = kinds.iter().map(|k| u64::from(counts[k.index()])).collect();
    let total: u64 = weights.iter().sum();
    if total == 0 {
        return None;
    }
    let mut r = ctx.rng.gen_range(0..total);
    let mut chosen = 0;
    while r >= weights[chosen] {
        r -= weights[chosen];
        chosen += 1;
    }
    Some(kinds[chosen])
}

/// The inode of the entry just created at `path`; in a dry run, where
/// there is none, a synthetic one (hard links are keyed by inode).
pub(crate) fn new_ino(ctx: &mut OpCtx, path: &Path) -> io::Result<u64> {
    if ctx.dry {
        Ok(ctx.model.synthetic_ino())
    } else {
        std::fs::symlink_metadata(path)
            .map(|m| m.ino())
            .map_err(|e| OpError::wrap(ctx.root, path, None, e))
    }
}

/// Shared helpers for the operation tests.
#[cfg(test)]
pub(crate) mod testutil {
    use std::collections::BTreeSet;
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};

    use rand_chacha::ChaCha8Rng;

    use super::{dispatch, Applied, OpCtx, OpOutcome};
    use crate::config::{load, Config};
    use crate::kind::OpKind;
    use crate::log::OpRecord;
    use crate::model::{Model, NodeId, NodeKind};
    use crate::walk::from_disk;

    pub fn config(sets: &[(&str, &str)]) -> Config {
        let sets: Vec<(String, String)> = sets
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        load(None, None, &sets).unwrap()
    }

    pub fn done(o: OpOutcome) -> Applied {
        match o {
            OpOutcome::Done(a) => a,
            OpOutcome::NoTarget => panic!("expected Done, got NoTarget"),
        }
    }

    pub fn is_no_target(o: &OpOutcome) -> bool {
        matches!(o, OpOutcome::NoTarget)
    }

    pub fn detail<'a>(rec: &'a OpRecord, key: &str) -> Option<&'a str> {
        rec.details
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Every live node as `(rel_path, kind, size)`.
    pub fn triples(m: &Model) -> BTreeSet<(Vec<u8>, NodeKind, u64)> {
        let mut out = BTreeSet::new();
        let mut stack = vec![m.root()];
        while let Some(id) = stack.pop() {
            for &c in &m.node(id).children {
                let n = m.node(c);
                out.insert((m.rel_path(c), n.kind, n.size));
                stack.push(c);
            }
        }
        out
    }

    /// The live model equals a fresh walk of the disk, byte totals too.
    pub fn assert_matches_disk(root: &Path, m: &Model) {
        let disk = from_disk(root).unwrap();
        assert_eq!(triples(&disk), triples(m));
        assert_eq!(disk.total_bytes(), m.total_bytes());
    }

    /// One step of `kind` through [`dispatch`], moving `anchor`.
    pub fn step(
        root: &Path,
        cfg: &Config,
        m: &mut Model,
        rng: &mut ChaCha8Rng,
        anchor: &mut NodeId,
        kind: OpKind,
        dry: bool,
    ) -> Option<Applied> {
        let mut ctx = OpCtx {
            root,
            cfg,
            model: m,
            rng,
            anchor: *anchor,
            dry,
            fsync: false,
        };
        match dispatch(kind)(&mut ctx).unwrap() {
            OpOutcome::Done(a) => {
                assert_eq!(a.rec.kind, kind);
                *anchor = a.anchor;
                Some(a)
            }
            OpOutcome::NoTarget => None,
        }
    }

    /// The parent part of a `/`-separated relative path (`b""` at the root).
    pub fn parent_of(rel: &[u8]) -> &[u8] {
        match rel.iter().rposition(|&b| b == b'/') {
            Some(i) => &rel[..i],
            None => &[],
        }
    }

    pub fn abs(root: &Path, rel: &[u8]) -> PathBuf {
        root.join(OsStr::from_bytes(rel))
    }

    /// Writes `data` to `rel` on disk and adds it to the model with its
    /// real inode; the parent must already be in the model.
    pub fn put_file(root: &Path, m: &mut Model, rel: &str, data: &[u8]) -> NodeId {
        let path = root.join(rel);
        fs::write(&path, data).unwrap();
        let ino = fs::metadata(&path).unwrap().ino();
        let (dir, name) = parent_and_name(m, rel);
        m.insert(dir, name, NodeKind::File, data.len() as u64, ino)
    }

    /// Creates the directory `rel` on disk and in the model.
    pub fn put_dir(root: &Path, m: &mut Model, rel: &str) -> NodeId {
        let path = root.join(rel);
        fs::create_dir(&path).unwrap();
        let ino = fs::metadata(&path).unwrap().ino();
        let (dir, name) = parent_and_name(m, rel);
        m.insert(dir, name, NodeKind::Dir, 0, ino)
    }

    fn parent_and_name(m: &Model, rel: &str) -> (NodeId, Vec<u8>) {
        match rel.rsplit_once('/') {
            Some((dir, name)) => (m.resolve(dir.as_bytes()).unwrap(), name.into()),
            None => (m.root(), rel.into()),
        }
    }
}
