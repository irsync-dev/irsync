//! The client's first run against an unknown host: it sets the server up
//! over (fake) ssh, saves the alias, then syncs over iroh.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use irsync::daemon_lock::probe;

fn fake_ssh() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-ssh")
}

/// A free loopback UDP port (iroh listens on UDP).
fn free_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A client HOME, a fake server HOME and the ssh log; kills the daemon
/// that `ssh-setup` started when dropped.
struct World {
    client: tempfile::TempDir,
    server: tempfile::TempDir,
    src: tempfile::TempDir,
    port: u16,
    uname: Option<String>,
}

impl World {
    fn new() -> World {
        let w = World {
            client: tempfile::tempdir().unwrap(),
            server: tempfile::tempdir().unwrap(),
            src: tempfile::tempdir().unwrap(),
            port: free_port(),
            uname: None,
        };
        w.server_config("", "");
        std::fs::write(w.src.path().join("a.txt"), "hello\n").unwrap();
        std::fs::create_dir(w.src.path().join("sub")).unwrap();
        std::fs::write(w.src.path().join("sub/b.txt"), "world\n").unwrap();
        w
    }

    /// An offline server config listening on `self.port`.
    fn server_config(&self, daemon_extra: &str, extra: &str) {
        let cfg = self.server.path().join(".config/irsync");
        std::fs::create_dir_all(&cfg).unwrap();
        let text = format!(
            "[daemon]\nrelay_mode = \"disabled\"\nportmap = false\n\
             listen = [\"127.0.0.1:{}\"]\n{daemon_extra}\
             [daemon.discovery]\ndht = false\ndns = false\nmdns = false\n{extra}",
            self.port
        );
        std::fs::write(cfg.join("irsync.toml"), text).unwrap();
    }

    fn ssh_log(&self) -> PathBuf {
        self.client.path().join("ssh.log")
    }

    fn ssh_calls(&self) -> usize {
        std::fs::read_to_string(self.ssh_log())
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    fn client_config(&self) -> String {
        std::fs::read_to_string(self.client.path().join(".config/irsync/irsync.toml"))
            .unwrap_or_default()
    }

    fn src_arg(&self) -> String {
        format!("{}/", self.src.path().display())
    }

    /// Runs the client irsync with `args` plus `--address`, bounded to 60 s.
    fn irsync(&self, args: &[&str]) -> Output {
        let addr = format!("127.0.0.1:{}", self.port);
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_irsync"));
        cmd.args(args)
            .args(["--address", &addr])
            .env("HOME", self.client.path())
            .env("XDG_STATE_HOME", self.client.path().join("xdg"))
            .env("IRSYNC_RSH", fake_ssh())
            .env("FAKE_SSH_HOME", self.server.path())
            .env("FAKE_SSH_LOG", self.ssh_log())
            .env_remove("IRSYNC_PASSPHRASE")
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match &self.uname {
            Some(u) => cmd.env("FAKE_SSH_UNAME", u),
            None => cmd.env_remove("FAKE_SSH_UNAME"),
        };
        let mut child = cmd.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("irsync {args:?} did not finish within 60 s");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        child.wait_with_output().unwrap()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let state = self.server.path().join("xdg/irsync");
        if let Some(info) = probe(&state) {
            if info.pid != 0 {
                let _ = Command::new("kill")
                    .args(["-9", &info.pid.to_string()])
                    .status();
            }
        }
    }
}

fn ok(o: &Output) -> String {
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(
        o.status.success(),
        "irsync failed ({})\nstdout: {}\nstderr: {err}",
        o.status,
        String::from_utf8_lossy(&o.stdout)
    );
    err
}

fn same_tree(a: &Path, b: &Path) {
    common::assert_trees_equal(a, b, &common::archive_opts().preserve);
}

#[test]
fn first_push_sets_up_and_syncs() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let o = w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]);
    let err = ok(&o);
    same_tree(w.src.path(), &dest);
    for want in [
        "set up myserver over ssh:",
        "installed irsync",
        "started the daemon",
        "saved host alias myserver",
        "user service",
    ] {
        assert!(err.contains(want), "missing {want:?} in stderr:\n{err}");
    }
    let cfg = w.client_config();
    assert!(cfg.contains("[hosts.myserver]"), "{cfg}");
    assert!(cfg.contains("ssh = \"myserver\""), "{cfg}");
    assert_eq!(w.ssh_calls(), 3);
}

#[test]
fn second_push_uses_no_ssh() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let target = format!("myserver:{}", dest.display());
    ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    assert_eq!(w.ssh_calls(), 3);
    std::fs::write(w.src.path().join("a.txt"), "changed\n").unwrap();
    let err = ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    assert!(!err.contains("set up"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dest.join("a.txt")).unwrap(),
        "changed\n"
    );
    assert_eq!(w.ssh_calls(), 3);
}

#[test]
fn user_at_host_alias() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let target = format!("u@myserver:{}", dest.display());
    let err = ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    assert!(err.contains("saved host alias u@myserver"), "{err}");
    let cfg = w.client_config();
    assert!(cfg.contains("ssh = \"u@myserver\""), "{cfg}");
    std::fs::write(w.src.path().join("a.txt"), "again\n").unwrap();
    ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    assert_eq!(
        std::fs::read_to_string(dest.join("a.txt")).unwrap(),
        "again\n"
    );
    assert_eq!(w.ssh_calls(), 3);
}

#[test]
fn immediate_off_still_saves_alias() {
    let w = World::new();
    // The client identity first, so the export can be granted to it.
    let out = common::irsync_bin(w.client.path(), &["id", "generate"]);
    assert!(out.status.success(), "{out:?}");
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    let id = text
        .split_whitespace()
        .find(|w| irsync::identity::parse_endpoint_id(w).is_ok())
        .unwrap_or_else(|| panic!("no EndpointId in {text}"))
        .to_string();
    let dest = w.server.path().join("dest");
    std::fs::create_dir(&dest).unwrap();
    w.server_config(
        "immediate = false\n",
        &format!(
            "[exports.dest]\npath = {:?}\nrw = [\"{id}\"]\n",
            dest.display()
        ),
    );

    let err = ok(&w.irsync(&["-a", &w.src_arg(), "myserver:dest"]));
    assert!(err.contains("immediate mode is off on myserver"), "{err}");
    same_tree(w.src.path(), &dest);
    let cfg = w.client_config();
    assert!(cfg.contains("[hosts.myserver]"), "{cfg}");
}

#[test]
fn other_platform_changes_nothing() {
    let mut w = World::new();
    w.uname = Some("Linux riscv64".into());
    std::fs::remove_dir_all(w.server.path().join(".config")).unwrap();
    let dest = w.server.path().join("dest");
    let o = w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(err.contains("install irsync on myserver by hand"), "{err}");
    assert!(
        !w.client_config().contains("[hosts"),
        "{}",
        w.client_config()
    );
    assert!(!w.server.path().join(".config/irsync").exists());
    assert!(!w.server.path().join(".local").exists());
    assert!(!w.server.path().join("xdg").exists());
}
