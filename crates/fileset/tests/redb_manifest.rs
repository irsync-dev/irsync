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
#[cfg(unix)]
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

/// The manifest as a killed `init` or `update` leaves it. Here the writer's locks keep
/// every other handle from reading its file, so it cannot be copied while open: a child
/// process writes it and exits without closing it.
#[cfg(windows)]
fn killed(root: &Path, finished: bool) {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args(["child_leaves_a_killed_manifest", "--exact"])
        .env(KILLED_AT, root.join(REDB_NAME))
        .stdout(std::process::Stdio::null());
    if finished {
        child.env(KILLED_FINISHED, "1");
    }
    let status = child.status().unwrap();
    assert!(status.success(), "{status}");
}

#[cfg(windows)]
const KILLED_AT: &str = "FILESET_TEST_KILLED_AT";
#[cfg(windows)]
const KILLED_FINISHED: &str = "FILESET_TEST_KILLED_FINISHED";

/// Not a test of its own: run in a child process by `killed`, it writes at `KILLED_AT`
/// the manifest a killed init (or, with `KILLED_FINISHED`, update) leaves, and exits
/// without closing it. Without `KILLED_AT` it does nothing.
#[cfg(windows)]
#[test]
fn child_leaves_a_killed_manifest() {
    let Some(p) = std::env::var_os(KILLED_AT) else {
        return;
    };
    let mut s = store::open(Path::new(&p), Backend::Redb, OpenMode::Create).unwrap();
    if std::env::var_os(KILLED_FINISHED).is_some() {
        let mut m = s.meta().unwrap();
        m.unfinished = false;
        s.set_meta(&m).unwrap();
    }
    // Exiting runs no destructor: the store is not closed.
    std::process::exit(0);
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

/// redb panics on some damaged files. `fileset` reports the manifest as damaged, and
/// redb's own panic message stays off stderr.
#[test]
fn a_panic_inside_redb_is_reported_as_damage_and_nothing_else() {
    let dir = many_files(200);
    run_update(dir.path(), Backend::Redb, false, false);
    let manifest = dir.path().join(REDB_NAME);
    let bytes = std::fs::read(&manifest).unwrap();
    let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut caught = 0;
    for case in 0..300u32 {
        let mut b = bytes.clone();
        let at = (rnd() as usize) % b.len();
        if case % 2 == 0 {
            let n = ((rnd() % 4096) as usize).min(b.len() - at);
            b[at..at + n].fill(0);
        } else {
            let n = ((rnd() % 64) as usize).min(b.len() - at);
            b[at..at + n].fill_with(|| rnd() as u8);
        }
        std::fs::write(&manifest, &b).unwrap();
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_fileset"))
            .arg("list")
            .arg(dir.path())
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!err.contains("panicked"), "case {case}: {err}");
        if err.contains("its redb database cannot be read") {
            assert_eq!(out.status.code(), Some(2), "case {case}: {err}");
            assert_eq!(err.lines().count(), 1, "case {case}: {err}");
            caught += 1;
        }
    }
    // Without a case where redb panicked, this would have shown nothing.
    assert!(caught > 0);
}
