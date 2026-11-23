//! `fileset list`: what a manifest holds, one line per entry.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use fileset::compare::Tier;
use fileset::engine::list::{list, ListOpts};
use fileset::engine::Outcome;
use fileset::platform;
use fileset::store::Backend;
use tempfile::TempDir;

/// A tree with every interesting neighbour: a directory, siblings that sort between it
/// and its contents, a symlink and a fifo.
fn files() -> TempDir {
    let d = tree(&[
        ("2024/a.jpg", "aaa"),
        ("2024/b.jpg", "bb"),
        ("2024-old/x", "x"),
        ("2024.txt", "t"),
        ("top.jpg", "j"),
    ]);
    symlink(d.path(), "latest", "2024/a.jpg");
    mkfifo(&d.path().join("pipe"));
    d
}

/// [`files`] with a manifest for `backend` inside it.
fn fixture(backend: Backend) -> TempDir {
    let d = files();
    run_update(d.path(), backend, false, false);
    d
}

fn opts(root: &Path, pattern: Option<&[u8]>, verbose: bool) -> ListOpts {
    ListOpts {
        root: root.to_path_buf(),
        manifest: None,
        pattern: pattern.map(<[u8]>::to_vec),
        verbose,
    }
}

fn run(o: &ListOpts) -> String {
    let mut out = Vec::new();
    assert_eq!(list(o, &mut out).unwrap(), Outcome::Clean);
    String::from_utf8(out).unwrap()
}

/// The first word of each line: the path, for fixture paths (they hold no space).
fn paths(listing: &str) -> Vec<&str> {
    listing
        .lines()
        .map(|l| l.split(' ').next().unwrap())
        .collect()
}

fn selected(root: &Path, pattern: &str) -> Vec<String> {
    paths(&run(&opts(root, Some(pattern.as_bytes()), false)))
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[test]
fn lists_every_entry_in_stored_order() {
    let d = fixture(Backend::Sqlite);
    let out = run(&opts(d.path(), None, false));
    assert_eq!(
        paths(&out),
        [
            ".",
            ".fileset.sqlite",
            "2024",
            "2024-old",
            "2024-old/x",
            "2024.txt",
            "2024/a.jpg",
            "2024/b.jpg",
            "latest",
            "pipe",
            "top.jpg"
        ]
    );
    assert!(out.ends_with('\n'));
}

/// One tree, a manifest of each backend beside it: the two listings are the same text.
#[test]
fn both_backends_print_the_same_listing() {
    let d = files();
    let side = tempfile::tempdir().unwrap();
    let mut listings = Vec::new();
    for (backend, name) in [(Backend::Sqlite, "m.sqlite"), (Backend::Fjall, "m.fjall")] {
        let manifest = side.path().join(name);
        let mut u = update_opts(d.path(), backend, false, false);
        u.common.manifest = Some(manifest.clone());
        run_update_opts(&u).unwrap();
        let mut o = opts(d.path(), None, true);
        o.manifest = Some(manifest);
        listings.push(run(&o));
    }
    assert_eq!(listings[0], listings[1]);
    assert_eq!(paths(&listings[0]).len(), 10);
}

#[test]
fn a_file_line_equals_the_check_verbose_line_without_its_prefix() {
    let d = fixture(Backend::Sqlite);
    let mut c = check_opts(d.path(), Tier::Default, &[], &[], false);
    c.common.verbose = true;
    let (_, checked, _) = run_check_opts(&c).unwrap();
    let eql = checked
        .lines()
        .find(|l| l.starts_with("EQL 2024/a.jpg "))
        .expect("an EQL line for 2024/a.jpg");
    let out = run(&opts(d.path(), None, false));
    assert!(
        out.lines().any(|l| l == &eql["EQL ".len()..]),
        "{eql}\n{out}"
    );
}

#[test]
fn a_symlink_shows_its_target_and_a_directory_no_checksum() {
    let d = fixture(Backend::Sqlite);
    let out = run(&opts(d.path(), None, false));
    let line = |path: &str| {
        out.lines()
            .find(|l| l.split(' ').next() == Some(path))
            .unwrap()
    };
    assert!(line("latest").starts_with("latest kind=symlink "));
    assert!(line("latest").ends_with(" target=2024/a.jpg"));
    assert!(line("2024").starts_with("2024 kind=dir "));
    assert!(!line("2024").contains("blake3="));
    assert!(line("pipe").starts_with("pipe kind=fifo "));
    assert!(line("top.jpg").contains(" blake3="));
    assert!(!line("top.jpg").contains("uid="));
}

#[test]
fn a_pattern_selects_by_whole_path() {
    let d = fixture(Backend::Sqlite);
    let r = d.path();
    assert_eq!(selected(r, "*.jpg"), ["top.jpg"]);
    assert_eq!(selected(r, "**/*.jpg"), ["2024/a.jpg", "2024/b.jpg"]);
    assert_eq!(selected(r, "2024/**"), ["2024/a.jpg", "2024/b.jpg"]);
    assert_eq!(selected(r, "2024"), ["2024"]);
    assert_eq!(selected(r, "2024/"), ["2024"]);
    assert_eq!(selected(r, "./2024"), ["2024"]);
    assert_eq!(selected(r, "2024*"), ["2024", "2024-old", "2024.txt"]);
    assert!(selected(r, "nope*").is_empty());
    assert!(selected(r, "").is_empty());
}

#[test]
fn verbose_adds_owner_links_and_attributes() {
    let d = files();
    #[cfg(unix)]
    let with_attr =
        platform::testutil::set_xattr(&d.path().join("top.jpg"), "user.fileset.test", b"v").is_ok();
    #[cfg(not(unix))]
    let with_attr = false;
    run_update(d.path(), Backend::Sqlite, false, false);
    let out = run(&opts(d.path(), None, true));
    for line in out.lines() {
        assert!(
            line.contains(" uid=") && line.contains(" gid=") && line.contains(" nlink="),
            "{line}"
        );
        assert!(!line.contains("rdev="), "{line}");
    }
    let brief = run(&opts(d.path(), None, false));
    for (v, b) in out.lines().zip(brief.lines()) {
        assert!(v.starts_with(b), "{v}\n{b}");
    }
    if with_attr {
        let top = out.lines().find(|l| l.starts_with("top.jpg ")).unwrap();
        assert!(top.contains(" xattr:user.fileset.test=v"), "{top}");
        assert!(!brief.contains("xattr:"), "{brief}");
    } else {
        eprintln!("skipped the attribute part: this filesystem takes no user attributes");
    }
}

#[test]
fn a_copied_manifest_lists_without_its_tree() {
    let d = fixture(Backend::Sqlite);
    let here = run(&opts(d.path(), None, false));
    let side = tempfile::tempdir().unwrap();
    let copy = side.path().join("copy.sqlite");
    std::fs::copy(d.path().join(".fileset.sqlite"), &copy).unwrap();
    let mut o = opts(&PathBuf::from("/nonexistent/fileset/tree"), None, false);
    o.manifest = Some(copy);
    assert_eq!(run(&o), here);
}

#[test]
fn a_path_that_is_not_utf8_lists_escaped_and_matches_by_bytes() {
    let d = files();
    let name = platform::bytes_to_os(b"x\xffy").unwrap();
    if std::fs::write(d.path().join(name), "z").is_err() {
        eprintln!("skipped: this filesystem refuses names that are not UTF-8");
        return;
    }
    run_update(d.path(), Backend::Sqlite, false, false);
    let all = run(&opts(d.path(), None, false));
    assert!(
        all.lines().any(|l| l.starts_with("x\\xffy kind=file ")),
        "{all}"
    );
    let one = run(&opts(d.path(), Some(b"x\xffy"), false));
    assert_eq!(paths(&one), ["x\\xffy"]);
    let any = run(&opts(d.path(), Some(b"x?y"), false));
    assert_eq!(paths(&any), ["x\\xffy"]);
}

#[test]
fn list_leaves_the_manifest_file_untouched() {
    let d = fixture(Backend::Sqlite);
    let m = d.path().join(".fileset.sqlite");
    let before = (
        std::fs::read(&m).unwrap(),
        m.metadata().unwrap().modified().unwrap(),
    );
    let names = |dir: &Path| {
        let mut v: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        v.sort();
        v
    };
    let listed_before = names(d.path());
    run(&opts(d.path(), None, true));
    run(&opts(d.path(), Some(b"**"), false));
    let after = (
        std::fs::read(&m).unwrap(),
        m.metadata().unwrap().modified().unwrap(),
    );
    assert!(before == after, "the manifest changed");
    assert_eq!(names(d.path()), listed_before, "list left files behind");
}

#[test]
fn no_manifest_is_the_error_check_gives() {
    let d = files();
    let listed = list(&opts(d.path(), None, false), &mut Vec::new())
        .unwrap_err()
        .to_string();
    let checked = run_check_opts(&check_opts(d.path(), Tier::Default, &[], &[], false))
        .unwrap_err()
        .to_string();
    assert_eq!(listed, checked);
    // And so is a root that is not there.
    let gone = d.path().join("gone");
    let listed = list(&opts(&gone, None, false), &mut Vec::new())
        .unwrap_err()
        .to_string();
    let checked = run_check_opts(&check_opts(&gone, Tier::Default, &[], &[], false))
        .unwrap_err()
        .to_string();
    assert_eq!(listed, checked);
}

/// A reader that goes away is not an error: the walk stops.
#[test]
fn a_closed_pipe_ends_the_listing_quietly() {
    struct Closed(usize);
    impl std::io::Write for Closed {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.0 == 0 {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            self.0 -= 1;
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let d = fixture(Backend::Sqlite);
    let o = opts(d.path(), None, false);
    assert_eq!(list(&o, &mut Closed(2)).unwrap(), Outcome::Clean);
    // Any other write error is one.
    struct Full;
    impl std::io::Write for Full {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::StorageFull.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    assert!(list(&o, &mut Full).is_err());
}
