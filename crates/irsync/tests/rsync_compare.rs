//! Proves `irsync -aH --delete` produces the same tree as `rsync -aH --delete`,
//! pushing to and pulling from a daemon. Skipped when `rsync` is not installed.
//!
//! Not compared: uid/gid (the test runs unprivileged, so both tools skip
//! ownership) and sparse-ness (rsync without `-S` writes the zeros out).
//! Modification times are compared to the nanosecond (rsync >= 3.1 keeps them).

mod common;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{symlink, FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use common::{Flow, Harness};

fn data(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2654435761).wrapping_add(1);
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

fn set_mtime(p: &Path, secs: i64, nsec: i64) {
    use nix::sys::stat::{utimensat, UtimensatFlags};
    use nix::sys::time::TimeSpec;
    let t = TimeSpec::new(secs, nsec);
    utimensat(
        nix::fcntl::AT_FDCWD,
        p,
        &t,
        &t,
        UtimensatFlags::NoFollowSymlink,
    )
    .expect("utimensat");
}

/// Builds the source tree. Deterministic: building it twice gives equal trees.
fn build_tree(root: &Path) {
    let files: Vec<(&str, Vec<u8>, u32)> = vec![
        ("empty", vec![], 0o644),
        ("small.txt", b"hello world\n".to_vec(), 0o644),
        ("secret", data(5000, 1), 0o600),
        ("exec.sh", b"#!/bin/sh\necho hi\n".to_vec(), 0o755),
        ("readonly", data(70_000, 2), 0o444),
        ("big.bin", data(3 * 1024 * 1024 + 123, 3), 0o644),
        ("a/b/c/d/deep.dat", data(20_000, 4), 0o640),
        ("a/b/mid.txt", data(300, 5), 0o664),
    ];
    fs::create_dir_all(root.join("a/b/c/d")).unwrap();
    fs::create_dir(root.join("emptydir")).unwrap();
    for (name, body, mode) in files {
        let p = root.join(name);
        fs::write(&p, body).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap();
    }
    // Symlinks: relative, absolute, dangling.
    symlink("small.txt", root.join("rel_link")).unwrap();
    symlink("/etc/hostname", root.join("abs_link")).unwrap();
    symlink("../nowhere/at/all", root.join("a/dangling")).unwrap();
    // Hard-link pair.
    fs::write(root.join("a/linked1"), data(40_000, 6)).unwrap();
    fs::hard_link(root.join("a/linked1"), root.join("a/b/linked2")).unwrap();
    // Non-UTF-8 name.
    fs::write(
        root.join(OsString::from_vec(b"bad\xff\xfename".to_vec())),
        b"x",
    )
    .unwrap();
    // FIFO.
    let fifo = root.join("fifo");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::from_bits_truncate(0o640)).unwrap();
    fs::set_permissions(&fifo, fs::Permissions::from_mode(0o640)).unwrap();
    // Sparse file: hole, data, hole.
    let mut f = fs::File::create(root.join("sparse")).unwrap();
    f.set_len(4 * 1024 * 1024).unwrap();
    f.seek(SeekFrom::Start(1024 * 1024)).unwrap();
    f.write_all(&data(8192, 7)).unwrap();
    drop(f);

    // Varied mtimes with nanoseconds: deepest paths first so directory times stick.
    let mut all = Vec::new();
    collect(root, &mut all);
    all.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for (i, p) in all.iter().enumerate() {
        let i = i as i64;
        set_mtime(p, 1_500_000_000 + i * 86_411, 123_456_789 - i * 1_000_003);
    }
    set_mtime(root, 1_400_000_000, 987_654_321);
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        out.push(e.path());
        if e.file_type().unwrap().is_dir() {
            collect(&e.path(), out);
        }
    }
}

/// Everything `-aH` preserves (minus ownership), for one path.
#[derive(Debug, PartialEq)]
struct Node {
    kind: &'static str,
    mode: u32,
    mtime: (i64, i64),
    body: Option<Vec<u8>>,
    link: Option<PathBuf>,
    nlink: u64,
}

type Snapshot = (BTreeMap<PathBuf, Node>, Vec<Vec<PathBuf>>);

fn snapshot(root: &Path) -> Snapshot {
    let mut paths = vec![root.to_path_buf()];
    collect(root, &mut paths);
    let mut nodes = BTreeMap::new();
    let mut inodes: BTreeMap<(u64, u64), Vec<PathBuf>> = BTreeMap::new();
    for p in paths {
        let m = fs::symlink_metadata(&p).unwrap();
        let t = m.file_type();
        let kind = if t.is_dir() {
            "dir"
        } else if t.is_symlink() {
            "symlink"
        } else if t.is_file() {
            "file"
        } else if t.is_fifo() {
            "fifo"
        } else {
            "other"
        };
        let rel = p.strip_prefix(root).unwrap().to_path_buf();
        if t.is_file() && m.nlink() > 1 {
            inodes
                .entry((m.dev(), m.ino()))
                .or_default()
                .push(rel.clone());
        }
        nodes.insert(
            rel,
            Node {
                kind,
                // Symlink modes are not settable on Linux.
                mode: if t.is_symlink() { 0 } else { m.mode() & 0o7777 },
                mtime: (m.mtime(), m.mtime_nsec()),
                body: t.is_file().then(|| fs::read(&p).unwrap()),
                link: t.is_symlink().then(|| fs::read_link(&p).unwrap()),
                nlink: m.nlink(),
            },
        );
    }
    let mut groups: Vec<Vec<PathBuf>> = inodes.into_values().collect();
    for g in &mut groups {
        g.sort();
    }
    groups.sort();
    (nodes, groups)
}

/// Compares two trees; reports every divergence at once.
fn compare(label: &str, a: &Path, b: &Path) {
    let (na, ga) = snapshot(a);
    let (nb, gb) = snapshot(b);
    let mut diffs = Vec::new();
    for (k, x) in &na {
        let Some(y) = nb.get(k) else {
            diffs.push(format!("{k:?}: only in rsync tree"));
            continue;
        };
        if x.kind != y.kind {
            diffs.push(format!("{k:?}: kind {} vs {}", x.kind, y.kind));
            continue;
        }
        if x.mode != y.mode {
            diffs.push(format!("{k:?}: mode {:o} vs {:o}", x.mode, y.mode));
        }
        if x.mtime != y.mtime {
            diffs.push(format!("{k:?}: mtime {:?} vs {:?}", x.mtime, y.mtime));
        }
        if x.body != y.body {
            diffs.push(format!("{k:?}: contents differ"));
        }
        if x.link != y.link {
            diffs.push(format!("{k:?}: link {:?} vs {:?}", x.link, y.link));
        }
        if x.nlink != y.nlink {
            diffs.push(format!("{k:?}: nlink {} vs {}", x.nlink, y.nlink));
        }
    }
    for k in nb.keys().filter(|k| !na.contains_key(*k)) {
        diffs.push(format!("{k:?}: only in irsync tree"));
    }
    if ga != gb {
        diffs.push(format!("hard-link groups differ: {ga:?} vs {gb:?}"));
    }
    assert!(
        diffs.is_empty(),
        "{label}: trees diverge:\n{}",
        diffs.join("\n")
    );
    assert!(!ga.is_empty(), "{label}: fixture lost its hard-link pair");
}

fn opts() -> irsync::cli::SyncOptions {
    let args = irsync::cli::Args::try_parse_from(["irsync", "-aH", "--delete", "src/", "host:m/"])
        .unwrap();
    irsync::cli::SyncOptions::from_args(&args)
}

/// True for GNU rsync >= 3.1 (nanosecond mtimes). Apple's bundled rsync 2.6.9
/// and openrsync are too different to serve as the reference.
fn rsync_present() -> bool {
    let Ok(out) = Command::new("rsync").arg("--version").output() else {
        return false;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.lines().next().unwrap_or("");
    let ver = first.split("version").nth(1).unwrap_or("").trim();
    let mut parts = ver.split(|c: char| !c.is_ascii_digit());
    let major: u32 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    out.status.success() && !first.contains("openrsync") && (major, minor) >= (3, 1)
}

#[tokio::test]
async fn matches_rsync_archive_mode() {
    if !rsync_present() {
        eprintln!("skipping: rsync not on PATH (or older than 3.1)");
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    let dst_rsync = work.path().join("dst_rsync");
    fs::create_dir(&src).unwrap();
    fs::create_dir(&dst_rsync).unwrap();
    build_tree(&src);
    // Stale entry that --delete must remove.
    fs::write(dst_rsync.join("stale"), b"old").unwrap();

    let st = Command::new("rsync")
        .args(["-aH", "--delete"])
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", dst_rsync.display()))
        .status()
        .unwrap();
    assert!(st.success(), "rsync failed: {st}");

    let opts = opts();
    for flow in [Flow::Push, Flow::Pull] {
        let h = Harness::new().await;
        build_tree(h.src(flow));
        fs::write(h.dst(flow).join("stale"), b"old").unwrap();
        fs::create_dir(h.dst(flow).join("staledir")).unwrap();
        let out = tokio::time::timeout(Duration::from_secs(60), h.run(&opts, flow))
            .await
            .expect("timed out")
            .expect("sync session");
        assert!(out.errors.is_empty(), "{flow:?}: {:?}", out.errors);
        let name = match flow {
            Flow::Push => "dst_irsync",
            Flow::Pull => "dst_irsync_pull",
        };
        compare(name, &dst_rsync, h.dst(flow));
    }
}
