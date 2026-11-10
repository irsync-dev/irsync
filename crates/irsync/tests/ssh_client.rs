//! The client's SSH driver, run against the fake-ssh fixture.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Mutex;

use irsync::ssh::client::{
    ensure_installed, parse_uname, probe, set_exe_override, Install, Rsh, REMOTE_BIN,
};

/// Tests that set process-wide env must not overlap.
static ENV: Mutex<()> = Mutex::new(());

fn fake_ssh() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-ssh")
}

struct Fake {
    home: tempfile::TempDir,
    log: PathBuf,
    rsh: Rsh,
    _guard: std::sync::MutexGuard<'static, ()>,
}

fn fake(uname: Option<&str>) -> Fake {
    let guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    let log = home.path().join("ssh.log");
    // SAFETY: serialized by ENV; no other thread reads these concurrently.
    unsafe {
        std::env::set_var("FAKE_SSH_HOME", home.path());
        std::env::set_var("FAKE_SSH_LOG", &log);
        match uname {
            Some(u) => std::env::set_var("FAKE_SSH_UNAME", u),
            None => std::env::remove_var("FAKE_SSH_UNAME"),
        }
    }
    set_exe_override(PathBuf::from(env!("CARGO_BIN_EXE_irsync")));
    let rsh = Rsh::from_opts(Some(fake_ssh().to_str().unwrap()));
    Fake {
        home,
        log,
        rsh,
        _guard: guard,
    }
}

#[test]
fn parse_uname_maps() {
    assert_eq!(parse_uname("Linux x86_64"), (Some("linux"), Some("x86_64")));
    assert_eq!(
        parse_uname("Darwin arm64"),
        (Some("macos"), Some("aarch64"))
    );
    assert_eq!(parse_uname("Linux amd64"), (Some("linux"), Some("x86_64")));
    assert_eq!(parse_uname("FreeBSD x86_64"), (None, Some("x86_64")));
}

#[test]
fn rsh_splits_like_rsync() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(
        Rsh::from_opts(Some("ssh -p 2222")).argv(),
        ["ssh", "-p", "2222"]
    );
    // SAFETY: serialized by ENV.
    unsafe { std::env::remove_var("IRSYNC_RSH") };
    assert_eq!(Rsh::from_opts(None).argv(), ["ssh"]);
}

#[test]
fn install_copies_then_is_current() {
    let f = fake(None);
    let p = probe(&f.rsh, "myserver").unwrap();
    assert_eq!(p.installed, None);
    let r = ensure_installed(&f.rsh, "myserver", &p).unwrap();
    assert_eq!(r, Install::Copied { replaced: None });
    let dst = f.home.path().join(REMOTE_BIN);
    assert_ne!(dst.metadata().unwrap().permissions().mode() & 0o111, 0);
    assert_eq!(
        std::fs::read(&dst).unwrap(),
        std::fs::read(env!("CARGO_BIN_EXE_irsync")).unwrap()
    );
    let p = probe(&f.rsh, "myserver").unwrap();
    assert_eq!(
        ensure_installed(&f.rsh, "myserver", &p).unwrap(),
        Install::Current
    );
    assert!(std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("myserver"));
}

#[test]
fn install_refuses_other_platform() {
    let f = fake(Some("Linux riscv64"));
    let p = probe(&f.rsh, "myserver").unwrap();
    let e = ensure_installed(&f.rsh, "myserver", &p).unwrap_err();
    assert!(e.to_string().contains("install irsync on"), "{e}");
    assert!(!f.home.path().join(".local").exists());
}

#[test]
fn install_replaces_other_version() {
    let f = fake(None);
    let dst = f.home.path().join(REMOTE_BIN);
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::write(&dst, "#!/bin/sh\necho 'irsync 0.0.1 (old)'\n").unwrap();
    std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755)).unwrap();
    let p = probe(&f.rsh, "myserver").unwrap();
    assert_eq!(p.installed.as_deref(), Some("irsync 0.0.1 (old)"));
    let r = ensure_installed(&f.rsh, "myserver", &p).unwrap();
    assert_eq!(
        r,
        Install::Copied {
            replaced: Some("irsync 0.0.1 (old)".into())
        }
    );
}

/// A one-line script standing in for ssh, in the fake home.
fn script(f: &Fake, name: &str, body: &str) -> Rsh {
    let p = f.home.path().join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    Rsh::from_opts(Some(p.to_str().unwrap()))
}

#[test]
fn probe_failure_names_the_destination() {
    let f = fake(None);
    let rsh = script(
        &f,
        "bad-ssh",
        "echo 'ssh: Could not resolve hostname x' >&2; exit 255",
    );
    let e = probe(&rsh, "x").unwrap_err();
    assert!(e.to_string().contains("cannot reach x"), "{e}");
}

#[test]
fn probe_ignores_a_login_banner() {
    let f = fake(None);
    let wrapper = script(
        &f,
        "noisy-ssh",
        &format!(
            "echo Welcome; echo 'Linux x86_64'; exec {} \"$@\"",
            fake_ssh().display()
        ),
    );
    let p = probe(&wrapper, "myserver").unwrap();
    assert_eq!(p.os, Some("linux"));
    assert_eq!(p.arch, Some("x86_64"));
}

#[test]
fn install_removes_a_short_upload() {
    let f = fake(None);
    // Truncates stdin to 10 bytes before it reaches the remote command.
    let wrapper = script(
        &f,
        "short-ssh",
        &format!("head -c 10 | {} \"$@\"", fake_ssh().display()),
    );
    let p = probe(&f.rsh, "myserver").unwrap();
    let e = ensure_installed(&wrapper, "myserver", &p).unwrap_err();
    assert!(e.to_string().contains("cut short"), "{e}");
    let bin = f.home.path().join(REMOTE_BIN);
    assert!(!bin.exists());
    let dir = bin.parent().unwrap();
    assert_eq!(std::fs::read_dir(dir).unwrap().count(), 0);
}
