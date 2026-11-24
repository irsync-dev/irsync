//! `Dir` on Windows: junctions, names the Win32 layer refuses, sharing,
//! long paths, identity and times, symlink targets, and every method once
//! more on its Windows-only edges.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::OpenOptionsExt;

use super::*;

const WRITE_NEW: Open = Open {
    read: false,
    write: true,
    create: Create::New,
    truncate: false,
    nonblock: false,
    mode: 0o600,
};

fn cause_of<T>(r: io::Result<T>) -> Cause {
    match r {
        Ok(_) => panic!("succeeded"),
        Err(e) => cause(&e),
    }
}

/// The WTF-8 bytes of `s`.
fn b(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

/// A root with `outside/` beside it holding `keep` and `sub/deep`.
struct Tree {
    _t: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
}

impl Tree {
    fn new() -> Tree {
        let t = tempfile::tempdir().unwrap();
        let (root, outside) = (t.path().join("root"), t.path().join("outside"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(outside.join("sub")).unwrap();
        std::fs::write(outside.join("keep"), b"keep").unwrap();
        std::fs::write(outside.join("sub").join("deep"), b"deep").unwrap();
        Tree {
            _t: t,
            root,
            outside,
        }
    }

    fn dir(&self) -> Dir {
        Dir::open_root(&self.root).unwrap()
    }

    fn outside_snapshot(&self) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        snapshot(&self.outside)
    }
}

#[test]
fn a_junction_is_refused_like_a_symlink() {
    let t = Tree::new();
    let before = t.outside_snapshot();
    testutil::junction(&t.outside, &t.root.join("j")).unwrap();
    let d = t.dir();
    assert_eq!(cause_of(d.open_dir(b"j")), Cause::LinkRefused);
    assert_eq!(cause_of(d.open_file(b"j", READ)), Cause::LinkRefused);
    assert_eq!(cause_of(d.open_file(b"j", NEW)), Cause::AlreadyThere);
    let if_missing = Open {
        create: Create::IfMissing,
        ..NEW
    };
    assert_eq!(cause_of(d.open_file(b"j", if_missing)), Cause::LinkRefused);
    assert_eq!(d.stat(b"j").unwrap().kind, Kind::Other);
    assert_eq!(
        d.read_link(b"j").unwrap(),
        os_to_bytes(std::path::absolute(&t.outside).unwrap().as_os_str())
    );
    // Not a directory to `rmdir`, as a Unix symlink; `remove_file`
    // removes the junction itself, never what is behind it.
    assert_eq!(cause_of(d.remove_dir(b"j")), Cause::NotADirectory);
    assert_eq!(d.stat(b"j").unwrap().kind, Kind::Other);
    d.remove_file(b"j").unwrap();
    assert_eq!(cause_of(d.stat(b"j")), Cause::NotThere);
    assert_eq!(t.outside_snapshot(), before);
    // A root may be a junction: opened by name, it is followed; opened
    // without following, it is refused.
    testutil::junction(&t.outside, &t.root.join("j")).unwrap();
    let through = Dir::open_root(&t.root.join("j")).unwrap();
    assert!(sorted(through.list().unwrap()).contains(&b("keep")));
    assert_eq!(
        cause_of(Dir::open_root_nofollow(&t.root.join("j"))),
        Cause::LinkRefused
    );
}

#[test]
fn names_win32_refuses_work() {
    let t = Tree::new();
    let d = t.dir();
    let lone = vec![0xED, 0xA0, 0x80]; // the unit 0xD800 alone
    let names = [
        b("CON"),
        b("trail."),
        b("trail "),
        b("NUL.txt"),
        lone.clone(),
    ];
    for n in &names {
        let f = d.open_file(n, NEW).unwrap();
        write_all_at(&f, n, 0).unwrap();
        drop(f);
        let st = d.stat(n).unwrap();
        assert_eq!((st.kind, st.size), (Kind::File, n.len() as u64), "{n:?}");
    }
    assert_eq!(sorted(d.list().unwrap()), sorted(names.to_vec()));
    // The lone surrogate is a real name on disk, as std sees it.
    let on_disk: Vec<_> = std::fs::read_dir(&t.root)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(on_disk.contains(&OsString::from_wide(&[0xD800])));
    for n in &names {
        let mut buf = vec![0; n.len()];
        read_exact_at(&d.open_file(n, READ).unwrap(), &mut buf, 0).unwrap();
        assert_eq!(&buf, n);
        d.remove_file(n).unwrap();
    }
    assert!(d.list().unwrap().is_empty());
    // The same as directories.
    for n in &names {
        d.create_dir(n, 0o755).unwrap();
        d.open_dir(n).unwrap();
        d.remove_dir(n).unwrap();
    }
    assert!(d.list().unwrap().is_empty());
}

#[test]
fn rename_replaces_a_read_only_file_and_one_open_elsewhere() {
    let t = Tree::new();
    let d = t.dir();
    let put = |name: &[u8], bytes: &[u8]| {
        let f = d.open_file(name, NEW).unwrap();
        write_all_at(&f, bytes, 0).unwrap();
    };
    // Read-only destination.
    put(b"src", b"new");
    put(b"ro", b"old");
    let mut perm = std::fs::metadata(t.root.join("ro")).unwrap().permissions();
    perm.set_readonly(true);
    std::fs::set_permissions(t.root.join("ro"), perm).unwrap();
    d.rename(b"src", &d, b"ro").unwrap();
    assert_eq!(std::fs::read(t.root.join("ro")).unwrap(), b"new");
    // Destination held open by another handle that shares delete.
    put(b"src", b"newer");
    let held = OpenOptions::new()
        .read(true)
        .share_mode(7) // read, write, delete
        .open(t.root.join("ro"))
        .unwrap();
    d.rename(b"src", &d, b"ro").unwrap();
    assert_eq!(std::fs::read(t.root.join("ro")).unwrap(), b"newer");
    drop(held);
    // Held without sharing delete: in use, and nothing moved.
    put(b"src", b"newest");
    let held = OpenOptions::new()
        .read(true)
        .share_mode(3) // read, write
        .open(t.root.join("ro"))
        .unwrap();
    assert_eq!(cause_of(d.rename(b"src", &d, b"ro")), Cause::InUse);
    assert_eq!(std::fs::read(t.root.join("ro")).unwrap(), b"newer");
    assert_eq!(std::fs::read(t.root.join("src")).unwrap(), b"newest");
    // And it cannot be removed either.
    assert_eq!(cause_of(d.remove_file(b"ro")), Cause::InUse);
    drop(held);
    // A read-only file is removed as any other.
    let mut perm = std::fs::metadata(t.root.join("ro")).unwrap().permissions();
    perm.set_readonly(true);
    std::fs::set_permissions(t.root.join("ro"), perm).unwrap();
    d.remove_file(b"ro").unwrap();
    // A file irsync has open does not stop another program from removing
    // or replacing it.
    let open = d.open_file(b"src", READ).unwrap();
    std::fs::remove_file(t.root.join("src")).unwrap();
    drop(open);
    assert_eq!(cause_of(d.stat(b"src")), Cause::NotThere);
}

#[test]
fn a_path_deeper_than_260_characters() {
    let t = Tree::new();
    let mut d = t.dir();
    let component = vec![b'd'; 60];
    let mut path = t.root.clone();
    for _ in 0..6 {
        d.create_dir(&component, 0o755).unwrap();
        d = d.open_dir(&component).unwrap();
        path.push(std::str::from_utf8(&component).unwrap());
    }
    assert!(path.as_os_str().len() > 360);
    let f = d.open_file(b"f", NEW).unwrap();
    write_all_at(&f, b"deep", 0).unwrap();
    drop(f);
    assert_eq!(d.stat(b"f").unwrap().size, 4);
    // By path too: the root and the path-taking functions take it.
    let by_path = Dir::open_root(&path).unwrap();
    assert_eq!(by_path.list().unwrap(), [b("f")]);
    assert_eq!(lstat(&path.join("f")).unwrap().size, 4);
    let mut s = String::new();
    open_read_nofollow(&path.join("f"))
        .unwrap()
        .read_to_string(&mut s)
        .unwrap();
    assert_eq!(s, "deep");
}

#[test]
fn file_identity_and_times() {
    let t = Tree::new();
    let d = t.dir();
    drop(d.open_file(b"one", NEW).unwrap());
    d.hard_link(b"two", &d, b"one").unwrap();
    let (one, two) = (d.stat(b"one").unwrap(), d.stat(b"two").unwrap());
    assert_eq!((one.ino, one.dev, one.nlink), (two.ino, two.dev, 2));
    assert_ne!(one.ino, 0);
    assert_eq!(
        (one.mode, one.uid, one.gid, one.rdev, one.sparse),
        (0, 0, 0, 0, false)
    );
    let other = {
        drop(d.open_file(b"three", NEW).unwrap());
        d.stat(b"three").unwrap()
    };
    assert_ne!(other.ino, one.ino);
    assert_eq!(other.dev, one.dev);
    d.set_mtime(b"one", (1_700_000_000, 123_456_700)).unwrap();
    assert_eq!(d.stat(b"two").unwrap().mtime, (1_700_000_000, 123_456_700));
    // Kept in 100 ns steps; the rest is cut off.
    d.set_mtime(b"one", (1_700_000_000, 123_456_789)).unwrap();
    assert_eq!(d.stat(b"one").unwrap().mtime, (1_700_000_000, 123_456_700));
    // Before 1970, and what std reads.
    d.set_mtime(b"one", (-1, 999_999_900)).unwrap();
    assert_eq!(d.stat(b"one").unwrap().mtime, (-1, 999_999_900));
    d.set_mtime(b"one", (1_000_000_000, 0)).unwrap();
    let std_mtime = std::fs::metadata(t.root.join("one"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        std_mtime,
        std::time::UNIX_EPOCH + Duration::from_secs(1_000_000_000)
    );
    // Only the modification time changes.
    let created = std::fs::metadata(t.root.join("one"))
        .unwrap()
        .created()
        .unwrap();
    d.set_mtime(b"one", (1_100_000_000, 0)).unwrap();
    assert_eq!(
        std::fs::metadata(t.root.join("one"))
            .unwrap()
            .created()
            .unwrap(),
        created
    );
    // Times NTFS cannot hold.
    assert_eq!(
        d.set_mtime(b"one", (-11_644_473_601, 0))
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    // A directory has size 0 and its own identity; the change time moves
    // with a change.
    d.create_dir(b"dir", 0).unwrap();
    let st = d.stat(b"dir").unwrap();
    assert_eq!((st.kind, st.size), (Kind::Dir, 0));
    assert_eq!(d.stat_self().unwrap().dev, one.dev);
    let ctime = d.stat(b"one").unwrap().ctime;
    std::thread::sleep(Duration::from_millis(20));
    d.rename(b"three", &d, b"two").unwrap();
    assert!(d.stat(b"one").unwrap().ctime > ctime || d.stat(b"one").unwrap().nlink == 1);
    assert_eq!(d.stat(b"one").unwrap().nlink, 1);
    // fstat of an open file is what stat says of its name.
    let f = d.open_file(b"one", READ).unwrap();
    assert_eq!(fstat(&f).unwrap(), d.stat(b"one").unwrap());
}

#[test]
fn symlinks_keep_their_exact_target() {
    if !testutil::symlinks_available() {
        return;
    }
    let t = Tree::new();
    let d = t.dir();
    let abs = os_to_bytes(t.outside.join("keep").as_os_str());
    let long: Vec<u8> = std::iter::repeat_n(&b"abcdefghi\\"[..], 400)
        .flatten()
        .copied()
        .collect();
    let targets: [(&str, Vec<u8>, LinkKind); 9] = [
        ("relative", b("keep"), LinkKind::File),
        ("relative up", b("..\\outside\\keep"), LinkKind::File),
        ("slashes", b("../outside/sub"), LinkKind::Dir),
        ("absolute", abs.clone(), LinkKind::File),
        ("verbatim", [&b"\\\\?\\"[..], &abs].concat(), LinkKind::File),
        ("unc", b("\\\\server\\share\\x"), LinkKind::File),
        ("long", long, LinkKind::File),
        (
            "lone surrogate",
            vec![b'x', 0xED, 0xA0, 0x80],
            LinkKind::File,
        ),
        ("dir", abs.clone(), LinkKind::Dir),
    ];
    for (i, (what, target, kind)) in targets.iter().enumerate() {
        let name = format!("l{i}").into_bytes();
        d.symlink(&name, target, *kind)
            .unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(&d.read_link(&name).unwrap(), target, "{what}");
        assert_eq!(d.stat(&name).unwrap().kind, Kind::Symlink, "{what}");
        assert_eq!(cause_of(d.open_dir(&name)), Cause::LinkRefused, "{what}");
        assert_eq!(
            cause_of(d.open_file(&name, READ)),
            Cause::LinkRefused,
            "{what}"
        );
        // What std reads back (the substitute name, so only where that
        // is the target as given): the same target.
        if !target.starts_with(b"\\\\") && !target.contains(&b'/') {
            let std_target = std::fs::read_link(t.root.join(format!("l{i}"))).unwrap();
            assert_eq!(os_to_bytes(std_target.as_os_str()), *target, "{what}");
        }
    }
    // A relative link is followed from where it is, by Windows itself.
    assert_eq!(std::fs::read(t.root.join("l1")).unwrap(), b"keep");
    assert!(t.root.join("l2").join("deep").is_file());
    // Not made, and nothing left: a taken name, bytes that are not a
    // name, an empty target, one too long for a reparse point.
    std::fs::write(t.root.join("taken"), b"x").unwrap();
    assert_eq!(
        cause_of(d.symlink(b"taken", b"t", LinkKind::File)),
        Cause::AlreadyThere
    );
    assert_eq!(std::fs::read(t.root.join("taken")).unwrap(), b"x");
    for (target, kind) in [
        (&b"\xff"[..], io::ErrorKind::InvalidInput),
        (b"", io::ErrorKind::InvalidInput),
        (b"a\0b", io::ErrorKind::InvalidInput),
        (&[b'x'; 5000][..], io::ErrorKind::InvalidInput),
    ] {
        assert_eq!(
            d.symlink(b"nope", target, LinkKind::File)
                .unwrap_err()
                .kind(),
            kind
        );
        assert_eq!(cause_of(d.stat(b"nope")), Cause::NotThere);
    }
    // A plain file is no link to read.
    assert_eq!(cause_of(d.read_link(b"taken")), Cause::Other);
}

#[test]
fn remove_file_removes_every_link_and_remove_dir_none() {
    if !testutil::symlinks_available() {
        return;
    }
    let t = Tree::new();
    let before = t.outside_snapshot();
    testutil::symlink(&t.outside.join("keep"), &t.root.join("file_link")).unwrap();
    testutil::symlink(&t.outside.join("sub"), &t.root.join("dir_link")).unwrap();
    testutil::junction(&t.outside.join("sub"), &t.root.join("junction")).unwrap();
    let d = t.dir();
    for name in [&b"file_link"[..], b"dir_link", b"junction"] {
        assert_eq!(
            cause_of(d.remove_dir(name)),
            Cause::NotADirectory,
            "{name:?}"
        );
        assert!(d.stat(name).is_ok());
        d.remove_file(name).unwrap();
        assert_eq!(cause_of(d.stat(name)), Cause::NotThere, "{name:?}");
    }
    assert_eq!(t.outside_snapshot(), before);
    // A plain directory is not `remove_file`'s, a file not `remove_dir`'s.
    d.create_dir(b"dir", 0).unwrap();
    drop(d.open_file(b"file", NEW).unwrap());
    assert_eq!(cause_of(d.remove_file(b"dir")), Cause::IsADirectory);
    assert_eq!(cause_of(d.remove_dir(b"file")), Cause::NotADirectory);
    assert_eq!(cause_of(d.remove_file(b"missing")), Cause::NotThere);
    assert_eq!(cause_of(d.remove_dir(b"missing")), Cause::NotThere);
    d.remove_dir(b"dir").unwrap();
    d.remove_file(b"file").unwrap();
}

#[test]
fn rename_of_and_over_directories() {
    let t = Tree::new();
    let d = t.dir();
    d.create_dir(b"a", 0).unwrap();
    drop(d.open_dir(b"a").unwrap().open_file(b"in", NEW).unwrap());
    // A directory moves, with what is in it, into another `Dir` too.
    d.rename(b"a", &d, b"b").unwrap();
    d.create_dir(b"c", 0).unwrap();
    let c = d.open_dir(b"c").unwrap();
    d.rename(b"b", &c, b"b").unwrap();
    assert_eq!(c.open_dir(b"b").unwrap().list().unwrap(), [b("in")]);
    // As Unix `rename`: a directory replaces an empty directory, never one
    // with entries; a file never replaces a directory, nor a directory a
    // file.
    d.create_dir(b"empty", 0).unwrap();
    d.create_dir(b"full", 0).unwrap();
    drop(d.open_dir(b"full").unwrap().open_file(b"x", NEW).unwrap());
    drop(d.open_file(b"file", NEW).unwrap());
    assert_eq!(cause_of(c.rename(b"b", &d, b"full")), Cause::NotEmpty);
    assert_eq!(
        cause_of(d.rename(b"file", &d, b"empty")),
        Cause::IsADirectory
    );
    assert_eq!(
        cause_of(d.rename(b"empty", &d, b"file")),
        Cause::NotADirectory
    );
    assert_eq!(c.open_dir(b"b").unwrap().list().unwrap(), [b("in")]);
    assert_eq!(d.open_dir(b"full").unwrap().list().unwrap(), [b("x")]);
    assert_eq!(d.stat(b"file").unwrap().kind, Kind::File);
    c.rename(b"b", &d, b"empty").unwrap();
    assert_eq!(d.open_dir(b"empty").unwrap().list().unwrap(), [b("in")]);
    assert_eq!(cause_of(c.stat(b"b")), Cause::NotThere);
    // A missing source; a link is moved itself.
    assert_eq!(cause_of(d.rename(b"missing", &d, b"x")), Cause::NotThere);
    testutil::junction(&t.outside, &t.root.join("j")).unwrap();
    d.rename(b"j", &c, b"j2").unwrap();
    assert_eq!(c.stat(b"j2").unwrap().kind, Kind::Other);
    assert!(t.outside.join("keep").is_file());
}

#[test]
fn bad_names_are_refused_and_long_ones_end_at_255_units() {
    let t = Tree::new();
    let d = t.dir();
    for bad in [&b"a\\b"[..], b"\\", b"\xff", b"\xed\xa0\xbd\xed\xb8\x80"] {
        assert_eq!(
            d.open_file(bad, NEW).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            d.create_dir(bad, 0).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(d.stat(bad).unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            d.rename(b"x", &d, bad).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
    // 255 units (NTFS's limit), some of them outside the BMP or lone.
    let mut longest = "é".repeat(253).into_bytes();
    longest.extend_from_slice(&[0xED, 0xA0, 0x80, b'x']);
    let f = d.open_file(&longest, NEW).unwrap();
    drop(f);
    assert_eq!(d.list().unwrap(), [longest.clone()]);
    let mut too_long = longest.clone();
    too_long.push(b'y');
    assert!(d.open_file(&too_long, NEW).is_err());
    assert!(d.create_dir(&too_long, 0).is_err());
    assert_eq!(d.list().unwrap(), [longest]);
}

#[test]
fn a_colon_names_a_stream_of_a_file_in_this_directory() {
    if !testutil::symlinks_available() {
        return;
    }
    let t = Tree::new();
    let before = t.outside_snapshot();
    let d = t.dir();
    // `a:s` is the stream `s` of the file `a`, here.
    let f = d.open_file(b"a:s", NEW).unwrap();
    write_all_at(&f, b"stream", 0).unwrap();
    drop(f);
    assert_eq!(d.list().unwrap(), [b("a")]);
    assert_eq!(d.stat(b"a").unwrap().size, 0);
    let mut buf = [0; 6];
    read_exact_at(&d.open_file(b"a:s", READ).unwrap(), &mut buf, 0).unwrap();
    assert_eq!(&buf, b"stream");
    // On a link, a stream is the link's own, never its target's: the open
    // is refused as for the link, and outside nothing changes.
    testutil::symlink(&t.outside.join("keep"), &t.root.join("fl")).unwrap();
    testutil::symlink(&t.outside.join("sub"), &t.root.join("dl")).unwrap();
    testutil::junction(&t.outside.join("sub"), &t.root.join("j")).unwrap();
    for link in ["fl", "dl", "j"] {
        for (name, how) in [(format!("{link}:s"), NEW), (format!("{link}::$DATA"), READ)] {
            assert!(d.open_file(name.as_bytes(), how).is_err(), "{name}");
        }
        assert!(d
            .open_dir(format!("{link}:$I30:$INDEX_ALLOCATION").as_bytes())
            .is_err());
    }
    assert_eq!(t.outside_snapshot(), before);
    let mut streams = Vec::new();
    for p in [t.outside.join("keep"), t.outside.join("sub")] {
        // A stream the open would have made on the target.
        let mut s = p.into_os_string();
        s.push(":s");
        streams.push(std::fs::metadata(&s).is_ok());
    }
    assert_eq!(streams, [false, false]);
}

#[test]
fn hard_link_links_a_link_itself() {
    if !testutil::symlinks_available() {
        return;
    }
    let t = Tree::new();
    let d = t.dir();
    testutil::symlink(&t.outside.join("keep"), &t.root.join("fl")).unwrap();
    testutil::junction(&t.outside.join("sub"), &t.root.join("j")).unwrap();
    d.hard_link(b"fl2", &d, b"fl").unwrap();
    let (a, b2) = (d.stat(b"fl").unwrap(), d.stat(b"fl2").unwrap());
    assert_eq!(
        (a.kind, b2.kind, a.ino, b2.nlink),
        (Kind::Symlink, Kind::Symlink, b2.ino, 2)
    );
    assert_eq!(std::fs::metadata(t.outside.join("keep")).unwrap().len(), 4);
    // A directory link (a junction) and a directory are refused.
    assert_eq!(cause_of(d.hard_link(b"j2", &d, b"j")), Cause::IsADirectory);
    d.create_dir(b"dir", 0).unwrap();
    assert_eq!(
        cause_of(d.hard_link(b"dir2", &d, b"dir")),
        Cause::IsADirectory
    );
    // An existing name is not replaced; a missing source is not there.
    drop(d.open_file(b"f", NEW).unwrap());
    assert_eq!(cause_of(d.hard_link(b"fl2", &d, b"f")), Cause::AlreadyThere);
    assert_eq!(cause_of(d.hard_link(b"x", &d, b"missing")), Cause::NotThere);
}

#[test]
fn open_file_never_changes_a_link_or_its_target() {
    if !testutil::symlinks_available() {
        return;
    }
    let t = Tree::new();
    let before = t.outside_snapshot();
    testutil::symlink(&t.outside.join("keep"), &t.root.join("fl")).unwrap();
    testutil::symlink(&t.outside.join("sub"), &t.root.join("dl")).unwrap();
    let d = t.dir();
    let every = [
        READ,
        Open {
            write: true,
            truncate: true,
            ..READ
        },
        Open {
            write: true,
            create: Create::IfMissing,
            truncate: true,
            ..READ
        },
    ];
    for how in every {
        for link in [&b"fl"[..], b"dl"] {
            assert_eq!(
                cause_of(d.open_file(link, how)),
                Cause::LinkRefused,
                "{how:?}"
            );
        }
    }
    assert_eq!(cause_of(d.open_file(b"fl", WRITE_NEW)), Cause::AlreadyThere);
    assert_eq!(
        d.read_link(b"fl").unwrap(),
        os_to_bytes(t.outside.join("keep").as_os_str())
    );
    assert_eq!(t.outside_snapshot(), before);
    // A truncate of a file: by handle, after the checks.
    let f = d.open_file(b"f", NEW).unwrap();
    write_all_at(&f, b"0123456789", 0).unwrap();
    drop(f);
    let f = d
        .open_file(
            b"f",
            Open {
                write: true,
                truncate: true,
                ..READ
            },
        )
        .unwrap();
    assert_eq!(fstat(&f).unwrap().size, 0);
}

#[test]
fn roots_and_paths() {
    let t = Tree::new();
    std::fs::write(t.root.join("f"), b"f").unwrap();
    testutil::junction(&t.outside, &t.root.join("j")).unwrap();
    // A root that is a file.
    assert_eq!(
        cause_of(Dir::open_root(&t.root.join("f"))),
        Cause::NotADirectory
    );
    assert_eq!(
        cause_of(Dir::open_root_nofollow(&t.root.join("f"))),
        Cause::NotADirectory
    );
    assert_eq!(
        cause_of(Dir::open_root(&t.root.join("missing"))),
        Cause::NotThere
    );
    Dir::open_root_nofollow(&t.root).unwrap();
    // `lstat` describes a final link; `open_read_nofollow` refuses one.
    assert_eq!(lstat(&t.root.join("j")).unwrap().kind, Kind::Other);
    assert_eq!(lstat(&t.root).unwrap().kind, Kind::Dir);
    assert_eq!(
        lstat(&t.root.join("f")).unwrap(),
        t.dir().stat(b"f").unwrap()
    );
    assert_eq!(
        cause_of(open_read_nofollow(&t.root.join("j"))),
        Cause::LinkRefused
    );
    assert_eq!(
        cause_of(open_read_nofollow(&t.root.join("missing"))),
        Cause::NotThere
    );
    // Links before the final name are followed.
    assert_eq!(lstat(&t.root.join("j").join("keep")).unwrap().size, 4);
    let mut s = String::new();
    open_read_nofollow(&t.root.join("j").join("keep"))
        .unwrap()
        .read_to_string(&mut s)
        .unwrap();
    assert_eq!(s, "keep");
    // A directory cannot be opened as a file.
    assert_eq!(cause_of(open_read_nofollow(&t.root)), Cause::IsADirectory);
    if testutil::symlinks_available() {
        testutil::symlink(&t.outside.join("keep"), &t.root.join("fl")).unwrap();
        assert_eq!(lstat(&t.root.join("fl")).unwrap().kind, Kind::Symlink);
        assert_eq!(
            cause_of(open_read_nofollow(&t.root.join("fl"))),
            Cause::LinkRefused
        );
        testutil::symlink(&t.outside, &t.root.join("dl")).unwrap();
        assert_eq!(
            cause_of(Dir::open_root_nofollow(&t.root.join("dl"))),
            Cause::LinkRefused
        );
        assert!(Dir::open_root(&t.root.join("dl"))
            .unwrap()
            .stat(b"keep")
            .is_ok());
    }
}

#[test]
fn positional_reads_and_writes() {
    let t = Tree::new();
    let d = t.dir();
    let f = d.open_file(b"f", NEW).unwrap();
    write_all_at(&f, b"hello world", 3).unwrap();
    let mut buf = [0; 5];
    read_exact_at(&f, &mut buf, 9).unwrap();
    assert_eq!(&buf, b"world");
    let mut past = [0; 5];
    assert_eq!(
        read_exact_at(&f, &mut past, 10).unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
    assert_eq!(read_at(&f, &mut past, 100).unwrap(), 0);
    assert_eq!(fstat(&f).unwrap().size, 14);
    // Opened again: the bytes are there for another handle.
    let mut all = String::new();
    d.open_file(b"f", READ)
        .unwrap()
        .read_to_string(&mut all)
        .unwrap();
    assert_eq!(all, "\0\0\0hello world");
    // A file for reading only cannot be written.
    assert!(write_at(&d.open_file(b"f", READ).unwrap(), b"x", 0).is_err());
}

#[test]
fn list_restarts_and_sees_many_names() {
    let t = Tree::new();
    let d = t.dir();
    // More names than one read of the directory returns.
    let names: Vec<Vec<u8>> = (0..3000)
        .map(|i| format!("name-{i:05}-{}", "x".repeat(40)).into_bytes())
        .collect();
    for n in &names {
        drop(d.open_file(n, NEW).unwrap());
    }
    assert_eq!(sorted(d.list().unwrap()), names);
    let mut first = Vec::new();
    d.list_while(|n| {
        first.push(n.to_vec());
        first.len() < 10
    })
    .unwrap();
    assert_eq!(first.len(), 10);
    let mut again = Vec::new();
    d.list_while(|n| {
        again.push(n.to_vec());
        again.len() < 10
    })
    .unwrap();
    assert_eq!(first, again);
    assert_eq!(d.list().unwrap().len(), 3000);
}
