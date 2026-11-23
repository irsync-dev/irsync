//! A damaged manifest must end in an error, never in a scan that goes on
//! for ever.

mod common;

use fileset::store::{self, OpenMode};
use std::path::Path;

/// A manifest whose `entries` rows are stored out of order, found by the
/// `manifest` fuzz target. SQLite reports it as malformed when it reaches
/// the bad row. A lookup during the scan made SQLite find its place in the
/// scan again by key, which in a misordered table is an earlier row: the
/// scan then went round the same rows for ever.
#[test]
fn scan_with_lookups_ends_on_misordered_rows() {
    let seed =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/manifest/rows-out-of-order");
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(seed, dir.path().join(store::SQLITE_NAME)).unwrap();
    let loc = store::locate(dir.path(), None, None, false).unwrap();
    let s = store::open(&loc.path, loc.backend, OpenMode::ReadOnly).unwrap();

    let mut visited = 0u32;
    let scanned = s.for_each(&mut |path, _| {
        visited += 1;
        anyhow::ensure!(visited < 1000, "the scan does not end");
        let _ = s.get(path);
        Ok(())
    });

    let err = scanned.unwrap_err().to_string();
    assert!(err.contains("out of order"), "{err}");
}

/// A redb manifest whose branch pages share children, ending in a leaf with no
/// entries (lambdanature/redb#1, cberner/redb#1704). A read-only open trusts the
/// page numbers in branch pages, so a scan walked such a file once per path:
/// 2^40 leaves here, about 18 hours, without returning an entry or an error.
mod crafted_redb {
    use super::common::*;
    use fileset::compare::Tier;
    use fileset::engine::list::{list, ListOpts};
    use fileset::store::{Backend, REDB_NAME};
    use std::sync::mpsc;
    use std::time::Duration;

    const PAGE: usize = 4096;
    const LEAF: u8 = 1;
    const BRANCH: u8 = 2;
    const LEVELS: usize = 40;

    /// A PageNumber as redb writes it: index in bits 0..20, region in 20..40, order 0.
    fn page_number(region: u64, index: u64) -> [u8; 8] {
        (index | (region << 20)).to_le_bytes()
    }

    /// A branch page with one key and two children, both `child`. The child checksums
    /// are left zero: a read-only open does not check them.
    fn branch_to(child: [u8; 8]) -> Vec<u8> {
        let mut p = vec![0u8; PAGE];
        p[0] = BRANCH;
        p[2..4].copy_from_slice(&1u16.to_le_bytes());
        p[40..48].copy_from_slice(&child);
        p[48..56].copy_from_slice(&child);
        p[56..60].copy_from_slice(&61u32.to_le_bytes());
        p[60] = b'k';
        p
    }

    /// The branch page of the entries table whose first child is the leaf holding
    /// `first`: its offset and PageNumber, and those of its children.
    fn find_branch(file: &[u8], first: &[u8]) -> Vec<(usize, [u8; 8])> {
        let offsets = (0..file.len() / PAGE).map(|i| i * PAGE);
        let first_leaf = offsets
            .clone()
            .find(|&o| {
                file[o] == LEAF && file[o..o + PAGE].windows(first.len()).any(|w| w == first)
            })
            .expect("the leaf holding the first path");
        for o in offsets {
            if file[o] != BRANCH {
                continue;
            }
            let n = u16::from_le_bytes(file[o + 2..o + 4].try_into().unwrap()) as usize + 1;
            let child = |i: usize| {
                let s = o + 8 + 16 * n + 8 * i;
                u64::from_le_bytes(file[s..s + 8].try_into().unwrap())
            };
            let (index0, region0) = (child(0) & 0xF_FFFF, (child(0) >> 20) & 0xF_FFFF);
            let Some(base) = first_leaf.checked_sub(index0 as usize * PAGE) else {
                continue;
            };
            let kids: Option<Vec<_>> = (0..n)
                .map(|i| {
                    let c = child(i);
                    let (index, region, order) = (c & 0xF_FFFF, (c >> 20) & 0xF_FFFF, c >> 59);
                    let off = base + index as usize * PAGE;
                    (region == region0
                        && order == 0
                        && off + PAGE <= file.len()
                        && matches!(file[off], LEAF | BRANCH))
                    .then_some((off, page_number(region, index)))
                })
                .collect();
            if let Some(kids) = kids {
                if kids[0].0 == first_leaf && (o - base) % PAGE == 0 {
                    let own = page_number(region0, ((o - base) / PAGE) as u64);
                    return std::iter::once((o, own)).chain(kids).collect();
                }
            }
        }
        panic!("no branch page of the entries table found");
    }

    /// A tree with a manifest, then the manifest rewritten: a chain of LEVELS branch
    /// pages, each listing the next twice, ending in an empty leaf.
    fn crafted() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..3000 {
            std::fs::write(dir.path().join(format!("f{i:05}")), b"x").unwrap();
        }
        run_update(dir.path(), Backend::Redb, false, false);
        let path = dir.path().join(REDB_NAME);
        let mut file = std::fs::read(&path).unwrap();
        let chain = find_branch(&file, b"f00000");
        assert!(chain.len() > LEVELS, "{} pages to chain", chain.len());
        for w in chain[..=LEVELS].windows(2) {
            file[w[0].0..w[0].0 + PAGE].copy_from_slice(&branch_to(w[1].1));
        }
        let last = chain[LEVELS].0;
        file[last..last + PAGE].fill(0);
        file[last] = LEAF;
        std::fs::write(&path, &file).unwrap();
        dir
    }

    /// Runs `f` on a thread; its error, or a failure if it does not end in time.
    fn ends_in_error(
        what: &str,
        f: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
    ) -> String {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(Ok(())) => panic!("{what} read the crafted manifest without an error"),
            Ok(Err(e)) => format!("{e:#}"),
            Err(_) => panic!("{what} still runs after 60 s"),
        }
    }

    #[test]
    fn check_and_list_end_on_shared_branch_pages() {
        let dir = crafted();
        let root = dir.path().to_path_buf();
        let err = ends_in_error("check", move || {
            run_check_opts(&check_opts(&root, Tier::Default, &[], &[], false)).map(|_| ())
        });
        assert!(err.contains("manifest is damaged"), "check: {err}");
        let root = dir.path().to_path_buf();
        let err = ends_in_error("list", move || {
            let o = ListOpts {
                root,
                manifest: None,
                pattern: None,
                verbose: false,
            };
            list(&o, &mut Vec::new()).map(|_| ())
        });
        assert!(err.contains("manifest is damaged"), "list: {err}");
    }
}
