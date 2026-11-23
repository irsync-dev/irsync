use std::process::Command;
fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_fileset"))
}

#[test]
fn help_lists_all_verbs() {
    let out = bin().arg("--help").output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("init") && s.contains("update") && s.contains("check"));
}

#[test]
fn only_init_takes_store_and_only_update_takes_rehash() {
    let help = |verb: &str| {
        let out = bin().args([verb, "--help"]).output().unwrap();
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let init = help("init");
    let update = help("update");
    assert!(
        init.contains("--store") && !init.contains("--rehash"),
        "{init}"
    );
    assert!(
        update.contains("--rehash") && !update.contains("--store"),
        "{update}"
    );
}

#[test]
fn only_update_takes_force_xattr_reads() {
    for (verb, has) in [("init", false), ("update", true), ("check", false)] {
        let out = bin().args([verb, "--help"]).output().unwrap();
        let s = String::from_utf8_lossy(&out.stdout);
        assert_eq!(s.contains("--force-xattr-reads"), has, "{verb}: {s}");
    }
}

#[test]
fn check_help_describes_comparison_flags() {
    let out = bin().args(["check", "--help"]).output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    for text in [
        "Compare only type, content and symlink target",
        "Compare everything that is recorded and comparable",
        "Add one field to the comparison (repeatable)",
        "Drop one field from the comparison (repeatable)",
        "may be outside the tree",
    ] {
        assert!(s.contains(text), "missing {text:?} in:\n{s}");
    }
}

#[test]
fn check_rejects_relaxed_with_paranoid() {
    let out = bin()
        .args(["check", "--relaxed", "--paranoid", "."])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot be used with"));
}

#[test]
fn jobs_must_be_at_least_one() {
    let out = bin().args(["check", "-j", "0", "."]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(e.contains("0 is not in 1..") && e.contains("--jobs"), "{e}");
}

#[test]
fn walk_threads_must_be_at_least_one() {
    let out = bin()
        .args(["check", "--walk-threads", "0", "."])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(
        e.contains("0 is not in 1..") && e.contains("--walk-threads"),
        "{e}"
    );
}

#[test]
fn every_common_flag_parses() {
    let d = tree();
    let t = d.path().join("tree");
    let m = d.path().join("m.sqlite");
    let flags: [&std::ffi::OsStr; 9] = [
        "--fail-on-warnings".as_ref(),
        "--cross-mount-points".as_ref(),
        "--no-dir-sort".as_ref(),
        "-j".as_ref(),
        "2".as_ref(),
        "--walk-threads".as_ref(),
        "3".as_ref(),
        "--manifest".as_ref(),
        m.as_os_str(),
    ];
    for verb in ["init", "update", "check"] {
        let out = bin()
            .arg(verb)
            .args(flags)
            .args(["--quiet".as_ref(), t.as_os_str()])
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{verb}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert!(m.is_file());
}

#[test]
fn every_field_name_is_accepted_by_ignore_and_require() {
    let d = tree();
    let t = d.path().join("tree");
    assert_eq!(code(&["init".as_ref(), t.as_os_str()]), 0);
    let fields = [
        "blake3", "target", "rdev", "mode", "mtime", "size", "uid", "gid", "nlink", "xattr",
    ];
    for (flag, tier) in [("--ignore", "--paranoid"), ("--require", "--relaxed")] {
        let mut cmd = bin();
        cmd.args(["check", tier]);
        for f in fields {
            cmd.args([flag, f]);
        }
        let out = cmd.arg(&t).output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{flag}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn cli_relative_manifest_inside_the_tree_is_the_store_not_content() {
    let d = tree();
    let run = |verb: &str| {
        let out = bin()
            .current_dir(d.path())
            .args([verb, "--manifest", "tree/m.sqlite", "tree"])
            .output()
            .unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };
    assert_eq!(run("init").0, Some(0));
    // Were the manifest taken for content, its own writes would show as a change.
    assert_eq!(run("update").0, Some(0));
    assert_eq!(run("check"), (Some(0), String::new()));
}

fn tree() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("tree")).unwrap();
    std::fs::write(d.path().join("tree/a.txt"), "hello").unwrap();
    std::fs::create_dir(d.path().join("tree/sub")).unwrap();
    std::fs::write(d.path().join("tree/sub/b.txt"), "world").unwrap();
    d
}

fn code(args: &[&std::ffi::OsStr]) -> i32 {
    bin().args(args).output().unwrap().status.code().unwrap()
}

#[test]
fn cli_init_update_check_exit_codes() {
    let d = tree();
    let t = d.path().join("tree");
    let t = t.as_os_str();
    assert_eq!(code(&["init".as_ref(), t]), 0);
    assert_eq!(code(&["update".as_ref(), t]), 0);
    assert_eq!(code(&["check".as_ref(), t]), 0);
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(d.path().join("tree/a.txt"))
        .unwrap();
    std::io::Write::write_all(&mut f, b" more").unwrap();
    drop(f);
    let out = bin().args(["check".as_ref(), t]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("CHG "));
}

#[test]
fn cli_init_twice_exits_2() {
    let d = tree();
    let t = d.path().join("tree");
    assert_eq!(code(&["init".as_ref(), t.as_os_str()]), 0);
    let out = bin()
        .args(["init".as_ref(), t.as_os_str()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(
        e.starts_with("error: ") && e.contains("already exists") && e.contains("fileset update"),
        "{e}"
    );
}

#[test]
fn cli_init_nested_manifest_exits_2() {
    let d = tree();
    let t = d.path().join("tree");
    std::fs::write(t.join("sub/.fileset.sqlite"), "not a manifest").unwrap();
    let out = bin()
        .args(["init".as_ref(), t.as_os_str()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(
        e.starts_with("error: ")
            && e.contains("nested manifest at sub/.fileset.sqlite")
            && e.contains("not supported"),
        "{e}"
    );
    assert!(!t.join(".fileset.sqlite").exists());
}

#[test]
fn cli_init_inside_fileset_exits_2() {
    let d = tree();
    let t = d.path().join("tree");
    assert_eq!(code(&["init".as_ref(), t.as_os_str()]), 0);
    let out = bin()
        .args(["init".as_ref(), t.join("sub").as_os_str()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(
        e.starts_with("error: ")
            && e.contains("inside the fileset")
            && e.contains(".fileset.sqlite")
            && e.contains("not supported"),
        "{e}"
    );
    assert!(!t.join("sub/.fileset.sqlite").exists());
}

#[test]
fn cli_update_missing_manifest_exits_2() {
    let d = tree();
    let t = d.path().join("tree");
    let out = bin()
        .args(["update".as_ref(), t.as_os_str()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(
        e.starts_with("error: ") && e.contains("no manifest") && e.contains("fileset init"),
        "{e}"
    );
    assert!(!t.join(".fileset.sqlite").exists());
}

#[test]
fn cli_check_missing_manifest_exits_2() {
    let d = tree();
    let out = bin()
        .args(["check".as_ref(), d.path().join("tree").as_os_str()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(
        e.contains(".fileset.sqlite") && e.contains(".fileset.fjall"),
        "{e}"
    );
}

#[test]
fn cli_unknown_field_exits_2() {
    let d = tree();
    let out = bin()
        .args(["check", "--ignore", "bogus"])
        .arg(d.path().join("tree"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(e.contains("unknown field 'bogus'"), "{e}");
}

#[test]
fn cli_fail_on_warnings() {
    use std::os::unix::fs::PermissionsExt;
    let d = tree();
    let secret = d.path().join("tree/secret");
    std::fs::write(&secret, "x").unwrap();
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&secret).is_ok() {
        return; // running as root: chmod 000 does not block reads
    }
    let t = d.path().join("tree");
    assert_eq!(code(&["init".as_ref(), t.as_os_str()]), 0);
    assert_eq!(
        code(&[
            "update".as_ref(),
            "--fail-on-warnings".as_ref(),
            t.as_os_str()
        ]),
        1
    );
}

#[test]
fn cli_store_fjall_creates_directory() {
    let d = tree();
    let t = d.path().join("tree");
    assert_eq!(
        code(&[
            "init".as_ref(),
            "--store".as_ref(),
            "fjall".as_ref(),
            t.as_os_str()
        ]),
        0
    );
    assert!(t.join(".fileset.fjall").is_dir());
    assert_eq!(code(&["check".as_ref(), t.as_os_str()]), 0);
}

#[test]
fn cli_store_redb_creates_one_file_that_reads_where_nothing_can_be_written() {
    let d = tree();
    let t = d.path().join("tree");
    let manifest = t.join(".fileset.redb");
    let run = |args: &[&str]| {
        let out = bin().args(args).arg(&t).output().unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    assert_eq!(run(&["init", "-q", "--store", "redb"]).0, Some(0));
    assert!(manifest.is_file());
    assert_eq!(run(&["update", "-q", "--no-compaction"]).0, Some(0));
    assert_eq!(run(&["update", "-q"]).0, Some(0));
    let names = |dir: &std::path::Path| {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    assert_eq!(names(&t), [".fileset.redb", "a.txt", "sub"]);

    // As on a medium that cannot be written to: nothing may be changed or created.
    use std::os::unix::fs::PermissionsExt;
    let chmod = |p: &std::path::Path, mode| {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap()
    };
    chmod(&manifest, 0o444);
    chmod(&t.join("sub"), 0o555);
    chmod(&t, 0o555);
    let writable = std::fs::write(t.join("probe"), b"").is_ok();
    let before = std::fs::read(&manifest).unwrap();
    let check = run(&["check", "-q", "--relaxed"]);
    let list = bin().arg("list").arg(&t).output().unwrap();
    let update = run(&["update", "-q"]);
    let after = std::fs::read(&manifest).unwrap();
    chmod(&t, 0o755);
    chmod(&t.join("sub"), 0o755);
    if writable {
        eprintln!("skipping: running with privileges that bypass file permissions");
        return;
    }
    assert_eq!(check.0, Some(0), "{}", check.1);
    assert!(list.status.success());
    assert_eq!(String::from_utf8_lossy(&list.stdout).lines().count(), 5);
    assert_eq!(update.0, Some(2), "{}", update.1);
    assert_eq!(before, after);
    assert_eq!(names(&t), [".fileset.redb", "a.txt", "sub"]);
}

/// The start of a command that runs the rest of it with `dir` mounted read-only, in a
/// mount namespace of its own. Bubblewrap and an unprivileged `unshare` need no root;
/// where neither is allowed (a GitHub runner), `sudo` without a password does it. None
/// where no way works.
#[cfg(target_os = "linux")]
fn read_only_mount(dir: &std::path::Path) -> Option<Command> {
    let remount = r#"mount --bind "$0" "$0" && mount -o remount,ro,bind "$0" && exec "$@""#;
    let ways = [
        vec![
            "bwrap",
            "--dev-bind",
            "/",
            "/",
            "--ro-bind",
            "DIR",
            "DIR",
            "--",
        ],
        vec!["unshare", "-rm", "sh", "-c", remount, "DIR"],
        vec!["sudo", "-n", "unshare", "-m", "sh", "-c", remount, "DIR"],
    ];
    let command = |way: &[&str]| {
        let mut c = Command::new(way[0]);
        for arg in &way[1..] {
            if *arg == "DIR" {
                c.arg(dir);
            } else {
                c.arg(arg);
            }
        }
        c
    };
    ways.iter().find_map(|way| {
        // The mount is there if a write fails for that reason.
        let probe = command(way)
            .args(["touch".as_ref(), dir.join("probe").as_os_str()])
            .output()
            .ok()?;
        String::from_utf8_lossy(&probe.stderr)
            .contains("Read-only file system")
            .then(|| command(way))
    })
}

/// What the redb store is for, on the real thing: `check` and `list` read a manifest on
/// a mount that cannot be written to, and nothing tries to write.
#[cfg(target_os = "linux")]
#[test]
fn cli_redb_manifest_reads_on_a_read_only_mount() {
    let d = tree();
    let t = d.path().join("tree");
    let manifest = t.join(".fileset.redb");
    let init = bin()
        .args(["init", "-q", "--store", "redb"])
        .arg(&t)
        .output()
        .unwrap();
    assert!(init.status.success());
    let before = std::fs::read(&manifest).unwrap();

    let Some(_) = read_only_mount(&t) else {
        // CI must not pass by skipping this: its runners have sudo.
        assert!(
            std::env::var_os("GITHUB_ACTIONS").is_none(),
            "no way to make a read-only mount on a GitHub runner"
        );
        eprintln!("skipping: no bwrap, unshare or sudo that can make a read-only mount");
        return;
    };
    let run = |args: &[&str]| {
        let out = read_only_mount(&t)
            .unwrap()
            .arg(env!("CARGO_BIN_EXE_fileset"))
            .args(args)
            .arg(&t)
            .output()
            .unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let check = run(&["check", "-q"]);
    assert_eq!((check.0, check.1.as_str()), (Some(0), ""), "{}", check.2);
    let list = run(&["list"]);
    assert_eq!(list.0, Some(0), "{}", list.2);
    assert_eq!(list.1.lines().count(), 5, "{}", list.1);
    // An update has to write, and says why it cannot.
    let update = run(&["update", "-q"]);
    assert_eq!(update.0, Some(2), "{}", update.2);
    assert!(update.2.contains("Read-only file system"), "{}", update.2);

    assert_eq!(std::fs::read(&manifest).unwrap(), before);
    let mut names: Vec<_> = std::fs::read_dir(&t)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    names.sort();
    assert_eq!(names, [".fileset.redb", "a.txt", "sub"]);
}

/// A disk image attached at `mount`, detached again when this is dropped.
#[cfg(target_os = "macos")]
struct Attached<'a> {
    mount: &'a std::path::Path,
}

#[cfg(target_os = "macos")]
impl<'a> Attached<'a> {
    /// Attaches `image` at `mount`, for reading only if `read_only`. An error is what
    /// `hdiutil` said.
    fn new(
        image: &std::path::Path,
        mount: &'a std::path::Path,
        read_only: bool,
    ) -> Result<Self, String> {
        let mut c = Command::new("hdiutil");
        c.args(["attach", "-quiet", "-nobrowse"]);
        if read_only {
            c.arg("-readonly");
        }
        hdiutil(c.arg("-mountpoint").arg(mount).arg(image))?;
        Ok(Attached { mount })
    }
}

#[cfg(target_os = "macos")]
impl Drop for Attached<'_> {
    fn drop(&mut self) {
        let mut c = Command::new("hdiutil");
        let _ = hdiutil(c.args(["detach", "-quiet", "-force"]).arg(self.mount));
    }
}

/// Runs an `hdiutil` command; an error is what it printed.
#[cfg(target_os = "macos")]
fn hdiutil(c: &mut Command) -> Result<(), String> {
    let out = c.output().map_err(|e| e.to_string())?;
    match out.status.success() {
        true => Ok(()),
        false => Err(format!(
            "{c:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )),
    }
}

/// The same on macOS, where a mount needs no root if it is a disk image: the tree and
/// its manifest are made on an image attached for writing, which is then attached
/// again for reading only.
#[cfg(target_os = "macos")]
#[test]
fn cli_redb_manifest_reads_on_a_read_only_disk_image() {
    let d = tempfile::tempdir().unwrap();
    let image = d.path().join("tree.dmg");
    let mount = d.path().join("mnt");
    std::fs::create_dir(&mount).unwrap();
    // The tree is a directory of the volume, not its root, which macOS keeps files
    // of its own in.
    let t = mount.join("tree");
    let manifest = t.join(".fileset.redb");

    let made = (|| {
        let mut c = Command::new("hdiutil");
        c.args(["create", "-quiet", "-size", "16m", "-fs", "HFS+"]);
        hdiutil(c.args(["-volname", "fileset-test"]).arg(&image))?;
        let _attached = Attached::new(&image, &mount, false)?;
        std::fs::create_dir_all(t.join("sub")).map_err(|e| e.to_string())?;
        std::fs::write(t.join("a.txt"), "hello").map_err(|e| e.to_string())?;
        std::fs::write(t.join("sub/b.txt"), "world").map_err(|e| e.to_string())?;
        let init = bin()
            .args(["init", "-q", "--store", "redb"])
            .arg(&t)
            .output()
            .unwrap();
        assert!(init.status.success(), "{init:?}");
        Ok::<_, String>(std::fs::read(&manifest).unwrap())
    })();
    let attached = made.and_then(|before| Ok((before, Attached::new(&image, &mount, true)?)));
    let (before, _attached) = match attached {
        Ok(a) => a,
        Err(e) => {
            // CI must not pass by skipping this.
            assert!(std::env::var_os("GITHUB_ACTIONS").is_none(), "{e}");
            eprintln!("skipping: no read-only disk image: {e}");
            return;
        }
    };
    let probe = std::fs::write(t.join("probe"), b"").unwrap_err();
    assert_eq!(probe.raw_os_error(), Some(30), "{probe}");

    let run = |args: &[&str]| {
        let out = bin().args(args).arg(&t).output().unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let check = run(&["check", "-q", "--relaxed"]);
    assert_eq!((check.0, check.1.as_str()), (Some(0), ""), "{}", check.2);
    let list = run(&["list"]);
    assert_eq!(list.0, Some(0), "{}", list.2);
    assert_eq!(list.1.lines().count(), 5, "{}", list.1);
    // An update has to write, and says why it cannot.
    let update = run(&["update", "-q"]);
    assert_eq!(update.0, Some(2), "{}", update.2);
    assert!(update.2.contains("Read-only file system"), "{}", update.2);

    assert_eq!(std::fs::read(&manifest).unwrap(), before);
    let mut names: Vec<_> = std::fs::read_dir(&t)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    names.sort();
    assert_eq!(names, [".fileset.redb", "a.txt", "sub"]);
}

#[test]
fn only_init_and_update_take_no_compaction() {
    for (verb, takes) in [
        ("init", true),
        ("update", true),
        ("check", false),
        ("list", false),
    ] {
        let out = bin().args([verb, "--help"]).output().unwrap();
        let help = String::from_utf8_lossy(&out.stdout);
        assert_eq!(help.contains("--no-compaction"), takes, "{verb}: {help}");
    }
}

#[test]
fn cli_no_dir_sort_still_checks_clean() {
    let d = tree();
    let t = d.path().join("tree");
    assert_eq!(code(&["init".as_ref(), t.as_os_str()]), 0);
    assert_eq!(
        code(&["check".as_ref(), "--no-dir-sort".as_ref(), t.as_os_str()]),
        0
    );
}

#[test]
fn cli_relative_path_with_trailing_slash() {
    let d = tree();
    let run = |verb: &str| {
        bin()
            .current_dir(d.path())
            .args([verb, "tree/"])
            .output()
            .unwrap()
            .status
            .code()
    };
    assert_eq!(run("init"), Some(0));
    assert_eq!(run("update"), Some(0));
    assert_eq!(run("check"), Some(0));
}

#[test]
fn cli_builtin_excludes_flags() {
    let d = tree();
    let t = d.path().join("tree");
    std::fs::create_dir(t.join(".Trashes")).unwrap();
    let out = bin()
        .args(["init".as_ref(), "-v".as_ref(), t.as_os_str()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let o = String::from_utf8_lossy(&out.stdout);
    assert!(o.contains("SKP .Trashes reason=builtin-exclude\n"), "{o}");
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(!e.contains("INFO"), "{e}");
    let out = bin()
        .args([
            "check".as_ref(),
            "--no-builtin-excludes".as_ref(),
            t.as_os_str(),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains(".Trashes"));
    assert_eq!(
        code(&[
            "check".as_ref(),
            "-q".as_ref(),
            "-v".as_ref(),
            t.as_os_str()
        ]),
        2
    );
}

#[test]
fn cli_check_verbose_lists_entries() {
    let d = tree();
    let t = d.path().join("tree");
    let t = t.as_os_str();
    assert_eq!(code(&["init".as_ref(), t]), 0);
    let out = bin()
        .args(["check".as_ref(), "-v".as_ref(), t])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let o = String::from_utf8_lossy(&out.stdout);
    assert!(!o.is_empty());
    assert!(o.lines().all(|l| l.starts_with("EQL ")), "{o}");
    assert!(o.contains("EQL sub/b.txt kind=file size=5 "), "{o}");
    let out = bin().args(["check".as_ref(), t]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());
}

#[test]
fn cli_verbose_help_names_the_entry_lines() {
    let out = bin().args(["check", "--help"]).output().unwrap();
    let o = String::from_utf8_lossy(&out.stdout);
    assert!(o.contains("one line per entry"), "{o}");
}

/// Runs `verb -v` on a tree of 2000 files with stdout already closed.
fn verbose_with_closed_stdout(verb: &str) -> std::process::Output {
    let d = tempfile::tempdir().unwrap();
    for i in 0..2000 {
        std::fs::write(d.path().join(format!("f{i:04}")), "x").unwrap();
    }
    let t = d.path().as_os_str();
    assert_eq!(code(&["init".as_ref(), "-q".as_ref(), t]), 0);
    let mut child = bin()
        .args([verb.as_ref(), "-v".as_ref(), t])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    child.wait_with_output().unwrap()
}

#[test]
fn cli_verbose_with_closed_stdout_exits_2() {
    for verb in ["check", "update"] {
        let out = verbose_with_closed_stdout(verb);
        let e = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{verb}: {:?} {e}", out.status);
        assert!(e.contains("error: "), "{verb}: {e}");
        assert!(!e.contains("panicked"), "{verb}: {e}");
    }
}

#[test]
fn cli_ls_is_list() {
    let d = tree();
    let t = d.path().join("tree");
    let t = t.as_os_str();
    assert_eq!(code(&["init".as_ref(), "-q".as_ref(), t]), 0);
    let list = bin().args(["list".as_ref(), t]).output().unwrap();
    let ls = bin().args(["ls".as_ref(), t]).output().unwrap();
    assert_eq!(list.status.code(), Some(0));
    assert!(list.stderr.is_empty(), "{:?}", list.stderr);
    let out = String::from_utf8_lossy(&list.stdout);
    assert!(out.contains("\nsub/b.txt kind=file size=5 "), "{out}");
    assert_eq!(list.stdout, ls.stdout);
    assert_eq!(ls.status.code(), Some(0));
    let help = bin().arg("--help").output().unwrap();
    assert!(String::from_utf8_lossy(&help.stdout).contains("list"));
}

#[test]
fn cli_list_exit_codes() {
    let d = tree();
    let t = d.path().join("tree");
    // No manifest yet, and a path that is not there: 2, with check's words.
    for path in [t.clone(), d.path().join("gone")] {
        let list = bin()
            .args(["list".as_ref(), path.as_os_str()])
            .output()
            .unwrap();
        let check = bin()
            .args(["check".as_ref(), path.as_os_str()])
            .output()
            .unwrap();
        assert_eq!(list.status.code(), Some(2), "{path:?}");
        assert!(list.stdout.is_empty());
        assert_eq!(list.stderr, check.stderr, "{path:?}");
    }
    assert_eq!(code(&["init".as_ref(), "-q".as_ref(), t.as_os_str()]), 0);
    // Nothing matches: still 0.
    let none = bin()
        .args(["list".as_ref(), t.as_os_str(), "nope*".as_ref()])
        .output()
        .unwrap();
    assert_eq!(none.status.code(), Some(0));
    assert!(none.stdout.is_empty() && none.stderr.is_empty());
    let some = bin()
        .args(["list".as_ref(), t.as_os_str(), "**/*.txt".as_ref()])
        .output()
        .unwrap();
    assert_eq!(some.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&some.stdout).starts_with("sub/b.txt kind=file "));
    assert_eq!(some.stdout.iter().filter(|&&b| b == b'\n').count(), 1);
}

#[test]
fn cli_list_takes_no_walk_options() {
    let d = tree();
    let t = d.path().join("tree");
    assert_eq!(code(&["init".as_ref(), "-q".as_ref(), t.as_os_str()]), 0);
    for flags in [
        &["--jobs", "2"][..],
        &["--walk-threads", "2"],
        &["--cross-mount-points"],
        &["--no-dir-sort"],
        &["--no-builtin-excludes"],
        &["--fail-on-warnings"],
        &["-q"],
    ] {
        let out = bin().arg("list").args(flags).arg(&t).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{flags:?}");
    }
    // The two it has.
    let out = bin()
        .args(["list", "-v", "--manifest"])
        .arg(t.join(".fileset.sqlite"))
        .arg(&t)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains(" uid="));
}

/// `fileset list tree *.txt`, unquoted: the shell hands over every name it matched.
#[test]
fn cli_list_says_to_quote_an_expanded_pattern() {
    let d = tree();
    let t = d.path().join("tree");
    assert_eq!(code(&["init".as_ref(), "-q".as_ref(), t.as_os_str()]), 0);
    let out = bin()
        .arg("list")
        .arg(&t)
        .args(["a.txt", "b.txt", "c.txt"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(e.contains("quote the pattern"), "{e}");
}

/// `fileset list tree | head -1`: the reader leaves, and that is fine.
#[test]
fn cli_list_stops_quietly_when_stdout_closes() {
    use std::io::{BufRead, BufReader};
    let d = tempfile::tempdir().unwrap();
    for i in 0..3000 {
        std::fs::write(d.path().join(format!("f{i:04}")), "x").unwrap();
    }
    let t = d.path().as_os_str();
    assert_eq!(code(&["init".as_ref(), "-q".as_ref(), t]), 0);
    let mut child = bin()
        .args(["list".as_ref(), t])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = String::new();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    reader.read_line(&mut first).unwrap();
    assert!(first.starts_with(". kind=dir "), "{first}");
    drop(reader);
    let out = child.wait_with_output().unwrap();
    let e = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{e}");
    assert!(e.is_empty(), "{e}");
}
