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
        let mut all = args.to_vec();
        all.extend(["--address", &addr]);
        self.irsync_bare(&all)
    }

    /// As [`World::irsync`], without `--address`: the client dials what it
    /// learns from the setup and from its lookups.
    fn irsync_bare(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_irsync"));
        cmd.args(args)
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
        match &self.release_url {
            Some(u) => cmd.env("IRSYNC_TEST_RELEASE_URL", u),
            None => cmd.env_remove("IRSYNC_TEST_RELEASE_URL"),
        };
        cmd.env("IRSYNC_TEST_HOSTNAME", &self.client_name);
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
        assert_eq!(cfg["hosts"][a]["ssh"].as_str(), Some("u@myserver"), "{a}");
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
        cfg["hosts"]["myserver"]["ssh"].as_str(),
        Some("192.168.1.5")
    );
    assert!(cfg["hosts"].get("192.168.1.5").is_none());
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
    // The other spelling of the address ends on the same alias.
    ok(&w.irsync(&[
        "-a",
        &w.src_arg(),
        &format!("u@192.168.1.5:{}", dest.display()),
    ]));
    let cfg: toml::Table = w.client_config().parse().unwrap();
    assert_eq!(cfg["hosts"].as_table().unwrap().len(), 1, "{cfg}");
    assert_eq!(
        cfg["hosts"]["myserver"]["ssh"].as_str(),
        Some("u@192.168.1.5")
    );
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
        "[hosts.a]\nendpoint_id = \"{}\"\nssh = \"192.168.1.5\"\n\
         [hosts.b]\nendpoint_id = \"{}\"\nssh = \"192.168.1.5\"\n",
        id(7),
        id(8)
    );
    std::fs::write(cfg.join("irsync.toml"), &text).unwrap();
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
    assert!(
        err.contains("there is no release binary for that platform"),
        "{err}"
    );
    assert!(err.contains("set the server up by hand"), "{err}");
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
        err.contains(&format!("(release binary for {platform})")),
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

/// Whether the server config grants `id` immediate access, by its id or
/// by a name its `[hosts]` has for it.
fn immediate_granted(w: &World, id: &str) -> bool {
    let doc: toml::Table = w.server_toml().parse().unwrap();
    let named = |entry: &str| {
        doc.get("hosts")
            .and_then(|h| h.get(entry))
            .and_then(|h| h.get("endpoint_id"))
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
        t["hosts"]["myclient"]["endpoint_id"].as_str(),
        Some(&*client_id(w.client.path()))
    );
    assert_eq!(t["immediate"]["rw"][0].as_str(), Some("myclient"));
    // The automatic export the daemon made for the push names it too.
    let exports = t["exports"].as_table().unwrap();
    assert_eq!(exports.len(), 1, "{t}");
    let (_, e) = exports.iter().next().unwrap();
    assert_eq!(e["rw"][0].as_str(), Some("myclient"), "{t}");
}

#[test]
fn a_taken_client_name_is_refused_and_changes_nothing() {
    let w = World::new();
    let other = iroh::SecretKey::from_bytes(&[9; 32]).public();
    w.server_config(
        "",
        &format!("[hosts.myclient]\nendpoint_id = \"{other}\"\n"),
    );
    let before = w.server_toml();
    let out = w.irsync(&["-a", &w.src_arg(), "myserver:dest"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("[hosts.myclient]"), "{err}");
    assert_eq!(w.server_toml(), before);
    assert!(
        probe(&w.server.path().join("xdg/irsync")).is_none(),
        "no daemon started"
    );
    assert_eq!(w.client_config(), "", "no alias saved");
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
    std::fs::write(&cfg, text.replace("dht = false", "dht = true")).unwrap();
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

/// A first push with no `--address` and every client lookup off: the only
/// way to the new daemon is the address `ssh-setup` hands back. Without it
/// the client finds no address (the daemon has published nothing yet).
#[test]
fn first_push_dials_the_address_setup_reports() {
    let w = World::new();
    let cfg = w.client.path().join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
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
        err.contains("https://irsync.org/book/how-to/run-the-daemon.html"),
        "{err}"
    );
    assert_eq!(w.ssh_calls(), 1, "only the probe");
    assert!(
        !w.client_config().contains("[hosts"),
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
        !w.client_config().contains("[hosts"),
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
