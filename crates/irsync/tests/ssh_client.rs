//! The client's SSH driver, run against the fake-ssh fixture.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use irsync::build::LONG_VERSION;
use irsync::config::hosts::short_host_name;
use irsync::ssh::client::{
    client_host_name, decode_name, ensure_installed, parse_uname, probe, set_exe_override,
    setup_command, Install, Rsh, REMOTE_BIN,
};
use irsync::ssh::onboard::SetupFlags;
use irsync::ssh::payload::Contents;

#[path = "common/release.rs"]
mod release;

/// Tests that set process-wide env must not overlap.
static ENV: Mutex<()> = Mutex::new(());

/// Neither `fileset` nor the man pages.
const ALONE: Contents = Contents {
    fileset: false,
    man: false,
};

/// `fileset` and the man pages.
const ALL: Contents = Contents {
    fileset: true,
    man: true,
};

fn fake_ssh() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-ssh")
}

struct Fake {
    /// The server's home. Only the remote commands write there.
    home: PathBuf,
    /// The test's own files: the ssh log, the remote stderr, stand-ins.
    aux: PathBuf,
    log: PathBuf,
    /// What the remote commands wrote to stderr.
    err: PathBuf,
    rsh: Rsh,
    _root: tempfile::TempDir,
    _guard: std::sync::MutexGuard<'static, ()>,
}

fn fake(uname: Option<&str>) -> Fake {
    fake_at(uname, "home")
}

/// A fake server whose home directory is named `home_name`.
fn fake_at(uname: Option<&str>, home_name: &str) -> Fake {
    let guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join(home_name);
    let aux = root.path().join("aux");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&aux).unwrap();
    let log = aux.join("ssh.log");
    let err = aux.join("ssh.err");
    // SAFETY: serialized by ENV; no other thread reads these concurrently.
    unsafe {
        std::env::set_var("FAKE_SSH_HOME", &home);
        std::env::set_var("FAKE_SSH_LOG", &log);
        std::env::set_var("FAKE_SSH_STDERR", &err);
        match uname {
            Some(u) => std::env::set_var("FAKE_SSH_UNAME", u),
            None => std::env::remove_var("FAKE_SSH_UNAME"),
        }
        std::env::remove_var("FAKE_SSH_HIDE");
        std::env::remove_var("IRSYNC_TEST_RELEASE_URL");
        std::env::set_var("FAKE_SSH_HOSTNAME", "-");
    }
    // A directory of its own, so no `fileset` or man pages from the build
    // directory sit beside it.
    let exe_dir = aux.join("exe");
    std::fs::create_dir(&exe_dir).unwrap();
    let exe = exe_dir.join("irsync");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_irsync"), &exe).unwrap();
    set_exe_override(exe);
    let rsh = Rsh::from_opts(Some(fake_ssh().to_str().unwrap()));
    Fake {
        home,
        aux,
        log,
        err,
        rsh,
        _root: root,
        _guard: guard,
    }
}

fn this_binary() -> Vec<u8> {
    std::fs::read(env!("CARGO_BIN_EXE_irsync")).unwrap()
}

/// The release target for this machine and "os cpu" as messages print it,
/// or `None` where no release exists for this machine (Intel Mac, FreeBSD).
fn this_platform() -> Option<(&'static str, String)> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let target = irsync::ssh::release::target_for(Some(os), Some(arch))?;
    Some((target, format!("{os} {arch}")))
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
    let r = ensure_installed(&f.rsh, "myserver", &p, false).unwrap();
    assert_eq!(
        r,
        Install::Copied {
            replaced: None,
            contents: ALONE
        }
    );
    let dst = f.home.join(REMOTE_BIN);
    assert_ne!(dst.metadata().unwrap().permissions().mode() & 0o111, 0);
    assert_eq!(std::fs::read(&dst).unwrap(), this_binary());
    let p = probe(&f.rsh, "myserver").unwrap();
    assert_eq!(
        ensure_installed(&f.rsh, "myserver", &p, false).unwrap(),
        Install::Current
    );
    assert!(std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("myserver"));
}

#[test]
fn no_install_with_the_same_build_is_current() {
    let f = fake(None);
    let p = probe(&f.rsh, "myserver").unwrap();
    ensure_installed(&f.rsh, "myserver", &p, false).unwrap();
    let p = probe(&f.rsh, "myserver").unwrap();
    assert_eq!(
        ensure_installed(&f.rsh, "myserver", &p, true).unwrap(),
        Install::Current
    );
}

#[test]
fn no_install_with_nothing_there_fails() {
    let f = fake(None);
    let p = probe(&f.rsh, "myserver").unwrap();
    let msg = ensure_installed(&f.rsh, "myserver", &p, true)
        .unwrap_err()
        .to_string();
    assert!(msg.contains("--no-install"), "{msg}");
    assert!(msg.contains("~/.local/bin/irsync"), "{msg}");
    assert!(msg.contains("found none"), "{msg}");
    assert!(msg.contains("ssh_install = true"), "{msg}");
    assert!(msg.contains("Nothing was changed."), "{msg}");
    assert_nothing_installed(&f);
}

#[test]
fn no_install_with_another_version_fails() {
    let f = fake(None);
    let bin = f.home.join(REMOTE_BIN);
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    let old = "#!/bin/sh\necho 'irsync 0.0.1 (old)'\n";
    std::fs::write(&bin, old).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let p = probe(&f.rsh, "myserver").unwrap();
    let msg = ensure_installed(&f.rsh, "myserver", &p, true)
        .unwrap_err()
        .to_string();
    assert!(msg.contains("found irsync 0.0.1 (old)"), "{msg}");
    assert!(msg.contains("Nothing was changed."), "{msg}");
    assert_eq!(std::fs::read_to_string(&bin).unwrap(), old);
    assert_no_work_dir(&f);
}

#[test]
fn install_lands_in_local_bin_with_no_leftovers() {
    let f = fake(None);
    let p = probe(&f.rsh, "myserver").unwrap();
    ensure_installed(&f.rsh, "myserver", &p, false).unwrap();
    let dst = f.home.join(".local/bin/irsync");
    assert_ne!(dst.metadata().unwrap().permissions().mode() & 0o111, 0);
    assert_eq!(std::fs::read(&dst).unwrap(), this_binary());
    assert_no_work_dir(&f);
    assert!(!f.home.join(".local/share/irsync").exists());
    // The installer's own messages reach ssh's stderr.
    let err = std::fs::read_to_string(&f.err).unwrap();
    assert!(err.contains("irsync: installed irsync"), "{err}");
}

#[test]
fn install_brings_fileset_and_pages_when_beside_the_exe() {
    let f = fake(None);
    let dir = f.aux.join("full");
    std::fs::create_dir(&dir).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_irsync"), dir.join("irsync")).unwrap();
    release::put_extras(&dir);
    set_exe_override(dir.join("irsync"));
    let p = probe(&f.rsh, "myserver").unwrap();
    let r = ensure_installed(&f.rsh, "myserver", &p, false).unwrap();
    assert_eq!(
        r,
        Install::Copied {
            replaced: None,
            contents: ALL
        }
    );
    assert_extras_installed(&f);
}

#[test]
fn install_refuses_a_platform_without_a_release() {
    let f = fake(Some("Linux riscv64"));
    let p = probe(&f.rsh, "myserver").unwrap();
    let e = ensure_installed(&f.rsh, "myserver", &p, false).unwrap_err();
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
    assert!(!f.home.join(".local").exists());
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
    let r = ensure_installed(&f.rsh, "myserver", &p, false).unwrap();
    assert_eq!(
        r,
        Install::Downloaded {
            replaced: None,
            platform: platform.into(),
            contents: ALONE
        }
    );
    assert_eq!(
        std::fs::read(f.home.join(REMOTE_BIN)).unwrap(),
        this_binary()
    );
    let p = probe(&f.rsh, "myserver").unwrap();
    assert_eq!(
        ensure_installed(&f.rsh, "myserver", &p, false).unwrap(),
        Install::Current
    );
}

#[test]
fn release_install_brings_everything() {
    let (uname, target, platform) = release::other_platform();
    let f = fake(Some(uname));
    let rel = tempfile::tempdir().unwrap();
    let ours = fake_exe(&f, Some(&format!("irsync {}", *LONG_VERSION)));
    set_release_url(&release::full_release(rel.path(), target, &ours));
    let p = probe(&f.rsh, "myserver").unwrap();
    let r = ensure_installed(&f.rsh, "myserver", &p, false).unwrap();
    assert_eq!(
        r,
        Install::Downloaded {
            replaced: None,
            platform: platform.into(),
            contents: ALL
        }
    );
    assert_eq!(
        std::fs::read(f.home.join(REMOTE_BIN)).unwrap(),
        std::fs::read(&ours).unwrap()
    );
    assert_extras_installed(&f);
    assert_no_work_dir(&f);
}

#[test]
fn same_platform_release_build_downloads_first() {
    let Some((target, platform)) = this_platform() else {
        return;
    };
    let f = fake(None);
    let rel = tempfile::tempdir().unwrap();
    let ours = fake_exe(&f, Some(&format!("irsync {}", *LONG_VERSION)));
    set_release_url(&release::full_release(rel.path(), target, &ours));
    let p = probe(&f.rsh, "myserver").unwrap();
    let r = ensure_installed(&f.rsh, "myserver", &p, false).unwrap();
    assert_eq!(
        r,
        Install::Downloaded {
            replaced: None,
            platform,
            contents: ALL
        }
    );
    assert_eq!(
        std::fs::read(f.home.join(REMOTE_BIN)).unwrap(),
        std::fs::read(&ours).unwrap()
    );
}

#[test]
fn same_platform_falls_back_when_the_download_fails() {
    if this_platform().is_none() {
        return;
    }
    let f = fake(None);
    let rel = tempfile::tempdir().unwrap();
    set_release_url(&release::url_of(rel.path()));
    let p = probe(&f.rsh, "myserver").unwrap();
    let r = ensure_installed(&f.rsh, "myserver", &p, false).unwrap();
    assert_eq!(
        r,
        Install::Copied {
            replaced: None,
            contents: ALONE
        }
    );
    assert_eq!(
        std::fs::read(f.home.join(REMOTE_BIN)).unwrap(),
        this_binary()
    );
}

#[test]
fn same_platform_falls_back_when_the_release_is_another_build() {
    let Some((target, _)) = this_platform() else {
        return;
    };
    let f = fake(None);
    let rel = tempfile::tempdir().unwrap();
    let other = fake_exe(&f, Some("irsync 0.0.0 (other)"));
    set_release_url(&release::full_release(rel.path(), target, &other));
    let p = probe(&f.rsh, "myserver").unwrap();
    let r = ensure_installed(&f.rsh, "myserver", &p, false).unwrap();
    assert_eq!(
        r,
        Install::Copied {
            replaced: None,
            contents: ALONE
        }
    );
    assert_eq!(
        std::fs::read(f.home.join(REMOTE_BIN)).unwrap(),
        this_binary()
    );
    assert_no_work_dir(&f);
}

#[test]
fn other_platform_has_no_fallback() {
    let (uname, target, _) = release::other_platform();
    let f = fake(Some(uname));
    let rel = tempfile::tempdir().unwrap();
    let other = fake_exe(&f, Some("irsync 0.0.0 (other)"));
    set_release_url(&release::full_release(rel.path(), target, &other));
    let p = probe(&f.rsh, "myserver").unwrap();
    let e = ensure_installed(&f.rsh, "myserver", &p, false)
        .unwrap_err()
        .to_string();
    assert!(e.contains("is not this build"), "{e}");
    assert!(!f.home.join(".local").exists());
    assert_no_work_dir(&f);
}

#[test]
fn home_with_a_space() {
    let f = fake_at(None, "my home");
    let p = probe(&f.rsh, "myserver").unwrap();
    let r = ensure_installed(&f.rsh, "myserver", &p, false).unwrap();
    assert_eq!(
        r,
        Install::Copied {
            replaced: None,
            contents: ALONE
        }
    );
    assert_eq!(
        std::fs::read(f.home.join(REMOTE_BIN)).unwrap(),
        this_binary()
    );
    assert_no_work_dir(&f);
    let p = probe(&f.rsh, "myserver").unwrap();
    assert_eq!(
        ensure_installed(&f.rsh, "myserver", &p, false).unwrap(),
        Install::Current
    );
}

#[test]
fn server_without_tar_changes_nothing() {
    let f = fake(None);
    // SAFETY: serialized by ENV, held in `f`.
    unsafe { std::env::set_var("FAKE_SSH_HIDE", "tar") };
    let p = probe(&f.rsh, "myserver").unwrap();
    let e = ensure_installed(&f.rsh, "myserver", &p, false)
        .unwrap_err()
        .to_string();
    assert!(e.contains("could not unpack"), "{e}");
    assert!(e.contains("Nothing was changed"), "{e}");
    let left: Vec<_> = std::fs::read_dir(&f.home).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
}

/// Gives `dir` its permissions back when dropped, so the temporary
/// directory holding it can be removed.
struct Restore(PathBuf, u32);

impl Drop for Restore {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(self.1));
    }
}

#[test]
fn unwritable_home_changes_nothing() {
    if nix::unistd::geteuid().is_root() {
        eprintln!("skipped: root can write to a read-only directory");
        return;
    }
    let f = fake(None);
    let p = probe(&f.rsh, "myserver").unwrap();
    let mode = f.home.metadata().unwrap().permissions().mode() & 0o7777;
    std::fs::set_permissions(&f.home, std::fs::Permissions::from_mode(0o555)).unwrap();
    let _restore = Restore(f.home.clone(), mode);
    let e = ensure_installed(&f.rsh, "myserver", &p, false)
        .unwrap_err()
        .to_string();
    assert!(e.contains("could not unpack"), "{e}");
    assert!(e.contains("Nothing was changed"), "{e}");
    let left: Vec<_> = std::fs::read_dir(&f.home).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn a_release_that_is_not_this_build_is_not_installed() {
    let (uname, target, _) = release::other_platform();
    let f = fake(Some(uname));
    let rel = tempfile::tempdir().unwrap();
    let other = fake_exe(&f, Some("irsync 9.9.9 (other)"));
    set_release_url(&release::fake_release(rel.path(), target, &other));
    let p = probe(&f.rsh, "myserver").unwrap();
    let e = ensure_installed(&f.rsh, "myserver", &p, false)
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
    let e = ensure_installed(&f.rsh, "myserver", &p, false)
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
    let e = ensure_installed(&f.rsh, "myserver", &p, false)
        .unwrap_err()
        .to_string();
    assert!(e.contains("could not download"), "{e}");
    assert!(!f.home.join(".local").exists());
}

#[test]
fn install_replaces_other_version() {
    let f = fake(None);
    let dst = f.home.join(REMOTE_BIN);
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::write(&dst, "#!/bin/sh\necho 'irsync 0.0.1 (old)'\n").unwrap();
    std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755)).unwrap();
    let p = probe(&f.rsh, "myserver").unwrap();
    assert_eq!(p.installed.as_deref(), Some("irsync 0.0.1 (old)"));
    let r = ensure_installed(&f.rsh, "myserver", &p, false).unwrap();
    assert_eq!(
        r,
        Install::Copied {
            replaced: Some("irsync 0.0.1 (old)".into()),
            contents: ALONE
        }
    );
    assert_eq!(std::fs::read(&dst).unwrap(), this_binary());
}

/// A one-line script standing in for ssh, outside the fake home.
fn script(f: &Fake, name: &str, body: &str) -> Rsh {
    let p = f.aux.join(name);
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
    let r = ensure_installed(&wrapper, "myserver", &p, false).unwrap();
    assert_eq!(
        r,
        Install::Copied {
            replaced: None,
            contents: ALONE
        }
    );
    let p = probe(&wrapper, "myserver").unwrap();
    assert_eq!(
        ensure_installed(&wrapper, "myserver", &p, false).unwrap(),
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
    ensure_installed(&f.rsh, "192.168.1.5", &p, false).unwrap();
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
    let all = SetupFlags {
        recheck: true,
        no_dht: true,
        no_install: true,
    };
    let cmd = setup_command(id, nasty, all);
    let fixed = format!("\"$HOME/{REMOTE_BIN}\" ssh-setup --client {id} --client-name-hex ");
    let rest = cmd.strip_prefix(&fixed).unwrap_or_else(|| panic!("{cmd}"));
    let (hex, flags) = rest.split_once(' ').unwrap();
    assert_eq!(flags, "--recheck --no-dht --keep-old");
    assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()), "{hex}");
    assert_eq!(decode_name(hex).unwrap(), nasty);
    assert!(setup_command(id, "myclient", SetupFlags::default()).ends_with(" 6d79636c69656e74"));
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
    // The real one: what `uname -n` prints, cut short, or an error when
    // that is no usable name.
    let out = std::process::Command::new("uname")
        .arg("-n")
        .output()
        .unwrap();
    let real = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(client_host_name().ok(), short_host_name(&real), "{real:?}");
}

/// The probe says whether the ssh user is root (`id -u` is 0).
#[test]
fn probe_sees_root() {
    let f = fake(Some("Linux x86_64"));
    assert!(!probe(&f.rsh, "myserver").unwrap().root || nix::unistd::geteuid().is_root());
    let bin = f.aux.join("fake-bin");
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
    let e = ensure_installed(&wrapper, "myserver", &p, false).unwrap_err();
    assert!(e.to_string().contains("cut short"), "{e}");
    assert_nothing_installed(&f);
}

/// A stand-in binary that reports `version` (or fails when `None`), alone
/// in a directory outside the fake home.
fn fake_exe(f: &Fake, version: Option<&str>) -> PathBuf {
    let dir = f.aux.join("fake-exe");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("fake-irsync");
    let body = match version {
        Some(v) => format!("#!/bin/sh\necho '{v}'\n"),
        None => "#!/bin/sh\nexit 126\n".to_owned(),
    };
    std::fs::write(&p, body).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// No entry in the fake home is the install's work directory.
fn assert_no_work_dir(f: &Fake) {
    let left: Vec<String> = std::fs::read_dir(&f.home)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".irsync-install"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

/// A fresh home stays as it was: no `.local`, no work directory.
fn assert_nothing_installed(f: &Fake) {
    assert!(!f.home.join(".local").exists());
    assert_no_work_dir(f);
}

/// `fileset` and the three man pages are in place under `~/.local`.
fn assert_extras_installed(f: &Fake) {
    let fileset = f.home.join(".local/bin/fileset");
    assert_ne!(fileset.metadata().unwrap().permissions().mode() & 0o111, 0);
    for page in ["man1/irsync.1", "man1/fileset.1", "man5/irsync.toml.5"] {
        let p = f.home.join(".local/share/man").join(page);
        assert!(p.is_file(), "{} missing", p.display());
    }
}

#[test]
fn install_rejects_a_copy_that_reports_another_version() {
    let f = fake(None);
    set_exe_override(fake_exe(&f, Some("irsync 9.9.9 (other)")));
    let p = probe(&f.rsh, "myserver").unwrap();
    let e = ensure_installed(&f.rsh, "myserver", &p, false)
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
    let e = ensure_installed(&f.rsh, "myserver", &p, false)
        .unwrap_err()
        .to_string();
    assert!(e.contains("does not run on myserver"), "{e}");
    assert_nothing_installed(&f);
}

#[test]
fn a_rejected_copy_keeps_the_old_binary() {
    let f = fake(None);
    let dst = f.home.join(REMOTE_BIN);
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::write(&dst, "#!/bin/sh\necho 'irsync 0.0.1 (old)'\n").unwrap();
    std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755)).unwrap();
    set_exe_override(fake_exe(&f, Some("irsync 9.9.9 (other)")));
    let p = probe(&f.rsh, "myserver").unwrap();
    ensure_installed(&f.rsh, "myserver", &p, false).unwrap_err();
    assert!(std::fs::read_to_string(&dst)
        .unwrap()
        .contains("0.0.1 (old)"));
    assert_eq!(std::fs::read_dir(dst.parent().unwrap()).unwrap().count(), 1);
    assert_no_work_dir(&f);
}

/// A server command that prints nothing does not shift the lines after
/// it: with a silent `uname -sm`, the host name is still the host name
/// and the version line is not taken for it.
#[test]
fn probe_keeps_its_lines_apart_when_one_is_empty() {
    let f = fake(None);
    // No stand-in from the fake ssh: the `uname` below is the one found.
    // SAFETY: serialized by ENV (held by `f`).
    unsafe { std::env::remove_var("FAKE_SSH_HOSTNAME") };
    let bin = f.home.join("fake-bin");
    std::fs::create_dir(&bin).unwrap();
    let uname = bin.join("uname");
    std::fs::write(
        &uname,
        "#!/bin/sh\nif [ \"$1\" = -n ]; then echo myserver.example.com; fi\n",
    )
    .unwrap();
    std::fs::set_permissions(&uname, std::fs::Permissions::from_mode(0o755)).unwrap();
    let wrapper = script(
        &f,
        "quiet-uname-ssh",
        &format!(
            "PATH={}:$PATH exec {} \"$@\"",
            bin.display(),
            fake_ssh().display()
        ),
    );
    let p = probe(&wrapper, "192.168.1.5").unwrap();
    assert_eq!((p.os, p.arch), (None, None));
    assert_eq!(p.hostname.as_deref(), Some("myserver"));
    assert_eq!(p.installed, None);
    assert!(!p.root || nix::unistd::geteuid().is_root());
    // With irsync there, its version is the version, not the host name.
    let exe = f.home.join(irsync::ssh::client::REMOTE_BIN);
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, "#!/bin/sh\necho irsync 9.9.9\necho more\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let p = probe(&wrapper, "192.168.1.5").unwrap();
    assert_eq!(p.hostname.as_deref(), Some("myserver"));
    assert_eq!(p.installed.as_deref(), Some("irsync 9.9.9"));
}
