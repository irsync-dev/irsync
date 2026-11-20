//! A damaged manifest must end in an error, never in a scan that goes on
//! for ever.

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
