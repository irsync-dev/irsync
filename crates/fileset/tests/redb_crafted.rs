//! A redb manifest made on purpose so that reading it would not end. redb does not
//! verify a file it opens for reading only, so `check` and `list` have to come to an
//! end on whatever they are given.
//!
//! The files are made by rewriting the child page numbers in the branch pages of a
//! sound manifest (the layout is in redb's `docs/design.md`).

use fileset::model::{Entry, Flags, Kind, Timespec};
use fileset::store::{open, Backend, OpenMode};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

const ENTRIES: u64 = 30_000;

fn entry(i: u64) -> Entry {
    Entry {
        kind: Kind::File,
        mode: 0o644,
        uid: 1000,
        gid: 1000,
        size: i,
        nlink: 1,
        ino: i,
        dev: 1,
        rdev: 0,
        mtime: Timespec { sec: 1, nsec: 2 },
        ctime: Timespec { sec: 3, nsec: 4 },
        blake3: Some([7; 32]),
        target: None,
        xattrs: vec![],
        flags: Flags::default(),
        generation: 1,
    }
}

/// The bytes of a finished manifest of `ENTRIES` entries: enough for a tree of pages
/// three levels deep, with nine branch pages.
fn manifest(dir: &Path) -> Vec<u8> {
    let path = dir.join("good");
    let mut s = open(&path, Backend::Redb, OpenMode::Create).unwrap();
    let items: Vec<_> = (0..ENTRIES)
        .map(|i| (format!("d{:02}/f{i:06}", i % 20).into_bytes(), entry(i)))
        .collect();
    for chunk in items.chunks(1000) {
        s.put_batch(chunk).unwrap();
    }
    let mut m = s.meta().unwrap();
    m.unfinished = false;
    s.set_meta(&m).unwrap();
    s.compact().unwrap();
    s.close().unwrap();
    std::fs::read(&path).unwrap()
}

fn u32_at(b: &[u8], at: usize) -> usize {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize
}

/// A branch page of the first region: where it starts in the file, its page number,
/// and how many keys it has (it has one child more).
#[derive(Clone, Copy, Debug)]
struct Branch {
    at: usize,
    page: u64,
    keys: usize,
}

impl Branch {
    /// Where the page number of child `child` is in the file.
    fn child(&self, child: usize) -> std::ops::Range<usize> {
        let at = self.at + 8 + 16 * (self.keys + 1) + 8 * child;
        at..at + 8
    }
}

/// Where the pages of the first region start, and the size of a page.
fn pages(b: &[u8]) -> (usize, usize) {
    let page = u32_at(b, 12);
    (page * (1 + u32_at(b, 16)), page)
}

/// The branch pages of the entries tree, the root first. The few other trees in the
/// file are too small to have a branch with more than one key.
fn branches(b: &[u8]) -> Vec<Branch> {
    let (start, page) = pages(b);
    let mut found = Vec::new();
    for i in 0..(b.len() - start) / page {
        let at = start + i * page;
        let keys = u16::from_le_bytes([b[at + 2], b[at + 3]]) as usize;
        if b[at] == 2 && keys > 1 && 8 + 24 * (keys + 1) < page {
            found.push(Branch {
                at,
                page: i as u64,
                keys,
            });
        }
    }
    // The root has the fewest children: it only holds the other branches.
    found.sort_by_key(|f| f.keys);
    assert!(found.len() >= 8, "{found:?}");
    found
}

/// What reading `bytes` as a manifest gives: the number of entries a scan visited, or
/// the error. Fails the test if the read has not ended after 20 seconds. `lookups_only`
/// leaves the scan out.
fn read(dir: &Path, bytes: &[u8], mode: OpenMode, lookups_only: bool) -> Result<u64, String> {
    let path = dir.join("bad");
    std::fs::write(&path, bytes).unwrap();
    let (tx, rx) = mpsc::channel();
    // On a thread of its own, left behind if it does not end.
    std::thread::spawn(move || {
        let read = || -> anyhow::Result<u64> {
            let s = open(&path, Backend::Redb, mode)?;
            s.meta()?;
            for path in [&b"d03/f000003"[..], b"zzz", b""] {
                s.get(path)?;
            }
            let mut n = 0;
            if !lookups_only {
                s.for_each(&mut |_, _| {
                    n += 1;
                    Ok(())
                })?;
            }
            Ok(n)
        };
        let _ = tx.send(read().map_err(|e| format!("{e:#}")));
    });
    rx.recv_timeout(Duration::from_secs(20))
        .expect("the read does not end")
}

/// A page that is its own child, or a child of one below it: redb gives up when it
/// has gone 128 pages down, and a scan that is handed an entry again stops there.
#[test]
fn a_loop_in_the_tree_of_pages_ends_in_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = manifest(dir.path());
    let found = branches(&bytes);
    let root = found[0];
    let mut refused = 0;
    let mut case = |what: &str, change: &dyn Fn(&mut Vec<u8>)| {
        let mut b = bytes.clone();
        change(&mut b);
        match read(dir.path(), &b, OpenMode::ReadOnly, false) {
            // Only a change that changed nothing reads as before.
            Ok(n) => assert_eq!(n, ENTRIES, "{what}"),
            Err(_) => refused += 1,
        }
    };
    for branch in &found {
        for (which, children) in [
            ("first", 0..1),
            ("last", branch.keys..branch.keys + 1),
            ("every", 0..branch.keys + 1),
        ] {
            case(&format!("{which} child of {branch:?} is itself"), &|b| {
                for c in children.clone() {
                    b[branch.child(c)].copy_from_slice(&branch.page.to_le_bytes());
                }
            });
        }
        case(&format!("first child of {branch:?} is the root"), &|b| {
            b[branch.child(0)].copy_from_slice(&root.page.to_le_bytes());
        });
    }
    assert!(refused >= 3 * found.len(), "{refused} refused");
}

/// No loop, and still no end: every child of each branch is the next branch, so the
/// one page at the bottom is reached by about a hundred to the power of eight ways.
/// When that page holds entries, the scan is handed one of them a second time and
/// stops. When it is empty, redb walks from way to way without handing anything back.
#[test]
fn a_tree_that_reaches_an_empty_page_by_endless_ways_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = manifest(dir.path());
    let found = branches(&bytes);
    let (start, page) = pages(&bytes);
    for empty in [false, true] {
        let mut b = bytes.clone();
        // The page every way ends in: the first child of the last branch.
        let bottom = found[found.len() - 1];
        let leaf = u64::from_le_bytes(b[bottom.child(0)].try_into().unwrap());
        for (i, branch) in found.iter().enumerate() {
            let to = found.get(i + 1).map_or(leaf, |next| next.page);
            for c in 0..=branch.keys {
                b[branch.child(c)].copy_from_slice(&to.to_le_bytes());
            }
        }
        let leaf_at = start + leaf as usize * page;
        assert_eq!(b[leaf_at], 1, "page {leaf} is a leaf");
        if empty {
            b[leaf_at + 2..leaf_at + 4].copy_from_slice(&0u16.to_le_bytes());
        }

        // A lookup only goes down, and ends.
        read(dir.path(), &b, OpenMode::ReadOnly, true).unwrap();
        let err = read(dir.path(), &b, OpenMode::ReadOnly, false).unwrap_err();
        let want = match empty {
            true => "a read of all its entries does not end",
            false => "entries out of order",
        };
        assert!(err.contains(want), "empty {empty}: {err}");
        assert!(!err.contains("I/O error"), "{err}");
        // An open for writing verifies every page first.
        read(dir.path(), &b, OpenMode::ReadWrite, true).unwrap_err();
    }
}
