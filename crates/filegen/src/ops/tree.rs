//! Structure operations: directories, renames, hard links and FIFOs.

use std::fs;
use std::io;
use std::time::Duration;

use nix::sys::stat::Mode;
use rand::Rng;

use crate::kind::OpKind;
use crate::log::OpRecord;
use crate::model::{NodeId, NodeKind};
use crate::ops::{
    applied, budget, draw_kind, new_ino, new_name, pick_kind, removed, syscalls, Applied, OpCtx,
    OpOutcome,
};
use crate::pick::{anchor_of, descend, pick_dest_dir, pick_dir_or_root};

/// How many directories `mkdir` draws before giving up on finding one
/// shallow enough.
const MKDIR_TRIES: usize = 8;

/// Probability that `rename` keeps the entry in its directory.
const RENAME_IN_PLACE: f64 = 0.5;

/// Every kind `rename` may move.
const RENAMABLE: [NodeKind; 5] = [
    NodeKind::Dir,
    NodeKind::File,
    NodeKind::Symlink,
    NodeKind::Fifo,
    NodeKind::Device,
];

/// A directory near the anchor, the root included.
fn pick_dir(ctx: &mut OpCtx) -> NodeId {
    pick_dir_or_root(ctx.rng, ctx.model, &ctx.cfg.locality, ctx.anchor)
}

/// A record of `id` moving from `path` to where it is now, anchored at
/// its new directory.
fn moved(ctx: &OpCtx, kind: OpKind, path: Vec<u8>, id: NodeId, dur: Duration) -> OpOutcome {
    let dir = ctx.model.node(id).parent.expect("a live node has a parent");
    OpOutcome::Done(Applied {
        rec: OpRecord {
            kind,
            path,
            to: Some(ctx.model.rel_path(id)),
            bytes: 0,
            details: Vec::new(),
        },
        anchor: anchor_of(ctx.model, dir),
        dur,
        gen: None,
    })
}

/// New directory in a drawn dir of `depth < max_depth`, redrawing up to
/// [`MKDIR_TRIES`] times. Anchors in the new directory.
pub fn mkdir(ctx: &mut OpCtx) -> io::Result<OpOutcome> {
    let Some(dir) = (0..MKDIR_TRIES).find_map(|_| Some(pick_dir(ctx)).filter(|&d| shallow(ctx, d)))
    else {
        return Ok(OpOutcome::NoTarget);
    };
    let Some(name) = new_name(ctx, dir) else {
        return Ok(OpOutcome::NoTarget);
    };
    let path = ctx.model.abs_path(ctx.root, dir).join(&name);
    let ((), dur) = syscalls(ctx, &path, None, |_| fs::create_dir(&path))?;
    let ino = new_ino(ctx, &path)?;
    let id = ctx
        .model
        .insert(dir, name.into_bytes(), NodeKind::Dir, 0, ino);
    Ok(applied(ctx, OpKind::Mkdir, id, 0, Vec::new(), dur))
}

/// Whether a new directory in `dir` stays within `max_depth`.
fn shallow(ctx: &OpCtx, dir: NodeId) -> bool {
    ctx.model.depth(dir) < ctx.cfg.max_depth
}

/// Remove an empty directory: a drawn dir, or (when it is not empty) the
/// first empty one met descending from it through drawn subdirectories.
/// Off-limits entries are children, so an empty dir has none below.
pub fn rmdir(ctx: &mut OpCtx) -> io::Result<OpOutcome> {
    let Some(mut id) = pick_kind(ctx, NodeKind::Dir) else {
        return Ok(OpOutcome::NoTarget);
    };
    while !ctx.model.node(id).children.is_empty() {
        match descend(ctx.rng, ctx.model, id, NodeKind::Dir) {
            Some(d) => id = d,
            None => return Ok(OpOutcome::NoTarget),
        }
    }
    let path = ctx.model.abs_path(ctx.root, id);
    let ((), dur) = syscalls(ctx, &path, None, |_| fs::remove_dir(&path))?;
    Ok(removed(ctx, OpKind::Rmdir, id, Vec::new(), dur))
}

/// Remove a drawn directory and everything below it, if it holds at most
/// `remove_tree_max_entries` entries and nothing off limits. Details:
/// `entries`, the number removed including the directory itself.
pub fn remove_tree(ctx: &mut OpCtx) -> io::Result<OpOutcome> {
    let Some(id) = pick_kind(ctx, NodeKind::Dir) else {
        return Ok(OpOutcome::NoTarget);
    };
    let below = ctx.model.subtree_entries(id);
    if below > ctx.cfg.remove_tree_max_entries || ctx.model.has_off_limits_below(id) {
        return Ok(OpOutcome::NoTarget);
    }
    let path = ctx.model.abs_path(ctx.root, id);
    let ((), dur) = syscalls(ctx, &path, None, |_| fs::remove_dir_all(&path))?;
    let details = vec![("entries", (below + 1).to_string())];
    Ok(removed(ctx, OpKind::RemoveTree, id, details, dur))
}

/// Rename an entry within its directory, or move it to a drawn one. A
/// directory never moves into its own subtree, never with off-limits
/// entries below it, and only if its deepest path and deepest directory
/// still fit `max_path_len` and `max_depth`. Anchors at the destination.
pub fn rename(ctx: &mut OpCtx) -> io::Result<OpOutcome> {
    let Some(kind) = draw_kind(ctx, &RENAMABLE) else {
        return Ok(OpOutcome::NoTarget);
    };
    let Some(id) = pick_kind(ctx, kind) else {
        return Ok(OpOutcome::NoTarget);
    };
    let is_dir = kind == NodeKind::Dir;
    let new_dir = if ctx.rng.gen::<f64>() < RENAME_IN_PLACE {
        ctx.model.node(id).parent.expect("a live node has a parent")
    } else {
        let not_inside = is_dir.then_some(id);
        pick_dest_dir(
            ctx.rng,
            ctx.model,
            &ctx.cfg.locality,
            ctx.anchor,
            not_inside,
        )
    };
    let Some(name) = new_name(ctx, new_dir) else {
        return Ok(OpOutcome::NoTarget);
    };
    if is_dir && !dir_fits(ctx, id, new_dir, name.len()) {
        return Ok(OpOutcome::NoTarget);
    }
    let from = ctx.model.abs_path(ctx.root, id);
    let to = ctx.model.abs_path(ctx.root, new_dir).join(&name);
    let ((), dur) = syscalls(ctx, &from, Some(&to), |_| fs::rename(&from, &to))?;
    let path = ctx.model.rel_path(id);
    ctx.model.rename(id, new_dir, name.into_bytes());
    Ok(moved(ctx, OpKind::Rename, path, id, dur))
}

/// Whether directory `id` may move into `new_dir` under a name of
/// `name_len` bytes.
fn dir_fits(ctx: &OpCtx, id: NodeId, new_dir: NodeId, name_len: usize) -> bool {
    let m = &*ctx.model;
    if m.has_off_limits_below(id) || new_dir == id || m.is_ancestor(id, new_dir) {
        return false;
    }
    let sep = usize::from(new_dir != m.root());
    let new_len = m.rel_path_len(new_dir) + sep + name_len;
    let deepest_path = m.max_rel_path_len_in(id) - m.rel_path_len(id) + new_len;
    let deepest_dir = m.max_dir_depth_in(id) + m.depth(new_dir) + 1;
    deepest_path <= ctx.cfg.max_path_len as usize && deepest_dir <= ctx.cfg.max_depth
}

/// Atomically replace one regular file with another, in any directory.
/// The replaced file's bytes are freed. `to` is the replaced path; anchors
/// at its directory.
pub fn rename_over(ctx: &mut OpCtx) -> io::Result<OpOutcome> {
    let Some(src) = pick_kind(ctx, NodeKind::File) else {
        return Ok(OpOutcome::NoTarget);
    };
    let root = ctx.model.root();
    let Some(dst) = descend(ctx.rng, ctx.model, root, NodeKind::File) else {
        return Ok(OpOutcome::NoTarget);
    };
    // Links to one inode: rename(2) would do nothing and succeed.
    if ctx.model.linked(src).contains(&dst) {
        return Ok(OpOutcome::NoTarget);
    }
    let from = ctx.model.abs_path(ctx.root, src);
    let to = ctx.model.abs_path(ctx.root, dst);
    let ((), dur) = syscalls(ctx, &from, Some(&to), |_| fs::rename(&from, &to))?;
    let path = ctx.model.rel_path(src);
    let dst_dir = ctx
        .model
        .node(dst)
        .parent
        .expect("a live node has a parent");
    let dst_name = ctx.model.node(dst).name.to_vec();
    ctx.model.remove(dst);
    ctx.model.rename(src, dst_dir, dst_name);
    Ok(moved(ctx, OpKind::RenameOver, path, src, dur))
}

/// New hard link to a regular file, in a drawn directory. The link shares
/// the file's inode and size, and its size counts toward
/// `max_total_size` again. `to` is the new link; anchors at its directory.
pub fn hardlink(ctx: &mut OpCtx) -> io::Result<OpOutcome> {
    let Some(src) = pick_kind(ctx, NodeKind::File) else {
        return Ok(OpOutcome::NoTarget);
    };
    let dir = pick_dir(ctx);
    let Some(name) = new_name(ctx, dir) else {
        return Ok(OpOutcome::NoTarget);
    };
    let size = ctx.model.node(src).size;
    if size > budget(ctx) {
        return Ok(OpOutcome::NoTarget);
    }
    let from = ctx.model.abs_path(ctx.root, src);
    let to = ctx.model.abs_path(ctx.root, dir).join(&name);
    let ((), dur) = syscalls(ctx, &from, Some(&to), |_| fs::hard_link(&from, &to))?;
    let ino = ctx.model.node(src).ino;
    let id = ctx
        .model
        .insert(dir, name.into_bytes(), NodeKind::File, size, ino);
    Ok(OpOutcome::Done(Applied {
        rec: OpRecord {
            kind: OpKind::Hardlink,
            path: ctx.model.rel_path(src),
            to: Some(ctx.model.rel_path(id)),
            bytes: 0,
            details: vec![("size", size.to_string())],
        },
        anchor: anchor_of(ctx.model, dir),
        dur,
        gen: None,
    }))
}

/// New FIFO (mode 0644, never opened) in a drawn directory.
pub fn mkfifo(ctx: &mut OpCtx) -> io::Result<OpOutcome> {
    let dir = pick_dir(ctx);
    let Some(name) = new_name(ctx, dir) else {
        return Ok(OpOutcome::NoTarget);
    };
    let path = ctx.model.abs_path(ctx.root, dir).join(&name);
    let ((), dur) = syscalls(ctx, &path, None, |_| {
        nix::unistd::mkfifo(&path, Mode::from_bits_truncate(0o644)).map_err(io::Error::from)
    })?;
    let ino = new_ino(ctx, &path)?;
    let id = ctx
        .model
        .insert(dir, name.into_bytes(), NodeKind::Fifo, 0, ino);
    Ok(applied(ctx, OpKind::Mkfifo, id, 0, Vec::new(), dur))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    use rand::{RngCore, SeedableRng};
    use rand_chacha::ChaCha8Rng;
    use tempfile::TempDir;

    use crate::config::Config;
    use crate::kind::OpKind;
    use crate::log::OpRecord;
    use crate::model::{Model, NodeId, NodeKind};
    use crate::ops::testutil::{
        abs, assert_matches_disk, config, detail, done, is_no_target, parent_of, put_dir, put_file,
        step, triples,
    };
    use crate::walk::from_disk;

    fn caps(extra: &[(&'static str, &'static str)]) -> Config {
        let mut sets = vec![("max_file_size", "64K"), ("max_total_size", "4M")];
        sets.extend_from_slice(extra);
        config(&sets)
    }

    fn seeded(seed: u64) -> (Model, ChaCha8Rng, NodeId) {
        (Model::new(), ChaCha8Rng::seed_from_u64(seed), 0)
    }

    /// Dirs of the model, with their depth.
    fn dir_depths(m: &Model) -> Vec<(Vec<u8>, u32)> {
        triples(m)
            .into_iter()
            .filter(|(_, k, _)| *k == NodeKind::Dir)
            .map(|(p, _, _)| {
                let id = m.resolve(&p).unwrap();
                (p, m.depth(id))
            })
            .collect()
    }

    #[test]
    fn mkdir_respects_max_depth() {
        let tmp = TempDir::new().unwrap();
        let cfg = caps(&[("max_depth", "2")]);
        let (mut m, mut rng, mut anchor) = seeded(1);
        let mut made = 0;
        for _ in 0..200 {
            let Some(a) = step(
                tmp.path(),
                &cfg,
                &mut m,
                &mut rng,
                &mut anchor,
                OpKind::Mkdir,
                false,
            ) else {
                continue;
            };
            made += 1;
            let id = m.resolve(&a.rec.path).unwrap();
            assert_eq!(m.node(id).kind, NodeKind::Dir);
            assert!(fs::symlink_metadata(abs(tmp.path(), &a.rec.path))
                .unwrap()
                .is_dir());
            assert_eq!(a.anchor, id, "mkdir anchors in the new dir");
        }
        assert!(made > 10, "only {made} mkdirs");
        let depths = dir_depths(&m);
        assert!(depths.iter().all(|&(_, d)| d <= 2), "{depths:?}");
        assert!(depths.iter().any(|&(_, d)| d == 2));
        // On disk too: a fresh walk sees no dir deeper than 2.
        let disk = from_disk(tmp.path()).unwrap();
        assert!(dir_depths(&disk).iter().all(|&(_, d)| d <= 2));
        assert_matches_disk(tmp.path(), &m);
    }

    /// A rename record's old path is gone and its new path exists, on
    /// disk and in the model, and the anchor is the destination dir.
    fn check_rename(tmp: &TempDir, m: &Model, a: &crate::ops::Applied) -> NodeId {
        let to = a.rec.to.as_ref().expect("rename has a destination");
        assert_ne!(&a.rec.path, to);
        assert!(fs::symlink_metadata(abs(tmp.path(), &a.rec.path)).is_err());
        assert!(fs::symlink_metadata(abs(tmp.path(), to)).is_ok());
        assert_eq!(m.resolve(&a.rec.path), None);
        let id = m.resolve(to).expect("moved node in model");
        assert_eq!(Some(a.anchor), m.node(id).parent);
        id
    }

    #[test]
    fn rename_moves_on_disk_and_model_consistently() {
        let tmp = TempDir::new().unwrap();
        let cfg = caps(&[]);
        let (mut m, mut rng, mut anchor) = seeded(2);
        let kinds = [
            OpKind::CreateFile,
            OpKind::Mkdir,
            OpKind::Rename,
            OpKind::CreateFile,
            OpKind::Mkdir,
            OpKind::Rename,
            OpKind::Rename,
        ];
        let (mut files, mut dirs, mut moves, mut in_place) = (0, 0, 0, 0);
        for &kind in kinds.iter().cycle().take(300) {
            let Some(a) = step(tmp.path(), &cfg, &mut m, &mut rng, &mut anchor, kind, false) else {
                continue;
            };
            if kind != OpKind::Rename {
                continue;
            }
            let id = check_rename(&tmp, &m, &a);
            match m.node(id).kind {
                NodeKind::File => files += 1,
                NodeKind::Dir => dirs += 1,
                k => panic!("unexpected kind {k:?}"),
            }
            if same_parent(&a.rec) {
                in_place += 1;
            } else {
                moves += 1;
            }
        }
        assert!(
            files > 0 && dirs > 0 && moves > 0 && in_place > 0,
            "files={files} dirs={dirs} moves={moves} in_place={in_place}"
        );
        assert_matches_disk(tmp.path(), &m);
    }

    /// Whether a rename record stayed within one directory.
    fn same_parent(rec: &OpRecord) -> bool {
        parent_of(&rec.path) == parent_of(rec.to.as_ref().unwrap())
    }

    #[test]
    fn rename_never_moves_dir_into_itself_or_past_path_len() {
        let tmp = TempDir::new().unwrap();
        let cfg = caps(&[("max_path_len", "60"), ("max_depth", "4")]);
        let (mut m, mut rng, mut anchor) = seeded(3);
        let kinds = [
            OpKind::Mkdir,
            OpKind::Mkdir,
            OpKind::CreateFile,
            OpKind::Rename,
            OpKind::Rename,
        ];
        let mut dir_moves = 0;
        for &kind in kinds.iter().cycle().take(500) {
            // Unwraps inside `step`: no rename ever fails with an io error.
            let a = step(tmp.path(), &cfg, &mut m, &mut rng, &mut anchor, kind, false);
            if let Some(a) = a.filter(|a| a.rec.kind == OpKind::Rename) {
                let id = check_rename(&tmp, &m, &a);
                if m.node(id).kind == NodeKind::Dir && !same_parent(&a.rec) {
                    dir_moves += 1;
                }
            }
            for (p, _, _) in triples(&m) {
                assert!(p.len() <= 60, "path too long: {p:?}");
            }
            assert!(dir_depths(&m).iter().all(|&(_, d)| d <= 4));
        }
        assert!(dir_moves > 0);
        assert_matches_disk(tmp.path(), &m);
    }

    #[test]
    fn rename_over_replaces_and_frees_bytes() {
        let tmp = TempDir::new().unwrap();
        let cfg = caps(&[]);
        let (mut m, mut rng, mut anchor) = seeded(4);
        put_dir(tmp.path(), &mut m, "d");
        put_file(tmp.path(), &mut m, "a", &[b'a'; 10]);
        put_file(tmp.path(), &mut m, "d/b", &[b'b'; 20]);
        let a = step(
            tmp.path(),
            &cfg,
            &mut m,
            &mut rng,
            &mut anchor,
            OpKind::RenameOver,
            false,
        )
        .expect("two distinct files");
        let to = a.rec.to.clone().unwrap();
        assert_ne!(a.rec.path, to);
        assert_eq!(a.rec.bytes, 0);
        let src_data: Vec<u8> = if a.rec.path == b"a" {
            vec![b'a'; 10]
        } else {
            vec![b'b'; 20]
        };
        assert!(fs::symlink_metadata(abs(tmp.path(), &a.rec.path)).is_err());
        assert_eq!(fs::read(abs(tmp.path(), &to)).unwrap(), src_data);
        assert_eq!(m.total_bytes(), src_data.len() as u64);
        let id = m.resolve(&to).unwrap();
        assert_eq!(m.node(id).size, src_data.len() as u64);
        assert_eq!(Some(a.anchor), m.node(id).parent);
        assert_eq!(m.entries(), 2);
        assert_matches_disk(tmp.path(), &m);

        // A single file cannot replace itself.
        for _ in 0..10 {
            let mut ctx = ctx!(tmp, cfg, m, rng);
            assert!(is_no_target(&rename_over(&mut ctx).unwrap()));
        }

        // Two links to one inode never replace each other.
        let src = abs(tmp.path(), &to);
        fs::hard_link(&src, tmp.path().join("link")).unwrap();
        let ino = fs::metadata(&src).unwrap().ino();
        let root = m.root();
        m.insert(
            root,
            b"link".to_vec(),
            NodeKind::File,
            src_data.len() as u64,
            ino,
        );
        for _ in 0..10 {
            let mut ctx = ctx!(tmp, cfg, m, rng);
            assert!(is_no_target(&rename_over(&mut ctx).unwrap()));
        }
        assert_matches_disk(tmp.path(), &m);
    }

    #[test]
    fn rmdir_removes_only_empty_dirs() {
        let tmp = TempDir::new().unwrap();
        let cfg = caps(&[]);
        let (mut m, mut rng, mut anchor) = seeded(5);
        put_dir(tmp.path(), &mut m, "a");
        put_dir(tmp.path(), &mut m, "a/b");
        put_file(tmp.path(), &mut m, "a/f", b"x");
        put_dir(tmp.path(), &mut m, "c");
        put_dir(tmp.path(), &mut m, "o");
        fs::write(tmp.path().join("o/.fileset.sqlite"), b"m").unwrap();
        let o = m.resolve(b"o").unwrap();
        m.insert(o, b".fileset.sqlite".to_vec(), NodeKind::OffLimits, 0, 0);
        let mut removed = Vec::new();
        for _ in 0..50 {
            if let Some(a) = step(
                tmp.path(),
                &cfg,
                &mut m,
                &mut rng,
                &mut anchor,
                OpKind::Rmdir,
                false,
            ) {
                assert!(fs::symlink_metadata(abs(tmp.path(), &a.rec.path)).is_err());
                let parent = parent_of(&a.rec.path);
                assert_eq!(a.anchor, m.resolve(parent).unwrap());
                removed.push(a.rec.path);
            }
        }
        removed.sort();
        assert_eq!(removed, [b"a/b".to_vec(), b"c".to_vec()]);
        assert_matches_disk(tmp.path(), &m);
        assert!(m.resolve(b"o/.fileset.sqlite").is_some());
    }

    #[test]
    fn remove_tree_respects_limit_and_off_limits() {
        let cfg = caps(&[("remove_tree_max_entries", "3")]);

        // Five entries below the only dir: over the limit.
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join("d")).unwrap();
        for i in 0..5 {
            fs::write(tmp.path().join(format!("d/f{i}")), b"x").unwrap();
        }
        let mut m = from_disk(tmp.path()).unwrap();
        let mut rng = ChaCha8Rng::seed_from_u64(6);
        for _ in 0..20 {
            assert!(is_no_target(
                &remove_tree(&mut ctx!(tmp, cfg, m, rng)).unwrap()
            ));
        }
        assert_matches_disk(tmp.path(), &m);

        // Off-limits below: never removed, however small.
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join("e")).unwrap();
        fs::write(tmp.path().join("e/.fileset.sqlite"), b"m").unwrap();
        let mut m = from_disk(tmp.path()).unwrap();
        for _ in 0..20 {
            assert!(is_no_target(
                &remove_tree(&mut ctx!(tmp, cfg, m, rng)).unwrap()
            ));
        }
        assert!(tmp.path().join("e/.fileset.sqlite").exists());

        // Within the limit: gone from disk and model.
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("g/h")).unwrap();
        fs::write(tmp.path().join("g/h/f"), b"abc").unwrap();
        fs::write(tmp.path().join("g/f2"), b"de").unwrap();
        let mut m = from_disk(tmp.path()).unwrap();
        let before = m.entries();
        let a = done(remove_tree(&mut ctx!(tmp, cfg, m, rng)).unwrap());
        assert_eq!(a.rec.kind, OpKind::RemoveTree);
        let removed: usize = detail(&a.rec, "entries").unwrap().parse().unwrap();
        assert_eq!(removed, before - m.entries());
        assert!(fs::symlink_metadata(abs(tmp.path(), &a.rec.path)).is_err());
        let parent = parent_of(&a.rec.path);
        assert_eq!(a.anchor, m.resolve(parent).unwrap());
        assert_matches_disk(tmp.path(), &m);
    }

    #[test]
    fn hardlink_shares_inode_and_counts_bytes() {
        let tmp = TempDir::new().unwrap();
        let cfg = config(&[("max_file_size", "1000"), ("max_total_size", "1000")]);
        let (mut m, mut rng, mut anchor) = seeded(7);
        put_dir(tmp.path(), &mut m, "d");
        put_file(tmp.path(), &mut m, "f", &[7; 400]);
        let a = step(
            tmp.path(),
            &cfg,
            &mut m,
            &mut rng,
            &mut anchor,
            OpKind::Hardlink,
            false,
        )
        .expect("room for one link");
        assert_eq!(a.rec.path, b"f");
        let to = a.rec.to.clone().unwrap();
        let ino = |p: &[u8]| fs::symlink_metadata(abs(tmp.path(), p)).unwrap().ino();
        assert_eq!(ino(b"f"), ino(&to));
        assert_eq!(detail(&a.rec, "size"), Some("400"));
        let link = m.resolve(&to).unwrap();
        assert_eq!(Some(a.anchor), m.node(link).parent);
        assert_eq!(m.node(link).ino, m.node(m.resolve(b"f").unwrap()).ino);
        assert_eq!(m.linked(link).len(), 2);
        assert_eq!(m.total_bytes(), 800);
        assert_matches_disk(tmp.path(), &m);
        // A third link would take the total to 1200 > 1000.
        for _ in 0..10 {
            let mut ctx = ctx!(tmp, cfg, m, rng);
            assert!(is_no_target(&hardlink(&mut ctx).unwrap()));
        }
        assert_eq!(m.total_bytes(), 800);
        assert_matches_disk(tmp.path(), &m);
    }

    #[test]
    fn content_ops_resize_every_hard_link() {
        let tmp = TempDir::new().unwrap();
        let cfg = config(&[("max_file_size", "4K"), ("max_total_size", "8K")]);
        let (mut m, mut rng, mut anchor) = seeded(8);
        put_dir(tmp.path(), &mut m, "d");
        let f = put_file(tmp.path(), &mut m, "f", &[1; 3000]);
        fs::hard_link(tmp.path().join("f"), tmp.path().join("d/g")).unwrap();
        let d = m.resolve(b"d").unwrap();
        let ino = m.node(f).ino;
        m.insert(d, b"g".to_vec(), NodeKind::File, 3000, ino);
        assert_eq!(m.total_bytes(), 6000);
        let kinds = [
            OpKind::Append,
            OpKind::Append,
            OpKind::PunchHole,
            OpKind::Append,
            OpKind::Rewrite,
            OpKind::OverwriteRange,
            OpKind::Truncate,
        ];
        let mut grew = 0;
        for &kind in kinds.iter().cycle().take(70) {
            let before = m.total_bytes();
            step(tmp.path(), &cfg, &mut m, &mut rng, &mut anchor, kind, false);
            grew += usize::from(m.total_bytes() > before);
            let disk = fs::metadata(tmp.path().join("f")).unwrap().len();
            let g = m.resolve(b"d/g").unwrap();
            assert_eq!(m.node(f).size, disk, "after {kind:?}");
            assert_eq!(m.node(g).size, disk, "after {kind:?}");
            assert_eq!(m.total_bytes(), 2 * disk);
            assert!(m.total_bytes() <= cfg.max_total_size.0);
            assert_matches_disk(tmp.path(), &m);
        }
        assert!(grew > 0);
    }

    #[test]
    fn mkfifo_creates_fifo() {
        let tmp = TempDir::new().unwrap();
        let cfg = caps(&[]);
        let (mut m, mut rng, mut anchor) = seeded(9);
        put_dir(tmp.path(), &mut m, "d");
        for _ in 0..5 {
            let a = step(
                tmp.path(),
                &cfg,
                &mut m,
                &mut rng,
                &mut anchor,
                OpKind::Mkfifo,
                false,
            )
            .expect("a dir is always available");
            let md = fs::symlink_metadata(abs(tmp.path(), &a.rec.path)).unwrap();
            assert!(md.file_type().is_fifo());
            let id = m.resolve(&a.rec.path).unwrap();
            assert_eq!(m.node(id).kind, NodeKind::Fifo);
            assert_eq!(m.node(id).size, 0);
            assert_eq!(m.node(id).ino, md.ino());
            assert_eq!(Some(a.anchor), m.node(id).parent);
        }
        assert_matches_disk(tmp.path(), &m);
    }

    /// The parts of a record a dry run must reproduce: everything but
    /// observed outcomes (whether a hole was emulated, and its bytes).
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

    #[test]
    fn dry_run_follows_real_run_through_structure_ops() {
        let tmp = TempDir::new().unwrap();
        let cfg = caps(&[
            ("max_depth", "4"),
            ("max_path_len", "80"),
            ("remove_tree_max_entries", "6"),
        ]);
        let kinds = [
            OpKind::CreateFile,
            OpKind::Mkdir,
            OpKind::CreateFile,
            OpKind::Hardlink,
            OpKind::Symlink,
            OpKind::Append,
            OpKind::Rename,
            OpKind::Mkdir,
            OpKind::Mkfifo,
            OpKind::RepointSymlink,
            OpKind::RenameOver,
            OpKind::CreateFile,
            OpKind::Rewrite,
            OpKind::Rmdir,
            OpKind::DeleteSymlink,
            OpKind::Mkdir,
            OpKind::RemoveTree,
            OpKind::DeleteFile,
        ];
        let run = |dry: bool| {
            let (mut m, mut rng, mut anchor) = seeded(10);
            let mut lines = Vec::new();
            for &kind in kinds.iter().cycle().take(400) {
                let a = step(tmp.path(), &cfg, &mut m, &mut rng, &mut anchor, kind, dry);
                lines.push(a.map_or("none".into(), |a| identity(&a.rec)));
            }
            (lines, m, rng.next_u64())
        };
        let (real, real_model, real_tail) = run(false);
        assert_matches_disk(tmp.path(), &real_model);
        for kind in kinds {
            let tag = format!("{kind:?} ");
            assert!(
                real.iter().any(|l| l.starts_with(&tag)),
                "{kind:?} never ran"
            );
        }
        // The dry run starts from an empty model over the same root, so
        // absolute symlink targets match; it must leave the disk alone.
        let snapshot = triples(&from_disk(tmp.path()).unwrap());
        let (dry, dry_model, dry_tail) = run(true);
        assert_eq!(real, dry);
        assert_eq!(real_tail, dry_tail);
        assert_eq!(triples(&real_model), triples(&dry_model));
        assert_eq!(real_model.total_bytes(), dry_model.total_bytes());
        assert_eq!(triples(&from_disk(tmp.path()).unwrap()), snapshot);
    }
}
