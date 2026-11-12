//! Symlink operations: create, re-point and delete symbolic links.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs as unix_fs;

use rand::distr::{weighted::WeightedIndex, Distribution};
use rand::RngExt;

use crate::kind::OpKind;
use crate::model::{Model, NodeId, NodeKind};
use crate::ops::{applied, new_ino, new_name, pick_kind, removed, syscalls, OpCtx, OpOutcome};
use crate::pick::{pick, pick_dir_or_root};

/// Characters of a dangling target's components.
const DANGLING_CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

/// How many times a dangling target's first component is redrawn while
/// it names an existing entry.
const DANGLING_TRIES: usize = 16;

/// A drawn symlink target.
struct Target {
    bytes: Vec<u8>,
    kind: &'static str,
    absolute: bool,
}

impl Target {
    fn details(&self) -> Vec<(&'static str, String)> {
        vec![
            ("target", String::from_utf8_lossy(&self.bytes).into_owned()),
            ("target_kind", self.kind.into()),
            ("absolute", u8::from(self.absolute).to_string()),
        ]
    }
}

/// New symlink in a drawn directory, with a drawn target (see
/// [`draw_target`]). Details `target`, `target_kind`, `absolute`.
pub fn symlink(ctx: &mut OpCtx) -> io::Result<OpOutcome> {
    let dir = pick_dir_or_root(ctx.rng, ctx.model, &ctx.cfg.locality, ctx.anchor);
    let Some(name) = new_name(ctx, dir) else {
        return Ok(OpOutcome::NoTarget);
    };
    let target = draw_target(ctx, dir);
    let path = ctx.model.abs_path(ctx.root, dir).join(&name);
    let ((), dur) = syscalls(ctx, &path, None, |_| {
        unix_fs::symlink(OsStr::from_bytes(&target.bytes), &path)
    })?;
    let ino = new_ino(ctx, &path)?;
    let id = ctx
        .model
        .insert(dir, name.into_bytes(), NodeKind::Symlink, 0, ino);
    Ok(applied(ctx, OpKind::Symlink, id, 0, target.details(), dur))
}

/// Replace a symlink's target with a newly drawn one (unlink, then
/// symlink). Details as for [`symlink`].
pub fn repoint_symlink(ctx: &mut OpCtx) -> io::Result<OpOutcome> {
    let Some(id) = pick_kind(ctx, NodeKind::Symlink) else {
        return Ok(OpOutcome::NoTarget);
    };
    let dir = ctx.model.node(id).parent.expect("a live node has a parent");
    let target = draw_target(ctx, dir);
    let path = ctx.model.abs_path(ctx.root, id);
    let ((), dur) = syscalls(ctx, &path, None, |_| {
        fs::remove_file(&path)?;
        unix_fs::symlink(OsStr::from_bytes(&target.bytes), &path)
    })?;
    Ok(applied(
        ctx,
        OpKind::RepointSymlink,
        id,
        0,
        target.details(),
        dur,
    ))
}

/// Unlink a symlink.
pub fn delete_symlink(ctx: &mut OpCtx) -> io::Result<OpOutcome> {
    let Some(id) = pick_kind(ctx, NodeKind::Symlink) else {
        return Ok(OpOutcome::NoTarget);
    };
    let path = ctx.model.abs_path(ctx.root, id);
    let ((), dur) = syscalls(ctx, &path, None, |_| fs::remove_file(&path))?;
    Ok(removed(ctx, OpKind::DeleteSymlink, id, Vec::new(), dur))
}

/// A target for a link in `dir`: the class by the `symlink` weights (an
/// existing file, an existing directory, or a dangling path; dangling
/// when the tree holds no node of the drawn kind), then absolute with
/// probability `symlink.absolute`. Relative targets climb from `dir` with
/// `..`; absolute ones are the root joined with the path.
fn draw_target(ctx: &mut OpCtx, dir: NodeId) -> Target {
    let s = &ctx.cfg.symlink;
    let class = WeightedIndex::new([s.file, s.dir, s.dangling])
        .map(|d| d.sample(ctx.rng))
        .unwrap_or(2);
    let absolute = ctx.rng.random::<f64>() < s.absolute;
    let kind = [Some(NodeKind::File), Some(NodeKind::Dir), None][class];
    let existing = kind.and_then(|k| pick(ctx.rng, ctx.model, &ctx.cfg.locality, dir, k));
    let (rel, label) = match existing {
        Some(id) => {
            let rel = ctx.model.rel_path(id);
            let rel = if absolute {
                rel
            } else {
                relative(&ctx.model.rel_path(dir), &rel)
            };
            (rel, if class == 0 { "file" } else { "dir" })
        }
        None => {
            let base = if absolute { ctx.model.root() } else { dir };
            (dangling(ctx, base), "dangling")
        }
    };
    let bytes = if absolute {
        ctx.root
            .join(OsStr::from_bytes(&rel))
            .into_os_string()
            .into_vec()
    } else {
        rel
    };
    Target {
        bytes,
        kind: label,
        absolute,
    }
}

/// A path of 1-3 components of `[a-z0-9]{1,12}` that does not exist
/// below `base`: its first component is redrawn while `base` holds an
/// entry folding to it.
fn dangling(ctx: &mut OpCtx, base: NodeId) -> Vec<u8> {
    let n = ctx.rng.random_range(1..=3);
    let mut first = component(ctx);
    for _ in 0..DANGLING_TRIES {
        let taken = std::str::from_utf8(&first)
            .is_ok_and(|s| ctx.model.has_folded_name(base, &Model::fold(s)));
        if !taken {
            break;
        }
        first = component(ctx);
    }
    let mut out = first;
    for _ in 1..n {
        out.push(b'/');
        out.extend(component(ctx));
    }
    out
}

fn component(ctx: &mut OpCtx) -> Vec<u8> {
    let len = ctx.rng.random_range(1..=12);
    (0..len)
        .map(|_| DANGLING_CHARS[ctx.rng.random_range(0..DANGLING_CHARS.len())])
        .collect()
}

/// The path to `to` from directory `from` (both relative to the root):
/// one `..` per component of `from` past their common prefix, then the
/// rest of `to`; `.` when they are the same.
fn relative(from: &[u8], to: &[u8]) -> Vec<u8> {
    let split = |p: &[u8]| -> Vec<Vec<u8>> {
        p.split(|&b| b == b'/')
            .filter(|c| !c.is_empty())
            .map(<[u8]>::to_vec)
            .collect()
    };
    let (from, to) = (split(from), split(to));
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let parts: Vec<Vec<u8>> = std::iter::repeat_n(b"..".to_vec(), from.len() - common)
        .chain(to[common..].iter().cloned())
        .collect();
    if parts.is_empty() {
        b".".to_vec()
    } else {
        parts.join(&b'/')
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use tempfile::TempDir;

    use crate::config::Config;
    use crate::kind::OpKind;
    use crate::log::OpRecord;
    use crate::model::{Model, NodeId, NodeKind};
    use crate::ops::testutil::{
        abs, assert_matches_disk, config, detail, done, is_no_target, parent_of, put_dir, put_file,
        step,
    };

    fn caps() -> Config {
        config(&[("max_file_size", "64K"), ("max_total_size", "4M")])
    }

    /// Checks a symlink record against the link on disk; returns its
    /// `(target_kind, absolute)`.
    fn check_link(root: &Path, m: &Model, a: &crate::ops::Applied) -> (String, bool) {
        let rec: &OpRecord = &a.rec;
        let link = abs(root, &rec.path);
        let target = fs::read_link(&link).unwrap();
        assert_eq!(
            target.as_os_str().as_bytes(),
            detail(rec, "target").unwrap().as_bytes()
        );
        let absolute = match detail(rec, "absolute").unwrap() {
            "0" => false,
            "1" => true,
            v => panic!("absolute={v}"),
        };
        if absolute {
            assert!(target.starts_with(root), "{target:?}");
        } else {
            assert!(!target.as_os_str().as_bytes().starts_with(b"/"));
        }
        let kind = detail(rec, "target_kind").unwrap().to_string();
        match kind.as_str() {
            "file" => assert!(fs::metadata(&link).unwrap().is_file()),
            "dir" => assert!(fs::metadata(&link).unwrap().is_dir()),
            "dangling" => assert!(fs::metadata(&link).is_err()),
            k => panic!("target_kind={k}"),
        }
        let id = m.resolve(&rec.path).unwrap();
        assert_eq!(m.node(id).kind, NodeKind::Symlink);
        assert_eq!(m.node(id).size, 0);
        assert_eq!(a.anchor, m.resolve(parent_of(&rec.path)).unwrap());
        (kind, absolute)
    }

    /// A tree of a few dirs and files built through the ops.
    fn populate(root: &Path, cfg: &Config, m: &mut Model, rng: &mut ChaCha8Rng) -> NodeId {
        let mut anchor = m.root();
        let kinds = [OpKind::Mkdir, OpKind::CreateFile, OpKind::CreateFile];
        for &kind in kinds.iter().cycle().take(30) {
            step(root, cfg, m, rng, &mut anchor, kind, false);
        }
        anchor
    }

    #[test]
    fn symlink_targets_resolve_as_declared() {
        let tmp = TempDir::new().unwrap();
        let cfg = caps();
        let mut m = Model::new();
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let mut anchor = populate(tmp.path(), &cfg, &mut m, &mut rng);
        let mut seen = BTreeSet::new();
        for _ in 0..100 {
            let a = step(
                tmp.path(),
                &cfg,
                &mut m,
                &mut rng,
                &mut anchor,
                OpKind::Symlink,
                false,
            )
            .expect("a dir is always available");
            seen.insert(check_link(tmp.path(), &m, &a));
            let md = fs::symlink_metadata(abs(tmp.path(), &a.rec.path)).unwrap();
            assert_eq!(m.node(m.resolve(&a.rec.path).unwrap()).ino, md.ino());
        }
        for kind in ["file", "dir", "dangling"] {
            for absolute in [false, true] {
                assert!(
                    seen.contains(&(kind.to_string(), absolute)),
                    "never saw {kind} absolute={absolute}: {seen:?}"
                );
            }
        }
        assert_matches_disk(tmp.path(), &m);
    }

    #[test]
    fn repoint_and_delete_symlink() {
        let tmp = TempDir::new().unwrap();
        let cfg = caps();
        let mut m = Model::new();
        let mut rng = ChaCha8Rng::seed_from_u64(2);
        put_dir(tmp.path(), &mut m, "d");
        put_file(tmp.path(), &mut m, "d/f", b"data");
        // Only a File: delete_symlink never takes it.
        for _ in 0..10 {
            let mut ctx = ctx!(tmp, cfg, m, rng);
            assert!(is_no_target(&delete_symlink(&mut ctx).unwrap()));
            let mut ctx = ctx!(tmp, cfg, m, rng);
            assert!(is_no_target(&repoint_symlink(&mut ctx).unwrap()));
        }
        let a = done(symlink(&mut ctx!(tmp, cfg, m, rng)).unwrap());
        check_link(tmp.path(), &m, &a);
        let path = a.rec.path.clone();
        let mut targets = BTreeSet::new();
        targets.insert(fs::read_link(abs(tmp.path(), &path)).unwrap());
        for _ in 0..10 {
            let a = done(repoint_symlink(&mut ctx!(tmp, cfg, m, rng)).unwrap());
            assert_eq!(a.rec.kind, OpKind::RepointSymlink);
            assert_eq!(a.rec.path, path);
            check_link(tmp.path(), &m, &a);
            targets.insert(fs::read_link(abs(tmp.path(), &path)).unwrap());
        }
        assert!(targets.len() > 1, "read_link never changed: {targets:?}");
        assert_eq!(m.entries(), 3);
        assert_matches_disk(tmp.path(), &m);

        let a = done(delete_symlink(&mut ctx!(tmp, cfg, m, rng)).unwrap());
        assert_eq!(a.rec.kind, OpKind::DeleteSymlink);
        assert_eq!(a.rec.path, path);
        assert!(fs::symlink_metadata(abs(tmp.path(), &path)).is_err());
        assert_eq!(a.anchor, m.resolve(parent_of(&path)).unwrap());
        assert!(is_no_target(
            &delete_symlink(&mut ctx!(tmp, cfg, m, rng)).unwrap()
        ));
        assert_eq!(fs::read(tmp.path().join("d/f")).unwrap(), b"data");
        assert_matches_disk(tmp.path(), &m);
    }

    #[test]
    fn relative_target_climbs_from_link_dir() {
        assert_eq!(relative(b"a/b", b"a/b/c"), b"c");
        assert_eq!(relative(b"a/b", b"a/x/y"), b"../x/y");
        assert_eq!(relative(b"", b"a/b"), b"a/b");
        assert_eq!(relative(b"a/b", b"z"), b"../../z");
        assert_eq!(relative(b"a/b", b"a"), b"..");
        assert_eq!(relative(b"a/b", b"a/b"), b".");
        // Components compare whole, not by prefix.
        assert_eq!(relative(b"ab", b"a/c"), b"../a/c");
    }
}
