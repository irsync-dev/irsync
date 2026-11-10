//! `irsync ssh-setup`: the server side of SSH onboarding.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::irsync_bin;
use iroh::{EndpointId, SecretKey};
use irsync::daemon_lock::probe;
use irsync::ssh::setup::{Created, DaemonOutcome, Dht, GrantOutcome, SetupReport};

/// A temp HOME with an offline daemon config; kills the daemon holding its
/// lock when dropped.
struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new(extra_daemon: &str) -> Home {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".config/irsync");
        std::fs::create_dir_all(&cfg).unwrap();
        let text = format!(
            "[daemon]\nrelay_mode = \"disabled\"\nportmap = false\n\
             listen = [\"127.0.0.1:0\"]\n{extra_daemon}\
             [daemon.discovery]\ndht = false\ndns = false\nmdns = false\n"
        );
        std::fs::write(cfg.join("irsync.toml"), text).unwrap();
        Home { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn state(&self) -> PathBuf {
        self.path().join("xdg/irsync")
    }

    fn config(&self) -> PathBuf {
        self.path().join(".config/irsync/irsync.toml")
    }

    fn setup(&self, client: &EndpointId) -> SetupReport {
        self.setup_with(client, &[])
    }

    fn setup_with(&self, client: &EndpointId, extra: &[&str]) -> SetupReport {
        let id = client.to_string();
        let mut args = vec!["ssh-setup", "--client", &id];
        args.extend_from_slice(extra);
        let out = irsync_bin(self.path(), &args);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "ssh-setup failed\nstdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        SetupReport::parse(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}"))
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        if let Some(info) = probe(&self.state()) {
            if info.pid != 0 {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(info.pid as i32),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
        }
    }
}

fn client_id() -> EndpointId {
    SecretKey::generate().public()
}

fn read_config(home: &Home) -> toml::Table {
    std::fs::read_to_string(home.config())
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn report_roundtrips() {
    let daemons = [
        DaemonOutcome::Running,
        DaemonOutcome::Reloaded,
        DaemonOutcome::Started,
        DaemonOutcome::Restarted {
            old_version: "irsync 0.1".into(),
        },
        DaemonOutcome::Foreign {
            exe: PathBuf::from("/usr/bin/other"),
            version: "irsync 0.0".into(),
        },
    ];
    let grants = [
        GrantOutcome::Added,
        GrantOutcome::Already,
        GrantOutcome::ImmediateOff,
        GrantOutcome::Missing,
    ];
    let ids = [Created::Existing, Created::Generated];
    let dhts = [Dht::On, Dht::Off, Dht::TurnedOff];
    for d in &daemons {
        for (g, dht) in grants.iter().zip(dhts.iter().cycle()) {
            for i in &ids {
                let r = SetupReport {
                    version: "irsync 1.2.3 (abc 2026-11-10)".into(),
                    endpoint_id: client_id(),
                    identity: i.clone(),
                    grant: g.clone(),
                    dht: dht.clone(),
                    daemon: d.clone(),
                    log: PathBuf::from("/home/u/.local/state/irsync/daemon.log"),
                    relay: g
                        .eq(&GrantOutcome::Added)
                        .then(|| "https://relay.example.net./".parse().unwrap()),
                    addrs: if *i == Created::Generated {
                        vec![
                            "192.0.2.1:7000".parse().unwrap(),
                            "[::1]:7000".parse().unwrap(),
                        ]
                    } else {
                        Vec::new()
                    },
                };
                let text = r.to_lines();
                assert_eq!(SetupReport::parse(&text).unwrap(), r, "{text}");
                let noisy = format!("Welcome to myserver\nidentity=existing\n{text}");
                assert_eq!(SetupReport::parse(&noisy).unwrap(), r);
            }
        }
    }
    assert!(SetupReport::parse("version=x\n").is_err());
}

#[test]
fn fresh_home_generates_grants_starts() {
    let home = Home::new("");
    let client = client_id();
    let r = home.setup(&client);
    assert_eq!(r.identity, Created::Generated);
    assert_eq!(r.grant, GrantOutcome::Added);
    assert_eq!(r.daemon, DaemonOutcome::Started);
    assert_eq!(r.version, *irsync::build::LONG_VERSION);
    assert_eq!(r.log, home.state().join("daemon.log"));
    let doc = read_config(&home);
    let rw = doc["immediate"]["rw"].as_array().unwrap();
    assert_eq!(rw.len(), 1);
    assert_eq!(rw[0].as_str().unwrap(), client.to_string());
    let info = probe(&home.state()).expect("daemon running");
    assert_ne!(info.pid, 0);
    // Relays are off in this config: the direct address is the report's.
    assert!(r.relay.is_none());
    assert!(
        r.addrs.iter().any(|a| a.ip().is_loopback()),
        "{:?}",
        r.addrs
    );
}

#[test]
fn second_run_changes_nothing() {
    let home = Home::new("");
    let client = client_id();
    let first = home.setup(&client);
    let bytes = std::fs::read(home.config()).unwrap();
    let pid = probe(&home.state()).unwrap().pid;
    let r = home.setup(&client);
    assert_eq!(r.identity, Created::Existing);
    assert_eq!(r.grant, GrantOutcome::Already);
    assert_eq!(r.daemon, DaemonOutcome::Running);
    assert_eq!(r.endpoint_id, first.endpoint_id);
    assert_eq!(std::fs::read(home.config()).unwrap(), bytes);
    assert_eq!(probe(&home.state()).unwrap().pid, pid);
}

/// The test config with DHT publishing on (no daemon is running yet, so
/// nothing ever publishes).
fn dht_on(home: &Home) {
    let text = std::fs::read_to_string(home.config()).unwrap();
    std::fs::write(home.config(), text.replace("dht = false", "dht = true")).unwrap();
}

/// Without `--no-dht` the setting is only reported. (With it on, the
/// daemon would publish, so "on" is covered by the reload test below.)
#[test]
fn dht_off_is_reported() {
    let home = Home::new("");
    let r = home.setup(&client_id());
    assert_eq!(r.dht, Dht::Off);
}

/// `--no-dht` writes `dht = false` before the daemon starts; a second run
/// finds it off. A running daemon whose setting was turned on by hand is
/// told to reload, not restarted.
#[test]
fn no_dht_turns_publishing_off_and_reloads() {
    let home = Home::new("");
    dht_on(&home);
    let client = client_id();
    let r = home.setup_with(&client, &["--no-dht"]);
    assert_eq!(r.dht, Dht::TurnedOff);
    assert_eq!(r.daemon, DaemonOutcome::Started);
    let doc = read_config(&home);
    assert_eq!(doc["daemon"]["discovery"]["dht"].as_bool(), Some(false));
    let pid = probe(&home.state()).unwrap().pid;
    let r = home.setup_with(&client, &["--no-dht"]);
    assert_eq!(r.dht, Dht::Off);
    assert_eq!(r.daemon, DaemonOutcome::Running);
    // Turned on by hand while the daemon (still with DHT off) runs.
    dht_on(&home);
    let r = home.setup_with(&client, &["--no-dht"]);
    assert_eq!(r.dht, Dht::TurnedOff);
    assert_eq!(r.daemon, DaemonOutcome::Reloaded);
    assert_eq!(probe(&home.state()).unwrap().pid, pid);
    let doc = read_config(&home);
    assert_eq!(doc["daemon"]["discovery"]["dht"].as_bool(), Some(false));
}

#[test]
fn immediate_off_reports_and_does_not_grant() {
    let home = Home::new("immediate = false\n");
    let r = home.setup(&client_id());
    assert_eq!(r.grant, GrantOutcome::ImmediateOff);
    assert!(!read_config(&home).contains_key("immediate"));
}

/// `--recheck` (the client's fallback for a saved host) never puts back an
/// immediate grant the owner removed; it adds one only when the daemon
/// identity was just generated (a rebuilt server).
#[test]
fn recheck_does_not_regrant() {
    let home = Home::new("");
    let client = client_id();
    assert_eq!(home.setup(&client).grant, GrantOutcome::Added);
    let out = irsync_bin(
        home.path(),
        &["grant", "remove", "immediate", &client.to_string()],
    );
    assert!(out.status.success(), "{out:?}");
    let bytes = std::fs::read(home.config()).unwrap();
    let r = home.setup_with(&client, &["--recheck"]);
    assert_eq!(r.grant, GrantOutcome::Missing);
    assert_eq!(r.identity, Created::Existing);
    assert_eq!(std::fs::read(home.config()).unwrap(), bytes);
    // Still granted: nothing to say.
    let other = client_id();
    let out = irsync_bin(
        home.path(),
        &["grant", "add", "immediate", &other.to_string()],
    );
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        home.setup_with(&other, &["--recheck"]).grant,
        GrantOutcome::Already
    );
    // A rebuilt server (fresh identity) gets the grant again.
    let fresh = Home::new("");
    let r = fresh.setup_with(&client, &["--recheck"]);
    assert_eq!(r.identity, Created::Generated);
    assert_eq!(r.grant, GrantOutcome::Added);
}

/// A child holding `daemon.lock` with a made-up record, as a daemon would.
struct Holder(Child);

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn have_flock() -> bool {
    Command::new("sh")
        .args(["-c", "command -v flock"])
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Holds the lock in `state` from a shell whose record names `exe` and
/// `version`; returns once `probe` sees it.
fn hold_lock(state: &Path, exe: &str, version: &str) -> Holder {
    std::fs::create_dir_all(state).unwrap();
    let child = Command::new("sh")
        .args([
            "-c",
            "exec 9>>\"$LOCK\"; flock 9; \
             printf 'pid=%s\\nexe=%s\\nversion=%s\\n' $$ \"$EXE\" \"$VER\" > \"$LOCK\"; \
             exec sleep 60",
        ])
        .env("LOCK", state.join("daemon.lock"))
        .env("EXE", exe)
        .env("VER", version)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let holder = Holder(child);
    let pid = holder.0.id();
    let deadline = Instant::now() + Duration::from_secs(10);
    while probe(state).map(|i| i.pid) != Some(pid) {
        assert!(Instant::now() < deadline, "helper never held the lock");
        std::thread::sleep(Duration::from_millis(20));
    }
    holder
}

#[test]
fn setup_restarts_own_stale_daemon() {
    if !have_flock() {
        eprintln!("skipping: flock(1) not installed");
        return;
    }
    let home = Home::new("");
    let mut helper = hold_lock(&home.state(), env!("CARGO_BIN_EXE_irsync"), "old");
    let r = home.setup(&client_id());
    assert_eq!(
        r.daemon,
        DaemonOutcome::Restarted {
            old_version: "old".into()
        }
    );
    assert!(
        helper.0.try_wait().unwrap().is_some(),
        "helper still running"
    );
    let info = probe(&home.state()).expect("new daemon running");
    assert_eq!(info.version, *irsync::build::LONG_VERSION);
}

#[test]
fn setup_leaves_foreign_daemon() {
    if !have_flock() {
        eprintln!("skipping: flock(1) not installed");
        return;
    }
    let home = Home::new("");
    let mut helper = hold_lock(&home.state(), "/usr/bin/other", "irsync 9.9");
    let r = home.setup(&client_id());
    assert_eq!(
        r.daemon,
        DaemonOutcome::Foreign {
            exe: PathBuf::from("/usr/bin/other"),
            version: "irsync 9.9".into()
        }
    );
    assert!(helper.0.try_wait().unwrap().is_none(), "helper was stopped");
    assert_eq!(probe(&home.state()).unwrap().pid, helper.0.id());
}
