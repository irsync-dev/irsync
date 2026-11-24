//! Index benchmark: how fast the index takes and answers a tree's entries,
//! and how long a whole pull of the tree takes. See
//! `docs/superpowers/specs/2026-11-16-index-hash-ids-design.md`.
//!
//! ```sh
//! cargo bench -p irsync --bench index --no-run
//! TMPDIR=/some/real/disk EXECUTABLE TREE N REPS [CASES]
//! ```
//!
//! `EXECUTABLE` is the path the first command prints.
//! `TREE` is `short` (`dNN/fNNNNN`), `long` (the 783-byte paths of
//! `tests/blocking_pool.rs`) or `paths:FILE` (the first `N` relative paths
//! listed in `FILE`, one a line). `CASES` is a comma-separated subset of
//! `writer,lookup,sync` (default: all). Every file holds 8 bytes: one chunk.
//! Each run prints one line of `key=value` fields.

#![deny(unsafe_code)]

#[path = "../tests/common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use irsync::fs::meta::lstat_entry_with_ctime;
use irsync::fs::paths::RelPath;
use irsync::index::{Index, NewEntry};
use irsync_proto::chunk::digest_reader;

use common::{archive_opts, Flow, Harness};

fn main() {
    // `cargo bench` passes `--bench`; nothing else starts with `-`.
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect();
    let [tree, n, reps, rest @ ..] = args.as_slice() else {
        eprintln!("usage: index TREE N REPS [writer,lookup,sync]");
        std::process::exit(2);
    };
    let n: usize = n.parse().expect("N");
    let reps: usize = reps.parse().expect("REPS");
    let cases = rest.first().map_or("writer,lookup,sync", String::as_str);
    let paths = tree_paths(tree, n);
    let label = tree.split('/').next_back().unwrap_or(tree);

    let src = tempfile::tempdir().unwrap();
    write_tree(src.path(), &paths);
    let entries: Vec<NewEntry> = paths.iter().map(|p| new_entry(src.path(), p)).collect();
    for rep in 0..reps {
        if cases.contains("writer") || cases.contains("lookup") {
            let state = tempfile::tempdir().unwrap();
            let ix = Index::open(state.path(), src.path()).unwrap().into_handle();
            let t = Instant::now();
            for e in &entries {
                ix.queue_entry(e.clone());
            }
            ix.flush();
            let flush = t.elapsed();
            if cases.contains("writer") {
                let mb = dir_bytes(state.path()) as f64 / (1 << 20) as f64;
                println!(
                    "case=writer tree={label} n={} rep={rep} secs={:.3} us_per_file={:.1} db_mb={mb:.1}",
                    paths.len(),
                    flush.as_secs_f64(),
                    flush.as_secs_f64() * 1e6 / paths.len() as f64,
                );
            }
            if cases.contains("lookup") {
                lookups(&ix, &entries, label, rep);
            }
        }
        if cases.contains("sync") {
            sync(&paths, label, rep);
        }
    }
}

/// `get_entry` for every path, in an order that jumps around the tree, then
/// `find_chunk` for every tenth file's chunk; each its own `read`, as the
/// hasher and the receiver call it.
fn lookups(ix: &irsync::index::IndexHandle, entries: &[NewEntry], label: &str, rep: usize) {
    let n = entries.len();
    // A stride coprime to n visits every index once, far from the last.
    let stride = (1..)
        .map(|k| n / 3 + k)
        .find(|s| gcd(*s, n) == 1)
        .unwrap_or(1);
    let t = Instant::now();
    for i in 0..n {
        let e = &entries[(i * stride) % n];
        let got = ix.read(|r| r.get_entry(&e.path)).unwrap();
        assert!(got.is_some(), "{:?} not indexed", e.path);
    }
    let get = t.elapsed();
    let t = Instant::now();
    let mut found = 0usize;
    for i in (0..n).step_by(10) {
        let e = &entries[(i * stride) % n];
        let hash = e.digest.as_ref().unwrap().chunks[0].hash;
        found += usize::from(ix.read(|r| r.find_chunk(&hash)).unwrap().is_some());
    }
    let find = t.elapsed();
    assert_eq!(found, n.div_ceil(10));
    println!(
        "case=lookup tree={label} n={n} rep={rep} get_us={:.2} find_chunk_us={:.2}",
        get.as_secs_f64() * 1e6 / n as f64,
        find.as_secs_f64() * 1e6 / found as f64,
    );
}

/// A pull of the tree into an empty destination, then the same pull again
/// with nothing changed. Each ends once the daemon's side has too (its
/// index flush included).
fn sync(paths: &[PathBuf], label: &str, rep: usize) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (fresh, again) = rt.block_on(async {
        let h = Harness::new().await;
        write_tree(h.src(Flow::Pull), paths);
        let opts = archive_opts();
        let t = Instant::now();
        let out = h.run(&opts, Flow::Pull).await.expect("first pull");
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let fresh = t.elapsed();
        let t = Instant::now();
        let out = h.run(&opts, Flow::Pull).await.expect("second pull");
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        (fresh, t.elapsed())
    });
    rt.shutdown_timeout(Duration::from_secs(5));
    println!(
        "case=sync tree={label} n={} rep={rep} fresh_secs={:.3} unchanged_secs={:.3}",
        paths.len(),
        fresh.as_secs_f64(),
        again.as_secs_f64(),
    );
}

fn tree_paths(tree: &str, n: usize) -> Vec<PathBuf> {
    match tree {
        "short" => (0..n)
            .map(|i| format!("d{:02}/f{:05}", i % 20, i / 20).into())
            .collect(),
        "long" => (0..n)
            .map(|i| {
                let d = i % 20;
                let mut p: PathBuf = (0..3).map(|l| format!("{l}{d:_>239}")).collect();
                p.push(format!("{:0>60}", i / 20));
                p
            })
            .collect(),
        _ => {
            let file = tree
                .strip_prefix("paths:")
                .expect("TREE: short, long or paths:FILE");
            let list = std::fs::read_to_string(file).expect("read the path list");
            let v: Vec<PathBuf> = list.lines().take(n).map(PathBuf::from).collect();
            assert_eq!(v.len(), n, "{file} lists fewer than {n} paths");
            v
        }
    }
}

fn write_tree(root: &Path, paths: &[PathBuf]) {
    for (i, p) in paths.iter().enumerate() {
        let full = root.join(p);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, (i as u64).to_le_bytes()).unwrap();
    }
}

/// The entry the hasher would queue for `rel`: its stat and its digest.
fn new_entry(root: &Path, rel: &Path) -> NewEntry {
    let digest = digest_reader(std::fs::File::open(root.join(rel)).unwrap()).unwrap();
    let path = RelPath::new(&irsync::platform::os_to_bytes(rel.as_os_str())).unwrap();
    let (mut entry, ctime) = lstat_entry_with_ctime(root, &path).unwrap();
    entry.whole_hash = Some(irsync_proto::v1::Hash {
        bytes: digest.hash.to_vec(),
    });
    NewEntry {
        path,
        entry,
        ctime,
        digest: Some(digest),
    }
}

fn dir_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_bytes(&e.path()),
            _ => e.metadata().map_or(0, |m| m.len()),
        })
        .sum()
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}
