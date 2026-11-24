//! The client's first run against an unknown host: it sets the server up
//! over (fake) ssh, saves the alias, then syncs over iroh.

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
        irsync::fs::private::write(cfg.join("irsync.toml"), text).unwrap();
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
            "[daemon]\nrelay_mode = \"disabled\"\nportmap = false\n\
             listen = [\"127.0.0.1:{}\"]\n\
             [daemon.discovery]\ndht = false\ndns = false\nmdns = false\n",
            self.client_port
        );
        irsync::fs::private::write(cfg.join("irsync.toml"), text).unwrap();
    }

    /// irsync run in `home` as the machine `name`, with a fake ssh that
    /// lands in `remote`.
    fn run(&self, home: &Path, remote: &Path, name: &str, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_irsync"));
        cmd.args(args)
            .env("HOME", home)
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
fn user_at_host_saves_the_plain_name_too() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let err = ok(&w.irsync(&[
        "-a",
        &w.src_arg(),
        &format!("u@myserver:{}", dest.display()),
    ]));
    assert!(err.contains("saved host alias u@myserver"), "{err}");
    assert!(err.contains("saved host alias myserver"), "{err}");
    let cfg: toml::Table = w.client_config().parse().unwrap();
    for a in ["u@myserver", "myserver"] {
        assert_eq!(cfg["servers"][a]["ssh"].as_str(), Some("u@myserver"), "{a}");
    }
    let calls = w.ssh_calls();
    ok(&w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]));
    assert_eq!(w.ssh_calls(), calls, "the plain name needs no ssh");
}

#[test]
fn ip_address_is_saved_under_the_server_name() {
    let mut w = World::new();
    w.hostname = Some("myserver.example.com".into());
    let dest = w.server.path().join("dest");
    let err = ok(&w.irsync(&[
        "-a",
        &w.src_arg(),
        &format!("192.168.1.5:{}", dest.display()),
    ]));
    assert!(err.contains("saved host alias myserver"), "{err}");
    let cfg: toml::Table = w.client_config().parse().unwrap();
    assert_eq!(
        cfg["servers"]["myserver"]["ssh"].as_str(),
        Some("192.168.1.5")
    );
    assert!(cfg["servers"].get("192.168.1.5").is_none());
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
    ok(&w.irsync(&["-a", &w.src_arg(), &other]));
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
    ok(&w.irsync(&[
        "-a",
        &w.src_arg(),
        &format!("mike@myserver:{}", dest.display()),
    ]));
    let err = ok(&w.irsync(&[
        "-a",
        &w.src_arg(),
        &format!("192.168.1.5:{}", dest.display()),
    ]));
    assert!(
        err.contains("host alias myserver keeps ssh = mike@myserver"),
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
    let dest = w.server.path().join("dest");
    ok(&w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]));
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
        cfg["immediate"]["rw"][0].as_str(),
        Some(w.client_name.as_str())
    );
}

#[test]
fn unusable_client_name_stops_before_ssh() {
    let mut w = World::new();
    w.client_name = "localhost".into();
    let out = w.irsync(&["-a", &w.src_arg(), "myserver:dest"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("host name"), "{err}");
    assert_eq!(w.ssh_calls(), 0);
}

#[test]
fn ipv6_without_a_server_name_changes_nothing() {
    let w = World::new();
    let out = w.irsync(&["-a", &w.src_arg(), "[fe80::1]:dest"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("host name"), "{err}");
    assert_eq!(w.ssh_calls(), 1, "the probe only");
    assert!(!w.server.path().join(".local/share/irsync").exists());
    assert_eq!(w.client_config(), "");
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
    assert!(cfg.contains("[servers.myserver]"), "{cfg}");
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
    let o = w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]);
    let err = ok(&o);
    assert!(err.contains("irsync: downloading irsync v"), "{err}");
    assert!(
        err.contains(&format!("(release files for {platform})")),
        "{err}"
    );
    same_tree(w.src.path(), &dest);
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

/// A server saved under two names (`mike@myserver` and `myserver`) and
/// rebuilt: the check through one name gives both the new id. A host
/// reached another way keeps what it has.
#[test]
fn rebuilt_server_updates_every_alias_of_that_ssh_destination() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let target = format!("mike@myserver:{}", dest.display());
    ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    let before = w.saved_id("myserver");
    assert_eq!(w.saved_id("mike@myserver"), before);
    ok(&w.irsync_bare(&["host", "add", "elsewhere", &before]));
    ok(&w.irsync_bare(&["config", "set", "servers.elsewhere.ssh", "nas"]));
    w.kill_daemon();
    for entry in std::fs::read_dir(w.server.path().join(".config/irsync")).unwrap() {
        let p = entry.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if name.starts_with("daemon_ed25519") {
            std::fs::remove_file(p).unwrap();
        }
    }
    let err = ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    assert!(err.contains("updated host alias mike@myserver:"), "{err}");
    assert!(err.contains("updated host alias myserver:"), "{err}");
    let now = w.saved_id("mike@myserver");
    assert_ne!(now, before);
    assert_eq!(w.saved_id("myserver"), now);
    assert_eq!(w.saved_id("elsewhere"), before);
    same_tree(w.src.path(), &dest);
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

/// Whether the server config grants `id` immediate access, by its id or
/// by a name its `[clients]` has for it.
fn immediate_granted(w: &World, id: &str) -> bool {
    let doc: toml::Table = w.server_toml().parse().unwrap();
    let named = |entry: &str| {
        let client = doc.get("clients").and_then(|c| c.get(entry));
        client
            .and_then(|c| c.get("client_id"))
            .and_then(|v| v.as_str())
            == Some(id)
    };
    doc.get("immediate")
        .and_then(|i| i.get("rw"))
        .and_then(|rw| rw.as_array())
        .is_some_and(|rw| {
            rw.iter()
                .filter_map(|v| v.as_str())
                .any(|e| e == id || named(e))
        })
}

#[test]
fn server_knows_the_client_by_name() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let err = ok(&w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]));
    assert!(err.contains("granted this client (myclient, "), "{err}");
    let t: toml::Table = w.server_toml().parse().unwrap();
    assert_eq!(
        t["clients"]["myclient"]["client_id"].as_str(),
        Some(&*client_id(w.client.path()))
    );
    assert_eq!(t["immediate"]["rw"][0].as_str(), Some("myclient"));
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
    ok(&w.irsync(&["-a", &w.src_arg(), &there]));
    let t = table(w.server.path());
    assert!(t.get("servers").is_none(), "{t}");
    assert!(t["clients"]["myclient"]["client_id"].is_str(), "{t}");

    // myserver -> myclient: the name the server knows the client by is
    // the name typed to dial it. Its daemon is set up over ssh.
    let back = w.client.path().join("back");
    let here = format!("myclient:{}", back.display());
    let err = ok(&w.irsync_back(&["-a", &w.src_arg(), &here]));
    assert!(
        err.contains("myclient is saved as a client only; setting it up over ssh"),
        "{err}"
    );
    assert!(err.contains("saved host alias myclient"), "{err}");
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
        assert_eq!(t["immediate"]["rw"][0].as_str(), Some(name), "{t}");
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
/// address: the first run saves its daemon as a server beside the client
/// of that name, and the grants that name the client stay with its id.
#[test]
fn a_first_run_joins_a_client_only_entry() {
    for back_host in ["mike@myclient", "192.168.1.9"] {
        let mut w = World::new();
        w.client_daemon_config();
        let there = format!("myserver:{}", w.server.path().join("dest").display());
        ok(&w.irsync(&["-a", &w.src_arg(), &there]));
        let client = client_id(w.client.path());

        // What the client machine's `uname -n` prints, for the address.
        w.hostname = Some("myclient".into());
        let back = w.client.path().join("back");
        let here = format!("{back_host}:{}", back.display());
        let err = ok(&w.irsync_back(&["-a", &w.src_arg(), &here]));
        assert!(err.contains("saved host alias myclient"), "{err}");
        same_tree(w.src.path(), &back);

        let t = config_in(w.server.path());
        let (s, c) = (&t["servers"]["myclient"], &t["clients"]["myclient"]);
        assert_eq!(c["client_id"].as_str(), Some(&*client), "{t}");
        assert!(s["endpoint_id"].is_str(), "{t}");
        assert_ne!(s["endpoint_id"], c["client_id"], "{t}");
        assert_eq!(s["ssh"].as_str(), Some(back_host), "{t}");
        assert_eq!(t["immediate"]["rw"][0].as_str(), Some("myclient"), "{t}");
        // The client still gets in.
        std::fs::write(w.src.path().join("a.txt"), "again\n").unwrap();
        ok(&w.irsync(&["-a", &w.src_arg(), &there]));
    }
}

/// `--ssh` on a host saved as a client only sets its daemon up, as a
/// plain run does.
#[test]
fn force_ssh_sets_up_a_client_only_host() {
    let w = World::new();
    w.client_daemon_config();
    let there = format!("myserver:{}", w.server.path().join("dest").display());
    ok(&w.irsync(&["-a", &w.src_arg(), &there]));
    let back = w.client.path().join("back");
    let here = format!("myclient:{}", back.display());
    let err = ok(&w.irsync_back(&["-a", "--ssh", &w.src_arg(), &here]));
    assert!(
        err.contains("myclient is saved as a client only; setting it up over ssh"),
        "{err}"
    );
    same_tree(w.src.path(), &back);
    let t = config_in(w.server.path());
    assert!(t["servers"]["myclient"]["endpoint_id"].is_str(), "{t}");
    // A first run, not a recheck: the server machine is granted there.
    let t = config_in(w.client.path());
    assert_eq!(t["immediate"]["rw"][0].as_str(), Some("myserver"), "{t}");
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
    ok(&w.run(home, home, "myclient", &args));
    same_tree(w.src.path(), &dest);
    let t = config_in(home);
    assert_eq!(t["servers"].as_table().unwrap().len(), 1, "{t}");
    assert_eq!(t["clients"].as_table().unwrap().len(), 1, "{t}");
    let (s, c) = (&t["servers"]["myclient"], &t["clients"]["myclient"]);
    assert_eq!(c["client_id"].as_str(), Some(&*client_id(home)), "{t}");
    assert!(s["endpoint_id"].is_str(), "{t}");
    assert_ne!(s["endpoint_id"], c["client_id"], "{t}");
    assert_eq!(t["immediate"]["rw"][0].as_str(), Some("myclient"), "{t}");
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

/// `--ssh` and `--no-remote-dht` with a host's address check the entry
/// the address is saved under.
#[test]
fn an_address_takes_ssh_and_no_remote_dht() {
    let mut w = World::new();
    w.hostname = Some("myserver".into());
    let dest = w.server.path().join("dest");
    let target = format!("192.168.1.5:{}", dest.display());
    ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    let cfg = w.server.path().join(".config/irsync/irsync.toml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    irsync::fs::private::write(&cfg, text.replace("dht = false", "dht = true")).unwrap();
    let calls = w.ssh_calls();
    for (flag, said) in [
        ("--no-remote-dht", "turned off DHT publishing on myserver"),
        ("--ssh", "irsync: checking myserver over ssh"),
    ] {
        let err = ok(&w.irsync(&["-a", flag, &w.src_arg(), &target]));
        assert!(err.contains("192.168.1.5 is saved as myserver"), "{err}");
        assert!(err.contains(said), "{flag}: {err}");
        assert!(!err.contains("not a saved alias"), "{err}");
    }
    assert_eq!(w.ssh_calls(), calls + 4);
    let log = std::fs::read_to_string(w.ssh_log()).unwrap();
    assert!(log.lines().all(|l| l == "192.168.1.5"), "{log}");
    let doc: toml::Table = w.server_toml().parse().unwrap();
    assert_eq!(doc["daemon"]["discovery"]["dht"].as_bool(), Some(false));
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
fn a_dead_daemon_found_by_address_is_restarted() {
    let mut w = World::new();
    w.hostname = Some("myserver".into());
    let dest = w.server.path().join("dest");
    let target = format!("192.168.1.5:{}", dest.display());
    ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    w.kill_daemon();
    std::fs::write(w.src.path().join("a.txt"), "changed\n").unwrap();
    let err = ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    assert!(
        err.contains("irsync: myserver did not answer; checking it over ssh"),
        "{err}"
    );
    assert!(err.contains("started the daemon"), "{err}");
    assert!(!err.contains("host alias"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dest.join("a.txt")).unwrap(),
        "changed\n"
    );
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
    let out = w.irsync(&["-a", &w.src_arg(), "myserver:dest"]);
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
    let out = w.irsync(&["-a", &w.src_arg(), "myserver:dest"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("[clients.myclient]"), "{err}");
    assert!(!err.contains(note), "{err}");
    assert_eq!(w.server_toml(), before);
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
    // A host set up over ssh never gets the hand-copied-ID hint.
    assert!(!err.contains("copied this host's ID by hand"), "{err}");
}

/// The client's config with the ssh link of `myserver` removed: the alias
/// then looks like one added by hand with `irsync host add`.
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
    ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    forget_ssh(&w);
    w.kill_daemon();
    let o = w.irsync(&["-a", "--dial-retries", "0", &w.src_arg(), &target]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(!err.contains("checking it over ssh"), "{err}");
    assert!(err.contains("copied this host's ID by hand"), "{err}");
    assert_eq!(err.matches("irsync id show --daemon").count(), 1, "{err}");
}

/// The same for a listing, the first remote command of a manual setup.
#[test]
fn hand_added_host_listing_gets_the_id_hint() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    ok(&w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]));
    forget_ssh(&w);
    w.kill_daemon();
    let o = w.irsync(&["myserver:"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(err.contains("copied this host's ID by hand"), "{err}");
    assert_eq!(err.matches("irsync id show --daemon").count(), 1, "{err}");
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
    let err = ok(&w.irsync(&["-a", &w.src_arg(), &format!("nas:{}", dest.display())]));
    assert!(
        err.contains("nas did not answer; checking it over ssh"),
        "{err}"
    );
    assert!(err.contains("updated host alias nas:"), "{err}");
    assert_ne!(w.saved_id("nas"), before);
    let cfg = w.client_config();
    assert!(!cfg.contains("[servers.myserver]"), "{cfg}");
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

/// `--ssh` goes through ssh for a saved host (probe and setup, no copy:
/// the build is current), reports it, then syncs.
#[test]
fn force_ssh_checks_a_saved_host() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let target = format!("myserver:{}", dest.display());
    ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    let calls = w.ssh_calls();
    std::fs::write(w.src.path().join("a.txt"), "changed\n").unwrap();
    let err = ok(&w.irsync(&["-a", "--ssh", &w.src_arg(), &target]));
    assert!(err.contains("irsync: checking myserver over ssh"), "{err}");
    assert!(err.contains("irsync: set up myserver over ssh:"), "{err}");
    assert!(!err.contains("not a saved alias"), "{err}");
    assert!(!err.contains("installed"), "{err}");
    assert_eq!(w.ssh_calls(), calls + 2);
    assert_eq!(
        std::fs::read_to_string(dest.join("a.txt")).unwrap(),
        "changed\n"
    );
}

/// `--ssh` with a bare EndpointId has nothing to ssh to.
#[test]
fn force_ssh_refuses_an_endpoint_id() {
    let w = World::new();
    let id = iroh::SecretKey::from_bytes(&[9; 32]).public().to_string();
    let o = w.irsync(&["-a", "--ssh", &w.src_arg(), &format!("{id}:/x")]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "{err}");
    assert!(
        err.contains("--ssh needs a host alias or an ssh destination, not an EndpointId"),
        "{err}"
    );
    assert_eq!(w.ssh_calls(), 0);
}

/// `--no-remote-dht` on the first push writes `dht = false` on the server
/// before its daemon starts (so this test never publishes), and says so.
#[test]
fn no_remote_dht_turns_server_publishing_off() {
    let w = World::new();
    let cfg = w.server.path().join(".config/irsync/irsync.toml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    irsync::fs::private::write(&cfg, text.replace("dht = false", "dht = true")).unwrap();
    let dest = w.server.path().join("dest");
    let o = w.irsync(&[
        "-a",
        "--no-remote-dht",
        &w.src_arg(),
        &format!("myserver:{}", dest.display()),
    ]);
    let err = ok(&o);
    same_tree(w.src.path(), &dest);
    assert!(
        err.contains(
            "irsync:   turned off DHT publishing on myserver ([daemon.discovery] dht = false)"
        ),
        "{err}"
    );
    let doc: toml::Table = w.server_toml().parse().unwrap();
    assert_eq!(doc["daemon"]["discovery"]["dht"].as_bool(), Some(false));
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
fn no_install_flag_stops_a_first_push() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let o = w.irsync(&[
        "-a",
        "--no-install",
        &w.src_arg(),
        &format!("myserver:{}", dest.display()),
    ]);
    assert_stopped_by_no_install(&w, &o);
}

#[test]
fn ssh_install_false_in_config_does_the_same() {
    let w = World::new();
    let cfg = w.client.path().join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    irsync::fs::private::write(cfg.join("irsync.toml"), "[client]\nssh_install = false\n").unwrap();
    let dest = w.server.path().join("dest");
    let o = w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]);
    assert_stopped_by_no_install(&w, &o);
}

/// A first push with no `--address` and every client lookup off: the only
/// way to the new daemon is the address `ssh-setup` hands back. Without it
/// the client finds no address (the daemon has published nothing yet).
#[test]
fn first_push_dials_the_address_setup_reports() {
    let w = World::new();
    let cfg = w.client.path().join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    irsync::fs::private::write(
        cfg.join("irsync.toml"),
        "[client]\ndht = false\nmdns = false\nrelay_mode = \"disabled\"\ndial_retries = 0\n",
    )
    .unwrap();
    let dest = w.server.path().join("dest");
    let o = w.irsync_bare(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]);
    ok(&o);
    same_tree(w.src.path(), &dest);
}

/// A root account is refused right after the probe: nothing is installed,
/// granted, started or saved.
#[test]
fn a_root_account_is_refused_before_anything_is_installed() {
    let mut w = World::new();
    w.probe_root = true;
    std::fs::remove_dir_all(w.server.path().join(".config")).unwrap();
    let dest = w.server.path().join("dest");
    let o = w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]);
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
    assert!(!dest.exists());
}

/// The probe did not see root, but the server command does: it refuses
/// before it writes a grant or an identity.
#[test]
fn ssh_setup_itself_refuses_root_and_grants_nothing() {
    let mut w = World::new();
    w.server_root = true;
    let dest = w.server.path().join("dest");
    let o = w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(
        err.contains("irsync does not set up a daemon as root"),
        "{err}"
    );
    assert!(
        !w.server_toml().contains("[immediate]"),
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

/// The automatic check of a saved host goes through the same probe: a
/// daemon that died is not started again through a root account.
#[test]
fn a_dead_daemon_is_not_restarted_through_a_root_account() {
    let mut w = World::new();
    let dest = w.server.path().join("dest");
    let target = format!("myserver:{}", dest.display());
    ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    w.kill_daemon();
    w.probe_root = true;
    // As after any failed check over ssh, the client tries the daemon once
    // more; no retries, so that try is short.
    let o = w.irsync(&["-a", "--dial-retries", "0", &w.src_arg(), &target]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(
        err.contains("checking myserver over ssh failed")
            && err.contains("irsync does not set up a daemon as root"),
        "{err}"
    );
    assert!(probe(&w.server.path().join("xdg/irsync")).is_none());
}

/// Where irsync's ssh install put its binary before it used the
/// installer's place, relative to the server HOME.
const OLD_BIN: &str = ".local/share/irsync/bin/irsync";

/// A daemon started from the old install path; killed when dropped, if
/// `ssh-setup` has not stopped it already.
struct OldDaemon(std::process::Child);

impl Drop for OldDaemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl World {
    fn old_bin(&self) -> PathBuf {
        self.server.path().join(OLD_BIN)
    }

    /// Puts a copy of this build at the old install path. A copy, not a
    /// link: the daemon records the path it runs from with links resolved.
    fn plant_old_bin(&self) {
        let old = self.old_bin();
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::copy(env!("CARGO_BIN_EXE_irsync"), &old).unwrap();
    }

    /// Starts `daemon -vv` from the old install path, as an older ssh
    /// install left it, and waits until it holds the lock.
    fn start_old_daemon(&self) -> OldDaemon {
        self.plant_old_bin();
        let child = Command::new(self.old_bin())
            .args(["daemon", "-vv"])
            .env("HOME", self.server.path())
            .env("XDG_STATE_HOME", self.server.path().join("xdg"))
            .env_remove("IRSYNC_PASSPHRASE")
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut d = OldDaemon(child);
        let state = self.server.path().join("xdg/irsync");
        let deadline = Instant::now() + Duration::from_secs(30);
        while !probe(&state).is_some_and(|i| i.pid == d.0.id()) {
            if let Some(status) = d.0.try_wait().unwrap() {
                panic!("the old daemon exited ({status})");
            }
            assert!(Instant::now() < deadline, "the old daemon did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        d
    }

    /// The executable of the daemon holding the server's lock, with links
    /// resolved (macOS records the path it was started as).
    fn lock_exe(&self) -> PathBuf {
        probe(&self.server.path().join("xdg/irsync"))
            .expect("a running daemon")
            .exe
            .canonicalize()
            .unwrap()
    }

    /// `rel` under the server HOME, with links resolved.
    fn server_real(&self, rel: &str) -> PathBuf {
        self.server.path().canonicalize().unwrap().join(rel)
    }

    /// Grants the client immediate access on the server before any daemon
    /// starts: a daemon `ssh-setup` leaves running reads its config only
    /// at the start.
    fn grant_client(&self) {
        let out = common::irsync_bin(self.client.path(), &["id", "generate"]);
        assert!(out.status.success(), "{out:?}");
        let id = client_id(self.client.path());
        let out = common::irsync_bin(self.server.path(), &["grant", "add", "immediate", &id]);
        assert!(out.status.success(), "{out:?}");
    }
}

/// A daemon from the old install path, with no service file naming it, is
/// irsync's own: it is restarted from the new path, and the old binary and
/// its directory go.
#[test]
fn old_daemon_is_moved_to_the_new_path() {
    let w = World::new();
    let _old = w.start_old_daemon();
    let dest = w.server.path().join("dest");
    let err = ok(&w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]));
    assert!(err.contains("irsync:   restarted the daemon"), "{err}");
    assert_eq!(w.lock_exe(), w.server_real(".local/bin/irsync"));
    assert!(!w.old_bin().exists());
    assert!(!w.old_bin().parent().unwrap().exists());
    same_tree(w.src.path(), &dest);
}

/// A systemd unit still starts the old binary: its daemon and the binary
/// are left alone, and the client is told to change the unit.
#[test]
fn service_file_keeps_the_old_binary_and_daemon() {
    let w = World::new();
    w.grant_client();
    let unit = w.server.path().join(".config/systemd/user/irsync.service");
    std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
    std::fs::write(
        &unit,
        "[Service]\nExecStart=%h/.local/share/irsync/bin/irsync daemon -vv\n",
    )
    .unwrap();
    let old = w.start_old_daemon();
    let dest = w.server.path().join("dest");
    let err = ok(&w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]));
    let info = probe(&w.server.path().join("xdg/irsync")).expect("a running daemon");
    assert_eq!(info.pid, old.0.id(), "{err}");
    assert_eq!(w.lock_exe(), w.server_real(OLD_BIN));
    assert!(w.old_bin().exists());
    assert!(err
        .lines()
        .any(|l| l.contains("~/.config/systemd/user/irsync.service still starts irsync")),);
    assert!(
        err.lines()
            .any(|l| l.contains("Change the path in that file to ~/.local/bin/irsync")),
        "{err}"
    );
    same_tree(w.src.path(), &dest);
}

/// An old binary that nothing runs goes, with its empty directory.
#[test]
fn stale_old_binary_without_a_daemon_is_removed() {
    let w = World::new();
    let old = w.old_bin();
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, "old irsync\n").unwrap();
    let dest = w.server.path().join("dest");
    let err = ok(&w.irsync(&["-a", &w.src_arg(), &format!("myserver:{}", dest.display())]));
    assert!(!old.exists(), "{err}");
    assert!(!old.parent().unwrap().exists());
    // Only the bin directory goes.
    assert!(w.server.path().join(".local/share/irsync").exists());
    same_tree(w.src.path(), &dest);
}

/// `--no-install` deletes nothing; a daemon from the old path is still
/// moved to the new one.
#[test]
fn no_install_keeps_the_old_binary() {
    let w = World::new();
    let dest = w.server.path().join("dest");
    let target = format!("myserver:{}", dest.display());
    ok(&w.irsync(&["-a", &w.src_arg(), &target]));
    w.kill_daemon();
    let _old = w.start_old_daemon();
    std::fs::write(w.src.path().join("a.txt"), "changed\n").unwrap();
    let err = ok(&w.irsync(&["-a", "--no-install", "--ssh", &w.src_arg(), &target]));
    assert!(err.contains("irsync:   restarted the daemon"), "{err}");
    assert_eq!(w.lock_exe(), w.server_real(".local/bin/irsync"));
    assert!(w.old_bin().exists());
    assert_eq!(
        std::fs::read_to_string(dest.join("a.txt")).unwrap(),
        "changed\n"
    );
}
