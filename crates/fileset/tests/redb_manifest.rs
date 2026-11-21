//! What the redb store is for: a manifest that is one file, as small as it gets after
//! every `init` and `update`, and that an interrupted run does not leave unusable.

mod common;

use common::*;
use fileset::engine::Outcome;
use fileset::store::{self, Backend, OpenMode, REDB_NAME};
use std::path::Path;

fn size(root: &Path) -> u64 {
    std::fs::metadata(root.join(REDB_NAME)).unwrap().len()
}

/// A tree of `n` small files in ten directories.
fn many_files(n: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for d in 0..10 {
        std::fs::create_dir(dir.path().join(format!("d{d}"))).unwrap();
    }
    for i in 0..n {
        std::fs::write(dir.path().join(format!("d{}/f{i:05}", i % 10)), b"x").unwrap();
    }
    dir
}

#[test]
fn update_compacts_the_manifest_unless_told_not_to() {
    let dir = many_files(3000);
    run_update(dir.path(), Backend::Redb, false, false);
    let fresh = size(dir.path());

    // Three directories go, and every file of three others changes.
    let churn = |round: u8| {
        for d in 0..3 {
            let gone = dir
                .path()
                .join(format!("d{}", d + 3 * (round as usize % 2)));
            let _ = std::fs::remove_dir_all(gone);
        }
        for d in 6..9 {
            for f in std::fs::read_dir(dir.path().join(format!("d{d}"))).unwrap() {
                std::fs::write(f.unwrap().path(), [round; 2]).unwrap();
            }
        }
    };
    churn(1);
    let mut opts = update_opts(dir.path(), Backend::Redb, false, false);
    opts.compact = false;
    assert_eq!(run_update_opts(&opts).unwrap().0, Outcome::Clean);
    let left = size(dir.path());
    assert!(left > fresh, "left at {left}, was {fresh} when new");
    // Not compacted, it is as good a manifest.
    let (outcome, out, _) = run_check(dir.path(), fileset::compare::Tier::Default, &[], &[], false);
    assert_eq!((outcome, out.as_str()), (Outcome::Clean, ""));

    churn(2);
    run_update(dir.path(), Backend::Redb, false, false);
    let compacted = size(dir.path());
    assert!(
        compacted < fresh,
        "{compacted} with fewer entries than the {fresh} it began with"
    );
    let (outcome, out, _) = run_check(dir.path(), fileset::compare::Tier::Default, &[], &[], false);
    assert_eq!((outcome, out.as_str()), (Outcome::Clean, ""));
    // Nothing but the manifest was added to the tree.
    assert!(std::fs::read_dir(dir.path()).unwrap().all(|e| {
        let name = e.unwrap().file_name();
        name == REDB_NAME || name.to_string_lossy().starts_with('d')
    }));
}

/// The manifest as a killed `init` or `update` leaves it: copied while the store that
/// wrote it was still open.
fn killed(root: &Path, finished: bool) {
    let scratch = tempfile::tempdir().unwrap();
    let p = scratch.path().join("m");
    let mut s = store::open(&p, Backend::Redb, OpenMode::Create).unwrap();
    if finished {
        let mut m = s.meta().unwrap();
        m.unfinished = false;
        s.set_meta(&m).unwrap();
    }
    std::fs::copy(&p, root.join(REDB_NAME)).unwrap();
    s.close().unwrap();
}

#[test]
fn a_killed_init_is_replaced_by_the_next_init() {
    let dir = tree(&[("f", "x")]);
    killed(dir.path(), false);
    // Reading it would take a repair.
    let err = run_check_opts(&check_opts(
        dir.path(),
        fileset::compare::Tier::Default,
        &[],
        &[],
        false,
    ))
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("was not closed cleanly"),
        "{err:#}"
    );
    // update repairs it, and finds nothing in it to bring up to date.
    let opts = update_opts(dir.path(), Backend::Redb, false, false);
    let err = run_update_opts(&opts).unwrap_err().to_string();
    assert!(err.contains("interrupted init"), "{err}");

    let mut opts = opts;
    opts.mode = fileset::engine::update::Mode::Init { backend: None };
    let (outcome, err) = run_update_opts(&opts).unwrap();
    assert_eq!(outcome, Outcome::Clean);
    assert!(err.contains("replacing the unfinished manifest"), "{err}");
    assert_eq!(
        keys(&*open_ro(dir.path(), Backend::Redb)),
        [".", REDB_NAME, "f"]
    );
}

#[test]
fn a_killed_update_is_repaired_by_the_next_update() {
    let dir = tree(&[("f", "x")]);
    killed(dir.path(), true);
    let err = run_check_opts(&check_opts(
        dir.path(),
        fileset::compare::Tier::Default,
        &[],
        &[],
        false,
    ))
    .unwrap_err();
    assert!(format!("{err:#}").contains("fileset update"), "{err:#}");
    let (outcome, _) = run_update(dir.path(), Backend::Redb, false, false);
    assert_eq!(outcome, Outcome::Clean);
    let (outcome, out, _) = run_check(dir.path(), fileset::compare::Tier::Default, &[], &[], false);
    assert_eq!((outcome, out.as_str()), (Outcome::Clean, ""));
}
