use std::process::Command;
fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_fileset"))
}

#[test]
fn help_lists_both_verbs() {
    let out = bin().arg("--help").output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("update") && s.contains("check"));
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
fn cli_update_then_check_exit_codes() {
    let d = tree();
    let t = d.path().join("tree");
    let t = t.as_os_str();
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
    assert_eq!(code(&["update".as_ref(), t.as_os_str()]), 0);
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
            "update".as_ref(),
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
fn cli_no_dir_sort_still_checks_clean() {
    let d = tree();
    let t = d.path().join("tree");
    assert_eq!(code(&["update".as_ref(), t.as_os_str()]), 0);
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
    assert_eq!(run("update"), Some(0));
    assert_eq!(run("check"), Some(0));
}
