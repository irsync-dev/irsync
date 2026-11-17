//! The client's SSH driver, run against the fake-ssh fixture.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use irsync::ssh::client::{
    client_host_name, decode_name, ensure_installed, parse_uname, probe, set_exe_override,
    setup_command, Install, Rsh, REMOTE_BIN,
};

#[path = "common/release.rs"]
mod release;

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
        std::env::remove_var("IRSYNC_TEST_RELEASE_URL");
        std::env::set_var("FAKE_SSH_HOSTNAME", "-");
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
    // FreeBSD calls x86_64 amd64.
    assert_eq!(
        parse_uname("FreeBSD amd64"),
        (Some("freebsd"), Some("x86_64"))
    );
    assert_eq!(parse_uname("SunOS x86_64"), (None, Some("x86_64")));
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
fn install_refuses_a_platform_without_a_release() {
    let f = fake(Some("Linux riscv64"));
    let p = probe(&f.rsh, "myserver").unwrap();
    let e = ensure_installed(&f.rsh, "myserver", &p).unwrap_err();
    let msg = e.to_string();
    assert!(
        msg.starts_with("myserver runs linux an unknown CPU, this irsync is "),
        "{msg}"
    );
    assert!(
        msg.contains("there is no release binary for that platform"),
        "{msg}"
    );
    assert!(
        msg.contains(
            "Install irsync on both machines and set the server up by hand: \
             https://irsync.org/book/tutorial/first-sync.html#set-the-server-up-by-hand"
        ),
        "{msg}"
    );
    assert!(!f.home.path().join(".local").exists());
}

/// Point the release download at `url`.
fn set_release_url(url: &str) {
    // SAFETY: the caller holds ENV through its `Fake`.
    unsafe { std::env::set_var("IRSYNC_TEST_RELEASE_URL", url) };
}

#[test]
fn install_downloads_for_another_platform() {
    let (uname, target, platform) = release::other_platform();
    let f = fake(Some(uname));
    let rel = tempfile::tempdir().unwrap();
    let this = Path::new(env!("CARGO_BIN_EXE_irsync"));
    set_release_url(&release::fake_release(rel.path(), target, this));
    let p = probe(&f.rsh, "myserver").unwrap();
    let r = ensure_installed(&f.rsh, "myserver", &p).unwrap();
    assert_eq!(
        r,
        Install::Downloaded {
            replaced: None,
            platform: platform.into()
        }
    );
    assert_eq!(
        std::fs::read(f.home.path().join(REMOTE_BIN)).unwrap(),
        std::fs::read(this).unwrap()
    );
    let p = probe(&f.rsh, "myserver").unwrap();
    assert_eq!(
        ensure_installed(&f.rsh, "myserver", &p).unwrap(),
        Install::Current
    );
}

#[test]
fn a_release_that_is_not_this_build_is_not_installed() {
    let (uname, target, _) = release::other_platform();
    let f = fake(Some(uname));
    let rel = tempfile::tempdir().unwrap();
    let other = fake_exe(&f, Some("irsync 9.9.9 (other)"));
    set_release_url(&release::fake_release(rel.path(), target, &other));
    let p = probe(&f.rsh, "myserver").unwrap();
    let e = ensure_installed(&f.rsh, "myserver", &p)
        .unwrap_err()
        .to_string();
    assert!(e.contains("development build"), "{e}");
    assert!(e.contains("Nothing was changed"), "{e}");
    assert_nothing_installed(&f);
}

#[test]
fn a_downloaded_binary_that_does_not_run_says_so() {
    let (uname, target, _) = release::other_platform();
    let f = fake(Some(uname));
    let rel = tempfile::tempdir().unwrap();
    let broken = fake_exe(&f, None);
    set_release_url(&release::fake_release(rel.path(), target, &broken));
    let p = probe(&f.rsh, "myserver").unwrap();
    let e = ensure_installed(&f.rsh, "myserver", &p)
        .unwrap_err()
        .to_string();
    assert!(e.contains("does not run on myserver"), "{e}");
    assert!(e.contains("Nothing was changed"), "{e}");
    assert!(!e.contains("development build"), "{e}");
    assert_nothing_installed(&f);
}

#[test]
fn a_failed_download_changes_nothing() {
    let (uname, _, _) = release::other_platform();
    let f = fake(Some(uname));
    let rel = tempfile::tempdir().unwrap();
    set_release_url(&release::url_of(rel.path()));
    let p = probe(&f.rsh, "myserver").unwrap();
    let e = ensure_installed(&f.rsh, "myserver", &p)
        .unwrap_err()
        .to_string();
    assert!(e.contains("could not download"), "{e}");
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
    let f = fake(Some("Darwin arm64"));
    let wrapper = script(
        &f,
        "noisy-ssh",
        &format!(
            "echo Welcome; echo 'Linux x86_64'; exec {} \"$@\"",
            fake_ssh().display()
        ),
    );
    let p = probe(&wrapper, "myserver").unwrap();
    // The server's uname, not the banner's.
    assert_eq!(p.os, Some("macos"));
    assert_eq!(p.arch, Some("aarch64"));
}

/// A login shell that is not sh (fish, tcsh) cannot run the remote
/// commands as written, so each one is handed to `/bin/sh` whole. The
/// stand-in refuses anything else, the way such a shell would choke.
#[test]
fn remote_commands_run_under_sh() {
    let f = fake(Some("Linux riscv64"));
    let wrapper = script(
        &f,
        "fish-ssh",
        &format!(
            "case \"$2\" in \"exec /bin/sh -c '\"*\"'\") ;; \
             *) echo \"fish: unsupported syntax\" >&2; exit 127 ;; esac; exec {} \"$@\"",
            fake_ssh().display()
        ),
    );
    let p = probe(&wrapper, "myserver").unwrap();
    // FAKE_SSH_UNAME still reaches the inner sh.
    assert_eq!((p.os, p.arch), (Some("linux"), None));
    // SAFETY: serialized by ENV, held in `f`.
    unsafe { std::env::remove_var("FAKE_SSH_UNAME") };
    let p = probe(&wrapper, "myserver").unwrap();
    let r = ensure_installed(&wrapper, "myserver", &p).unwrap();
    assert_eq!(r, Install::Copied { replaced: None });
    let p = probe(&wrapper, "myserver").unwrap();
    assert_eq!(
        ensure_installed(&wrapper, "myserver", &p).unwrap(),
        Install::Current
    );
}

/// The probe asks for the server's host name and keeps its short form,
/// with or without an installed irsync before it in the reply.
#[test]
fn probe_reports_the_host_name() {
    let f = fake(None);
    assert_eq!(probe(&f.rsh, "myserver").unwrap().hostname, None);
    // SAFETY: serialized by ENV (held by `f`).
    unsafe { std::env::set_var("FAKE_SSH_HOSTNAME", "myserver.example.com") };
    let p = probe(&f.rsh, "192.168.1.5").unwrap();
    assert_eq!(p.hostname.as_deref(), Some("myserver"));
    assert_eq!(p.installed, None);
    ensure_installed(&f.rsh, "192.168.1.5", &p).unwrap();
    let p = probe(&f.rsh, "192.168.1.5").unwrap();
    assert_eq!(p.hostname.as_deref(), Some("myserver"));
    assert!(p.installed.is_some());
    unsafe { std::env::set_var("FAKE_SSH_HOSTNAME", "localhost") };
    assert_eq!(probe(&f.rsh, "192.168.1.5").unwrap().hostname, None);
}

/// The client's name goes into a shell command on the server. It travels
/// as hex digits, so whatever it holds, no shell (sh, fish, tcsh) reads
/// any of it as syntax.
#[test]
fn client_name_travels_as_hex() {
    let id = iroh::SecretKey::from_bytes(&[1; 32]).public();
    let nasty = "x'; touch \"$HOME/pwned\"; echo '\\ `id` ü";
    let cmd = setup_command(id, nasty, true, true);
    let fixed = format!("\"$HOME/{REMOTE_BIN}\" ssh-setup --client {id} --client-name-hex ");
    let rest = cmd.strip_prefix(&fixed).unwrap_or_else(|| panic!("{cmd}"));
    let (hex, flags) = rest.split_once(' ').unwrap();
    assert_eq!(flags, "--recheck --no-dht");
    assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()), "{hex}");
    assert_eq!(decode_name(hex).unwrap(), nasty);
    assert!(setup_command(id, "myclient", false, false).ends_with(" 6d79636c69656e74"));
    // Not hex, half a byte, not UTF-8.
    for bad in ["zz", "abc", "ff", ""] {
        assert!(decode_name(bad).is_err(), "{bad:?}");
    }
}

/// The short host name of this machine; a test build takes it from
/// `IRSYNC_TEST_HOSTNAME`.
#[test]
fn client_host_name_is_the_short_usable_one() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: serialized by ENV.
    unsafe { std::env::set_var("IRSYNC_TEST_HOSTNAME", "myclient.example.com") };
    assert_eq!(client_host_name().unwrap(), "myclient");
    unsafe { std::env::set_var("IRSYNC_TEST_HOSTNAME", "localhost") };
    let e = client_host_name().unwrap_err().to_string();
    assert!(
        e.contains("\"localhost\"") && e.contains("host name"),
        "{e}"
    );
    unsafe { std::env::remove_var("IRSYNC_TEST_HOSTNAME") };
    // The real one: whatever it is, the call does not panic.
    let _ = client_host_name();
}

/// The probe says whether the ssh user is root (`id -u` is 0).
#[test]
fn probe_sees_root() {
    let f = fake(Some("Linux x86_64"));
    assert!(!probe(&f.rsh, "myserver").unwrap().root || nix::unistd::geteuid().is_root());
    let bin = f.home.path().join("fake-bin");
    std::fs::create_dir(&bin).unwrap();
    let id = bin.join("id");
    std::fs::write(&id, "#!/bin/sh\necho 0\n").unwrap();
    std::fs::set_permissions(&id, std::fs::Permissions::from_mode(0o755)).unwrap();
    let wrapper = script(
        &f,
        "root-ssh",
        &format!(
            "PATH={}:$PATH exec {} \"$@\"",
            bin.display(),
            fake_ssh().display()
        ),
    );
    let p = probe(&wrapper, "myserver").unwrap();
    assert!(p.root);
    assert_eq!(p.os, Some("linux"));
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

/// A stand-in binary that reports `version` (or fails when `None`).
fn fake_exe(f: &Fake, version: Option<&str>) -> PathBuf {
    let p = f.home.path().join("fake-irsync");
    let body = match version {
        Some(v) => format!("#!/bin/sh\necho '{v}'\n"),
        None => "#!/bin/sh\nexit 126\n".to_owned(),
    };
    std::fs::write(&p, body).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn assert_nothing_installed(f: &Fake) {
    let bin = f.home.path().join(REMOTE_BIN);
    assert!(!bin.exists());
    assert_eq!(std::fs::read_dir(bin.parent().unwrap()).unwrap().count(), 0);
}

#[test]
fn install_rejects_a_copy_that_reports_another_version() {
    let f = fake(None);
    set_exe_override(fake_exe(&f, Some("irsync 9.9.9 (other)")));
    let p = probe(&f.rsh, "myserver").unwrap();
    let e = ensure_installed(&f.rsh, "myserver", &p)
        .unwrap_err()
        .to_string();
    assert!(e.contains("does not run on myserver"), "{e}");
    assert!(e.contains("Nothing was changed"), "{e}");
    assert_nothing_installed(&f);
}

#[test]
fn install_rejects_a_copy_that_does_not_run() {
    let f = fake(None);
    set_exe_override(fake_exe(&f, None));
    let p = probe(&f.rsh, "myserver").unwrap();
    let e = ensure_installed(&f.rsh, "myserver", &p)
        .unwrap_err()
        .to_string();
    assert!(e.contains("does not run on myserver"), "{e}");
    assert_nothing_installed(&f);
}

#[test]
fn a_rejected_copy_keeps_the_old_binary() {
    let f = fake(None);
    let dst = f.home.path().join(REMOTE_BIN);
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::write(&dst, "#!/bin/sh\necho 'irsync 0.0.1 (old)'\n").unwrap();
    std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755)).unwrap();
    set_exe_override(fake_exe(&f, Some("irsync 9.9.9 (other)")));
    let p = probe(&f.rsh, "myserver").unwrap();
    ensure_installed(&f.rsh, "myserver", &p).unwrap_err();
    assert!(std::fs::read_to_string(&dst)
        .unwrap()
        .contains("0.0.1 (old)"));
    assert_eq!(std::fs::read_dir(dst.parent().unwrap()).unwrap().count(), 1);
}
