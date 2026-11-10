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

    /// Runs the client irsync with `args` plus `--address`, bounded to 180 s
    /// (a dial to a dead daemon takes the 60 s handshake timeout).
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
        let deadline = Instant::now() + Duration::from_secs(180);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("irsync {args:?} did not finish within 180 s");
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
        "irsync: myserver is not a saved alias; setting it up over ssh",
        "irsync: set up myserver over ssh:",
        "irsync:   installed irsync",
        "irsync:   started the daemon",
        "irsync:   saved host alias myserver",
    ] {
        assert!(err.contains(want), "missing {want:?} in stderr:\n{err}");
    }
    // The notice comes before the report; the how-to is unprefixed so it
    // pastes.
    assert!(err.find("not a saved alias") < err.find("set up myserver"));
    let lines: Vec<&str> = err.lines().collect();
    for want in [
        "To keep the daemon running after a reboot, make it a user service.",
        "[Service]",
        "  systemctl --user enable --now irsync",
    ] {
        assert!(lines.contains(&want), "no line {want:?} in stderr:\n{err}");
    }
    assert!(
        lines.iter().any(|l| l.starts_with("  pid=\"$(sed -n")),
        "{err}"
    );
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

#[test]
fn unsavable_alias_stops_before_ssh() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    // `all` means every peer: it can never be an alias.
    let o = w.irsync(&["-a", &w.src_arg(), &format!("all:{}", dest.display())]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(!err.contains("over ssh"), "{err}");
    // A split hosts.toml can't take the alias either.
    let cfg = w.client.path().join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(cfg.join("hosts.toml"), "").unwrap();
    let o = w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(err.contains("hosts.toml"), "{err}");
    assert!(!err.contains("over ssh"), "{err}");
    assert_eq!(w.ssh_calls(), 0);
    assert!(!w.server.path().join(".local").exists());
}

impl World {
    /// Kills the daemon `ssh-setup` started and waits until it is gone.
    fn kill_daemon(&self) {
        let info = probe(&self.server.path().join("xdg/irsync")).expect("a running daemon");
        let pid = info.pid.to_string();
        Command::new("kill").args(["-9", &pid]).status().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Command::new("kill")
            .args(["-0", &pid])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
        {
            assert!(Instant::now() < deadline, "daemon {pid} did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The `endpoint_id` saved for `alias` in the client config.
    fn saved_id(&self, alias: &str) -> String {
        let doc: toml::Table = self.client_config().parse().unwrap();
        doc["hosts"][alias]["endpoint_id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn server_toml(&self) -> String {
        std::fs::read_to_string(self.server.path().join(".config/irsync/irsync.toml")).unwrap()
    }
}

/// The client's EndpointId, as `irsync id show` prints it.
fn client_id(home: &Path) -> String {
    let out = common::irsync_bin(home, &["id", "show"]);
    assert!(out.status.success(), "{out:?}");
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    text.split_whitespace()
        .find(|w| irsync::identity::parse_endpoint_id(w).is_ok())
        .unwrap_or_else(|| panic!("no EndpointId in {text}"))
        .to_string()
}

#[test]
fn dead_daemon_is_restarted_over_ssh() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let target = format!("myserver:{}", dest.display());
    ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    let calls = w.ssh_calls();
    w.kill_daemon();
    std::fs::write(w.src.path().join("a.txt"), "changed\n").unwrap();
    // Default retry settings: one short dial, then ssh, well under the
    // 60 s a single dial to a dead daemon would take.
    let started = Instant::now();
    let err = ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    assert!(started.elapsed() < Duration::from_secs(40), "{err}");
    assert!(
        err.contains("irsync: myserver did not answer; checking it over ssh"),
        "{err}"
    );
    assert!(err.contains("started the daemon"), "{err}");
    // Same id: the alias line is left out.
    assert!(!err.contains("host alias"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dest.join("a.txt")).unwrap(),
        "changed\n"
    );
    assert!(w.ssh_calls() > calls);
}

#[test]
fn rebuilt_server_updates_alias() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let target = format!("myserver:{}", dest.display());
    ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    let before = w.saved_id("myserver");
    w.kill_daemon();
    for entry in std::fs::read_dir(w.server.path().join(".config/irsync")).unwrap() {
        let p = entry.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if name.starts_with("daemon_ed25519") {
            std::fs::remove_file(p).unwrap();
        }
    }
    let started = Instant::now();
    let err = ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    assert!(started.elapsed() < Duration::from_secs(40), "{err}");
    assert!(err.contains("updated host alias myserver:"), "{err}");
    assert_ne!(w.saved_id("myserver"), before);
    same_tree(w.src.path(), &dest);
}

#[test]
fn refusal_never_uses_ssh() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    ok(&w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]));
    let calls = w.ssh_calls();
    let id = client_id(w.client.path());
    let out = common::irsync_bin(w.server.path(), &["grant", "remove", "immediate", &id]);
    assert!(out.status.success(), "{out:?}");
    let other = w.server.path().join("other");
    let o = w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", other.display())]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(
        err.contains("the daemon rejected this client's key"),
        "{err}"
    );
    assert!(!err.contains("over ssh"), "{err}");
    assert_eq!(w.ssh_calls(), calls);
    let doc: toml::Table = w.server_toml().parse().unwrap();
    let granted = doc
        .get("immediate")
        .and_then(|i| i.get("rw"))
        .and_then(|rw| rw.as_array())
        .is_some_and(|rw| rw.iter().any(|v| v.as_str() == Some(id.as_str())));
    assert!(!granted, "{}", w.server_toml());
}

/// Whether the server config grants `id` immediate access.
fn immediate_granted(w: &World, id: &str) -> bool {
    let doc: toml::Table = w.server_toml().parse().unwrap();
    doc.get("immediate")
        .and_then(|i| i.get("rw"))
        .and_then(|rw| rw.as_array())
        .is_some_and(|rw| rw.iter().any(|v| v.as_str() == Some(id)))
}

/// The owner removed the immediate grant and the daemon is down: the ssh
/// check restarts the daemon but must not put the grant back; the sync
/// stops with the refusal's hint and is not retried.
#[test]
fn fallback_after_grant_removed_does_not_regrant() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let target = format!("myserver:{}", dest.display());
    ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    let id = client_id(w.client.path());
    let out = common::irsync_bin(w.server.path(), &["grant", "remove", "immediate", &id]);
    assert!(out.status.success(), "{out:?}");
    w.kill_daemon();
    let other = w.server.path().join("other");
    let o = w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", other.display())]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(err.contains("checking it over ssh"), "{err}");
    assert!(
        err.contains("the daemon rejected this client's key"),
        "{err}"
    );
    assert!(!immediate_granted(&w, &id), "{}", w.server_toml());
    assert!(!other.exists());
}

/// ssh itself fails (here: `--rsh false`): the client says so, tries the
/// daemon once more with the usual settings, and reports both failures.
#[test]
fn ssh_failure_tries_the_daemon_again() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let target = format!("myserver:{}", dest.display());
    ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    w.kill_daemon();
    let o = w.irsync(&[
        "-a",
        "--rsh",
        "false",
        "--dial-retries",
        "0",
        &w.src_arg(),
        &target,
    ]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(
        err.contains("irsync: checking myserver over ssh failed: ")
            && err.contains("; trying the daemon again"),
        "{err}"
    );
    let last = err.lines().last().unwrap_or_default();
    assert!(last.contains("cannot reach myserver over false"), "{err}");
    assert!(last.contains("gave up after 1 attempt"), "{err}");
}

/// A host set up in this run that then does not answer is not checked
/// over ssh again straight away: the setup just did that.
#[test]
fn no_second_ssh_round_right_after_setup() {
    let mut w = World::new();
    // The daemon listens on the old port; the client dials another.
    w.port = free_port();
    let dest = w.server.path().join("dest");
    let o = w.irsync(&[
        "-a",
        "--dial-retries",
        "0",
        &w.src_arg(),
        &format!("myserver:{}", dest.display()),
    ]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(err.contains("saved host alias myserver"), "{err}");
    assert!(!err.contains("checking it over ssh"), "{err}");
    assert_eq!(w.ssh_calls(), 3, "{err}");
}

/// An alias whose name is not its ssh destination: the check goes to the
/// destination and updates that alias, not one named after the
/// destination.
#[test]
fn fallback_updates_the_alias_not_its_ssh_name() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    ok(&w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]));
    let path = w.client.path().join(".config/irsync/irsync.toml");
    let cfg = w.client_config().replace("[hosts.myserver]", "[hosts.nas]");
    std::fs::write(&path, cfg).unwrap();
    let before = w.saved_id("nas");
    w.kill_daemon();
    for entry in std::fs::read_dir(w.server.path().join(".config/irsync")).unwrap() {
        let p = entry.unwrap().path();
        if p.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("daemon_ed25519")
        {
            std::fs::remove_file(p).unwrap();
        }
    }
    let err = ok(&w.irsync(&["-a", &w.src_arg(), &format!("nas:{}", dest.display())]));
    assert!(
        err.contains("nas did not answer; checking it over ssh"),
        "{err}"
    );
    assert!(err.contains("updated host alias nas:"), "{err}");
    assert_ne!(w.saved_id("nas"), before);
    let cfg = w.client_config();
    assert!(!cfg.contains("[hosts.myserver]"), "{cfg}");
    assert!(cfg.contains("ssh = \"myserver\""), "{cfg}");
    same_tree(w.src.path(), &dest);
}

#[test]
fn alias_without_ssh_does_not_fall_back() {
    let w = World::new();
    let dead = iroh::SecretKey::from_bytes(&[9; 32]).public().to_string();
    let out = common::irsync_bin(w.client.path(), &["host", "add", "myserver", &dead]);
    assert!(out.status.success(), "{out:?}");
    let dest = w.server.path().join("dest");
    let o = w.irsync(&[
        "-a",
        "--dial-retries",
        "0",
        &w.src_arg(),
        &format!("myserver:{}", dest.display()),
    ]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(err.contains("gave up after 1 attempt"), "{err}");
    assert!(!err.contains("over ssh"), "{err}");
    assert_eq!(w.ssh_calls(), 0);
}
