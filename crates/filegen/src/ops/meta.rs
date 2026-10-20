//! Metadata operations: modes, times, extended attributes, ownership, and
//! device nodes.

use std::fs::{self, Permissions};
use std::io;
use std::os::unix::fs::PermissionsExt;

use nix::fcntl::{AtFlags, AT_FDCWD};
use nix::sys::stat::{utimensat, Mode, SFlag, UtimensatFlags};
use nix::sys::time::TimeSpec;
use nix::unistd::{fchownat, geteuid, Gid, Uid};
use rand::distributions::{Distribution, WeightedIndex};
use rand::{Rng, RngCore};

use crate::kind::OpKind;
use crate::model::{NodeId, NodeKind};
use crate::ops::{applied, draw_kind, new_ino, new_name, pick_kind, syscalls, OpCtx, OpOutcome};
use crate::pick::pick_dir_or_root;

/// The instant `set_mtime` draws relative to (2026-10-20T00:00:00Z). A
/// constant rather than the clock, so the drawn times, and the log
/// records naming them, depend on the seed alone.
pub const MTIME_REFERENCE: jiff::Timestamp = jiff::Timestamp::constant(1_792_454_400, 0);

/// Seconds in a (Julian) year.
pub const YEAR: i64 = 31_557_600;

/// How far back the `recent` mtime class reaches, in seconds.
const RECENT_SPAN: i64 = 30 * 86_400;

/// suid, sgid and sticky: one is OR'd in with probability `special_weight`.
const SPECIAL_BITS: [u32; 3] = [0o4000, 0o2000, 0o1000];

/// A node of one of `kinds`: the kind drawn by how many of each the tree
/// holds, then one of that kind near the anchor.
fn pick_any(ctx: &mut OpCtx, kinds: &[NodeKind]) -> Option<NodeId> {
    let kind = draw_kind(ctx, kinds)?;
    pick_kind(ctx, kind)
}

/// Set a drawn mode on a file, dir or FIFO: one of `chmod.modes`, with
/// probability `special_weight` one special bit OR'd in, and always the
/// owner's read and write bits (plus search on a dir). Sticky is dropped
/// on non-dirs (BSDs refuse it). Detail `mode` in 4-digit octal.
pub fn chmod(ctx: &mut OpCtx) -> io::Result<OpOutcome> {
    if ctx.cfg.chmod.modes.is_empty() {
        return Ok(OpOutcome::NoTarget);
    }
    let Some(id) = pick_any(ctx, &[NodeKind::File, NodeKind::Dir, NodeKind::Fifo]) else {
        return Ok(OpOutcome::NoTarget);
    };
    let modes = &ctx.cfg.chmod.modes;
    let mut mode = modes[ctx.rng.gen_range(0..modes.len())].0 & 0o7777;
    if ctx.rng.gen::<f64>() < ctx.cfg.chmod.special_weight {
        mode |= SPECIAL_BITS[ctx.rng.gen_range(0..SPECIAL_BITS.len())];
    }
    if ctx.model.node(id).kind == NodeKind::Dir {
        mode |= 0o700;
    } else {
        mode = (mode | 0o600) & !0o1000;
    }
    let path = ctx.model.abs_path(ctx.root, id);
    let ((), dur) = syscalls(ctx, |_| {
        fs::set_permissions(&path, Permissions::from_mode(mode))
    })?;
    let details = vec![("mode", format!("{mode:04o}"))];
    Ok(applied(ctx, OpKind::Chmod, id, 0, details, dur))
}

/// Set the mtime (and atime) of any entry, symlinks included (without
/// following them), to a drawn instant: `recent` (up to 30 days before
/// [`MTIME_REFERENCE`]), `distant` (up to `distant_years` before, never
/// before the epoch) or `future` (up to `future_years` after), plus drawn
/// nanoseconds. Details `mtime` (RFC 3339, nanoseconds) and `class`.
pub fn set_mtime(ctx: &mut OpCtx) -> io::Result<OpOutcome> {
    let kinds = [
        NodeKind::File,
        NodeKind::Dir,
        NodeKind::Symlink,
        NodeKind::Fifo,
        NodeKind::Device,
    ];
    let Some(id) = pick_any(ctx, &kinds) else {
        return Ok(OpOutcome::NoTarget);
    };
    let m = &ctx.cfg.mtime;
    let class = WeightedIndex::new([m.recent, m.distant, m.future])
        .map(|d| d.sample(ctx.rng))
        .unwrap_or(0);
    let (label, span, sign) = match class {
        0 => ("recent", RECENT_SPAN, -1),
        1 => ("distant", i64::from(m.distant_years) * YEAR, -1),
        _ => ("future", i64::from(m.future_years) * YEAR, 1),
    };
    let offset = ctx.rng.gen_range(0..=span);
    let nanos: i32 = ctx.rng.gen_range(0..1_000_000_000);
    let secs = (MTIME_REFERENCE.as_second() + sign * offset).max(0);
    let ts = jiff::Timestamp::new(secs, nanos).expect("within jiff's range");
    let path = ctx.model.abs_path(ctx.root, id);
    let ((), dur) = syscalls(ctx, |_| {
        let spec = TimeSpec::new(secs as libc::time_t, nanos.into());
        utimensat(
            AT_FDCWD,
            &path,
            &spec,
            &spec,
            UtimensatFlags::NoFollowSymlink,
        )
        .map_err(io::Error::from)
    })?;
    let details = vec![
        ("mtime", ts.strftime("%Y-%m-%dT%H:%M:%S%.9fZ").to_string()),
        ("class", label.into()),
    ];
    Ok(applied(ctx, OpKind::SetMtime, id, 0, details, dur))
}

/// Set, replace or remove a user xattr on a file or dir. The action, the
/// name (from `xattr.names`) and a value of up to `xattr.max_value` bytes
/// are always drawn, so the draws do not depend on the disk. `replace`
/// and `remove` of an absent name set it instead, and the record says
/// `set`: that is observed, so a dry run reports the drawn action.
/// Details `name`, `action`, `len` (of the value drawn).
pub fn set_xattr(ctx: &mut OpCtx) -> io::Result<OpOutcome> {
    if ctx.cfg.xattr.names.is_empty() {
        return Ok(OpOutcome::NoTarget);
    }
    let Some(id) = pick_any(ctx, &[NodeKind::File, NodeKind::Dir]) else {
        return Ok(OpOutcome::NoTarget);
    };
    let drawn = ["set", "replace", "remove"][ctx.rng.gen_range(0..3)];
    let names = &ctx.cfg.xattr.names;
    let name = names[ctx.rng.gen_range(0..names.len())].clone();
    let len = ctx.rng.gen_range(0..=ctx.cfg.xattr.max_value.0) as usize;
    let mut value = vec![0u8; len];
    ctx.rng.fill_bytes(&mut value);
    let path = ctx.model.abs_path(ctx.root, id);
    let (taken, dur) = syscalls(ctx, |_| {
        let exists = drawn != "set" && xattr::get(&path, &name)?.is_some();
        Ok(Some(match drawn {
            "remove" if exists => {
                xattr::remove(&path, &name)?;
                "remove"
            }
            _ => {
                xattr::set(&path, &name, &value)?;
                if exists {
                    "replace"
                } else {
                    "set"
                }
            }
        }))
    })?;
    let details = vec![
        ("name", name),
        ("action", taken.unwrap_or(drawn).into()),
        ("len", len.to_string()),
    ];
    Ok(applied(ctx, OpKind::SetXattr, id, 0, details, dur))
}

/// Change an entry's owner and/or group (without following symlinks) to
/// ids drawn from `chown.uids` and `chown.gids`; an empty list keeps that
/// id. `NoTarget`, without drawing, when both lists are empty. Details
/// `uid` and `gid` (`-` when kept).
pub fn chown(ctx: &mut OpCtx) -> io::Result<OpOutcome> {
    let (uids, gids) = (&ctx.cfg.chown.uids, &ctx.cfg.chown.gids);
    if uids.is_empty() && gids.is_empty() {
        return Ok(OpOutcome::NoTarget);
    }
    let kinds = [
        NodeKind::File,
        NodeKind::Dir,
        NodeKind::Symlink,
        NodeKind::Fifo,
        NodeKind::Device,
    ];
    let Some(id) = pick_any(ctx, &kinds) else {
        return Ok(OpOutcome::NoTarget);
    };
    let (uids, gids) = (&ctx.cfg.chown.uids, &ctx.cfg.chown.gids);
    let uid = (!uids.is_empty()).then(|| uids[ctx.rng.gen_range(0..uids.len())]);
    let gid = (!gids.is_empty()).then(|| gids[ctx.rng.gen_range(0..gids.len())]);
    let path = ctx.model.abs_path(ctx.root, id);
    let ((), dur) = syscalls(ctx, |_| {
        fchownat(
            AT_FDCWD,
            &path,
            uid.map(Uid::from_raw),
            gid.map(Gid::from_raw),
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )
        .map_err(io::Error::from)
    })?;
    let show = |v: Option<u32>| v.map_or("-".to_string(), |v| v.to_string());
    let details = vec![("uid", show(uid)), ("gid", show(gid))];
    Ok(applied(ctx, OpKind::Chown, id, 0, details, dur))
}

/// New character (probability `mknod.char_fraction`) or block device in a
/// drawn dir, with major 1..=255 and minor 0..=255. Everything is drawn
/// first; then `NoTarget` unless running as root, so the draw count does
/// not depend on privilege. Details `kind`, `major`, `minor`.
pub fn mknod(ctx: &mut OpCtx) -> io::Result<OpOutcome> {
    let dir = pick_dir_or_root(ctx.rng, ctx.model, &ctx.cfg.locality, ctx.anchor);
    let Some(name) = new_name(ctx, dir) else {
        return Ok(OpOutcome::NoTarget);
    };
    let char_dev = ctx.rng.gen::<f64>() < ctx.cfg.mknod.char_fraction;
    let major: u32 = ctx.rng.gen_range(1..256);
    let minor: u32 = ctx.rng.gen_range(0..256);
    if !geteuid().is_root() {
        return Ok(OpOutcome::NoTarget);
    }
    let (kind, label) = if char_dev {
        (SFlag::S_IFCHR, "char")
    } else {
        (SFlag::S_IFBLK, "block")
    };
    let path = ctx.model.abs_path(ctx.root, dir).join(&name);
    let ((), dur) = syscalls(ctx, |_| {
        let rdev = libc::makedev(major as _, minor as _);
        nix::sys::stat::mknod(&path, kind, Mode::from_bits_truncate(0o644), rdev)
            .map_err(io::Error::from)
    })?;
    let ino = new_ino(ctx, &path)?;
    let id = ctx
        .model
        .insert(dir, name.into_bytes(), NodeKind::Device, 0, ino);
    let details = vec![
        ("kind", label.into()),
        ("major", major.to_string()),
        ("minor", minor.to_string()),
    ];
    Ok(applied(ctx, OpKind::Mknod, id, 0, details, dur))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::fs;
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    use std::path::Path;

    use rand::{RngCore, SeedableRng};
    use rand_chacha::ChaCha8Rng;
    use tempfile::TempDir;

    use crate::config::Config;
    use crate::kind::OpKind;
    use crate::log::OpRecord;
    use crate::model::{Model, NodeId, NodeKind};
    use crate::ops::testutil::{
        abs, assert_matches_disk, config, detail, done, is_no_target, put_file, step, triples,
    };
    use crate::ops::Applied;
    use crate::walk::from_disk;

    fn caps(extra: &[(&'static str, &'static str)]) -> Config {
        let mut sets = vec![("max_file_size", "64K"), ("max_total_size", "4M")];
        sets.extend_from_slice(extra);
        config(&sets)
    }

    /// A tree of dirs, files, symlinks and FIFOs built through the ops.
    fn populate(root: &Path, cfg: &Config, m: &mut Model, rng: &mut ChaCha8Rng) -> NodeId {
        let mut anchor = m.root();
        let kinds = [
            OpKind::Mkdir,
            OpKind::CreateFile,
            OpKind::CreateFile,
            OpKind::Symlink,
            OpKind::Mkfifo,
        ];
        for &kind in kinds.iter().cycle().take(40) {
            step(root, cfg, m, rng, &mut anchor, kind, false);
        }
        anchor
    }

    /// One step of `kind` that must find a target.
    fn run(
        root: &Path,
        cfg: &Config,
        m: &mut Model,
        rng: &mut ChaCha8Rng,
        anchor: &mut NodeId,
        kind: OpKind,
    ) -> Applied {
        step(root, cfg, m, rng, anchor, kind, false).expect("a target exists")
    }

    fn mtime_of(md: &fs::Metadata) -> jiff::Timestamp {
        jiff::Timestamp::new(md.mtime(), md.mtime_nsec() as i32).unwrap()
    }

    #[test]
    fn chmod_keeps_owner_access() {
        let tmp = TempDir::new().unwrap();
        let cfg = caps(&[("chmod.special_weight", "1.0")]);
        let mut m = Model::new();
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let mut anchor = populate(tmp.path(), &cfg, &mut m, &mut rng);
        let (mut files, mut dirs, mut fifos, mut special) = (0, 0, 0, 0);
        for _ in 0..300 {
            let a = run(
                tmp.path(),
                &cfg,
                &mut m,
                &mut rng,
                &mut anchor,
                OpKind::Chmod,
            );
            let id = m.resolve(&a.rec.path).unwrap();
            let md = fs::symlink_metadata(abs(tmp.path(), &a.rec.path)).unwrap();
            let mode = md.mode() & 0o7777;
            let text = detail(&a.rec, "mode").unwrap();
            assert_eq!(text.len(), 4, "{text}");
            let logged = u32::from_str_radix(text, 8).unwrap();
            // The kernel may drop setgid silently; the permission bits are exact.
            assert_eq!(mode & 0o777, logged & 0o777, "{:?}", a.rec.path);
            match m.node(id).kind {
                NodeKind::Dir => {
                    dirs += 1;
                    assert!(md.is_dir());
                    assert_eq!(mode & 0o700, 0o700);
                    assert_eq!(a.anchor, id);
                }
                NodeKind::File => {
                    files += 1;
                    assert!(md.is_file());
                    assert_eq!(mode & 0o600, 0o600);
                    assert_eq!(logged & 0o1000, 0, "sticky bit on a file");
                }
                NodeKind::Fifo => {
                    fifos += 1;
                    assert!(md.file_type().is_fifo());
                    assert_eq!(mode & 0o600, 0o600);
                }
                k => panic!("chmod on {k:?}"),
            }
            if logged & 0o7000 != 0 {
                special += 1;
            }
        }
        assert!(files > 0 && dirs > 0 && fifos > 0, "{files} {dirs} {fifos}");
        // Sticky on a non-dir is dropped, so not every draw keeps a bit.
        assert!(special > 150, "special bits seen only {special} times");
        assert_matches_disk(tmp.path(), &m);
    }

    #[test]
    fn chmod_without_modes_is_no_target() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = caps(&[]);
        cfg.chmod.modes.clear();
        let mut m = Model::new();
        let mut rng = ChaCha8Rng::seed_from_u64(2);
        put_file(tmp.path(), &mut m, "f", b"x");
        assert!(is_no_target(&chmod(&mut ctx!(tmp, cfg, m, rng)).unwrap()));
    }

    #[test]
    fn set_mtime_classes_and_symlinks() {
        let tmp = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let target = outside.path().join("t");
        fs::write(&target, b"target").unwrap();
        let before = mtime_of(&fs::metadata(&target).unwrap());
        let cfg = caps(&[]);
        let mut m = Model::new();
        let mut rng = ChaCha8Rng::seed_from_u64(3);
        // The only entry is a symlink, so every set_mtime takes it.
        let link = tmp.path().join("l");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let ino = fs::symlink_metadata(&link).unwrap().ino();
        m.insert(m.root(), b"l".to_vec(), NodeKind::Symlink, 0, ino);
        let mut classes = BTreeSet::new();
        let mut nanos = false;
        for _ in 0..100 {
            let a = done(set_mtime(&mut ctx!(tmp, cfg, m, rng)).unwrap());
            assert_eq!(a.rec.kind, OpKind::SetMtime);
            assert_eq!(a.rec.path, b"l");
            assert_eq!(a.anchor, m.root());
            let md = fs::symlink_metadata(&link).unwrap();
            assert!(md.file_type().is_symlink());
            let logged: jiff::Timestamp = detail(&a.rec, "mtime").unwrap().parse().unwrap();
            assert_eq!(mtime_of(&md), logged);
            nanos |= md.mtime_nsec() != 0;
            let class = detail(&a.rec, "class").unwrap().to_string();
            let secs = logged.as_second() - MTIME_REFERENCE.as_second();
            match class.as_str() {
                "recent" => assert!((-30 * 86_400 - 1..=0).contains(&secs), "{secs}"),
                "distant" => assert!((-40 * YEAR - 1..=0).contains(&secs), "{secs}"),
                "future" => assert!((0..=5 * YEAR).contains(&secs), "{secs}"),
                c => panic!("class={c}"),
            }
            classes.insert(class);
        }
        assert!(nanos, "nanoseconds always zero");
        assert_eq!(classes.len(), 3, "{classes:?}");
        assert_eq!(mtime_of(&fs::metadata(&target).unwrap()), before);

        // Future class only: always after the reference instant.
        let cfg = caps(&[
            ("mtime.recent", "0"),
            ("mtime.distant", "0"),
            ("mtime.future", "1"),
        ]);
        put_file(tmp.path(), &mut m, "f", b"data");
        for _ in 0..20 {
            let a = done(set_mtime(&mut ctx!(tmp, cfg, m, rng)).unwrap());
            assert_eq!(detail(&a.rec, "class"), Some("future"));
            let md = fs::symlink_metadata(abs(tmp.path(), &a.rec.path)).unwrap();
            assert!(mtime_of(&md) > MTIME_REFERENCE);
        }
        assert_matches_disk(tmp.path(), &m);
    }

    /// Whether `e` says the filesystem has no user xattrs.
    fn unsupported(e: &io::Error) -> bool {
        e.raw_os_error() == Some(libc::ENOTSUP) || e.raw_os_error() == Some(libc::EOPNOTSUPP)
    }

    /// Every xattr in the tree, by relative path and name.
    fn xattrs(root: &Path, m: &Model) -> Vec<(Vec<u8>, String, Vec<u8>)> {
        let mut out = Vec::new();
        for (rel, kind, _) in triples(m) {
            if !matches!(kind, NodeKind::File | NodeKind::Dir) {
                continue;
            }
            let p = abs(root, &rel);
            let mut names: Vec<_> = xattr::list(&p).unwrap().collect();
            names.sort();
            for n in names {
                let v = xattr::get(&p, &n).unwrap().unwrap();
                out.push((rel.clone(), n.to_string_lossy().into_owned(), v));
            }
        }
        out
    }

    #[test]
    fn set_xattr_round_trip() {
        let probe = TempDir::new().unwrap();
        let p = probe.path().join("p");
        fs::write(&p, b"").unwrap();
        if let Err(e) = xattr::set(&p, "user.filegen.probe", b"1") {
            if unsupported(&e) {
                eprintln!("skipping set_xattr_round_trip: no user xattrs here ({e})");
                return;
            }
            panic!("{e}");
        }
        let cfg = caps(&[("xattr.max_value", "64")]);
        let run_in = |seed: u64| {
            let tmp = TempDir::new().unwrap();
            let mut m = Model::new();
            let mut rng = ChaCha8Rng::seed_from_u64(seed);
            let mut anchor = populate(tmp.path(), &cfg, &mut m, &mut rng);
            let mut actions = BTreeSet::new();
            for _ in 0..200 {
                let a = run(
                    tmp.path(),
                    &cfg,
                    &mut m,
                    &mut rng,
                    &mut anchor,
                    OpKind::SetXattr,
                );
                let path = abs(tmp.path(), &a.rec.path);
                let kind = m.node(m.resolve(&a.rec.path).unwrap()).kind;
                assert!(matches!(kind, NodeKind::File | NodeKind::Dir), "{kind:?}");
                let name = detail(&a.rec, "name").unwrap();
                assert!(cfg.xattr.names.iter().any(|n| n == name));
                let len: usize = detail(&a.rec, "len").unwrap().parse().unwrap();
                assert!(len <= 64);
                let got = xattr::get(&path, name).unwrap();
                let action = detail(&a.rec, "action").unwrap().to_string();
                match action.as_str() {
                    "set" | "replace" => assert_eq!(got.map(|v| v.len()), Some(len)),
                    "remove" => assert_eq!(got, None),
                    other => panic!("action={other}"),
                }
                actions.insert(action);
            }
            assert_eq!(actions.len(), 3, "{actions:?}");
            assert_matches_disk(tmp.path(), &m);
            xattrs(tmp.path(), &m)
        };
        let first = run_in(4);
        assert!(!first.is_empty());
        assert_eq!(first, run_in(4), "same seed, different xattrs");
    }

    #[test]
    fn chown_without_ids_is_no_target() {
        let tmp = TempDir::new().unwrap();
        let cfg = caps(&[]);
        let mut m = Model::new();
        let mut rng = ChaCha8Rng::seed_from_u64(5);
        put_file(tmp.path(), &mut m, "f", b"x");
        let next = rng.clone().next_u64();
        assert!(is_no_target(&chown(&mut ctx!(tmp, cfg, m, rng)).unwrap()));
        assert_eq!(rng.next_u64(), next, "no draws without ids");
    }

    #[test]
    fn chown_to_own_ids() {
        // An unprivileged user may chown to its own uid and gid.
        let tmp = TempDir::new().unwrap();
        let uid = nix::unistd::geteuid().as_raw();
        let gid = nix::unistd::getegid().as_raw();
        let mut cfg = caps(&[]);
        cfg.chown.uids = vec![uid];
        cfg.chown.gids = vec![gid];
        let mut m = Model::new();
        let mut rng = ChaCha8Rng::seed_from_u64(6);
        let mut anchor = populate(tmp.path(), &cfg, &mut m, &mut rng);
        for _ in 0..30 {
            let a = run(
                tmp.path(),
                &cfg,
                &mut m,
                &mut rng,
                &mut anchor,
                OpKind::Chown,
            );
            assert_eq!(detail(&a.rec, "uid"), Some(uid.to_string().as_str()));
            assert_eq!(detail(&a.rec, "gid"), Some(gid.to_string().as_str()));
            let md = fs::symlink_metadata(abs(tmp.path(), &a.rec.path)).unwrap();
            assert_eq!((md.uid(), md.gid()), (uid, gid));
        }
        cfg.chown.gids.clear();
        let a = run(
            tmp.path(),
            &cfg,
            &mut m,
            &mut rng,
            &mut anchor,
            OpKind::Chown,
        );
        assert_eq!(detail(&a.rec, "gid"), Some("-"));
        assert_matches_disk(tmp.path(), &m);
    }

    #[test]
    fn mknod_without_root_is_no_target() {
        let tmp = TempDir::new().unwrap();
        let cfg = caps(&[]);
        let mut m = Model::new();
        let mut rng = ChaCha8Rng::seed_from_u64(7);
        if nix::unistd::geteuid().is_root() {
            let a = done(mknod(&mut ctx!(tmp, cfg, m, rng)).unwrap());
            let md = fs::symlink_metadata(abs(tmp.path(), &a.rec.path)).unwrap();
            let ft = md.file_type();
            assert!(ft.is_char_device() || ft.is_block_device());
            let id = m.resolve(&a.rec.path).unwrap();
            assert_eq!(m.node(id).kind, NodeKind::Device);
            assert_matches_disk(tmp.path(), &m);
            return;
        }
        // The draws happen anyway, so the draw count does not depend on
        // privilege.
        let next = rng.clone().next_u64();
        assert!(is_no_target(&mknod(&mut ctx!(tmp, cfg, m, rng)).unwrap()));
        assert_ne!(rng.next_u64(), next);
        assert_eq!(m.entries(), 0);
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    /// A record without what a dry run cannot know: the `set_xattr`
    /// action actually taken.
    fn identity(rec: &OpRecord) -> String {
        let mut details = rec.details.clone();
        if rec.kind == OpKind::SetXattr {
            details.retain(|(k, _)| *k != "action");
        }
        format!(
            "{:?} {:?} {:?} {} {:?}",
            rec.kind, rec.path, rec.to, rec.bytes, details
        )
    }

    #[test]
    fn dry_run_follows_real_run_through_meta_ops() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = caps(&[]);
        cfg.chown.uids = vec![nix::unistd::geteuid().as_raw()];
        let kinds = [
            OpKind::CreateFile,
            OpKind::Mkdir,
            OpKind::Chmod,
            OpKind::Symlink,
            OpKind::SetMtime,
            OpKind::Mkfifo,
            OpKind::Chown,
            OpKind::Mknod,
            OpKind::Chmod,
            OpKind::SetMtime,
        ];
        let go = |dry: bool| {
            let mut m = Model::new();
            let mut rng = ChaCha8Rng::seed_from_u64(8);
            let mut anchor = m.root();
            let mut lines = Vec::new();
            for &kind in kinds.iter().cycle().take(200) {
                let a = step(tmp.path(), &cfg, &mut m, &mut rng, &mut anchor, kind, dry);
                lines.push(a.map_or("none".into(), |a| identity(&a.rec)));
            }
            (lines, m, rng.next_u64())
        };
        let (real, real_model, real_tail) = go(false);
        assert_matches_disk(tmp.path(), &real_model);
        for kind in [OpKind::Chmod, OpKind::SetMtime, OpKind::Chown] {
            let tag = format!("{kind:?} ");
            assert!(
                real.iter().any(|l| l.starts_with(&tag)),
                "{kind:?} never ran"
            );
        }
        let before = triples(&from_disk(tmp.path()).unwrap());
        let (dry, dry_model, dry_tail) = go(true);
        assert_eq!(real, dry);
        assert_eq!(real_tail, dry_tail);
        assert_eq!(triples(&real_model), triples(&dry_model));
        assert_eq!(triples(&from_disk(tmp.path()).unwrap()), before);
    }
}
