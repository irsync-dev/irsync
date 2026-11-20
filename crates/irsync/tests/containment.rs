//! A hostile sender must not make the receiver write, link or chmod outside
//! its root: listings naming irsync's own temp files and hard links whose
//! targets were not received as regular files are refused.

mod common;

use std::fs;
use std::os::unix::fs::{symlink, MetadataExt};
use std::sync::Arc;
use std::time::Duration;

use common::{archive_opts, Flow, Harness};
use irsync_proto::v1::{file_entry::Kind, FileEntry};

async fn within30<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(30), f)
        .await
        .expect("test timed out after 30 s")
}

fn entry(path: &[u8], kind: Kind, size: u64, link_target: &[u8]) -> FileEntry {
    let mode = match kind {
        Kind::Symlink => 0o120777,
        Kind::Directory => 0o040755,
        _ => 0o100644,
    };
    FileEntry {
        path: path.to_vec(),
        kind: kind as i32,
        size,
        mtime_sec: 1_600_000_000,
        mode,
        nlink: 2,
        link_target: link_target.to_vec(),
        ..FileEntry::default()
    }
}

/// Pushes the local tree with `inject` adding entries after the entry at `after`.
async fn push_with(
    h: &Harness,
    after: &'static [u8],
    extra: Vec<FileEntry>,
) -> irsync::session::Outcome {
    let mut local = h.local_side();
    local.hooks.inject_listing = Some(Arc::new(move |e: &FileEntry| {
        if e.path == after {
            extra.clone()
        } else {
            Vec::new()
        }
    }));
    within30(h.run_side(&archive_opts(), Flow::Push, local))
        .await
        .expect("session")
}

fn has_error(out: &irsync::session::Outcome, path: &[u8], needle: &str) -> bool {
    out.errors
        .iter()
        .any(|(p, m)| p.as_bytes() == path && m.contains(needle))
}

#[tokio::test]
async fn peer_listing_temp_name_is_rejected() {
    let h = Harness::new().await;
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("victim");
    fs::write(&victim, b"keep me").unwrap();
    fs::write(h.local.path().join("x"), b"real content").unwrap();
    // A symlink named like the receiver's `.part` for `x`, listed before
    // `x`: if accepted, the writer for `x` would write through it.
    let target = victim.as_os_str().as_encoded_bytes().to_vec();
    let out = push_with(
        &h,
        b"",
        vec![
            entry(b".irsync.x.part", Kind::Symlink, 0, &target),
            entry(b".irsync.y.tmp", Kind::Regular, 3, b""),
        ],
    )
    .await;
    assert!(
        has_error(&out, b".irsync.x.part", "reserved"),
        "{:?}",
        out.errors
    );
    assert!(has_error(&out, b".irsync.y.tmp", "reserved"));
    assert!(fs::symlink_metadata(h.export.path().join(".irsync.x.part")).is_err());
    assert!(fs::symlink_metadata(h.export.path().join(".irsync.y.tmp")).is_err());
    assert_eq!(
        fs::read(h.export.path().join("x")).unwrap(),
        b"real content"
    );
    assert_eq!(fs::read(&victim).unwrap(), b"keep me");
    assert_eq!(out.exit_code(), 23);
}

#[tokio::test]
async fn hardlink_target_outside_root_rejected() {
    let h = Harness::new().await;
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret"), b"private key").unwrap();
    // A symlink inside the export leading out of it.
    symlink(outside.path(), h.export.path().join("esc")).unwrap();
    fs::write(h.local.path().join("a"), b"a").unwrap();
    let out = push_with(
        &h,
        b"a",
        vec![entry(b"b", Kind::Hardlink, 11, b"esc/secret")],
    )
    .await;
    assert!(has_error(&out, b"b", "hard link"), "{:?}", out.errors);
    assert!(fs::symlink_metadata(h.export.path().join("b")).is_err());
    assert_eq!(
        fs::metadata(outside.path().join("secret")).unwrap().nlink(),
        1
    );
}

#[tokio::test]
async fn hardlink_to_failed_target_rejected() {
    let h = Harness::new().await;
    // Stale content at the target path on the receiver.
    fs::create_dir(h.export.path().join("zz")).unwrap();
    fs::write(h.export.path().join("zz/t"), b"stale").unwrap();
    fs::write(h.local.path().join("a"), b"a").unwrap();
    // `zz/t` is listed without its parent directory (a planner error), then
    // a link to it.
    let out = push_with(
        &h,
        b"a",
        vec![
            entry(b"zz/t", Kind::Regular, 5, b""),
            entry(b"zzz", Kind::Hardlink, 5, b"zz/t"),
        ],
    )
    .await;
    assert!(has_error(&out, b"zz/t", "parent"), "{:?}", out.errors);
    assert!(has_error(&out, b"zzz", "hard link"), "{:?}", out.errors);
    assert!(fs::symlink_metadata(h.export.path().join("zzz")).is_err());
    assert_eq!(fs::read(h.export.path().join("zz/t")).unwrap(), b"stale");
}

/// A single-file session is for one file. A sender that lists more must
/// not get anything else placed in the directory that holds it.
#[tokio::test]
async fn single_file_session_takes_only_its_file() {
    let h = Harness::new().await;
    let outside = tempfile::tempdir().unwrap();
    let src = h.local.path().join("one");
    fs::write(&src, b"the one file").unwrap();
    fs::write(h.export.path().join("kept"), b"already here").unwrap();

    let (parent, name) = irsync::session::single::split_last(&src);
    let mut hello = Harness::hello(Flow::Push);
    hello.path = b"m".to_vec();
    hello.contents_only = true;
    hello.single_file = true;
    hello.name = name.clone();
    let mut local = Harness::side_for(&parent, h.client_state.path()).with_single(name);
    local.contents_only = false;
    let target = outside.path().as_os_str().as_encoded_bytes().to_vec();
    local.hooks.inject_listing = Some(Arc::new(move |e: &FileEntry| match &e.path[..] {
        // Listed in order around the real entry, so the listing is valid.
        b"" => vec![
            entry(b"extra", Kind::Symlink, 0, &target),
            entry(b"kept", Kind::Symlink, 0, &target),
        ],
        b"one" => vec![
            entry(b"zdir", Kind::Directory, 0, b""),
            entry(b"zdir/link", Kind::Symlink, 0, &target),
        ],
        _ => Vec::new(),
    }));
    let out = within30(h.run_hello(&archive_opts(), hello, local))
        .await
        .expect("session");

    let export = h.export.path();
    assert_eq!(fs::read(export.join("one")).unwrap(), b"the one file");
    for extra in ["extra", "zdir", "zdir/link"] {
        assert!(
            fs::symlink_metadata(export.join(extra)).is_err(),
            "{extra} was placed"
        );
        assert!(
            has_error(&out, extra.as_bytes(), "single file"),
            "{extra}: {:?}",
            out.errors
        );
    }
    // A file that was there is not replaced either.
    assert_eq!(fs::read(export.join("kept")).unwrap(), b"already here");
    assert!(has_error(&out, b"kept", "single file"), "{:?}", out.errors);
    assert_eq!(out.exit_code(), 23);
}

/// The one name of a single-file session is never made a directory.
#[tokio::test]
async fn single_file_session_takes_no_directory_under_its_name() {
    let h = Harness::new().await;
    // The file is gone from the source, so the honest listing has only the
    // root; the directory under the file's name is the sender's invention.
    let src = h.local.path().join("one");
    fs::write(h.export.path().join("one"), b"already here").unwrap();

    let (parent, name) = irsync::session::single::split_last(&src);
    let mut hello = Harness::hello(Flow::Push);
    hello.single_file = true;
    hello.name = name.clone();
    let mut local = Harness::side_for(&parent, h.client_state.path()).with_single(name);
    local.contents_only = false;
    local.hooks.inject_listing = Some(Arc::new(|e: &FileEntry| {
        if e.path.is_empty() {
            vec![entry(b"one", Kind::Directory, 0, b"")]
        } else {
            Vec::new()
        }
    }));
    let out = within30(h.run_hello(&archive_opts(), hello, local))
        .await
        .expect("session");
    assert!(has_error(&out, b"one", "single file"), "{:?}", out.errors);
    assert_eq!(
        fs::read(h.export.path().join("one")).unwrap(),
        b"already here"
    );
}

/// The receiver holds the same exclude rules as the sender. A sender that
/// lists an excluded path anyway must not get it placed: the user said
/// those paths are not part of the sync, and an excluded file that is
/// already in the destination must stay as it is.
#[tokio::test]
async fn excluded_paths_in_the_peers_listing_are_refused() {
    use irsync::cli::{Args, SyncOptions};
    let h = Harness::new().await;
    let outside = tempfile::tempdir().unwrap();
    fs::write(h.local.path().join("real.txt"), b"real content").unwrap();
    fs::write(h.export.path().join("keep.o"), b"mine").unwrap();

    let opts = SyncOptions::from_args(
        &Args::try_parse_from([
            "irsync",
            "-a",
            "--exclude",
            "*.o",
            "--exclude",
            "build/",
            "src/",
            "host:m/",
        ])
        .unwrap(),
    );
    let mut local = h.local_side();
    local.filters = Arc::new(irsync::filter::FilterSet::from_rules(&opts.filters).unwrap());
    let target = outside.path().as_os_str().as_encoded_bytes().to_vec();
    local.hooks.inject_listing = Some(Arc::new(move |e: &FileEntry| {
        if e.path.is_empty() {
            vec![
                entry(b"build", Kind::Directory, 0, b""),
                entry(b"build/in", Kind::Symlink, 0, &target),
                entry(b"evil.o", Kind::Symlink, 0, &target),
                entry(b"keep.o", Kind::Symlink, 0, &target),
            ]
        } else {
            Vec::new()
        }
    }));
    let out = within30(h.run_side(&opts, Flow::Push, local))
        .await
        .expect("session");

    let export = h.export.path();
    assert_eq!(fs::read(export.join("real.txt")).unwrap(), b"real content");
    for gone in ["build", "build/in", "evil.o"] {
        assert!(
            fs::symlink_metadata(export.join(gone)).is_err(),
            "{gone} was placed"
        );
    }
    assert_eq!(fs::read(export.join("keep.o")).unwrap(), b"mine");
    for refused in ["build", "evil.o", "keep.o"] {
        assert!(
            has_error(&out, refused.as_bytes(), "excluded"),
            "{refused}: {:?}",
            out.errors
        );
    }
    assert_eq!(out.exit_code(), 23);
}
