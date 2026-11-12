//! File content operations: create, grow, shrink, overwrite, rewrite,
//! sparsify and delete regular files (delete also takes FIFOs and device
//! nodes).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::time::Duration;

use rand::RngExt;

use crate::content::Content;
use crate::kind::OpKind;
use crate::model::{NodeId, NodeKind};
use crate::ops::{
    applied, budget, draw_kind, maybe_fsync, new_name, pick_kind, removed, syscalls, OpCtx,
    OpOutcome,
};
use crate::pick::pick_dir_or_root;
use crate::sizes::{draw_delta, draw_file_size, draw_range, draw_truncate};

/// Probability that `punch_hole` extends the file instead.
const EXTEND_PROBABILITY: f64 = 0.3;

/// A regular file near the anchor.
fn pick_file(ctx: &mut OpCtx) -> Option<NodeId> {
    pick_kind(ctx, NodeKind::File)
}

/// How many links share `id`'s inode: every content change resizes them
/// all, so growth per link is bounded by `budget / links`.
fn links(ctx: &OpCtx, id: NodeId) -> u64 {
    ctx.model.linked(id).len() as u64
}

/// New regular file in a drawn dir, size from the distribution.
pub fn create_file(ctx: &mut OpCtx) -> io::Result<OpOutcome> {
    let dir = pick_dir_or_root(ctx.rng, ctx.model, &ctx.cfg.locality, ctx.anchor);
    let Some(name) = new_name(ctx, dir) else {
        return Ok(OpOutcome::NoTarget);
    };
    let budget = budget(ctx);
    let size = draw_file_size(
        ctx.rng,
        &ctx.cfg.size_distribution,
        ctx.cfg.max_file_size.0,
        budget,
    );
    let mut content = Content::draw(ctx.rng, &ctx.cfg.content, size);
    let label = content.label();
    let path = ctx.model.abs_path(ctx.root, dir).join(&name);
    let (ino, dur) = syscalls(ctx, &path, None, |ctx| {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        content.write_to(&mut f)?;
        maybe_fsync(ctx, &f)?;
        Ok(f.metadata()?.ino())
    })?;
    let (dur, gen) = content_timing(dur, &content);
    // A dry run has no real inode; give it a unique one, since hard links
    // are keyed by inode.
    let ino = if ctx.dry {
        ctx.model.synthetic_ino()
    } else {
        ino
    };
    let id = ctx
        .model
        .insert(dir, name.into_bytes(), NodeKind::File, size, ino);
    let details = vec![("size", size.to_string()), ("content", label.into())];
    Ok(with_gen(
        applied(ctx, OpKind::CreateFile, id, size, details, dur),
        gen,
    ))
}

/// Grow a file (and so every hard link to it) by a drawn amount, within
/// both caps.
pub fn append(ctx: &mut OpCtx) -> io::Result<OpOutcome> {
    let Some(id) = pick_file(ctx) else {
        return Ok(OpOutcome::NoTarget);
    };
    let old = ctx.model.node(id).size;
    let budget = budget(ctx) / links(ctx, id);
    let delta = draw_delta(
        ctx.rng,
        &ctx.cfg.size_distribution,
        ctx.cfg.max_file_size.0,
        old,
        budget,
    );
    let mut content = Content::draw(ctx.rng, &ctx.cfg.content, delta);
    let label = content.label();
    let path = ctx.model.abs_path(ctx.root, id);
    let ((), dur) = syscalls(ctx, &path, None, |ctx| {
        let mut f = OpenOptions::new().append(true).open(&path)?;
        content.write_to(&mut f)?;
        maybe_fsync(ctx, &f)
    })?;
    let (dur, gen) = content_timing(dur, &content);
    let size = old + delta;
    ctx.model.set_size_linked(id, size);
    let details = vec![("size", size.to_string()), ("content", label.into())];
    Ok(with_gen(
        applied(ctx, OpKind::Append, id, delta, details, dur),
        gen,
    ))
}

/// Shorten a file to a drawn length, possibly zero.
pub fn truncate(ctx: &mut OpCtx) -> io::Result<OpOutcome> {
    let Some(id) = pick_file(ctx) else {
        return Ok(OpOutcome::NoTarget);
    };
    let size = draw_truncate(ctx.rng, ctx.model.node(id).size);
    let path = ctx.model.abs_path(ctx.root, id);
    let ((), dur) = syscalls(ctx, &path, None, |ctx| {
        let f = OpenOptions::new().write(true).open(&path)?;
        f.set_len(size)?;
        maybe_fsync(ctx, &f)
    })?;
    ctx.model.set_size_linked(id, size);
    let details = vec![("size", size.to_string())];
    Ok(applied(ctx, OpKind::Truncate, id, 0, details, dur))
}

/// Rewrite a drawn byte range in place; the size is unchanged.
pub fn overwrite_range(ctx: &mut OpCtx) -> io::Result<OpOutcome> {
    let Some(id) = pick_file(ctx) else {
        return Ok(OpOutcome::NoTarget);
    };
    let (offset, len) = draw_range(ctx.rng, ctx.model.node(id).size);
    let mut content = Content::draw(ctx.rng, &ctx.cfg.content, len);
    let label = content.label();
    let path = ctx.model.abs_path(ctx.root, id);
    let ((), dur) = syscalls(ctx, &path, None, |ctx| {
        let mut f = OpenOptions::new().write(true).open(&path)?;
        f.seek(SeekFrom::Start(offset))?;
        content.write_to(&mut f)?;
        maybe_fsync(ctx, &f)
    })?;
    let (dur, gen) = content_timing(dur, &content);
    let details = vec![
        ("offset", offset.to_string()),
        ("len", len.to_string()),
        ("content", label.into()),
    ];
    Ok(with_gen(
        applied(ctx, OpKind::OverwriteRange, id, len, details, dur),
        gen,
    ))
}

/// Replace a file's content with a new drawn size. The old bytes of every
/// link are freed first, so they count toward the budget, which the new
/// size then takes once per link.
pub fn rewrite(ctx: &mut OpCtx) -> io::Result<OpOutcome> {
    let Some(id) = pick_file(ctx) else {
        return Ok(OpOutcome::NoTarget);
    };
    let links = links(ctx, id);
    let budget = (budget(ctx) + ctx.model.node(id).size * links) / links;
    let size = draw_file_size(
        ctx.rng,
        &ctx.cfg.size_distribution,
        ctx.cfg.max_file_size.0,
        budget,
    );
    let mut content = Content::draw(ctx.rng, &ctx.cfg.content, size);
    let label = content.label();
    let path = ctx.model.abs_path(ctx.root, id);
    let ((), dur) = syscalls(ctx, &path, None, |ctx| {
        let mut f = File::create(&path)?;
        content.write_to(&mut f)?;
        maybe_fsync(ctx, &f)
    })?;
    let (dur, gen) = content_timing(dur, &content);
    ctx.model.set_size_linked(id, size);
    let details = vec![("size", size.to_string()), ("content", label.into())];
    Ok(with_gen(
        applied(ctx, OpKind::Rewrite, id, size, details, dur),
        gen,
    ))
}

/// Make a drawn range of a file of at least 2 bytes sparse, or (with
/// probability [`EXTEND_PROBABILITY`], when the caps allow) extend it with
/// a trailing hole.
pub fn punch_hole(ctx: &mut OpCtx) -> io::Result<OpOutcome> {
    let Some(id) = pick_file(ctx) else {
        return Ok(OpOutcome::NoTarget);
    };
    let old = ctx.model.node(id).size;
    if old < 2 {
        return Ok(OpOutcome::NoTarget);
    }
    let path = ctx.model.abs_path(ctx.root, id);
    if ctx.rng.random::<f64>() < EXTEND_PROBABILITY {
        let budget = budget(ctx) / links(ctx, id);
        let extend = draw_delta(
            ctx.rng,
            &ctx.cfg.size_distribution,
            ctx.cfg.max_file_size.0,
            old,
            budget,
        );
        if extend > 0 {
            let size = old + extend;
            let ((), dur) = syscalls(ctx, &path, None, |ctx| {
                let f = OpenOptions::new().write(true).open(&path)?;
                f.set_len(size)?;
                maybe_fsync(ctx, &f)
            })?;
            ctx.model.set_size_linked(id, size);
            let details = vec![
                ("offset", old.to_string()),
                ("len", extend.to_string()),
                ("mode", "extend".into()),
                ("size", size.to_string()),
            ];
            return Ok(applied(ctx, OpKind::PunchHole, id, 0, details, dur));
        }
    }
    let (offset, len) = align_range(draw_range(ctx.rng, old), old);
    let (emulated, dur) = syscalls(ctx, &path, None, |ctx| {
        let mut f = OpenOptions::new().write(true).open(&path)?;
        let emulated = !punch(&f, offset, len)?;
        if emulated {
            write_zeros(&mut f, offset, len)?;
        }
        maybe_fsync(ctx, &f)?;
        Ok(emulated)
    })?;
    let mut details = vec![("offset", offset.to_string()), ("len", len.to_string())];
    // `emulated` (and the `bytes` it implies) is an observed outcome, like
    // `dur`: it depends on the filesystem, a dry run cannot know it, and it
    // is not part of the record's dry/real identity.
    if emulated {
        details.push(("emulated", "1".into()));
    }
    let bytes = if emulated { len } else { 0 };
    Ok(applied(ctx, OpKind::PunchHole, id, bytes, details, dur))
}

/// Widens a drawn hole to whole 4 KiB blocks (macOS `F_PUNCHHOLE` needs
/// block alignment), clamped to the file size; files smaller than a block
/// keep the drawn range.
fn align_range((offset, len): (u64, u64), size: u64) -> (u64, u64) {
    const BLOCK: u64 = 4096;
    if size < BLOCK {
        return (offset, len);
    }
    let start = offset / BLOCK * BLOCK;
    let end = (offset + len)
        .div_ceil(BLOCK)
        .saturating_mul(BLOCK)
        .min(size);
    (start, end - start)
}

/// Deallocates `[offset, offset + len)` of `f`, keeping its size; `false`
/// if the filesystem cannot.
#[cfg(target_os = "linux")]
fn punch(f: &File, offset: u64, len: u64) -> io::Result<bool> {
    use nix::errno::Errno;
    use nix::fcntl::{fallocate, FallocateFlags};
    let flags = FallocateFlags::FALLOC_FL_PUNCH_HOLE | FallocateFlags::FALLOC_FL_KEEP_SIZE;
    match fallocate(f, flags, offset as libc::off_t, len as libc::off_t) {
        Ok(()) => Ok(true),
        Err(Errno::EOPNOTSUPP | Errno::ENOSYS) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

#[cfg(target_os = "macos")]
fn punch(f: &File, offset: u64, len: u64) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    let arg = libc::fpunchhole_t {
        fp_flags: 0,
        reserved: 0,
        fp_offset: offset as libc::off_t,
        fp_length: len as libc::off_t,
    };
    // SAFETY: `arg` is a valid fpunchhole_t that outlives the call.
    let r = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_PUNCHHOLE, &arg) };
    Ok(r == 0)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn punch(_: &File, _: u64, _: u64) -> io::Result<bool> {
    Ok(false)
}

/// Splits the timed span of a content write into the syscall part and the
/// part `Content::write_to` spent generating bytes.
fn content_timing(dur: Duration, content: &Content) -> (Duration, Duration) {
    let gen = content.generated();
    (dur.saturating_sub(gen), gen)
}

/// Attaches the generation time to an applied content write.
fn with_gen(outcome: OpOutcome, gen: Duration) -> OpOutcome {
    match outcome {
        OpOutcome::Done(mut a) => {
            a.gen = Some(gen);
            OpOutcome::Done(a)
        }
        other => other,
    }
}

/// Overwrites `[offset, offset + len)` of `f` with zeros.
fn write_zeros(f: &mut File, offset: u64, len: u64) -> io::Result<()> {
    const CHUNK: u64 = 1 << 20;
    let zeros = vec![0u8; CHUNK.min(len) as usize];
    f.seek(SeekFrom::Start(offset))?;
    let mut left = len;
    while left > 0 {
        let n = CHUNK.min(left) as usize;
        f.write_all(&zeros[..n])?;
        left -= n as u64;
    }
    Ok(())
}

/// Unlink a regular file, FIFO or device node: the kind is drawn by how
/// many of each the tree holds, then one of that kind is picked.
pub fn delete_file(ctx: &mut OpCtx) -> io::Result<OpOutcome> {
    let Some(kind) = draw_kind(ctx, &[NodeKind::File, NodeKind::Fifo, NodeKind::Device]) else {
        return Ok(OpOutcome::NoTarget);
    };
    let label = match kind {
        NodeKind::File => "file",
        NodeKind::Fifo => "fifo",
        _ => "device",
    };
    let Some(id) = pick_kind(ctx, kind) else {
        return Ok(OpOutcome::NoTarget);
    };
    let path = ctx.model.abs_path(ctx.root, id);
    let ((), dur) = syscalls(ctx, &path, None, |_| fs::remove_file(&path))?;
    let details = vec![("kind", label.into())];
    Ok(removed(ctx, OpKind::DeleteFile, id, details, dur))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::time::Duration;

    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use tempfile::TempDir;

    use crate::config::Config;
    use crate::kind::OpKind;
    use crate::log::OpRecord;
    use crate::model::{Model, NodeKind};
    use crate::ops::testutil::{config, detail, done};
    use crate::ops::{dispatch, OpFn, OpOutcome};

    fn small() -> Config {
        config(&[("max_file_size", "64K"), ("max_total_size", "1M")])
    }

    fn ctx_in(_tmp: &TempDir, _cfg: &Config, seed: u64) -> (Model, ChaCha8Rng) {
        (Model::new(), ChaCha8Rng::seed_from_u64(seed))
    }

    /// Every entry on disk below `root`: files map to their length, every
    /// other kind to 0.
    fn tree_sizes(root: &Path) -> BTreeMap<Vec<u8>, u64> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<Vec<u8>, u64>) {
            for e in fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                let md = fs::symlink_metadata(&p).unwrap();
                let rel = p.strip_prefix(root).unwrap().as_os_str().as_bytes();
                out.insert(rel.to_vec(), if md.is_file() { md.len() } else { 0 });
                if md.is_dir() {
                    walk(root, &p, out);
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    /// The model in the shape of [`tree_sizes`].
    fn model_sizes(m: &Model) -> BTreeMap<Vec<u8>, u64> {
        let mut out = BTreeMap::new();
        let mut stack = vec![m.root()];
        while let Some(id) = stack.pop() {
            for &c in &m.node(id).children {
                let n = m.node(c);
                let size = if n.kind == NodeKind::File { n.size } else { 0 };
                out.insert(m.rel_path(c), size);
                stack.push(c);
            }
        }
        out
    }

    /// Disk equals model, and both caps hold.
    fn assert_consistent(tmp: &TempDir, cfg: &Config, m: &Model) {
        let disk = tree_sizes(tmp.path());
        assert_eq!(disk, model_sizes(m));
        assert_eq!(disk.values().sum::<u64>(), m.total_bytes());
        assert!(m.total_bytes() <= cfg.max_total_size.0);
        assert!(disk.values().all(|&v| v <= cfg.max_file_size.0));
    }

    /// Writes a file of `len` bytes of 0xab at the root, on disk and in
    /// the model.
    fn put_file(tmp: &TempDir, m: &mut Model, name: &str, len: u64) {
        fs::write(tmp.path().join(name), vec![0xab; len as usize]).unwrap();
        let root = m.root();
        m.insert(root, name.into(), NodeKind::File, len, 0);
    }

    #[test]
    fn create_file_writes_content_and_updates_model() {
        let tmp = TempDir::new().unwrap();
        let cfg = small();
        let (mut m, mut rng) = ctx_in(&tmp, &cfg, 1);
        for _ in 0..10 {
            let a = done(create_file(&mut ctx!(tmp, cfg, m, rng)).unwrap());
            assert_eq!(a.rec.kind, OpKind::CreateFile);
            let id = m.resolve(&a.rec.path).expect("created node in model");
            assert_eq!(m.node(id).kind, NodeKind::File);
            let p = tmp.path().join(OsStr::from_bytes(&a.rec.path));
            let len = fs::metadata(&p).unwrap().len();
            assert_eq!(len, m.node(id).size);
            assert_eq!(detail(&a.rec, "size"), Some(len.to_string().as_str()));
            assert!(detail(&a.rec, "content").is_some());
            assert_eq!(a.rec.bytes, len);
            assert_eq!(m.node(a.anchor).kind, NodeKind::Dir);
            assert_consistent(&tmp, &cfg, &m);
        }
        assert_eq!(m.entries(), 10);
    }

    #[test]
    fn create_file_respects_budget() {
        let tmp = TempDir::new().unwrap();
        let cfg = config(&[("max_file_size", "60"), ("max_total_size", "100")]);
        let (mut m, mut rng) = ctx_in(&tmp, &cfg, 2);
        for _ in 0..20 {
            done(create_file(&mut ctx!(tmp, cfg, m, rng)).unwrap());
            assert!(m.total_bytes() <= 100);
            assert!(tree_sizes(tmp.path()).values().sum::<u64>() <= 100);
            assert_consistent(&tmp, &cfg, &m);
        }
        assert!(m.total_bytes() > 0);
    }

    #[test]
    fn append_truncate_overwrite_rewrite_track_size() {
        let tmp = TempDir::new().unwrap();
        let cfg = small();
        let (mut m, mut rng) = ctx_in(&tmp, &cfg, 3);
        put_file(&tmp, &mut m, "f", 1000);
        let path = tmp.path().join("f");
        let size = |m: &Model| m.node(m.resolve(b"f").unwrap()).size;
        let mut shrunk = 0;
        for _ in 0..20 {
            let before = size(&m);
            let a = done(append(&mut ctx!(tmp, cfg, m, rng)).unwrap());
            assert_eq!(a.rec.kind, OpKind::Append);
            assert_eq!(a.rec.bytes, size(&m) - before);
            assert_eq!(detail(&a.rec, "size"), Some(size(&m).to_string().as_str()));
            assert!(detail(&a.rec, "content").is_some());
            assert_consistent(&tmp, &cfg, &m);

            let old = fs::read(&path).unwrap();
            let a = done(overwrite_range(&mut ctx!(tmp, cfg, m, rng)).unwrap());
            assert_eq!(a.rec.kind, OpKind::OverwriteRange);
            let off: usize = detail(&a.rec, "offset").unwrap().parse().unwrap();
            let len: usize = detail(&a.rec, "len").unwrap().parse().unwrap();
            assert!(detail(&a.rec, "content").is_some());
            let new = fs::read(&path).unwrap();
            assert_eq!(new.len(), old.len());
            assert!(off + len <= old.len());
            assert_eq!(new[..off], old[..off]);
            assert_eq!(new[off + len..], old[off + len..]);
            assert_consistent(&tmp, &cfg, &m);

            let before = size(&m);
            let a = done(truncate(&mut ctx!(tmp, cfg, m, rng)).unwrap());
            assert_eq!(a.rec.kind, OpKind::Truncate);
            assert!(before == 0 || size(&m) < before);
            if size(&m) < before {
                shrunk += 1;
            }
            assert_eq!(detail(&a.rec, "size"), Some(size(&m).to_string().as_str()));
            assert_consistent(&tmp, &cfg, &m);

            let a = done(rewrite(&mut ctx!(tmp, cfg, m, rng)).unwrap());
            assert_eq!(a.rec.kind, OpKind::Rewrite);
            assert_eq!(detail(&a.rec, "size"), Some(size(&m).to_string().as_str()));
            assert_eq!(a.rec.bytes, size(&m));
            assert!(detail(&a.rec, "content").is_some());
            assert_consistent(&tmp, &cfg, &m);
        }
        assert!(shrunk > 0);
        assert_eq!(m.entries(), 1);
    }

    #[test]
    fn delete_file_removes_fifo_too() {
        let tmp = TempDir::new().unwrap();
        let cfg = small();
        let (mut m, mut rng) = ctx_in(&tmp, &cfg, 4);
        nix::unistd::mkfifo(
            &tmp.path().join("p"),
            nix::sys::stat::Mode::from_bits_truncate(0o644),
        )
        .unwrap();
        let root = m.root();
        m.insert(root, b"p".to_vec(), NodeKind::Fifo, 0, 0);
        let a = done(delete_file(&mut ctx!(tmp, cfg, m, rng)).unwrap());
        assert_eq!(a.rec.kind, OpKind::DeleteFile);
        assert_eq!(a.rec.path, b"p");
        assert_eq!(detail(&a.rec, "kind"), Some("fifo"));
        assert!(fs::symlink_metadata(tmp.path().join("p")).is_err());
        assert_eq!(m.entries(), 0);
        assert!(matches!(
            delete_file(&mut ctx!(tmp, cfg, m, rng)).unwrap(),
            OpOutcome::NoTarget
        ));

        put_file(&tmp, &mut m, "f", 10);
        let a = done(delete_file(&mut ctx!(tmp, cfg, m, rng)).unwrap());
        assert_eq!(detail(&a.rec, "kind"), Some("file"));
        assert_eq!(m.total_bytes(), 0);
        assert_consistent(&tmp, &cfg, &m);
    }

    #[test]
    fn punch_hole_keeps_size_or_extends_within_cap() {
        let tmp = TempDir::new().unwrap();
        let cfg = small();
        let (mut m, mut rng) = ctx_in(&tmp, &cfg, 5);
        put_file(&tmp, &mut m, "f", 40 << 10);
        let path = tmp.path().join("f");
        let (mut plain, mut extend) = (0, 0);
        for _ in 0..40 {
            let before = fs::metadata(&path).unwrap().len();
            let a = done(punch_hole(&mut ctx!(tmp, cfg, m, rng)).unwrap());
            assert_eq!(a.rec.kind, OpKind::PunchHole);
            let after = fs::metadata(&path).unwrap().len();
            let data = fs::read(&path).unwrap();
            if detail(&a.rec, "mode") == Some("extend") {
                extend += 1;
                assert!(after > before);
                assert_eq!(detail(&a.rec, "size"), Some(after.to_string().as_str()));
                assert!(data[before as usize..].iter().all(|&b| b == 0));
            } else {
                plain += 1;
                assert_eq!(after, before);
                let off: usize = detail(&a.rec, "offset").unwrap().parse().unwrap();
                let len: usize = detail(&a.rec, "len").unwrap().parse().unwrap();
                assert!(len >= 1 && off + len <= before as usize);
                assert!(off.is_multiple_of(4096));
                assert!((off + len).is_multiple_of(4096) || off + len == before as usize);
                assert!(data[off..off + len].iter().all(|&b| b == 0));
            }
            assert!(after <= cfg.max_file_size.0);
            assert_consistent(&tmp, &cfg, &m);
        }
        assert!(plain > 0 && extend > 0, "plain={plain} extend={extend}");
    }

    #[test]
    fn align_range_widens_to_blocks_within_size() {
        assert_eq!(align_range((5000, 10), 100_000), (4096, 4096));
        assert_eq!(align_range((4096, 4096), 100_000), (4096, 4096));
        assert_eq!(align_range((8000, 1000), 10_000), (4096, 10_000 - 4096));
        assert_eq!(align_range((0, 1), 4096), (0, 4096));
        // Smaller than a block: unchanged.
        assert_eq!(align_range((100, 50), 4000), (100, 50));
    }

    #[test]
    fn punch_hole_skips_tiny_files() {
        let tmp = TempDir::new().unwrap();
        let cfg = small();
        let (mut m, mut rng) = ctx_in(&tmp, &cfg, 6);
        put_file(&tmp, &mut m, "f", 1);
        assert!(matches!(
            punch_hole(&mut ctx!(tmp, cfg, m, rng)).unwrap(),
            OpOutcome::NoTarget
        ));
        assert_eq!(fs::read(tmp.path().join("f")).unwrap(), vec![0xab]);
    }

    #[test]
    fn no_target_when_empty() {
        let tmp = TempDir::new().unwrap();
        let cfg = small();
        let (mut m, mut rng) = ctx_in(&tmp, &cfg, 7);
        let ops: [OpFn; 6] = [
            append,
            truncate,
            overwrite_range,
            rewrite,
            punch_hole,
            delete_file,
        ];
        for op in ops {
            assert!(matches!(
                op(&mut ctx!(tmp, cfg, m, rng)).unwrap(),
                OpOutcome::NoTarget
            ));
        }
        assert!(tree_sizes(tmp.path()).is_empty());
        assert_eq!(m.entries(), 0);
    }

    #[test]
    fn content_ops_report_generation_time() {
        let tmp = TempDir::new().unwrap();
        let cfg = small();
        let (mut m, mut rng) = ctx_in(&tmp, &cfg, 21);
        let a = done(create_file(&mut ctx!(tmp, cfg, m, rng)).unwrap());
        assert!(a.gen.is_some(), "a content write reports generation time");
        let a = done(truncate(&mut ctx!(tmp, cfg, m, rng)).unwrap());
        assert_eq!(a.gen, None, "a non-content op reports none");
        let mut ctx = ctx!(tmp, cfg, m, rng);
        ctx.dry = true;
        let a = done(create_file(&mut ctx).unwrap());
        assert_eq!((a.dur, a.gen), (Duration::ZERO, Some(Duration::ZERO)));
    }

    #[test]
    fn dry_run_touches_nothing_but_updates_model() {
        let tmp = TempDir::new().unwrap();
        let cfg = small();
        let (mut m, mut rng) = ctx_in(&tmp, &cfg, 8);
        let mut ctx = ctx!(tmp, cfg, m, rng);
        ctx.dry = true;
        let a = done(create_file(&mut ctx).unwrap());
        assert_eq!(a.dur, Duration::ZERO);
        assert!(tree_sizes(tmp.path()).is_empty());
        let id = m.resolve(&a.rec.path).unwrap();
        assert_eq!(m.node(id).kind, NodeKind::File);
        assert_eq!(m.entries(), 1);
        let first_ino = m.node(id).ino;
        assert_ne!(first_ino, 0, "dry-run files get a synthetic inode");
        let mut ctx = ctx!(tmp, cfg, m, rng);
        ctx.dry = true;
        let b = done(create_file(&mut ctx).unwrap());
        let second_ino = m.node(m.resolve(&b.rec.path).unwrap()).ino;
        assert_ne!(second_ino, 0);
        assert_ne!(second_ino, first_ino);

        let ops: [OpFn; 7] = [
            create_file,
            append,
            overwrite_range,
            punch_hole,
            truncate,
            rewrite,
            delete_file,
        ];
        for op in ops.iter().cycle().take(70) {
            let mut ctx = ctx!(tmp, cfg, m, rng);
            ctx.dry = true;
            if let OpOutcome::Done(a) = op(&mut ctx).unwrap() {
                assert_eq!(a.dur, Duration::ZERO);
            }
            assert!(tree_sizes(tmp.path()).is_empty());
            assert!(m.total_bytes() <= cfg.max_total_size.0);
        }
    }

    /// A record without its observed outcomes: like `dur`, whether a hole
    /// was emulated (and the `bytes` that implies) depends on the
    /// filesystem and is not part of the dry/real record identity.
    fn identity(rec: &OpRecord) -> String {
        let mut details = rec.details.clone();
        let mut bytes = rec.bytes;
        if rec.kind == OpKind::PunchHole {
            details.retain(|(k, _)| *k != "emulated");
            bytes = 0;
        }
        format!(
            "{:?} {:?} {:?} {} {:?}",
            rec.kind, rec.path, rec.to, bytes, details
        )
    }

    /// One run's step lines: full records and their [`identity`].
    struct Run {
        full: Vec<String>,
        ident: Vec<String>,
        model: Model,
        /// The next rng output after the run.
        tail: u64,
    }

    /// Runs 60 operations cycling through all seven kinds, real or dry.
    fn run(tmp: &TempDir, cfg: &Config, seed: u64, dry: bool) -> Run {
        use rand::Rng;
        let (mut m, mut rng) = ctx_in(tmp, cfg, seed);
        let kinds = [
            OpKind::CreateFile,
            OpKind::CreateFile,
            OpKind::Append,
            OpKind::Truncate,
            OpKind::OverwriteRange,
            OpKind::CreateFile,
            OpKind::Rewrite,
            OpKind::PunchHole,
            OpKind::DeleteFile,
        ];
        let mut anchor = m.root();
        let (mut full, mut ident) = (Vec::new(), Vec::new());
        for &kind in kinds.iter().cycle().take(60) {
            let mut ctx = ctx!(tmp, cfg, m, rng);
            ctx.anchor = anchor;
            ctx.dry = dry;
            match dispatch(kind)(&mut ctx).unwrap() {
                OpOutcome::Done(a) => {
                    anchor = a.anchor;
                    full.push(format!(
                        "{:?} {:?} {:?} {} {:?}",
                        a.rec.kind, a.rec.path, a.rec.to, a.rec.bytes, a.rec.details
                    ));
                    ident.push(identity(&a.rec));
                }
                OpOutcome::NoTarget => {
                    full.push("none".into());
                    ident.push("none".into());
                }
            }
        }
        let tail = rng.next_u64();
        Run {
            full,
            ident,
            model: m,
            tail,
        }
    }

    #[test]
    fn ops_are_deterministic() {
        let cfg = config(&[("max_file_size", "16K"), ("max_total_size", "64K")]);
        let (t1, t2, t3) = (
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
        );
        let r1 = run(&t1, &cfg, 42, false);
        let r2 = run(&t2, &cfg, 42, false);
        assert_eq!(r1.full, r2.full);
        assert_eq!(r1.tail, r2.tail);
        assert_eq!(tree_sizes(t1.path()), tree_sizes(t2.path()));
        assert_consistent(&t1, &cfg, &r1.model);
        assert!(r1.model.entries() > 1);
        let l1 = &r1.full;
        for kind in OpKind::ALL.iter().take(7) {
            let tag = format!("{kind:?} ");
            assert!(l1.iter().any(|l| l.starts_with(&tag)), "{kind:?} never ran");
        }
        // A dry run makes the same decisions and ends in the same model.
        // Records are compared by identity: observed outcomes (`emulated`
        // and the bytes it implies, like `dur`) may differ.
        let r3 = run(&t3, &cfg, 42, true);
        assert_eq!(r1.ident, r3.ident);
        assert_eq!(r1.tail, r3.tail);
        assert_eq!(model_sizes(&r3.model), model_sizes(&r2.model));
        assert!(tree_sizes(t3.path()).is_empty());
    }
}
