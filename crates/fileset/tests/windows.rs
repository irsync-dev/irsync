//! What `fileset` records on Windows: long paths, names the shell refuses, hard links,
//! symlinks and junctions (never followed), a file another program holds, and the POSIX
//! fields it leaves at zero.

#![cfg(windows)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;
use fileset::compare::Tier;
use fileset::engine::list::{list, ListOpts};
use fileset::engine::Outcome;
use fileset::model::{Entry, Flags, Kind};
use fileset::platform::{self, testutil};
use fileset::store::{self, Backend, OpenMode};
use tempfile::TempDir;

/// A temp dir and its canonical path (on Windows, the `\\?\` form).
fn root() -> (TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let r = std::fs::canonicalize(t.path()).unwrap();
    (t, r)
}

/// Every stored path and entry of the SQLite manifest in `root`, in store order.
fn entries(root: &Path) -> Vec<(Vec<u8>, Entry)> {
    let mut all = Vec::new();
    open_ro(root, Backend::Sqlite)
        .for_each(&mut |k, e| {
            all.push((k.to_vec(), e.clone()));
            Ok(())
        })
        .unwrap();
    all
}

fn entry(root: &Path, rel: &[u8]) -> Entry {
    open_ro(root, Backend::Sqlite)
        .get(rel)
        .unwrap()
        .unwrap_or_else(|| panic!("no entry {}", String::from_utf8_lossy(rel)))
}

fn init(root: &Path) -> (Outcome, String) {
    run_update(root, Backend::Sqlite, false, false)
}

fn check_clean(root: &Path) {
    let (outcome, out, err) = run_check(root, Tier::Default, &[], &[], false);
    assert_eq!((outcome, out.as_str()), (Outcome::Clean, ""), "{err}");
}

/// No stored path starts with `prefix`.
fn nothing_below(root: &Path, prefix: &[u8]) {
    for (k, _) in entries(root) {
        assert!(!k.starts_with(prefix), "{}", String::from_utf8_lossy(&k));
    }
}

#[test]
fn a_tree_deeper_than_260_characters() {
    let (_t, r) = root();
    let mut deepest = r.clone();
    for i in 0..30 {
        deepest.push(format!("{i:02}-directory-name-x"));
    }
    std::fs::create_dir_all(&deepest).unwrap();
    std::fs::write(deepest.join("f"), "x").unwrap();
    assert!(deepest.join("f").as_os_str().len() > 600);
    let (outcome, err) = init(&r);
    assert_eq!(outcome, Outcome::Clean, "{err}");
    let opts = ListOpts {
        root: r.clone(),
        manifest: None,
        pattern: Some(b"**/f".to_vec()),
        verbose: false,
    };
    let mut out = Vec::new();
    assert_eq!(list(&opts, &mut out).unwrap(), Outcome::Clean);
    let out = String::from_utf8(out).unwrap();
    let rel: Vec<String> = (0..30)
        .map(|i| format!("{i:02}-directory-name-x"))
        .collect();
    let want = format!("{}/f kind=file ", rel.join("/"));
    assert!(out.starts_with(&want), "{out}");
    assert_eq!(out.lines().count(), 1, "{out}");
    check_clean(&r);
}

#[test]
fn names_the_shell_refuses() {
    let (_t, r) = root();
    let surrogate: &[u8] = &[b'x', 0xED, 0xA0, 0x80];
    let names: [&[u8]; 4] = [b"CON", b"trail.", b"trail ", surrogate];
    // Through the canonical `\\?\` root, Windows takes each name as it is.
    for name in names {
        let p = r.join(platform::bytes_to_os(name).unwrap());
        std::fs::write(&p, "x").unwrap();
    }
    let (outcome, err) = init(&r);
    assert_eq!(outcome, Outcome::Clean, "{err}");
    for name in names {
        assert_eq!(entry(&r, name).kind, Kind::File, "{name:?}");
    }
    assert_eq!(
        entries(&r).len(),
        4 + 2,
        "the four, the root and the manifest"
    );
    check_clean(&r);
}

#[test]
fn hard_links_share_an_identity() {
    let (_t, r) = root();
    std::fs::write(r.join("a"), "x").unwrap();
    std::fs::hard_link(r.join("a"), r.join("b")).unwrap();
    init(&r);
    let (a, b) = (entry(&r, b"a"), entry(&r, b"b"));
    assert_ne!(a.ino, 0);
    assert_eq!((a.ino, a.dev), (b.ino, b.dev));
    assert_eq!((a.nlink, b.nlink), (2, 2));
}

#[test]
fn the_three_symlink_kinds_are_recorded_and_not_followed() {
    let (_t, r) = root();
    if !testutil::symlinks_available(&r) {
        return;
    }
    std::fs::write(r.join("f"), "x").unwrap();
    std::fs::create_dir(r.join("d")).unwrap();
    std::fs::write(r.join("d").join("inside"), "y").unwrap();
    symlink(&r, "to_file", "f");
    symlink(&r, "to_dir", "d");
    symlink(&r, "dangling", "missing");
    let (outcome, err) = init(&r);
    assert_eq!(outcome, Outcome::Clean, "{err}");
    for (link, target) in [
        (&b"to_file"[..], &b"f"[..]),
        (b"to_dir", b"d"),
        (b"dangling", b"missing"),
    ] {
        let e = entry(&r, link);
        assert_eq!(e.kind, Kind::Symlink, "{link:?}");
        assert_eq!(e.target.as_deref(), Some(target), "{link:?}");
        assert_eq!(e.blake3, None, "{link:?}");
        assert_eq!(e.size, 0, "{link:?}");
    }
    nothing_below(&r, b"to_dir/");
    assert!(entries(&r).iter().any(|(k, _)| k == b"d/inside"));
    check_clean(&r);
}

#[test]
fn a_junction_is_other_and_not_entered() {
    let (_t, r) = root();
    std::fs::create_dir(r.join("d")).unwrap();
    std::fs::write(r.join("d").join("f"), "x").unwrap();
    testutil::junction(&r.join("d"), &r.join("j"));
    let (outcome, err) = init(&r);
    assert_eq!(outcome, Outcome::Clean, "{err}");
    assert_eq!(entry(&r, b"j").kind, Kind::Other);
    nothing_below(&r, b"j/");
    check_clean(&r);
}

#[test]
fn a_junction_to_its_own_parent_ends_the_walk() {
    let (_t, r) = root();
    std::fs::create_dir(r.join("d")).unwrap();
    testutil::junction(&r.join("d"), &r.join("d").join("loop"));
    let (outcome, err) = init(&r);
    assert_eq!(outcome, Outcome::Clean, "{err}");
    assert_eq!(entry(&r, b"d/loop").kind, Kind::Other);
    nothing_below(&r, b"d/loop/");
    check_clean(&r);
}

#[test]
fn a_manifest_that_is_a_junction_is_refused() {
    let (_t, r) = root();
    let (_elsewhere, e) = root();
    std::fs::write(r.join("f"), "x").unwrap();
    let real = e.join(Backend::Fjall.file_name());
    store::open(&real, Backend::Fjall, OpenMode::Create)
        .unwrap()
        .close()
        .unwrap();
    testutil::junction(&real, &r.join(Backend::Fjall.file_name()));
    let mut opts = update_opts(&r, Backend::Fjall, false, false);
    assert!(matches!(
        opts.mode,
        fileset::engine::update::Mode::Update { .. }
    ));
    let err = format!("{:#}", run_update_opts(&opts).unwrap_err());
    assert!(err.contains("--manifest"), "{err}");
    assert!(err.contains("is a symbolic link"), "{err}");
    opts.mode = fileset::engine::update::Mode::Init { backend: None };
    let err = format!("{:#}", run_update_opts(&opts).unwrap_err());
    assert!(err.contains("--manifest"), "{err}");
    assert!(err.contains("is a symbolic link"), "{err}");
    let copts = check_opts(&r, Tier::Default, &[], &[], false);
    let err = format!("{:#}", run_check_opts(&copts).unwrap_err());
    assert!(err.contains("--manifest"), "{err}");
    assert!(err.contains("is a symbolic link"), "{err}");
}

#[test]
fn a_fjall_manifest_with_a_junction_inside_is_refused() {
    let listing = |dir: &Path| {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let fresh = |root: &Path| {
        let path = root.join(Backend::Fjall.file_name());
        store::open(&path, Backend::Fjall, OpenMode::Create)
            .unwrap()
            .close()
            .unwrap();
        path
    };
    let refused = |path: &Path, mode| match store::open(path, Backend::Fjall, mode) {
        Ok(_) => panic!("{} opened", path.display()),
        Err(e) => format!("{e:#}"),
    };
    for mode in [OpenMode::ReadOnly, OpenMode::ReadWrite] {
        // The keyspaces directory itself is a junction to a directory of the user.
        let ((_tree, tree), (_mine, mine)) = (root(), root());
        let path = fresh(&tree);
        for year in ["2023", "2024"] {
            std::fs::create_dir(mine.join(year)).unwrap();
            std::fs::write(mine.join(year).join("photo"), b"keep").unwrap();
        }
        let before = listing(&mine);
        std::fs::rename(path.join("keyspaces"), tree.join("moved")).unwrap();
        testutil::junction(&mine, &path.join("keyspaces"));
        let err = refused(&path, mode);
        assert!(err.contains("neither a file nor a directory"), "{err}");
        assert_eq!(listing(&mine), before);
        assert_eq!(std::fs::read(mine.join("2023/photo")).unwrap(), b"keep");

        // A junction deeper inside.
        let (_tree, tree) = root();
        let path = fresh(&tree);
        testutil::junction(&mine, &path.join("keyspaces").join("7"));
        let err = refused(&path, mode);
        assert!(err.contains("neither a file nor a directory"), "{err}");
        assert_eq!(listing(&mine), before);
    }
}

#[test]
fn hashing_a_file_swapped_for_a_symlink_fails() {
    let (_t, r) = root();
    if !testutil::symlinks_available(&r) {
        return;
    }
    std::fs::write(r.join("real"), "hello").unwrap();
    symlink(&r, "f", "real");
    assert!(fileset::hash::hash_file(&r.join("f")).is_err());
    // A junction in the file's place is refused too.
    std::fs::create_dir(r.join("d")).unwrap();
    testutil::junction(&r.join("d"), &r.join("j"));
    assert!(fileset::hash::hash_file(&r.join("j")).is_err());
}

#[test]
fn a_file_held_without_sharing_is_unreadable_not_fatal() {
    let (_t, r) = root();
    std::fs::write(r.join("held"), "secret").unwrap();
    std::fs::write(r.join("free"), "x").unwrap();
    let Some(_held) = testutil::make_unreadable(&r.join("held")) else {
        panic!("a file held without sharing could still be opened");
    };
    let (outcome, err) = init(&r);
    assert_eq!(outcome, Outcome::Clean, "{err}");
    assert!(err.starts_with("WARN cannot hash held: "), "{err}");
    assert!(err.ends_with(" warnings=1\n"), "{err}");
    let held = entry(&r, b"held");
    assert!(held.flags.contains(Flags::UNREADABLE));
    assert_eq!(held.blake3, None);
    assert_eq!(
        entry(&r, b"free").blake3,
        Some(*blake3::hash(b"x").as_bytes())
    );
}

/// A stored file the walk cannot stat is unverified, a difference, even when its name
/// still shows in the directory listing.
#[test]
fn a_file_this_user_may_not_stat_is_unverified() {
    // Not the `\\?\` form: icacls does not take it.
    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    std::fs::write(r.join("a"), "x").unwrap();
    std::fs::write(r.join("b"), "y").unwrap();
    let (outcome, err) = init(r);
    assert_eq!(outcome, Outcome::Clean, "{err}");
    let Some(_denied) = testutil::deny_read(&r.join("b")) else {
        return;
    };
    let (outcome, out, err) = run_check(r, Tier::Default, &[], &[], false);
    assert_eq!(out, "CHG b kind=file→unverified\n", "{err}");
    assert!(err.starts_with("WARN cannot stat b: "), "{err}");
    assert!(
        err.ends_with("\nadded=0 deleted=0 changed=1 unchanged=3 warnings=1\n"),
        "{err}"
    );
    assert_eq!(outcome, Outcome::Differences);
}

#[test]
fn a_relative_root_with_slashes_and_dots_works() {
    let (_t, r) = root();
    std::fs::create_dir_all(r.join("a").join("b")).unwrap();
    std::fs::write(r.join("a").join("b").join("f"), "x").unwrap();
    for args in [["init", "a/b/../b"], ["check", r"a\b"]] {
        let out = Command::new(env!("CARGO_BIN_EXE_fileset"))
            .args(args)
            .current_dir(&r)
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn posix_fields_are_zero() {
    let (_t, r) = root();
    std::fs::create_dir(r.join("d")).unwrap();
    std::fs::write(r.join("d").join("f"), "x").unwrap();
    std::fs::write(r.join("g"), "y").unwrap();
    init(&r);
    let all = entries(&r);
    assert_eq!(all.len(), 5);
    for (k, e) in all {
        let name = String::from_utf8_lossy(&k);
        assert_eq!((e.mode, e.uid, e.gid, e.rdev), (0, 0, 0, 0), "{name}");
        assert!(e.xattrs.is_empty(), "{name}");
    }
}
