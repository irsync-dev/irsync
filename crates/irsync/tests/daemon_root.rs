//! `irsync daemon` as root: refused unless the server's owner opts in with
//! the flag or the config key. The tests make the binary believe it is
//! root through `IRSYNC_TEST_EUID_ROOT`, which only a test build reads.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use irsync::daemon_lock::probe;

/// An offline daemon config (no relay, no port mapping, no discovery) on a
/// loopback port the OS picks, with `extra` under `[daemon]`.
fn config(extra: &str) -> String {
    format!(
        "[daemon]\nrelay_mode = \"disabled\"\nportmap = false\n\
         listen = [\"127.0.0.1:0\"]\n{extra}\
         [daemon.discovery]\ndht = false\ndns = false\nmdns = false\n"
    )
}

/// A fresh home with `text` as its `irsync.toml`.
fn home(text: &str) -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join(".config/irsync")).unwrap();
    irsync::fs::private::write(home.join(".config/irsync/irsync.toml"), text).unwrap();
    (d, home)
}

/// What one `irsync daemon` run did.
struct Run {
    /// It took the daemon lock (and was then sent SIGTERM).
    started: bool,
    /// Its exit code when it ended on its own.
    code: Option<i32>,
    err: String,
}

/// Runs `irsync daemon ARGS` under `home` until it exits on its own or is
/// seen running, then stops it.
fn daemon(home: &Path, as_root: bool, args: &[&str]) -> Run {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_irsync"));
    cmd.arg("daemon")
        .args(args)
        .env("HOME", home)
        .env("XDG_STATE_HOME", home.join("xdg"))
        .env_remove("IRSYNC_PASSPHRASE")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if as_root {
        cmd.env("IRSYNC_TEST_EUID_ROOT", "1");
    } else {
        cmd.env_remove("IRSYNC_TEST_EUID_ROOT");
    }
    let mut child = cmd.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut started = false;
    let mut on_its_own = true;
    while child.try_wait().unwrap().is_none() {
        if probe(&home.join("xdg/irsync")).is_some() {
            started = true;
            on_its_own = false;
            let pid = child.id().to_string();
            Command::new("kill").args(["-TERM", &pid]).status().unwrap();
            break;
        }
        assert!(
            Instant::now() < deadline,
            "daemon neither started nor exited"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().unwrap();
    Run {
        started,
        code: if on_its_own { out.status.code() } else { None },
        err: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn really_root() -> bool {
    irsync::platform::is_root()
}

#[test]
fn as_root_the_daemon_refuses_and_creates_nothing() {
    let (_d, home) = home(&config(""));
    let r = daemon(&home, true, &[]);
    assert!(!r.started, "{}", r.err);
    assert_eq!(r.code, Some(1), "{}", r.err);
    for part in [
        "irsync daemon: refusing to run as root: the daemon has not had a security audit,",
        "  and as root any client with write access can create setuid files and device nodes.",
        "  Run it as an unprivileged account. To run as root anyway, pass",
        "  --run-as-root-i-know-what-i-am-doing or set `allow_root = true` under [daemon].",
    ] {
        assert!(r.err.contains(part), "{}", r.err);
    }
    assert!(!home.join(".config/irsync/daemon_ed25519").exists());
    assert!(!home.join("xdg").exists());
}

#[test]
fn as_root_the_flag_lets_the_daemon_start_with_a_warning() {
    let (_d, home) = home(&config(""));
    let r = daemon(&home, true, &["--run-as-root-i-know-what-i-am-doing"]);
    assert!(r.started, "{}", r.err);
    assert!(r.err.contains("running as root"), "{}", r.err);
    assert!(!r.err.contains("refusing to run as root"), "{}", r.err);
}

/// The key counts in the file the daemon is given, not only the default one.
#[test]
fn as_root_the_config_key_lets_the_daemon_start_from_another_file() {
    let (d, home) = home(&config(""));
    let other = d.path().join("other.toml");
    irsync::fs::private::write(&other, config("allow_root = true\n")).unwrap();
    let r = daemon(&home, true, &["--config", other.to_str().unwrap()]);
    assert!(r.started, "{}", r.err);
    assert!(r.err.contains("running as root"), "{}", r.err);
}

/// A config that cannot be read is the error, not "refusing to run as
/// root": the owner may have set the key in it.
#[test]
fn as_root_a_broken_config_is_reported_as_such() {
    let (_d, home) = home("[daemon]\nallow_root = true\n[hosts\n");
    let r = daemon(&home, true, &[]);
    assert!(!r.started, "{}", r.err);
    assert_eq!(r.code, Some(1), "{}", r.err);
    assert!(r.err.contains("irsync.toml"), "{}", r.err);
    assert!(!r.err.contains("refusing to run as root"), "{}", r.err);
}

#[test]
fn not_as_root_nothing_changes() {
    if really_root() {
        eprintln!("skipping: running as root");
        return;
    }
    let (_d, home) = home(&config(""));
    let r = daemon(&home, false, &[]);
    assert!(r.started, "{}", r.err);
    assert!(!r.err.contains("root"), "{}", r.err);
}

/// `irsync config set` knows the key as a boolean.
#[test]
fn config_set_writes_the_key_as_a_boolean() {
    let (_d, home) = home("");
    let set = |value: &str| {
        Command::new(env!("CARGO_BIN_EXE_irsync"))
            .args(["config", "set", "daemon.allow_root", value])
            .env("HOME", &home)
            .env("XDG_STATE_HOME", home.join("xdg"))
            .output()
            .unwrap()
    };
    let o = set("true");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        std::fs::read_to_string(home.join(".config/irsync/irsync.toml")).unwrap(),
        "config_version = 2\n\n[daemon]\nallow_root = true\n"
    );
    assert!(!set("yes").status.success());
}

/// The warning is not a log line: `-q` and `RUST_LOG` cannot hide it.
#[test]
fn as_root_the_warning_survives_quiet() {
    let (_d, home) = home(&config(""));
    let r = daemon(&home, true, &["-q", "--run-as-root-i-know-what-i-am-doing"]);
    assert!(r.started, "{}", r.err);
    assert!(
        r.err.contains("irsync daemon: warning: running as root"),
        "{}",
        r.err
    );
}
