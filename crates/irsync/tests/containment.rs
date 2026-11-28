//! A hostile sender must not make the receiver write, link or chmod outside
//! its root: listings naming irsync's own temp files and hard links whose
//! targets were not received as regular files are refused.

mod common;

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use common::{archive_opts, Flow, Harness};
use irsync::platform::testutil::{nlink, symlink};
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
    symlink(outside.path(), &h.export.path().join("esc")).unwrap();
    fs::write(h.local.path().join("a"), b"a").unwrap();
    let out = push_with(
        &h,
        b"a",
        vec![entry(b"b", Kind::Hardlink, 11, b"esc/secret")],
    )
    .await;
    assert!(has_error(&out, b"b", "hard link"), "{:?}", out.errors);
    assert!(fs::symlink_metadata(h.export.path().join("b")).is_err());
    assert_eq!(nlink(&outside.path().join("secret")).unwrap(), 1);
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
    hello.req.path = b"m".to_vec();
    hello.req.contents_only = true;
    hello.req.single_file = true;
    hello.req.name = name.clone();
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
    hello.req.single_file = true;
    hello.req.name = name.clone();
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
    use irsync::cli::Args;
    use irsync::options::SyncOptions;
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

/// The containment layer under attack: while each operation runs over and
/// over, another thread swaps the directory `a/b` on its way for a link to
/// a directory outside the root and back, many times a second. Each test
/// goes on until it has seen its operation both succeed and be refused
/// because of the link, so it cannot pass without meeting the attack.
/// Whatever the operation meets, nothing outside is read, written or
/// removed.
mod attack {
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::{self, Read, Write};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use irsync::delete::DeletePass;
    use irsync::fs::paths::RelPath;
    use irsync::fs::safe::{create_dir_all_beneath, open_regular_beneath, Beneath, DirStack};
    use irsync::options::DeleteMode;
    use irsync::platform::testutil::swap_for_link;
    use irsync::platform::{Create, Open};
    use irsync_proto::v1::file_entry::Kind;

    /// Tries of the operation per test at the least, each racing the
    /// swapping thread.
    const MIN_TRIES: usize = 500;
    /// A test that has not met the link by then fails.
    const LIMIT: Duration = Duration::from_secs(20);

    /// What the tries of an operation came to.
    #[derive(Debug, Default)]
    struct Counts {
        ok: usize,
        /// Refused because a component on the way was the link.
        link: usize,
        /// Any other failure: mostly `a/b` missing for a moment.
        other: usize,
    }

    impl Counts {
        /// Counts `r` and hands it on.
        fn note<T>(&mut self, r: io::Result<T>) -> io::Result<T> {
            match &r {
                Ok(_) => self.ok += 1,
                Err(e) if refused_for_link(e) => self.link += 1,
                Err(_) => self.other += 1,
            }
            r
        }
    }

    /// The error of `fs::safe` for a component that is a symlink (here
    /// `a/b` never becomes anything else that is not a directory).
    fn refused_for_link(e: &io::Error) -> bool {
        e.kind() == io::ErrorKind::InvalidInput && e.to_string().contains("symlink")
    }

    /// Every file and directory below `dir`, with each file's bytes.
    fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        fn walk(top: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
            for e in fs::read_dir(dir).unwrap() {
                let path = e.unwrap().path();
                let rel = path.strip_prefix(top).unwrap().to_path_buf();
                let meta = fs::symlink_metadata(&path).unwrap();
                if meta.is_dir() {
                    out.insert(rel, None);
                    walk(top, &path, out);
                } else if meta.is_file() {
                    out.insert(rel, Some(fs::read(&path).unwrap()));
                } else {
                    let target = fs::read_link(&path).unwrap();
                    out.insert(rel, Some(target.as_os_str().as_encoded_bytes().to_vec()));
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(dir, dir, &mut out);
        out
    }

    /// A root with `a/b/f`, `a/b/victim` and `a/b/gone` in it, and a
    /// directory outside with files of the same names (and one more
    /// level), so that any operation led outside would find something to
    /// read or change there.
    struct Tree {
        root: tempfile::TempDir,
        outside: tempfile::TempDir,
    }

    impl Tree {
        fn new() -> Tree {
            let (root, outside) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            fs::create_dir_all(root.path().join("a/b")).unwrap();
            fs::write(root.path().join("a/b/f"), b"inside").unwrap();
            fs::write(root.path().join("a/b/victim"), b"inside victim").unwrap();
            fs::write(root.path().join("a/b/gone"), b"inside gone").unwrap();
            fs::create_dir(outside.path().join("sub")).unwrap();
            fs::write(outside.path().join("f"), b"outside").unwrap();
            fs::write(outside.path().join("victim"), b"keep").unwrap();
            fs::write(outside.path().join("gone"), b"keep too").unwrap();
            fs::write(outside.path().join("sub/deep"), b"deep").unwrap();
            Tree { root, outside }
        }

        fn at(&self, p: &str) -> Beneath {
            Beneath::new(self.root.path(), rel(p))
        }

        /// Runs `op` (try number, counts) while `a/b` is swapped: at least
        /// [`MIN_TRIES`] times, and until the link was put in place, an
        /// operation succeeded and one was refused for the link; fails
        /// after [`LIMIT`] without that. Then checks that the outside
        /// directory is exactly as it was.
        fn attack(&self, mut op: impl FnMut(usize, &mut Counts)) {
            let before = snapshot(self.outside.path());
            let mut n = Counts::default();
            let start = Instant::now();
            let (tries, swaps) = {
                // Dropped at the end of this block, or while a failed
                // assertion unwinds: either way its thread is stopped and
                // joined, with `a/b` back in place.
                let swap = swap_for_link(&self.root.path().join("a/b"), self.outside.path());
                let mut i = 0;
                while i < MIN_TRIES || n.ok == 0 || n.link == 0 || swap.swaps() == 0 {
                    assert!(
                        start.elapsed() < LIMIT,
                        "the attack was not met within {LIMIT:?}: {i} tries, {} swaps, {n:?}",
                        swap.swaps()
                    );
                    op(i, &mut n);
                    i += 1;
                }
                (i, swap.swaps())
            };
            eprintln!("{tries} tries, {swaps} swaps, {n:?}");
            assert_eq!(snapshot(self.outside.path()), before);
            // The swapping thread has put the directory back.
            assert!(fs::symlink_metadata(self.root.path().join("a/b"))
                .unwrap()
                .is_dir());
        }
    }

    fn rel(p: &str) -> RelPath {
        RelPath::new(p.as_bytes()).unwrap()
    }

    fn open(read: bool, write: bool, create: Create, truncate: bool) -> Open {
        Open {
            read,
            write,
            create,
            truncate,
            nonblock: true,
            mode: 0o644,
        }
    }

    #[test]
    fn open_regular_beneath_never_reads_outside() {
        let t = Tree::new();
        t.attack(|_, n| {
            if let Ok(mut f) = n.note(open_regular_beneath(t.root.path(), &rel("a/b/f"))) {
                let mut got = Vec::new();
                f.read_to_end(&mut got).unwrap();
                assert_eq!(got, b"inside", "read through the link");
            }
        });
    }

    #[test]
    fn beneath_open_create_never_writes_outside() {
        let t = Tree::new();
        t.attack(|i, n| {
            let existing = open(false, true, Create::IfMissing, true);
            if let Ok(mut f) = n.note(t.at("a/b/victim").open(existing)) {
                f.write_all(b"written").unwrap();
            }
            let new = open(true, true, Create::New, false);
            if let Ok(mut f) = n.note(t.at(&format!("a/b/new{i}")).open(new)) {
                f.write_all(b"new").unwrap();
            }
        });
    }

    /// Each rename goes one way only and to a name never used again, so a
    /// rename led outside cannot be undone by a later one: a file taken
    /// from the outside directory, or one put into it, stays where it went
    /// and the snapshot shows it.
    #[test]
    fn beneath_unlink_and_rename_never_touch_outside() {
        let t = Tree::new();
        let make = open(false, true, Create::IfMissing, false);
        t.attack(|i, n| {
            // Something to remove, made where it belongs.
            let _ = t.at("a/b/gone").open(make);
            let _ = n.note(t.at("a/b/gone").unlink());
            // Out of `a/b`.
            let _ = t.at("a/b/victim").open(make);
            let _ = n.note(t.at("a/b/victim").rename_to(&t.at(&format!("a/out{i}"))));
            // Into `a/b`.
            let src = format!("a/in{i}");
            fs::write(t.root.path().join(&src), format!("pushed {i}")).unwrap();
            let _ = n.note(t.at(&src).rename_to(&t.at(&format!("a/b/pushed{i}"))));
        });
    }

    #[test]
    fn create_dir_all_beneath_never_creates_outside() {
        let t = Tree::new();
        t.attack(|i, n| {
            let deep = rel(&format!("a/b/n{i}/deep"));
            let _ = n.note(create_dir_all_beneath(t.root.path(), &deep, 0o755));
        });
    }

    #[test]
    fn dir_stack_never_descends_outside() {
        let t = Tree::new();
        let mut want = vec![b"f".to_vec(), b"gone".to_vec(), b"victim".to_vec()];
        want.sort();
        let mut stack = DirStack::new(t.root.path()).unwrap();
        t.attack(|_, n| {
            // Back to the base, so each try opens `a` and `a/b` afresh.
            stack.dir(&RelPath::root()).unwrap();
            if let Ok(d) = n.note(stack.dir(&rel("a/b"))) {
                let mut names = d.list().unwrap();
                names.sort();
                assert_eq!(names, want, "listed through the link");
            }
        });
    }

    /// The receiver's deletion pass over the one candidate `path`, as an
    /// `io::Result`: `Ok` when it was removed; an `InvalidInput` error
    /// when the pass reports a symlink on the way (as `fs::safe` words
    /// it); any other failure, or nothing there to remove, otherwise.
    fn delete(rt: &tokio::runtime::Runtime, root: &Path, path: &str, kind: Kind) -> io::Result<()> {
        let mut pass = DeletePass::new(DeleteMode::After, None, false, false, false);
        pass.plan(rel(path), kind);
        let rep = rt
            .block_on(pass.execute(root.to_path_buf(), false))
            .expect("a complete listing is not refused");
        if !rep.deleted.is_empty() {
            return Ok(());
        }
        Err(match rep.errors.first() {
            Some((_, msg)) if msg.contains("symlink") => {
                io::Error::new(io::ErrorKind::InvalidInput, msg.clone())
            }
            Some((_, msg)) => io::Error::other(msg.clone()),
            None => io::Error::new(io::ErrorKind::NotFound, "nothing was removed"),
        })
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    /// The deleter removes a file below the swapped directory; the file of
    /// the same name outside stays.
    #[test]
    fn delete_remove_one_never_removes_outside() {
        let t = Tree::new();
        let rt = runtime();
        let make = open(false, true, Create::IfMissing, false);
        t.attack(|_, n| {
            // Something to remove, made where it belongs.
            let _ = t.at("a/b/gone").open(make);
            let _ = n.note(delete(&rt, t.root.path(), "a/b/gone", Kind::Regular));
        });
    }

    /// A directory the deleter removes holds only irsync's leftover
    /// temporaries, which it clears first. Outside, a directory of the same
    /// name holds a file named like one of them: it stays.
    ///
    /// What the counts prove: the link refusals counted here happen where
    /// the deleter opens the parent `a/b` (`remove_one`), and the outside
    /// snapshot shows nothing was removed through the link there. What they
    /// do not prove: `clear_temporaries`' own descent into `a/b/c` runs from
    /// the parent already held open, which no swap can change, so no refusal
    /// here comes from it. A `clear_temporaries` that went by path would be
    /// caught only when a swap lands while it runs, after `remove_one` has
    /// opened the real `a/b`: a short window, not met on every run.
    #[test]
    fn clear_temporaries_never_removes_outside() {
        let t = Tree::new();
        fs::create_dir(t.outside.path().join("c")).unwrap();
        fs::write(t.outside.path().join("c/.irsync.x.part"), b"outside part").unwrap();
        fs::write(t.outside.path().join("c/.irsync.y.tmp"), b"outside tmp").unwrap();
        let rt = runtime();
        let make = open(false, true, Create::IfMissing, false);
        t.attack(|_, n| {
            // The leftovers, made where they belong.
            let _ = create_dir_all_beneath(t.root.path(), &rel("a/b/c"), 0o755);
            let _ = t.at("a/b/c/.irsync.x.part").open(make);
            let _ = t.at("a/b/c/.irsync.y.tmp").open(make);
            let _ = n.note(delete(&rt, t.root.path(), "a/b/c", Kind::Directory));
        });
    }
}
