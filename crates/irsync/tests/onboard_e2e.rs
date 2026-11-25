//! The client's first run against an unknown host: it sets the server up
//! over (fake) ssh, saves the alias, then syncs over iroh.
#![cfg(unix)]

mod common;
#[path = "common/release.rs"]
mod release;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use irsync::daemon_lock::probe;

fn fake_ssh() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-ssh")
}

/// Stand-ins for systemctl and loginctl.
fn fake_service_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-service-bin")
}

/// A free loopback UDP port (iroh listens on UDP) for a daemon that binds
/// it later. Taken below the ports the OS hands out for `:0` (from 49152 on
/// macOS, 32768 on Linux): such a port, released here, could go to another
/// test's socket before the daemon binds it. A counter keeps this process's
/// tests apart, the pid other processes.
fn free_port() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    const BASE: u16 = 20_000;
    const SPAN: u16 = 12_000;
    static NEXT: AtomicU16 = AtomicU16::new(0);
    let start = (std::process::id() % u32::from(SPAN)) as u16;
    for _ in 0..SPAN {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let port = BASE + (start + n % SPAN) % SPAN;
        if std::net::UdpSocket::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
    panic!("no free UDP port in {BASE}..{}", BASE + SPAN);
}

/// A client HOME, a fake server HOME and the ssh log; kills the daemon
/// that `ssh-setup` started when dropped.
struct World {
    client: tempfile::TempDir,
    server: tempfile::TempDir,
    src: tempfile::TempDir,
    port: u16,
    /// Where a daemon in the client's home listens, once
    /// [`World::client_daemon_config`] wrote its config.
    client_port: u16,
    uname: Option<String>,
    /// Where the client downloads a release from, instead of GitHub.
    release_url: Option<String>,
    /// This client's host name, as its irsync sees it.
    client_name: String,
    /// What the server's `uname -n` prints; `None`: an empty line.
    hostname: Option<String>,
    /// The ssh probe's `id -u` prints 0.
    probe_root: bool,
    /// The server's irsync believes it runs as root (the probe does not
    /// say so).
    server_root: bool,
    /// The server has a (fake) systemd user session: `pair` finds
    /// systemctl and loginctl there. Without it, no service manager.
    service: bool,
    /// Extra environment for every irsync run, such as the `FAKE_*`
    /// settings of the fake service tools.
    env: Vec<(String, String)>,
}

impl World {
    fn new() -> World {
        let w = World {
            client: tempfile::tempdir().unwrap(),
            server: tempfile::tempdir().unwrap(),
            src: tempfile::tempdir().unwrap(),
            port: free_port(),
            client_port: free_port(),
            uname: None,
            release_url: None,
            client_name: "myclient".into(),
            hostname: None,
            probe_root: false,
            server_root: false,
            service: false,
            env: Vec::new(),
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
            "[daemon]\nrelay = \"disabled\"\nportmap = false\n\
             listen = [\"127.0.0.1:{}\"]\n{daemon_extra}\
             dht = false\ndns = false\nmdns = false\n{extra}",
            self.port
        );
        irsync::fs::private::write(cfg.join("irsync.toml"), text).unwrap();
    }

    fn ssh_log(&self) -> PathBuf {
        self.client.path().join("ssh.log")
    }

    /// The calls of the fake systemctl and loginctl, one per line.
    fn service_log(&self) -> String {
        std::fs::read_to_string(self.client.path().join("service.log")).unwrap_or_default()
    }

    /// The systemd unit in the server HOME.
    fn unit(&self) -> PathBuf {
        self.server
            .path()
            .join(".config/systemd/user/irsync.service")
    }

    /// The pid of the daemon the lock in the server HOME names.
    fn lock_pid(&self) -> u32 {
        probe(&self.server.path().join("xdg/irsync"))
            .expect("a running daemon")
            .pid
    }

    /// The pid the fake systemctl started last, if any.
    fn service_pid(&self) -> Option<u32> {
        std::fs::read_to_string(self.server.path().join(".fake-service.pid"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
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

    /// Pairs the client with `host`, over ssh; returns the stderr.
    fn pair(&self, host: &str) -> String {
        ok(&self.irsync_bare(&["pair", host]))
    }

    /// As [`World::pair`], run on the server machine, pairing it with the
    /// client machine.
    fn pair_back(&self, host: &str) -> String {
        ok(&self.run(
            self.server.path(),
            self.client.path(),
            "myserver",
            &["pair", host],
        ))
    }

    fn src_arg(&self) -> String {
        format!("{}/", self.src.path().display())
    }

    /// Runs the client irsync with `args` plus `--address`, bounded to 180 s.
    fn irsync(&self, args: &[&str]) -> Output {
        let addr = format!("127.0.0.1:{}", self.port);
        let mut all = args.to_vec();
        all.extend(["--address", &addr]);
        self.irsync_bare(&all)
    }

    /// As [`World::irsync`], without `--address`: the client dials what it
    /// learns from the setup and from its lookups.
    fn irsync_bare(&self, args: &[&str]) -> Output {
        self.run(
            self.client.path(),
            self.server.path(),
            &self.client_name,
            args,
        )
    }

    /// The other direction: irsync run on the server machine (host name
    /// `myserver`), whose ssh reaches the client machine. Dials the
    /// client's daemon at `client_port`.
    fn irsync_back(&self, args: &[&str]) -> Output {
        let addr = format!("127.0.0.1:{}", self.client_port);
        let mut all = args.to_vec();
        all.extend(["--address", &addr]);
        self.run(self.server.path(), self.client.path(), "myserver", &all)
    }

    /// A daemon config in the client's home, so the client machine can be
    /// set up as a server too.
    fn client_daemon_config(&self) {
        let cfg = self.client.path().join(".config/irsync");
        std::fs::create_dir_all(&cfg).unwrap();
        let text = format!(
            "[daemon]\nrelay = \"disabled\"\nportmap = false\n\
             listen = [\"127.0.0.1:{}\"]\n\
             dht = false\ndns = false\nmdns = false\n",
            self.client_port
        );
        irsync::fs::private::write(cfg.join("irsync.toml"), text).unwrap();
    }

    /// irsync run in `home` as the machine `name`, with a fake ssh that
    /// lands in `remote`.
    fn run(&self, home: &Path, remote: &Path, name: &str, args: &[&str]) -> Output {
        let mut cmd = common::irsync_command(home);
        cmd.args(args)
            .env("XDG_STATE_HOME", home.join("xdg"))
            .env("IRSYNC_RSH", fake_ssh())
            .env("FAKE_SSH_HOME", remote)
            .env("FAKE_SSH_LOG", home.join("ssh.log"))
            .env_remove("IRSYNC_PASSPHRASE")
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match &self.uname {
            Some(u) => cmd.env("FAKE_SSH_UNAME", u),
            None => cmd.env_remove("FAKE_SSH_UNAME"),
        };
        match &self.release_url {
            Some(u) => cmd.env("IRSYNC_TEST_RELEASE_URL", u),
            None => cmd.env_remove("IRSYNC_TEST_RELEASE_URL"),
        };
        // The fake releases are signed with this test key.
        cmd.env("IRSYNC_TEST_RELEASE_KEYS", release::keys_file());
        cmd.env("IRSYNC_TEST_HOSTNAME", name);
        // A dial to a daemon that is not there gives up after this long,
        // not after the real 15 s or 60 s; plenty for one on this machine.
        cmd.env("IRSYNC_TEST_DIAL_SECS", "5");
        cmd.env("FAKE_SSH_HOSTNAME", self.hostname.as_deref().unwrap_or("-"));
        if self.probe_root {
            cmd.env("FAKE_SSH_ID_U", "0");
        } else {
            cmd.env_remove("FAKE_SSH_ID_U");
        }
        // The fake ssh hands the client's environment on to the server
        // commands.
        if self.server_root {
            cmd.env("IRSYNC_TEST_EUID_ROOT", "1");
        } else {
            cmd.env_remove("IRSYNC_TEST_EUID_ROOT");
        }
        if self.service {
            cmd.env("IRSYNC_TEST_SERVICE_BIN_DIR", fake_service_bin());
        } else {
            cmd.env_remove("IRSYNC_TEST_SERVICE_BIN_DIR");
        }
        cmd.env("FAKE_SERVICE_LOG", self.client.path().join("service.log"));
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
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
        // A daemon the fake systemctl started, even one that never took
        // the lock.
        if let Some(pid) = self.service_pid() {
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
        }
        for home in [self.server.path(), self.client.path()] {
            if let Some(info) = probe(&home.join("xdg/irsync")) {
                if info.pid != 0 {
                    let _ = Command::new("kill")
                        .args(["-9", &info.pid.to_string()])
                        .status();
                }
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
fn pair_reports_the_setup_and_the_service_howto() {
    let w = World::new();
    let err = w.pair("myserver");
    for want in [
        "irsync: pairing with myserver over ssh",
        "irsync: set up myserver over ssh:",
        "irsync:   installed irsync",
        "irsync:   started the daemon",
        "irsync:   saved server myserver",
    ] {
        assert!(err.contains(want), "missing {want:?} in stderr:\n{err}");
    }
    // The notice comes before the report; the how-to is unprefixed so it
    // pastes.
    assert!(err.find("pairing with") < err.find("set up myserver"));
    // The fake server is this machine, so the how-to is for its platform.
    let lines: Vec<&str> = err.lines().collect();
    let service: &[&str] = if cfg!(target_os = "macos") {
        &["  launchctl load -w ~/Library/LaunchAgents/org.irsync.daemon.plist"]
    } else {
        &["[Service]", "  systemctl --user enable --now irsync"]
    };
    for want in ["To keep the daemon running after a reboot, make it a user service."]
        .iter()
        .chain(service)
    {
        assert!(lines.contains(want), "no line {want:?} in stderr:\n{err}");
    }
    assert!(
        lines.iter().any(|l| l.starts_with("  pid=\"$(sed -n")),
        "{err}"
    );
    let cfg = w.client_config();
    assert!(cfg.contains("[servers.myserver]"), "{cfg}");
    assert!(cfg.contains("ssh = \"myserver\""), "{cfg}");
    assert_eq!(w.ssh_calls(), 3);
}

#[test]
fn pair_then_push_syncs() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let target = format!("myserver:{}", dest.display());
    w.pair("myserver");
    ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    let calls = w.ssh_calls();
    assert!(calls > 0);
    std::fs::write(w.src.path().join("a.txt"), "changed\n").unwrap();
    let err = ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    assert!(!err.contains("set up"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dest.join("a.txt")).unwrap(),
        "changed\n"
    );
    assert_eq!(w.ssh_calls(), calls);
}

/// A sync to a host that was never paired stops and says how to pair it.
#[test]
fn first_push_to_an_unknown_host_stops_without_ssh() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let o = w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(err.contains("irsync pair myserver"), "{err}");
    assert_eq!(w.ssh_calls(), 0);
    assert_eq!(w.client_config(), "", "no client config written");
    assert!(!dest.exists());
}

/// A typo in the host name is not an ssh destination any more: it runs no
/// ssh, and the message says how to pair it if it was meant.
#[test]
fn a_typo_in_the_host_runs_no_ssh() {
    let w = World::new();
    w.pair("myserver");
    let calls = w.ssh_calls();
    let dest = w.server.path().join("dest");
    let o = w.irsync(&["-a", &w.src_arg(), &format!("myservr:{}", dest.display())]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(err.contains("irsync pair myservr"), "{err}");
    assert_eq!(w.ssh_calls(), calls);
}

#[test]
fn user_at_host_alias() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let target = format!("u@myserver:{}", dest.display());
    let err = w.pair("u@myserver");
    assert!(err.contains("saved server u@myserver"), "{err}");
    ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    let cfg = w.client_config();
    assert!(cfg.contains("ssh = \"u@myserver\""), "{cfg}");
    let calls = w.ssh_calls();
    std::fs::write(w.src.path().join("a.txt"), "again\n").unwrap();
    ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    assert_eq!(
        std::fs::read_to_string(dest.join("a.txt")).unwrap(),
        "again\n"
    );
    assert_eq!(w.ssh_calls(), calls);
}

#[test]
fn user_at_host_saves_the_plain_name_too() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let err = w.pair("u@myserver");
    assert!(err.contains("saved server u@myserver"), "{err}");
    assert!(err.contains("saved server myserver"), "{err}");
    let cfg: toml::Table = w.client_config().parse().unwrap();
    for a in ["u@myserver", "myserver"] {
        assert_eq!(cfg["servers"][a]["ssh"].as_str(), Some("u@myserver"), "{a}");
    }
    let calls = w.ssh_calls();
    ok(&w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]));
    assert_eq!(w.ssh_calls(), calls, "the plain name needs no ssh");
}

#[test]
fn pair_sets_up_a_new_host() {
    let w = World::new();
    let err = ok(&w.irsync_bare(&["pair", "myserver"]));
    assert!(
        err.contains("irsync: paired with myserver; syncs go over iroh, not ssh"),
        "{err}"
    );
    assert!(err.contains("saved server myserver"), "{err}");
    let calls = w.ssh_calls();
    assert!(calls > 0);
    let dest = w.server.path().join("dest");
    let target = format!("myserver:{}", dest.display());
    ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    assert!(dest.join("a.txt").exists());
    assert_eq!(w.ssh_calls(), calls, "a push needs no ssh");
}

#[test]
fn pair_user_at_host_saves_both_names() {
    let w = World::new();
    let err = ok(&w.irsync_bare(&["pair", "u@myserver"]));
    assert!(err.contains("saved server u@myserver"), "{err}");
    assert!(err.contains("saved server myserver"), "{err}");
    let cfg: toml::Table = w.client_config().parse().unwrap();
    for a in ["u@myserver", "myserver"] {
        assert_eq!(cfg["servers"][a]["ssh"].as_str(), Some("u@myserver"), "{a}");
    }
}

/// A second pairing checks the server but does not put back a grant the
/// owner removed.
#[test]
fn pair_again_does_not_regrant() {
    let w = World::new();
    ok(&w.irsync_bare(&["pair", "myserver"]));
    let id = client_id(w.client.path());
    common::set_all_paths(w.server.path(), &id, None);
    w.kill_daemon();
    let o = w.irsync_bare(&["pair", "myserver"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(
        err.contains("all_paths on myserver was lowered or removed"),
        "{err}"
    );
    assert!(
        err.contains("the daemon rejected this client's key"),
        "{err}"
    );
    assert!(!err.contains("paired with"), "{err}");
    assert!(!all_paths_granted(&w, &id), "{}", w.server_toml());
}

#[test]
fn ip_address_is_saved_under_the_server_name() {
    let mut w = World::new();
    w.hostname = Some("myserver.example.com".into());
    let dest = w.server.path().join("dest");
    let err = w.pair("192.168.1.5");
    assert!(err.contains("saved server myserver"), "{err}");
    let cfg: toml::Table = w.client_config().parse().unwrap();
    assert_eq!(
        cfg["servers"]["myserver"]["ssh"].as_str(),
        Some("192.168.1.5")
    );
    assert!(cfg["servers"].get("192.168.1.5").is_none());
    ok(&w.irsync(&[
        "-a",
        &w.src_arg(),
        &format!("192.168.1.5:{}", dest.display()),
    ]));
    same_tree(w.src.path(), &dest);
    // Typed again, the address finds the saved host: no ssh.
    let calls = w.ssh_calls();
    std::fs::write(w.src.path().join("a.txt"), "again\n").unwrap();
    let err = ok(&w.irsync(&[
        "-a",
        &w.src_arg(),
        &format!("192.168.1.5:{}", dest.display()),
    ]));
    assert!(err.contains("192.168.1.5 is saved as myserver"), "{err}");
    assert_eq!(w.ssh_calls(), calls, "found by its ssh destination");
    assert_eq!(
        std::fs::read_to_string(dest.join("a.txt")).unwrap(),
        "again\n"
    );
    // The other spelling of the address ends on the same alias, and is
    // remembered beside the first.
    let other = format!("u@192.168.1.5:{}", dest.display());
    w.pair("u@192.168.1.5");
    let cfg: toml::Table = w.client_config().parse().unwrap();
    assert_eq!(cfg["servers"].as_table().unwrap().len(), 1, "{cfg}");
    let h = &cfg["servers"]["myserver"];
    assert_eq!(h["ssh"].as_str(), Some("192.168.1.5"), "{cfg}");
    assert_eq!(h["ssh_aliases"][0].as_str(), Some("u@192.168.1.5"), "{cfg}");
    // From here on neither spelling needs ssh.
    let calls = w.ssh_calls();
    for target in [&other, &format!("192.168.1.5:{}", dest.display())] {
        let err = ok(&w.irsync(&["-a", &w.src_arg(), target]));
        assert!(err.contains("is saved as myserver"), "{err}");
    }
    assert_eq!(w.ssh_calls(), calls);
    let cfg: toml::Table = w.client_config().parse().unwrap();
    let also = cfg["servers"]["myserver"]["ssh_aliases"]
        .as_array()
        .unwrap();
    assert_eq!(also.len(), 1, "{cfg}");
}

/// A host saved with a name to reach it by keeps that name when its
/// address is typed later: an address goes stale, a name does not.
#[test]
fn an_address_does_not_replace_a_saved_ssh_name() {
    let mut w = World::new();
    w.hostname = Some("myserver".into());
    let dest = w.server.path().join("dest");
    w.pair("mike@myserver");
    let err = w.pair("192.168.1.5");
    assert!(
        err.contains("server myserver keeps ssh = mike@myserver"),
        "{err}"
    );
    let cfg: toml::Table = w.client_config().parse().unwrap();
    assert_eq!(cfg["servers"].as_table().unwrap().len(), 2, "{cfg}");
    for alias in ["myserver", "mike@myserver"] {
        assert_eq!(
            cfg["servers"][alias]["ssh"].as_str(),
            Some("mike@myserver"),
            "{alias}"
        );
    }
    // The address is remembered beside the name: typed again, no ssh.
    assert_eq!(
        cfg["servers"]["myserver"]["ssh_aliases"][0].as_str(),
        Some("192.168.1.5"),
        "{cfg}"
    );
    let calls = w.ssh_calls();
    let err = ok(&w.irsync(&[
        "-a",
        &w.src_arg(),
        &format!("192.168.1.5:{}", dest.display()),
    ]));
    assert!(err.contains("192.168.1.5 is saved as myserver"), "{err}");
    assert_eq!(w.ssh_calls(), calls);
}

/// Two saved daemons set up by one address: the address alone cannot say
/// which one is meant, and nothing is tried.
#[test]
fn two_hosts_with_one_ssh_destination_are_refused() {
    let w = World::new();
    let id = |n: u8| iroh::SecretKey::from_bytes(&[n; 32]).public();
    let cfg = w.client.path().join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    let text = format!(
        "[servers.a]\nendpoint_id = \"{}\"\nssh = \"192.168.1.5\"\n\
         [servers.b]\nendpoint_id = \"{}\"\nssh = \"192.168.1.5\"\n",
        id(7),
        id(8)
    );
    irsync::fs::private::write(cfg.join("irsync.toml"), &text).unwrap();
    let out = w.irsync(&["-a", &w.src_arg(), "192.168.1.5:dest"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("192.168.1.5") && err.contains("a, b"), "{err}");
    assert_eq!(w.ssh_calls(), 0);
    assert_eq!(w.client_config(), text);
}

/// The client's name is data on the server's command line, never shell.
#[test]
fn client_name_with_shell_characters_runs_nothing() {
    let mut w = World::new();
    // A valid alias (no `/`, `:` or `.`) that a shell would act on.
    w.client_name = "x';cd;touch pwned;echo '$(cd;touch pwned)`cd;touch pwned`".into();
    w.pair("myserver");
    assert!(!w.server.path().join("pwned").exists());
    assert!(!w.client.path().join("pwned").exists());
    // And it arrived as it was sent: the server saved the client under it.
    let cfg: toml::Table = w.server_toml().parse().unwrap();
    assert_eq!(
        cfg["clients"][w.client_name.as_str()]["client_id"].as_str(),
        Some(client_id(w.client.path()).as_str()),
        "{cfg}"
    );
    assert_eq!(
        cfg["clients"][w.client_name.as_str()]["all_paths"].as_str(),
        Some("rw")
    );
}

#[test]
fn unusable_client_name_stops_before_ssh() {
    let mut w = World::new();
    w.client_name = "localhost".into();
    let out = w.irsync_bare(&["pair", "myserver"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("host name"), "{err}");
    assert_eq!(w.ssh_calls(), 0);
}

#[test]
fn ipv6_without_a_server_name_changes_nothing() {
    let w = World::new();
    let out = w.irsync_bare(&["pair", "fe80::1"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("host name"), "{err}");
    assert_eq!(w.ssh_calls(), 1, "the probe only");
    assert!(!w.server.path().join(".local/bin/irsync").exists());
    assert_eq!(w.client_config(), "");
}

#[test]
fn all_paths_off_still_saves_alias() {
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
        "allow_all_paths = false\n",
        &format!(
            "[exports.dest]\npath = {:?}\nrw = [\"{id}\"]\n",
            dest.display()
        ),
    );

    let err = w.pair("myserver");
    assert!(err.contains("allow_all_paths is off on myserver"), "{err}");
    ok(&w.irsync(&["-a", &w.src_arg(), "myserver:dest"]));
    same_tree(w.src.path(), &dest);
    let cfg = w.client_config();
    assert!(cfg.contains("[servers.myserver]"), "{cfg}");
}

#[test]
fn other_platform_changes_nothing() {
    let mut w = World::new();
    w.uname = Some("Linux riscv64".into());
    std::fs::remove_dir_all(w.server.path().join(".config")).unwrap();
    let o = w.irsync_bare(&["pair", "myserver"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(
        err.contains("there is no release binary for that platform"),
        "{err}"
    );
    assert!(err.contains("set the server up by hand"), "{err}");
    assert!(
        !w.client_config().contains("[servers"),
        "{}",
        w.client_config()
    );
    assert!(!w.server.path().join(".config/irsync").exists());
    assert!(!w.server.path().join(".local").exists());
    assert!(!w.server.path().join("xdg").exists());
}

#[test]
fn other_platform_gets_the_release_binary() {
    let (uname, target, platform) = release::other_platform();
    let mut w = World::new();
    let rel = tempfile::tempdir().unwrap();
    w.uname = Some(uname.into());
    w.release_url = Some(release::fake_release(
        rel.path(),
        target,
        Path::new(env!("CARGO_BIN_EXE_irsync")),
    ));
    let dest = w.server.path().join("dest");
    let err = w.pair("myserver");
    assert!(err.contains("irsync: downloading irsync v"), "{err}");
    assert!(
        err.contains(&format!("(release files for {platform})")),
        "{err}"
    );
    ok(&w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]));
    same_tree(w.src.path(), &dest);
}

#[test]
fn unsavable_alias_stops_before_ssh() {
    let w = World::new();
    // `all` means every peer: it can never be an alias.
    let o = w.irsync_bare(&["pair", "all"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
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
        doc["servers"][alias]["endpoint_id"]
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

/// A dead daemon stops the sync with a hint that names `irsync pair`; no ssh
/// runs. With the default retry settings a paired host is dialled once,
/// bounded (here to the test's 5 s, really 15 s), not retried for minutes.
/// Pairing again starts the daemon, and the push then works.
#[test]
fn dead_daemon_stops_the_sync_with_the_pair_hint() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let target = format!("myserver:{}", dest.display());
    w.pair("myserver");
    let calls = w.ssh_calls();
    w.kill_daemon();
    std::fs::write(w.src.path().join("a.txt"), "changed\n").unwrap();
    let started = Instant::now();
    let o = w.irsync(&["-a", &w.src_arg(), &target]);
    let took = started.elapsed();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "{err}");
    assert!(took < Duration::from_secs(40), "took {took:?}:\n{err}");
    assert!(err.contains("gave up after 1 attempt"), "{err}");
    for want in [
        "myserver did not answer",
        "systemctl --user status irsync",
        "irsync pair myserver",
    ] {
        assert!(err.contains(want), "missing {want:?} in:\n{err}");
    }
    assert!(!err.contains("added this server's ID by hand"), "{err}");
    assert_eq!(w.ssh_calls(), calls);
    let err = w.pair("myserver");
    assert!(err.contains("started the daemon"), "{err}");
    ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    assert_eq!(
        std::fs::read_to_string(dest.join("a.txt")).unwrap(),
        "changed\n"
    );
}

/// A server saved under two names (`mike@myserver` and `myserver`) and
/// rebuilt: the check through one name gives both the new id. A host
/// reached another way keeps what it has.
#[test]
fn rebuilt_server_updates_every_alias_of_that_ssh_destination() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let target = format!("mike@myserver:{}", dest.display());
    w.pair("mike@myserver");
    let before = w.saved_id("myserver");
    assert_eq!(w.saved_id("mike@myserver"), before);
    ok(&w.irsync_bare(&["server", "add", "elsewhere", &before]));
    ok(&w.irsync_bare(&["config", "set", "servers.elsewhere.ssh", "nas"]));
    w.kill_daemon();
    for entry in std::fs::read_dir(w.server.path().join(".config/irsync")).unwrap() {
        let p = entry.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if name.starts_with("daemon_ed25519") {
            std::fs::remove_file(p).unwrap();
        }
    }
    let err = w.pair("mike@myserver");
    assert!(err.contains("updated server mike@myserver:"), "{err}");
    assert!(err.contains("updated server myserver:"), "{err}");
    let now = w.saved_id("mike@myserver");
    assert_ne!(now, before);
    assert_eq!(w.saved_id("myserver"), now);
    assert_eq!(w.saved_id("elsewhere"), before);
    ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    same_tree(w.src.path(), &dest);
}

#[test]
fn rebuilt_server_updates_alias() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let target = format!("myserver:{}", dest.display());
    w.pair("myserver");
    let before = w.saved_id("myserver");
    w.kill_daemon();
    for entry in std::fs::read_dir(w.server.path().join(".config/irsync")).unwrap() {
        let p = entry.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if name.starts_with("daemon_ed25519") {
            std::fs::remove_file(p).unwrap();
        }
    }
    let err = w.pair("myserver");
    assert!(err.contains("updated server myserver:"), "{err}");
    assert_ne!(w.saved_id("myserver"), before);
    ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    same_tree(w.src.path(), &dest);
}

#[test]
fn refusal_never_uses_ssh() {
    let w = World::new();
    w.pair("myserver");
    let calls = w.ssh_calls();
    let id = client_id(w.client.path());
    common::set_all_paths(w.server.path(), &id, None);
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
    assert!(!all_paths_granted(&w, &id), "{}", w.server_toml());
}

/// Whether the server config gives the client `id` `all_paths = "rw"`.
fn all_paths_granted(w: &World, id: &str) -> bool {
    let doc: toml::Table = w.server_toml().parse().unwrap();
    doc.get("clients")
        .and_then(|c| c.as_table())
        .is_some_and(|clients| {
            clients.values().any(|c| {
                c.get("client_id").and_then(|v| v.as_str()) == Some(id)
                    && c.get("all_paths").and_then(|v| v.as_str()) == Some("rw")
            })
        })
}

#[test]
fn server_knows_the_client_by_name() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let err = w.pair("myserver");
    assert!(err.contains("granted this client (myclient, "), "{err}");
    let t: toml::Table = w.server_toml().parse().unwrap();
    assert_eq!(
        t["clients"]["myclient"]["client_id"].as_str(),
        Some(&*client_id(w.client.path()))
    );
    assert_eq!(t["clients"]["myclient"]["all_paths"].as_str(), Some("rw"));
    ok(&w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]));
    let t: toml::Table = w.server_toml().parse().unwrap();
    // The automatic export the daemon made for the push names it too.
    let exports = t["exports"].as_table().unwrap();
    assert_eq!(exports.len(), 1, "{t}");
    let (_, e) = exports.iter().next().unwrap();
    assert_eq!(e["rw"][0].as_str(), Some("myclient"), "{t}");
}

/// Two machines, each a client of the other. A machine has a server and a
/// client on the other side, both under its host name: its daemon's id to
/// dial and its client id for grants. Neither direction gets in the
/// other's way.
#[test]
fn two_machines_sync_in_both_directions() {
    let w = World::new();
    w.client_daemon_config();
    let table = |home: &Path| -> toml::Table {
        std::fs::read_to_string(home.join(".config/irsync/irsync.toml"))
            .unwrap()
            .parse()
            .unwrap()
    };
    // myclient -> myserver.
    let there = format!("myserver:{}", w.server.path().join("dest").display());
    w.pair("myserver");
    ok(&w.irsync(&["-a", &w.src_arg(), &there]));
    let t = table(w.server.path());
    assert!(t.get("servers").is_none(), "{t}");
    assert!(t["clients"]["myclient"]["client_id"].is_str(), "{t}");

    // myserver -> myclient: the name the server knows the client by is
    // the name typed to dial it. Its daemon is set up over ssh.
    let back = w.client.path().join("back");
    let here = format!("myclient:{}", back.display());
    let o = w.irsync_back(&["-a", &w.src_arg(), &here]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "{err}");
    assert!(err.contains("irsync pair myclient"), "{err}");
    let err = w.pair_back("myclient");
    assert!(err.contains("pairing with myclient over ssh"), "{err}");
    assert!(err.contains("saved server myclient"), "{err}");
    ok(&w.irsync_back(&["-a", &w.src_arg(), &here]));
    same_tree(w.src.path(), &back);

    // One server and one client per machine, on both sides.
    let (server, client) = (table(w.server.path()), table(w.client.path()));
    for (t, name) in [(&server, "myclient"), (&client, "myserver")] {
        let (servers, clients) = (
            t["servers"].as_table().unwrap(),
            t["clients"].as_table().unwrap(),
        );
        assert_eq!((servers.len(), clients.len()), (1, 1), "{t}");
        let (s, c) = (&servers[name], &clients[name]);
        assert!(s["endpoint_id"].is_str() && c["client_id"].is_str(), "{t}");
        assert_ne!(s["endpoint_id"], c["client_id"], "{t}");
        assert_eq!(s["ssh"].as_str(), Some(name), "{t}");
        assert_eq!(c["all_paths"].as_str(), Some("rw"), "{t}");
    }
    assert_eq!(
        server["clients"]["myclient"]["client_id"].as_str(),
        Some(&*client_id(w.client.path()))
    );

    // Both directions again: no ssh, and each still gets in.
    let calls = (w.ssh_calls(), ssh_calls_in(w.server.path()));
    std::fs::write(w.src.path().join("a.txt"), "again\n").unwrap();
    ok(&w.irsync(&["-a", &w.src_arg(), &there]));
    ok(&w.irsync_back(&["-a", &w.src_arg(), &here]));
    assert_eq!((w.ssh_calls(), ssh_calls_in(w.server.path())), calls);
    for dest in [w.server.path().join("dest"), back] {
        assert_eq!(
            std::fs::read_to_string(dest.join("a.txt")).unwrap(),
            "again\n"
        );
    }
}

/// The config in `home`, parsed.
fn config_in(home: &Path) -> toml::Table {
    std::fs::read_to_string(home.join(".config/irsync/irsync.toml"))
        .unwrap()
        .parse()
        .unwrap()
}

/// A machine known as a client only, dialled as `user@host` or by its
/// address: pairing saves its daemon as a server beside the client of
/// that name, and the grants that name the client stay with its id.
#[test]
fn a_first_run_joins_a_client_only_entry() {
    for back_host in ["mike@myclient", "192.168.1.9"] {
        let mut w = World::new();
        w.client_daemon_config();
        let there = format!("myserver:{}", w.server.path().join("dest").display());
        w.pair("myserver");
        let client = client_id(w.client.path());

        // What the client machine's `uname -n` prints, for the address.
        w.hostname = Some("myclient".into());
        let back = w.client.path().join("back");
        let here = format!("{back_host}:{}", back.display());
        let err = w.pair_back(back_host);
        assert!(err.contains("saved server myclient"), "{err}");
        ok(&w.irsync_back(&["-a", &w.src_arg(), &here]));
        same_tree(w.src.path(), &back);

        let t = config_in(w.server.path());
        let (s, c) = (&t["servers"]["myclient"], &t["clients"]["myclient"]);
        assert_eq!(c["client_id"].as_str(), Some(&*client), "{t}");
        assert!(s["endpoint_id"].is_str(), "{t}");
        assert_ne!(s["endpoint_id"], c["client_id"], "{t}");
        assert_eq!(s["ssh"].as_str(), Some(back_host), "{t}");
        assert_eq!(c["all_paths"].as_str(), Some("rw"), "{t}");
        // The client still gets in.
        std::fs::write(w.src.path().join("a.txt"), "again\n").unwrap();
        ok(&w.irsync(&["-a", &w.src_arg(), &there]));
    }
}

/// `pair` on a host saved as a client only sets its daemon up, as a
/// plain run does.
#[test]
fn pair_sets_up_a_client_only_host() {
    let w = World::new();
    w.client_daemon_config();
    w.pair("myserver");
    let back = w.client.path().join("back");
    let here = format!("myclient:{}", back.display());
    let err = w.pair_back("myclient");
    assert!(err.contains("pairing with myclient over ssh"), "{err}");
    ok(&w.irsync_back(&["-a", &w.src_arg(), &here]));
    same_tree(w.src.path(), &back);
    let t = config_in(w.server.path());
    assert!(t["servers"]["myclient"]["endpoint_id"].is_str(), "{t}");
    // A first run, not a recheck: the server machine is granted there.
    let t = config_in(w.client.path());
    assert_eq!(
        t["clients"]["myserver"]["all_paths"].as_str(),
        Some("rw"),
        "{t}"
    );
}

/// A machine that syncs to itself has a server and a client for itself:
/// its daemon's id to dial, its client id for the grant.
#[test]
fn a_machine_syncs_to_itself() {
    let w = World::new();
    w.client_daemon_config();
    let home = w.client.path();
    let addr = format!("127.0.0.1:{}", w.client_port);
    let dest = home.join("dest");
    let target = format!("myclient:{}", dest.display());
    let args = ["-a", &w.src_arg(), &target, "--address", &addr];
    ok(&w.run(home, home, "myclient", &["pair", "myclient"]));
    ok(&w.run(home, home, "myclient", &args));
    same_tree(w.src.path(), &dest);
    let t = config_in(home);
    assert_eq!(t["servers"].as_table().unwrap().len(), 1, "{t}");
    assert_eq!(t["clients"].as_table().unwrap().len(), 1, "{t}");
    let (s, c) = (&t["servers"]["myclient"], &t["clients"]["myclient"]);
    assert_eq!(c["client_id"].as_str(), Some(&*client_id(home)), "{t}");
    assert!(s["endpoint_id"].is_str(), "{t}");
    assert_ne!(s["endpoint_id"], c["client_id"], "{t}");
    assert_eq!(c["all_paths"].as_str(), Some("rw"), "{t}");
    // Again: no ssh.
    let calls = ssh_calls_in(home);
    std::fs::write(w.src.path().join("a.txt"), "again\n").unwrap();
    ok(&w.run(home, home, "myclient", &args));
    assert_eq!(ssh_calls_in(home), calls);
    assert_eq!(
        std::fs::read_to_string(dest.join("a.txt")).unwrap(),
        "again\n"
    );
}

/// `pair --server-no-dht` and `pair --no-install` with a host's address check the entry
/// the address is saved under.
#[test]
fn pair_takes_an_address_and_no_dht() {
    let mut w = World::new();
    w.hostname = Some("myserver".into());
    let dest = w.server.path().join("dest");
    let target = format!("192.168.1.5:{}", dest.display());
    w.pair("192.168.1.5");
    let cfg = w.server.path().join(".config/irsync/irsync.toml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    irsync::fs::private::write(&cfg, text.replace("dht = false", "dht = true")).unwrap();
    let calls = w.ssh_calls();
    for (flag, said) in [
        ("--server-no-dht", "turned off DHT publishing on myserver"),
        ("--no-install", "irsync: pairing with myserver over ssh"),
    ] {
        let err = ok(&w.irsync_bare(&["pair", flag, "192.168.1.5"]));
        assert!(err.contains("192.168.1.5 is saved as myserver"), "{err}");
        assert!(err.contains(said), "{flag}: {err}");
    }
    assert!(w.ssh_calls() > calls);
    // The address finds the saved host for a sync, with no ssh.
    let calls = w.ssh_calls();
    ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    assert_eq!(w.ssh_calls(), calls);
    let log = std::fs::read_to_string(w.ssh_log()).unwrap();
    assert!(log.lines().all(|l| l == "192.168.1.5"), "{log}");
    let doc: toml::Table = w.server_toml().parse().unwrap();
    assert_eq!(doc["daemon"]["dht"].as_bool(), Some(false));
    let t = config_in(w.client.path());
    assert_eq!(t["servers"].as_table().unwrap().len(), 1, "{t}");
    assert_eq!(
        t["servers"]["myserver"]["ssh"].as_str(),
        Some("192.168.1.5")
    );
}

/// A daemon that died is started again when the host is typed as its
/// address: the check goes to the entry the address is saved under.
#[test]
fn a_dead_daemon_found_by_address_names_its_alias() {
    let mut w = World::new();
    w.hostname = Some("myserver".into());
    let dest = w.server.path().join("dest");
    let target = format!("192.168.1.5:{}", dest.display());
    w.pair("192.168.1.5");
    w.kill_daemon();
    std::fs::write(w.src.path().join("a.txt"), "changed\n").unwrap();
    let o = w.irsync(&["-a", "--dial-retries", "0", &w.src_arg(), &target]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "{err}");
    assert!(err.contains("myserver did not answer"), "{err}");
    assert!(err.contains("irsync pair myserver"), "{err}");
    assert!(!err.contains("irsync pair 192.168.1.5"), "{err}");
    let t = config_in(w.client.path());
    assert_eq!(t["servers"].as_table().unwrap().len(), 1, "{t}");
}

/// The ssh calls made by the irsync that ran in `home`.
fn ssh_calls_in(home: &Path) -> usize {
    std::fs::read_to_string(home.join("ssh.log"))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

#[test]
fn a_taken_client_name_is_refused_and_changes_no_configuration() {
    let w = World::new();
    let other = iroh::SecretKey::from_bytes(&[9; 32]).public();
    w.server_config(
        "",
        &format!("[clients.myclient]\nclient_id = \"{other}\"\n"),
    );
    let before = w.server_toml();
    let out = w.irsync_bare(&["pair", "myserver"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("[clients.myclient]"), "{err}");
    assert_eq!(w.server_toml(), before);
    assert!(
        probe(&w.server.path().join("xdg/irsync")).is_none(),
        "no daemon started"
    );
    assert_eq!(w.client_config(), "", "no alias saved");
    // The name is checked by the irsync just installed there: that one
    // stays, and the message says so.
    let note = "irsync was installed on myserver before this; that stays";
    assert!(err.contains(note), "{err}");
    assert!(w.server.path().join(".local/bin/irsync").exists());
    // Again, with nothing to install: nothing at all is changed.
    let out = w.irsync_bare(&["pair", "myserver"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("[clients.myclient]"), "{err}");
    assert!(!err.contains(note), "{err}");
    assert_eq!(w.server_toml(), before);
}

/// The client's config with the ssh link of `myserver` removed: the alias
/// then looks like one added by hand with `irsync server add`.
fn forget_ssh(w: &World) {
    let path = w.client.path().join(".config/irsync/irsync.toml");
    let cfg = w.client_config();
    let kept: Vec<&str> = cfg.lines().filter(|l| !l.starts_with("ssh = ")).collect();
    assert_ne!(kept.len(), cfg.lines().count(), "{cfg}");
    std::fs::write(&path, kept.join("\n") + "\n").unwrap();
}

/// A host added by hand that never answers: the error says the ID must be
/// the daemon's (the usual mistake is copying the server's client ID).
#[test]
fn hand_added_host_that_never_answers_gets_the_id_hint() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let target = format!("myserver:{}", dest.display());
    w.pair("myserver");
    forget_ssh(&w);
    w.kill_daemon();
    let o = w.irsync(&["-a", "--dial-retries", "0", &w.src_arg(), &target]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(!err.contains("checking it over ssh"), "{err}");
    assert!(err.contains("added this server's ID by hand"), "{err}");
    assert_eq!(err.matches("irsync id show --daemon").count(), 1, "{err}");
}

/// The same for a listing, the first remote command of a manual setup.
#[test]
fn hand_added_host_listing_gets_the_id_hint() {
    let w = World::new();
    w.pair("myserver");
    forget_ssh(&w);
    w.kill_daemon();
    let o = w.irsync(&["myserver:"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(err.contains("added this server's ID by hand"), "{err}");
    assert_eq!(err.matches("irsync id show --daemon").count(), 1, "{err}");
}

/// An alias whose name is not its ssh destination: the check goes to the
/// destination and updates that alias, not one named after the
/// destination.
#[test]
fn pair_updates_the_alias_not_its_ssh_name() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    w.pair("myserver");
    let path = w.client.path().join(".config/irsync/irsync.toml");
    let cfg = w
        .client_config()
        .replace("[servers.myserver]", "[servers.nas]");
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
    let err = w.pair("nas");
    assert!(err.contains("pairing with nas over ssh"), "{err}");
    assert!(err.contains("updated server nas:"), "{err}");
    assert_ne!(w.saved_id("nas"), before);
    let cfg = w.client_config();
    assert!(!cfg.contains("[servers.myserver]"), "{cfg}");
    assert!(cfg.contains("ssh = \"myserver\""), "{cfg}");
    ok(&w.irsync(&["-a", &w.src_arg(), &format!("nas:{}", dest.display())]));
    same_tree(w.src.path(), &dest);
}

#[test]
fn alias_without_ssh_does_not_fall_back() {
    let w = World::new();
    let dead = iroh::SecretKey::from_bytes(&[9; 32]).public().to_string();
    let out = common::irsync_bin(w.client.path(), &["server", "add", "myserver", &dead]);
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
    assert!(!err.contains("irsync pair"), "{err}");
    assert_eq!(w.ssh_calls(), 0);
}

/// `pair` goes through ssh for a saved host (probe and setup, no copy:
/// the build is current), reports it, then a push works.
#[test]
fn pair_checks_a_saved_host() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let target = format!("myserver:{}", dest.display());
    w.pair("myserver");
    let calls = w.ssh_calls();
    let err = w.pair("myserver");
    assert!(
        err.contains("irsync: pairing with myserver over ssh"),
        "{err}"
    );
    assert!(err.contains("irsync: set up myserver over ssh:"), "{err}");
    assert!(!err.contains("installed"), "{err}");
    assert_eq!(w.ssh_calls(), calls + 2);
    ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    assert!(dest.join("a.txt").exists());
}

/// `pair` with a bare EndpointId has nothing to ssh to.
#[test]
fn pair_refuses_an_endpoint_id() {
    let w = World::new();
    let id = iroh::SecretKey::from_bytes(&[9; 32]).public().to_string();
    let o = w.irsync_bare(&["pair", &id]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "{err}");
    assert!(
        err.contains("pair needs a server name or an ssh destination, not an EndpointId"),
        "{err}"
    );
    assert_eq!(w.ssh_calls(), 0);
}

/// `pair --server-no-dht` on a first pairing writes `dht = false` on the server
/// before its daemon starts (so this test never publishes), and says so.
#[test]
fn no_dht_turns_server_publishing_off() {
    let w = World::new();
    let cfg = w.server.path().join(".config/irsync/irsync.toml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    irsync::fs::private::write(&cfg, text.replace("dht = false", "dht = true")).unwrap();
    let err = ok(&w.irsync_bare(&["pair", "--server-no-dht", "myserver"]));
    assert!(
        err.contains("irsync:   turned off DHT publishing on myserver ([daemon] dht = false)"),
        "{err}"
    );
    let doc: toml::Table = w.server_toml().parse().unwrap();
    assert_eq!(doc["daemon"]["dht"].as_bool(), Some(false));
}

/// A first push that must stop: the server has no irsync, and nothing is
/// installed, changed or saved.
fn assert_stopped_by_no_install(w: &World, o: &Output) {
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "{err}");
    assert!(err.contains("--no-install"), "{err}");
    assert!(err.contains("Nothing was changed."), "{err}");
    assert!(
        !w.client_config().contains("[servers"),
        "{}",
        w.client_config()
    );
    assert!(!w.server.path().join(".local").exists());
    assert!(!w.server.path().join("xdg").exists());
}

#[test]
fn pair_no_install_stops_a_first_pairing() {
    let w = World::new();
    let o = w.irsync_bare(&["pair", "--no-install", "myserver"]);
    assert_stopped_by_no_install(&w, &o);
}

#[test]
fn ssh_install_false_in_config_stops_a_first_pairing() {
    let w = World::new();
    let cfg = w.client.path().join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    irsync::fs::private::write(cfg.join("irsync.toml"), "[defaults]\nssh_install = false\n")
        .unwrap();
    let o = w.irsync_bare(&["pair", "myserver"]);
    assert_stopped_by_no_install(&w, &o);
}

/// A root account is refused right after the probe: nothing is installed,
/// granted, started or saved.
#[test]
fn a_root_account_is_refused_before_anything_is_installed() {
    let mut w = World::new();
    w.probe_root = true;
    std::fs::remove_dir_all(w.server.path().join(".config")).unwrap();
    let o = w.irsync_bare(&["pair", "myserver"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(
        err.contains(
            "the ssh account on myserver is root, and irsync does not set up a daemon as root"
        ),
        "{err}"
    );
    assert!(
        err.contains("https://irsync.org/book/how-to/run-the-daemon"),
        "{err}"
    );
    assert_eq!(w.ssh_calls(), 1, "only the probe");
    assert!(
        !w.client_config().contains("[servers"),
        "{}",
        w.client_config()
    );
    assert!(!w.server.path().join(".config/irsync").exists());
    assert!(!w.server.path().join(".local").exists());
    assert!(!w.server.path().join("xdg").exists());
}

/// The probe did not see root, but the server command does: it refuses
/// before it writes a grant or an identity.
#[test]
fn ssh_setup_itself_refuses_root_and_grants_nothing() {
    let mut w = World::new();
    w.server_root = true;
    let o = w.irsync_bare(&["pair", "myserver"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(
        err.contains("irsync does not set up a daemon as root"),
        "{err}"
    );
    assert!(
        !w.server_toml().contains("all_paths"),
        "{}",
        w.server_toml()
    );
    assert!(!w
        .server
        .path()
        .join(".config/irsync/daemon_ed25519")
        .exists());
    assert!(
        !w.client_config().contains("[servers"),
        "{}",
        w.client_config()
    );
}

/// Pairing again with a saved host goes through the same probe: a daemon
/// that died is not started again through a root account.
#[test]
fn pair_through_a_root_account_is_refused() {
    let mut w = World::new();
    w.pair("myserver");
    w.kill_daemon();
    w.probe_root = true;
    let o = w.irsync_bare(&["pair", "myserver"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(
        err.contains("checking myserver over ssh failed")
            && err.contains("irsync does not set up a daemon as root"),
        "{err}"
    );
    assert!(probe(&w.server.path().join("xdg/irsync")).is_none());
}

/// A copy of irsync that is not the one `pair` installs, relative to the
/// server HOME: its daemon is not `ssh-setup`'s own.
#[cfg(target_os = "linux")]
const OTHER_BIN: &str = "other/bin/irsync";

/// A daemon started from [`OTHER_BIN`]; killed when dropped.
#[cfg(target_os = "linux")]
struct OtherDaemon(std::process::Child);

#[cfg(target_os = "linux")]
impl Drop for OtherDaemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(target_os = "linux")]
impl World {
    /// Starts `daemon -vv` from a copy of this build at [`OTHER_BIN`], and
    /// waits until it holds the lock. A copy, not a link: the daemon
    /// records the path it runs from with links resolved.
    fn start_other_daemon(&self) -> OtherDaemon {
        let other = self.server.path().join(OTHER_BIN);
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::copy(env!("CARGO_BIN_EXE_irsync"), &other).unwrap();
        let mut tries = 0;
        let child = loop {
            let spawned = Command::new(&other)
                .args(["daemon", "-vv"])
                .env("HOME", self.server.path())
                .env("XDG_CONFIG_HOME", self.server.path().join(".config"))
                .env("XDG_STATE_HOME", self.server.path().join("xdg"))
                .env_remove("IRSYNC_PASSPHRASE")
                .env_remove("RUST_LOG")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            match spawned {
                // A child another test thread forked while the copy was
                // open for writing holds it open until it runs its own
                // program: wait for that.
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 100 => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(50));
                }
                r => break r.unwrap(),
            }
        };
        let mut d = OtherDaemon(child);
        let state = self.server.path().join("xdg/irsync");
        let deadline = Instant::now() + Duration::from_secs(30);
        while !probe(&state).is_some_and(|i| i.pid == d.0.id()) {
            if let Some(status) = d.0.try_wait().unwrap() {
                panic!("the other daemon exited ({status})");
            }
            assert!(Instant::now() < deadline, "the other daemon did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        d
    }

    /// Gives the client `all_paths = "rw"` on the server before any daemon
    /// starts: a daemon `ssh-setup` leaves running reads its config only
    /// at the start.
    fn grant_client(&self) {
        let out = common::irsync_bin(self.client.path(), &["id", "generate"]);
        assert!(out.status.success(), "{out:?}");
        let id = client_id(self.client.path());
        common::set_all_paths(
            self.server.path(),
            &id,
            Some(irsync::authz::Access::ReadWrite),
        );
    }
}

/// Whether process `pid` is still there.
fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

// Only the Linux-only service tests use it.
#[cfg(target_os = "linux")]
const SYSTEMD_LINE: &str =
    "irsync:   the daemon runs as a systemd user service (irsync.service), also after a reboot";
const HOWTO: &str = "To keep the daemon running after a reboot, make it a user service.";

#[cfg(target_os = "linux")]
#[test]
fn pair_makes_a_systemd_user_service() {
    let mut w = World::new();
    w.service = true;
    let err = w.pair("myserver");
    let unit = std::fs::read_to_string(w.unit()).unwrap();
    assert!(unit.contains(irsync::ssh::service::MARKER), "{unit}");
    let log = w.service_log();
    assert!(
        log.contains("systemctl --user enable --now irsync"),
        "{log}"
    );
    assert!(err.lines().any(|l| l == SYSTEMD_LINE), "{err}");
    assert!(!err.contains(HOWTO), "{err}");
    assert_eq!(Some(w.lock_pid()), w.service_pid());
    let dest = w.server.path().join("dest");
    ok(&w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]));
    same_tree(w.src.path(), &dest);
}

#[cfg(target_os = "linux")]
#[test]
fn pair_twice_keeps_one_daemon() {
    let mut w = World::new();
    w.service = true;
    w.pair("myserver");
    let pid = w.lock_pid();
    let err = w.pair("myserver");
    assert!(err.lines().any(|l| l == SYSTEMD_LINE), "{err}");
    let log = w.service_log();
    assert!(!log.contains("restart"), "{log}");
    assert_eq!(log.matches("enable --now").count(), 1, "{log}");
    assert_eq!(w.lock_pid(), pid);
}

#[cfg(target_os = "linux")]
#[test]
fn pair_moves_a_background_daemon_into_the_service() {
    let mut w = World::new();
    w.service = true;
    ok(&w.irsync_bare(&["pair", "--no-service", "myserver"]));
    let old = w.lock_pid();
    let err = w.pair("myserver");
    assert!(err.lines().any(|l| l == SYSTEMD_LINE), "{err}");
    assert!(!alive(old), "the background daemon {old} still runs");
    assert_eq!(Some(w.lock_pid()), w.service_pid());
}

#[test]
fn pair_with_no_service_writes_no_unit() {
    let mut w = World::new();
    w.service = true;
    let err = ok(&w.irsync_bare(&["pair", "--no-service", "myserver"]));
    assert!(
        err.lines().any(|l| {
            l
            == "irsync:   the daemon runs in the background until the server reboots (--no-service)"
        }),
        "{err}"
    );
    assert!(!w.unit().exists());
    assert!(!w.service_log().contains("enable"), "{}", w.service_log());
    assert!(alive(w.lock_pid()));
}

#[test]
fn pair_without_a_user_session_falls_back() {
    let mut w = World::new();
    w.service = true;
    w.env.push(("FAKE_SYSTEMD_DOWN".into(), "1".into()));
    let err = w.pair("myserver");
    assert!(
        err.lines().any(|l| l
            == "irsync:   no systemd or launchd user session there: the daemon runs in the background until the server reboots"),
        "{err}"
    );
    assert!(err.lines().any(|l| l == HOWTO), "{err}");
    assert!(!w.unit().exists());
    assert!(alive(w.lock_pid()));
}

#[cfg(target_os = "linux")]
#[test]
fn pair_falls_back_when_linger_is_refused() {
    let mut w = World::new();
    w.service = true;
    w.env.push(("FAKE_LINGER".into(), "no".into()));
    w.env.push(("FAKE_LINGER_REFUSED".into(), "1".into()));
    let err = w.pair("myserver");
    assert!(
        err.contains("irsync:   warning: could not make the daemon a user service"),
        "{err}"
    );
    assert!(err.lines().any(|l| l == HOWTO), "{err}");
    assert!(!w.unit().exists());
    assert!(alive(w.lock_pid()));
    assert_eq!(w.service_pid(), None);
}

#[cfg(target_os = "linux")]
#[test]
fn pair_leaves_a_foreign_unit_alone() {
    let mut w = World::new();
    w.service = true;
    let text = "[Service]\nExecStart=/usr/bin/irsync daemon\n";
    std::fs::create_dir_all(w.unit().parent().unwrap()).unwrap();
    std::fs::write(w.unit(), text).unwrap();
    let err = w.pair("myserver");
    assert_eq!(std::fs::read_to_string(w.unit()).unwrap(), text);
    assert!(
        err.lines().any(|l| l
            == "irsync:   ~/.config/systemd/user/irsync.service was not written by irsync; left it alone. The daemon runs in the background until the server reboots"),
        "{err}"
    );
    assert!(!w.service_log().contains("enable"), "{}", w.service_log());
    assert!(alive(w.lock_pid()));
}

/// A refused linger leaves the daemon that runs in the background alone:
/// nothing is stopped before the service is ready.
#[cfg(target_os = "linux")]
#[test]
fn pair_keeps_a_background_daemon_when_linger_is_refused() {
    let mut w = World::new();
    w.service = true;
    ok(&w.irsync_bare(&["pair", "--no-service", "myserver"]));
    let pid = w.lock_pid();
    w.env.push(("FAKE_LINGER".into(), "no".into()));
    w.env.push(("FAKE_LINGER_REFUSED".into(), "1".into()));
    let err = w.pair("myserver");
    assert!(
        err.contains("irsync:   warning: could not make the daemon a user service"),
        "{err}"
    );
    assert!(!err.contains("started the daemon"), "{err}");
    assert_eq!(w.lock_pid(), pid);
}

/// A service that never starts the daemon is turned off again, so the
/// daemon started in the background is the only one.
#[cfg(target_os = "linux")]
#[test]
fn pair_turns_off_a_service_that_starts_nothing() {
    let mut w = World::new();
    w.service = true;
    w.env.push(("FAKE_SERVICE_NO_START".into(), "1".into()));
    let err = w.pair("myserver");
    assert!(
        err.contains(
            "irsync:   warning: could not make the daemon a user service: \
             the service did not start the daemon"
        ),
        "{err}"
    );
    let log = w.service_log();
    assert!(
        log.contains("systemctl --user disable --now irsync"),
        "{log}"
    );
    assert_eq!(w.service_pid(), None);
    assert!(alive(w.lock_pid()));
    // Our unit stays, for the next `pair`.
    assert!(w.unit().exists());
}

/// A unit written by hand from irsync's own steps (`ExecStart=%h/...`)
/// starts this binary: it is used as it is, not called foreign.
#[cfg(target_os = "linux")]
#[test]
fn pair_uses_a_hand_written_unit_of_this_binary() {
    let mut w = World::new();
    w.service = true;
    let text = "[Unit]\nDescription=irsync daemon\n[Service]\n\
                ExecStart=%h/.local/bin/irsync daemon -vv\nRestart=on-failure\n\
                [Install]\nWantedBy=default.target\n";
    std::fs::create_dir_all(w.unit().parent().unwrap()).unwrap();
    std::fs::write(w.unit(), text).unwrap();
    let err = w.pair("myserver");
    assert!(!err.contains("not written by irsync"), "{err}");
    assert!(err.lines().any(|l| l == SYSTEMD_LINE), "{err}");
    assert_eq!(std::fs::read_to_string(w.unit()).unwrap(), text);
    assert_eq!(Some(w.lock_pid()), w.service_pid());
}

/// A service that is enabled but fails to start is turned off again, not
/// left to start after a reboot beside the background daemon.
#[cfg(target_os = "linux")]
#[test]
fn pair_turns_off_a_service_whose_start_fails() {
    let mut w = World::new();
    w.service = true;
    w.env.push(("FAKE_SERVICE_START_FAILS".into(), "1".into()));
    let err = w.pair("myserver");
    assert!(
        err.contains("irsync:   warning: could not make the daemon a user service: "),
        "{err}"
    );
    let log = w.service_log();
    assert!(
        log.contains("systemctl --user disable --now irsync"),
        "{log}"
    );
    assert!(alive(w.lock_pid()));
}

/// Another daemon holds the lock: no unit is written and no service
/// started, so the two never fight over the lock.
#[cfg(target_os = "linux")]
#[test]
fn pair_leaves_a_foreign_daemon_without_a_service() {
    let mut w = World::new();
    w.service = true;
    w.grant_client();
    let unit = w.unit();
    let text = "[Service]\nExecStart=%h/other/bin/irsync daemon -vv\n";
    std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
    std::fs::write(&unit, text).unwrap();
    let other = w.start_other_daemon();
    let err = w.pair("myserver");
    assert_eq!(w.lock_pid(), other.0.id(), "{err}");
    assert_eq!(std::fs::read_to_string(&unit).unwrap(), text);
    let log = w.service_log();
    assert!(!log.contains("enable"), "{log}");
    assert!(!err.contains("user service"), "{err}");
}
