//! The tests of [`Dir`], on every platform: each operation, that none
//! follows a link, that each error has its meaning, and that a directory
//! swapped for a link while operations run below it is never left through.
//! What only one platform has (modes, owners and FIFOs on Unix; junctions,
//! Win32 names and sharing on Windows) is under `cfg` inside a test or in
//! the `windows` module below.

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::*;

const READ: Open = Open {
    read: true,
    write: false,
    create: Create::No,
    truncate: false,
    nonblock: false,
    mode: 0,
};

const NEW: Open = Open {
    read: true,
    write: true,
    create: Create::New,
    truncate: false,
    nonblock: false,
    mode: 0o600,
};

fn sorted(mut names: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    names.sort();
    names
}

/// Whether this test can make symlinks; on Windows it may not (no
/// privilege, no Developer Mode), and then it is skipped, said so, unless
/// `CI` is set, where that fails.
#[cfg(windows)]
fn symlinks() -> bool {
    testutil::symlinks_available()
}

/// Whether this test can make symlinks: always, here.
#[cfg(not(windows))]
fn symlinks() -> bool {
    true
}

#[test]
fn every_operation_takes_one_name_and_follows_no_link() {
    if !symlinks() {
        return;
    }
    let (t, outside) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let root = t.path().join("d");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("f"), b"f").unwrap();
    std::fs::write(root.join("sub/g"), b"g").unwrap();
    std::fs::create_dir(outside.path().join("dir")).unwrap();
    std::fs::write(outside.path().join("f"), b"outside").unwrap();
    testutil::symlink(&outside.path().join("dir"), &root.join("link_to_dir")).unwrap();
    testutil::symlink(&outside.path().join("f"), &root.join("link_to_file")).unwrap();

    let d = Dir::open_root(&root).unwrap();
    assert_eq!(
        sorted(d.list().unwrap()),
        [&b"f"[..], b"link_to_dir", b"link_to_file", b"sub"]
    );
    let e = d.open_dir(b"link_to_dir").unwrap_err();
    assert_eq!(cause(&e), Cause::LinkRefused, "{e}");
    let e = d.open_file(b"link_to_file", READ).unwrap_err();
    assert_eq!(cause(&e), Cause::LinkRefused, "{e}");
    assert_eq!(d.stat(b"link_to_file").unwrap().kind, Kind::Symlink);
    assert_eq!(d.stat(b"f").unwrap().kind, Kind::File);
    assert_eq!(d.stat(b"sub").unwrap().kind, Kind::Dir);
    assert_eq!(d.stat_self().unwrap().kind, Kind::Dir);
    assert_eq!(
        d.read_link(b"link_to_file").unwrap(),
        os_to_bytes(outside.path().join("f").as_os_str())
    );

    // The link goes, what it leads to stays.
    d.remove_file(b"link_to_file").unwrap();
    assert!(std::fs::symlink_metadata(root.join("link_to_file")).is_err());
    assert_eq!(std::fs::read(outside.path().join("f")).unwrap(), b"outside");

    // A rename replaces what is at the new name.
    let sub = d.open_dir(b"sub").unwrap();
    d.rename(b"f", &sub, b"g").unwrap();
    assert!(!root.join("f").exists());
    assert_eq!(std::fs::read(root.join("sub/g")).unwrap(), b"f");
    assert_eq!(sorted(sub.list().unwrap()), [&b"g"[..]]);

    // A second name of the same file.
    d.hard_link(b"h", &sub, b"g").unwrap();
    let (h, g) = (d.stat(b"h").unwrap(), sub.stat(b"g").unwrap());
    assert_eq!((h.nlink, g.nlink), (2, 2));
    assert_eq!((h.ino, h.dev), (g.ino, g.dev));

    // A link's own time changes, not its target's.
    let before = std::fs::metadata(outside.path().join("dir"))
        .unwrap()
        .modified()
        .unwrap();
    d.set_mtime(b"link_to_dir", (1_000_000_000, 5)).unwrap();
    assert_eq!(d.stat(b"link_to_dir").unwrap().mtime.0, 1_000_000_000);
    let after = std::fs::metadata(outside.path().join("dir"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(before, after);

    // The rest of the operations, on names inside.
    d.create_dir(b"new", 0o755).unwrap();
    d.open_dir(b"new").unwrap();
    d.remove_dir(b"new").unwrap();
    d.symlink(b"l", b"sub/g", LinkKind::File).unwrap();
    assert_eq!(d.read_link(b"l").unwrap(), b"sub/g");
    #[cfg(unix)]
    {
        d.set_mode(b"h", 0o640).unwrap();
        assert_eq!(d.stat(b"h").unwrap().mode, 0o640);
        let st = d.stat(b"h").unwrap();
        d.set_owner(b"h", Some(st.uid), Some(st.gid)).unwrap();
        d.make_fifo(b"p", 0o600).unwrap();
        assert_eq!(d.stat(b"p").unwrap().kind, Kind::Fifo);
    }
    #[cfg(windows)]
    {
        let not_supported = |r: io::Result<()>| cause(&r.unwrap_err()) == Cause::NotSupported;
        assert!(not_supported(d.set_mode(b"h", 0o640)));
        assert!(not_supported(d.set_owner(b"h", Some(0), Some(0))));
        assert!(not_supported(d.make_fifo(b"p", 0o600)));
        assert!(not_supported(d.make_device(b"p", Kind::CharDev, 0o600, 0)));
        assert!(d.stat(b"p").is_err());
    }
    let write = Open {
        read: true,
        write: true,
        create: Create::New,
        truncate: false,
        nonblock: false,
        mode: 0o600,
    };
    let file = d.open_file(b"w", write).unwrap();
    assert_eq!(write_at(&file, b"abc", 2).unwrap(), 3);
    let mut buf = [0u8; 5];
    assert_eq!(read_at(&file, &mut buf, 0).unwrap(), 5);
    assert_eq!(&buf, b"\0\0abc");
    let st = fstat(&file).unwrap();
    assert_eq!((st.kind, st.size), (Kind::File, 5));
    assert_eq!(st, d.stat(b"w").unwrap());
}

#[test]
fn list_while_stops_when_told() {
    let t = tempfile::tempdir().unwrap();
    for n in ["a", "b", "c"] {
        std::fs::write(t.path().join(n), n).unwrap();
    }
    let d = Dir::open_root(t.path()).unwrap();
    // Told to stop at the first name: called once.
    let mut calls = 0;
    d.list_while(|_| {
        calls += 1;
        false
    })
    .unwrap();
    assert_eq!(calls, 1);
    // Never told to stop: every name, without `.` and `..`.
    let mut names = Vec::new();
    d.list_while(|n| {
        names.push(n.to_vec());
        true
    })
    .unwrap();
    assert_eq!(sorted(names), [&b"a"[..], b"b", b"c"]);
    // Stopped after the second name: called twice, and a later listing
    // starts from the first name again.
    let mut calls = 0;
    d.list_while(|_| {
        calls += 1;
        calls < 2
    })
    .unwrap();
    assert_eq!(calls, 2);
    assert_eq!(sorted(d.list().unwrap()), [&b"a"[..], b"b", b"c"]);
}

#[test]
fn error_meanings() {
    let t = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(t.path().join("sub")).unwrap();
    std::fs::write(t.path().join("sub/g"), b"g").unwrap();
    std::fs::write(t.path().join("f"), b"f").unwrap();
    let d = Dir::open_root(t.path()).unwrap();
    let cause_of = |r: io::Result<()>| cause(&r.unwrap_err());
    assert_eq!(cause_of(d.open_dir(b"missing").map(drop)), Cause::NotThere);
    assert_eq!(cause_of(d.open_dir(b"f").map(drop)), Cause::NotADirectory);
    assert_eq!(cause_of(d.create_dir(b"sub", 0o755)), Cause::AlreadyThere);
    let new = Open {
        write: true,
        create: Create::New,
        mode: 0o600,
        ..READ
    };
    assert_eq!(
        cause_of(d.open_file(b"f", new).map(drop)),
        Cause::AlreadyThere
    );
    assert_eq!(cause_of(d.remove_dir(b"sub")), Cause::NotEmpty);
    let write = Open {
        read: false,
        write: true,
        ..READ
    };
    assert_eq!(
        cause_of(d.open_file(b"sub", write).map(drop)),
        Cause::IsADirectory
    );
}

#[test]
fn a_name_with_a_separator_or_nul_is_refused() {
    let t = tempfile::tempdir().unwrap();
    std::fs::create_dir(t.path().join("a")).unwrap();
    std::fs::write(t.path().join("a/b"), b"b").unwrap();
    let d = Dir::open_root(t.path()).unwrap();
    let invalid = |r: io::Result<()>| r.unwrap_err().kind() == io::ErrorKind::InvalidInput;
    for bad in [&b"a/b"[..], b"", b".", b"..", b"a\0"] {
        let ops: [(&str, io::Result<()>); 19] = [
            ("open_dir", d.open_dir(bad).map(drop)),
            ("create_dir", d.create_dir(bad, 0o755)),
            ("open_file", d.open_file(bad, READ).map(drop)),
            ("stat", d.stat(bad).map(drop)),
            ("read_link", d.read_link(bad).map(drop)),
            ("remove_file", d.remove_file(bad)),
            ("remove_dir", d.remove_dir(bad)),
            ("rename from", d.rename(bad, &d, b"x")),
            ("rename to", d.rename(b"a", &d, bad)),
            ("hard_link name", d.hard_link(bad, &d, b"a")),
            ("hard_link from", d.hard_link(b"x", &d, bad)),
            ("symlink", d.symlink(bad, b"a", LinkKind::File)),
            ("set_mtime", d.set_mtime(bad, (0, 0))),
            ("set_mode", d.set_mode(bad, 0o644)),
            ("set_owner", d.set_owner(bad, None, None)),
            ("make_fifo", d.make_fifo(bad, 0o644)),
            ("make_device", d.make_device(bad, Kind::Fifo, 0o644, 0)),
            (
                "make_device socket",
                d.make_device(bad, Kind::Socket, 0o644, 0),
            ),
            (
                "make_device char",
                d.make_device(bad, Kind::CharDev, 0o644, 0),
            ),
        ];
        for (op, r) in ops {
            assert!(invalid(r), "{op} with {:?}", String::from_utf8_lossy(bad));
        }
    }
    // Nothing was touched: `a` is still the directory with `b` in it.
    assert_eq!(std::fs::read(t.path().join("a/b")).unwrap(), b"b");
    assert_eq!(sorted(d.list().unwrap()), [&b"a"[..]]);
}

/// A rename onto a link replaces the link itself, as Unix `rename` does,
/// and never touches what it leads to; making anything at a link's name is
/// [`Cause::AlreadyThere`]. This is how the receiver places a file
/// (`rename(tmp, name)`), so a link planted at `name` must not lead it
/// outside.
#[test]
fn a_rename_onto_a_link_replaces_the_link_never_its_target() {
    if !symlinks() {
        return;
    }
    let (t, outside) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::create_dir(outside.path().join("dir")).unwrap();
    std::fs::write(outside.path().join("dir").join("inner"), b"inner").unwrap();
    std::fs::write(outside.path().join("f"), b"outside").unwrap();
    let link = |target: &str, name: &str| {
        testutil::symlink(&outside.path().join(target), &t.path().join(name)).unwrap()
    };
    link("f", "file_link");
    link("dir", "dir_link");
    let before = snapshot(outside.path());
    let d = Dir::open_root(t.path()).unwrap();
    let write = |name: &[u8], bytes: &[u8]| {
        let f = d.open_file(name, NEW).unwrap();
        write_at(&f, bytes, 0).unwrap();
    };
    // A file over a file link and over a directory link: the link goes,
    // the file takes its name.
    for name in [&b"file_link"[..], b"dir_link"] {
        write(b"new", name);
        d.rename(b"new", &d, name).unwrap();
        assert_eq!(d.stat(name).unwrap().kind, Kind::File);
        let mut got = Vec::new();
        d.open_file(name, READ)
            .unwrap()
            .read_to_end(&mut got)
            .unwrap();
        assert_eq!(got, name);
    }
    assert_eq!(snapshot(outside.path()), before);
    // A directory over a file link or a directory link: not a directory.
    link("f", "file_link2");
    link("dir", "dir_link2");
    d.create_dir(b"dir", 0o755).unwrap();
    for name in [&b"file_link2"[..], b"dir_link2"] {
        let e = d.rename(b"dir", &d, name).unwrap_err();
        assert_eq!(cause(&e), Cause::NotADirectory, "{e}");
        assert_eq!(d.stat(name).unwrap().kind, Kind::Symlink);
    }
    // A link moves over a file, which it replaces...
    write(b"plain", b"plain");
    d.rename(b"dir_link2", &d, b"plain").unwrap();
    assert_eq!(d.stat(b"plain").unwrap().kind, Kind::Symlink);
    // ...but a file link never over a directory.
    d.create_dir(b"empty", 0o755).unwrap();
    let e = d.rename(b"file_link2", &d, b"empty").unwrap_err();
    assert_eq!(cause(&e), Cause::IsADirectory, "{e}");
    // Nothing is made at a link's name.
    write(b"src", b"src");
    let already = |r: io::Result<()>| cause(&r.unwrap_err());
    assert_eq!(
        already(d.create_dir(b"file_link2", 0o755)),
        Cause::AlreadyThere
    );
    assert_eq!(
        already(d.symlink(b"file_link2", b"x", LinkKind::File)),
        Cause::AlreadyThere
    );
    assert_eq!(
        already(d.hard_link(b"file_link2", &d, b"src")),
        Cause::AlreadyThere
    );
    assert_eq!(
        already(d.open_file(b"file_link2", NEW).map(drop)),
        Cause::AlreadyThere
    );
    assert_eq!(d.stat(b"file_link2").unwrap().kind, Kind::Symlink);
    assert_eq!(snapshot(outside.path()), before);
}

/// Every file and directory below `dir`, with each file's bytes and each
/// link's target; links are not followed.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn walk(top: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let path = e.unwrap().path();
            let rel = path.strip_prefix(top).unwrap().to_path_buf();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                out.insert(rel, None);
                walk(top, &path, out);
            } else if meta.is_file() {
                out.insert(rel, Some(std::fs::read(&path).unwrap()));
            } else {
                let target = std::fs::read_link(&path).unwrap();
                out.insert(rel, Some(os_to_bytes(target.as_os_str())));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// While a thread swaps `a/b` for a link to a directory outside (a
/// junction on Windows, a symlink elsewhere), opens `a` then `b` through
/// `Dir` again and again and reads, creates, renames and removes below
/// `b`. Nothing outside may change, every read gives the inside bytes, and
/// the attack must be met: at least one swap, one success and one refusal
/// for the link.
#[test]
fn a_directory_swapped_for_a_link_is_never_left_through() {
    const MIN_TRIES: usize = 500;
    const LIMIT: Duration = Duration::from_secs(20);
    // How much of the attack must be met: on Windows (where the swap is a
    // junction made beside the directory and renamed into place) a real
    // number of each; elsewhere at least one, as the attack tests in
    // `tests/containment.rs` require.
    #[cfg(windows)]
    const MIN_SWAPS_OK_REFUSED: (usize, usize, usize) = (100, 100, 20);
    #[cfg(not(windows))]
    const MIN_SWAPS_OK_REFUSED: (usize, usize, usize) = (1, 1, 1);
    let (min_swaps, min_ok, min_refused) = MIN_SWAPS_OK_REFUSED;
    let (t, outside) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::create_dir_all(t.path().join("a/b")).unwrap();
    std::fs::write(t.path().join("a/b/f"), b"inside").unwrap();
    std::fs::write(t.path().join("a/b/victim"), b"inside victim").unwrap();
    std::fs::create_dir(outside.path().join("sub")).unwrap();
    std::fs::write(outside.path().join("f"), b"outside").unwrap();
    std::fs::write(outside.path().join("victim"), b"keep").unwrap();
    std::fs::write(outside.path().join("sub/deep"), b"deep").unwrap();
    let before = snapshot(outside.path());
    let root = Dir::open_root(t.path()).unwrap();
    let (mut ok, mut link, mut other) = (0, 0, 0);
    let start = Instant::now();
    let (tries, swaps) = {
        // Dropped at the end of this block, or while a failed assertion
        // unwinds: its thread stops with `a/b` back in place.
        let swap = testutil::swap_for_link(&t.path().join("a").join("b"), outside.path());
        let mut i = 0;
        while i < MIN_TRIES || ok < min_ok || link < min_refused || swap.swaps() < min_swaps {
            assert!(
                start.elapsed() < LIMIT,
                "the attack was not met within {LIMIT:?}: {i} tries, {} swaps, \
                 {ok} ok, {link} refused, {other} other",
                swap.swaps()
            );
            match root.open_dir(b"a").and_then(|a| a.open_dir(b"b")) {
                Ok(b) => {
                    ok += 1;
                    // `b` is the directory itself, wherever it is now.
                    if let Ok(mut f) = b.open_file(b"f", READ) {
                        let mut got = Vec::new();
                        f.read_to_end(&mut got).unwrap();
                        assert_eq!(got, b"inside", "read through the link");
                    }
                    // Each may fail (the directory may be moved aside
                    // or gone for a moment); none may act outside.
                    let new = format!("new{i}").into_bytes();
                    if b.open_file(&new, NEW).is_ok() {
                        let _ = b.remove_file(&new);
                    }
                    if b.rename(b"victim", &b, b"victim2").is_ok() {
                        // Back, in the directory `b` holds; a scanner may
                        // hold `victim2` for a moment without delete
                        // sharing, so "in use" is waited out, briefly.
                        let mut back = b.rename(b"victim2", &b, b"victim");
                        for _ in 0..100 {
                            match &back {
                                Err(e) if cause(e) == Cause::InUse => {
                                    std::thread::sleep(Duration::from_millis(10));
                                    back = b.rename(b"victim2", &b, b"victim");
                                }
                                _ => break,
                            }
                        }
                        back.unwrap();
                    }
                    if let Ok(st) = b.stat(b"f") {
                        assert_eq!(st.size, 6, "stat through the link");
                    }
                }
                Err(e) if cause(&e) == Cause::LinkRefused => link += 1,
                Err(_) => other += 1,
            }
            i += 1;
        }
        (i, swap.swaps())
    };
    eprintln!("{tries} tries, {swaps} swaps, {ok} ok, {link} refused, {other} other");
    assert_eq!(snapshot(outside.path()), before);
    assert!(std::fs::symlink_metadata(t.path().join("a/b"))
        .unwrap()
        .is_dir());
    assert_eq!(
        std::fs::read(t.path().join("a/b/victim")).unwrap(),
        b"inside victim"
    );
}

#[cfg(windows)]
mod windows;

/// What `ignores_case` says agrees with the directory: where a name is
/// found in another case it never says case is kept apart, and where it is
/// not found it never says case is ignored.
#[test]
fn ignores_case_agrees_with_a_lookup_in_another_case() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("probe")).unwrap();
    let folds = tmp.path().join("PROBE").exists();
    let said = Dir::open_root(tmp.path()).unwrap().ignores_case();
    // macOS always knows, per volume.
    #[cfg(target_os = "macos")]
    assert_eq!(said, Some(folds), "{}", tmp.path().display());
    assert_ne!(
        said,
        Some(!folds),
        "{}: lookup in another case found it: {folds}",
        tmp.path().display()
    );
}
