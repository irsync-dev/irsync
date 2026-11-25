//! A path the local user types is read as Windows reads it (`dst.` is
//! `dst`), once, where it comes in; what the session opens, what it makes
//! and what its index is keyed by are then the same directory.
#![cfg(windows)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{archive_opts, Flow, Harness};

async fn within30<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(30), f)
        .await
        .expect("test timed out after 30 s")
}

/// `path` in the verbatim form, so the test sees a name exactly as it is.
fn verbatim(path: &Path) -> PathBuf {
    let shown = path.display().to_string();
    assert!(shown.len() > 3 && &shown[1..3] == r":\", "{shown}");
    PathBuf::from(format!(r"\\?\{shown}"))
}

/// The roots the client's index files are keyed by.
fn index_roots(state: &Path) -> Vec<PathBuf> {
    fs::read_dir(state.join("index"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "sqlite"))
        .filter_map(|p| irsync::index::Index::root_of_file(&p).unwrap())
        .collect()
}

/// `irsync host:m/ dst.`: one directory `dst` holds the files, there is no
/// `dst.`, and the index is keyed by `dst`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_typed_destination_with_a_trailing_dot_is_the_directory_windows_names() {
    for tail in [".", " ", ". ."] {
        let h = Harness::new().await;
        fs::write(h.export.path().join("f"), b"pulled").unwrap();
        let parent = tempfile::tempdir().unwrap();
        let typed = PathBuf::from(format!("{}{tail}", parent.path().join("dst").display()));
        // What the command line does with a local operand.
        let root = irsync::platform::local_path(&typed);
        let state = tempfile::tempdir().unwrap();
        let local = Harness::side_for(&root, state.path());
        let out = within30(h.run_side(&archive_opts(), Flow::Pull, local))
            .await
            .unwrap_or_else(|e| panic!("{tail:?}: {e:#}"));
        assert!(out.errors.is_empty(), "{tail:?}: {:?}", out.errors);
        let names: Vec<_> = fs::read_dir(verbatim(parent.path()))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["dst"], "{tail:?}");
        assert_eq!(
            fs::read(parent.path().join("dst").join("f")).unwrap(),
            b"pulled"
        );
        let want = fs::canonicalize(parent.path().join("dst")).unwrap();
        assert_eq!(index_roots(state.path()), [want], "{tail:?}");
    }
}

/// `irsync src. host:m/`: the directory `src` is what is pushed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_typed_source_with_a_trailing_dot_is_the_directory_windows_names() {
    let h = Harness::new().await;
    let parent = tempfile::tempdir().unwrap();
    let src = parent.path().join("src");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("f"), b"pushed").unwrap();
    let typed = PathBuf::from(format!("{}.", src.display()));
    let root = irsync::platform::local_path(&typed);
    let state = tempfile::tempdir().unwrap();
    let local = Harness::side_for(&root, state.path());
    let out = within30(h.run_side(&archive_opts(), Flow::Push, local))
        .await
        .unwrap_or_else(|e| panic!("{e:#}"));
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(fs::read(h.export.path().join("f")).unwrap(), b"pushed");
}
