//! Operations: the framework shared by every kind, and the kinds.
//!
//! Every operation follows one pattern: draw its target and parameters
//! from `ctx.rng` and `ctx.model` (returning [`OpOutcome::NoTarget`] if
//! there is nothing valid to act on), perform the filesystem syscalls
//! inside [`timed`] unless `ctx.dry`, update the model, and describe what
//! it did in an [`OpRecord`]. A dry run makes the same draws and the same
//! model updates, so its chain follows the real one.

pub mod file;

use std::fs::File;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use rand_chacha::ChaCha8Rng;

use crate::config::Config;
use crate::kind::OpKind;
use crate::log::OpRecord;
use crate::model::{Model, NodeId};
use crate::names::draw_name;

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
        OpKind::Mkdir
        | OpKind::Rmdir
        | OpKind::RemoveTree
        | OpKind::Rename
        | OpKind::RenameOver
        | OpKind::Symlink
        | OpKind::RepointSymlink
        | OpKind::DeleteSymlink
        | OpKind::Hardlink
        | OpKind::Mkfifo
        | OpKind::Chmod
        | OpKind::SetMtime
        | OpKind::SetXattr
        | OpKind::Chown
        | OpKind::Mknod => not_yet,
    }
}

/// Placeholder for kinds implemented in later tasks.
fn not_yet(_: &mut OpCtx) -> io::Result<OpOutcome> {
    Ok(OpOutcome::NoTarget)
}

/// Runs `f`, returning its result and how long it took.
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
