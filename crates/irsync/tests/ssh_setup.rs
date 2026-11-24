//! `irsync ssh-setup`: the server side of SSH onboarding.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::irsync_bin;
use iroh::{EndpointId, SecretKey};
use irsync::authz::Access;
use irsync::config::file::Config;
use irsync::config::reader;
use irsync::config::writer::write;
use irsync::daemon_lock::probe;
use irsync::platform::testutil::{send_signal, Signal};
use irsync::ssh::setup::{
    check_client_name, grant_client, Created, DaemonOutcome, Dht, GrantOutcome, OldBin, SetupReport,
};
use irsync_proto::config as pb;

/// `name` as `--client-name-hex` takes it.
fn hex(name: &str) -> String {
    name.bytes().map(|b| format!("{b:02x}")).collect()
}

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
        irsync::fs::private::write(cfg.join("irsync.toml"), text).unwrap();
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
        self.setup_named(client, "myclient", extra)
    }

    /// `ssh-setup` as the client `name` runs it; the raw result.
    fn try_setup(&self, client: &EndpointId, name: &str, extra: &[&str]) -> std::process::Output {
        let (id, name) = (client.to_string(), hex(name));
        let mut args = vec!["ssh-setup", "--client", &id, "--client-name-hex", &name];
        args.extend_from_slice(extra);
        irsync_bin(self.path(), &args)
    }

    fn setup_named(&self, client: &EndpointId, name: &str, extra: &[&str]) -> SetupReport {
        let out = self.try_setup(client, name, extra);
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
                let _ = send_signal(info.pid, Signal::Kill);
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
        GrantOutcome::AllPathsOff,
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
                    stale_services: Vec::new(),
                    old_bin: OldBin::Absent,
                    service: irsync::ssh::service::ServiceOutcome::Skipped,
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
    // The client is saved under its host name, with all_paths = "rw".
    assert!(!doc.contains_key("immediate"));
    assert_eq!(doc["clients"]["myclient"]["all_paths"].as_str(), Some("rw"));
    assert_eq!(
        doc["clients"]["myclient"]["client_id"].as_str().unwrap(),
        client.to_string()
    );
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
    irsync::fs::private::write(home.config(), text.replace("dht = false", "dht = true")).unwrap();
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
fn all_paths_off_reports_and_does_not_grant() {
    let home = Home::new("allow_all_paths = false\n");
    let r = home.setup(&client_id());
    assert_eq!(r.grant, GrantOutcome::AllPathsOff);
    assert!(!read_config(&home).contains_key("clients"));
}

/// A client the owner lowered to `all_paths = "ro"` is not raised again
/// by a recheck.
#[test]
fn a_recheck_after_lowering_to_ro_reports_missing() {
    let home = Home::new("");
    let client = client_id();
    assert_eq!(home.setup(&client).grant, GrantOutcome::Added);
    common::set_all_paths(home.path(), &client.to_string(), Some(Access::ReadOnly));
    let bytes = std::fs::read(home.config()).unwrap();
    let r = home.setup_with(&client, &["--recheck"]);
    assert_eq!(r.grant, GrantOutcome::Missing);
    assert_eq!(std::fs::read(home.config()).unwrap(), bytes);
    let doc = read_config(&home);
    assert_eq!(doc["clients"]["myclient"]["all_paths"].as_str(), Some("ro"));
}

/// `--recheck` (the client's fallback for a saved host) never puts back an
/// all_paths right the owner removed; it adds one only when the daemon
/// identity was just generated (a rebuilt server).
#[test]
fn recheck_does_not_regrant() {
    let home = Home::new("");
    let client = client_id();
    assert_eq!(home.setup(&client).grant, GrantOutcome::Added);
    common::set_all_paths(home.path(), &client.to_string(), None);
    let bytes = std::fs::read(home.config()).unwrap();
    let r = home.setup_with(&client, &["--recheck"]);
    assert_eq!(r.grant, GrantOutcome::Missing);
    assert_eq!(r.identity, Created::Existing);
    assert_eq!(std::fs::read(home.config()).unwrap(), bytes);
    // Still granted: nothing to say.
    let other = client_id();
    common::set_all_paths(home.path(), &other.to_string(), Some(Access::ReadWrite));
    assert_eq!(
        home.setup_named(&other, "other", &["--recheck"]).grant,
        GrantOutcome::Already
    );
    // It had all_paths under another name; that name stays.
    let doc = read_config(&home);
    assert_eq!(
        doc["clients"]["granted"]["client_id"].as_str().unwrap(),
        other.to_string()
    );
    assert!(!doc["clients"].as_table().unwrap().contains_key("other"));
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
    // A newer irsync than this one: it reads what setup writes.
    let mut helper = hold_lock(&home.state(), "/usr/bin/other", "9.9.9 (abc)");
    let r = home.setup(&client_id());
    assert_eq!(
        r.daemon,
        DaemonOutcome::Foreign {
            exe: PathBuf::from("/usr/bin/other"),
            version: "9.9.9 (abc)".into()
        }
    );
    assert!(helper.0.try_wait().unwrap().is_none(), "helper was stopped");
    assert_eq!(probe(&home.state()).unwrap().pid, helper.0.id());
}

#[test]
fn setup_refuses_to_write_past_an_old_foreign_daemon() {
    if !have_flock() {
        eprintln!("skipping: flock(1) not installed");
        return;
    }
    let home = Home::new("");
    let before = std::fs::read_to_string(home.config()).unwrap();
    // Setup leaves a foreign daemon running, and this one cannot read
    // config_version 2.
    let mut helper = hold_lock(&home.state(), "/usr/bin/other", "0.1.0-beta.1 (abc)");
    let out = home.try_setup(&client_id(), "myclient", &[]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("restart the daemon"), "{stderr}");
    assert_eq!(std::fs::read_to_string(home.config()).unwrap(), before);
    assert!(helper.0.try_wait().unwrap().is_none(), "helper was stopped");
}

#[test]
fn a_client_with_an_alias_gets_all_paths_under_it() {
    let home = Home::new("");
    let client = client_id();
    let mut text = std::fs::read_to_string(home.config()).unwrap();
    text.push_str(&format!("[clients.laptop]\nclient_id = \"{client}\"\n"));
    irsync::fs::private::write(home.config(), text).unwrap();
    assert_eq!(home.setup(&client).grant, GrantOutcome::Added);
    let doc = read_config(&home);
    assert_eq!(doc["clients"]["laptop"]["all_paths"].as_str(), Some("rw"));
    // The right is recognised on a second run.
    assert_eq!(home.setup(&client).grant, GrantOutcome::Already);
}

#[test]
fn the_report_still_parses_with_logging_on() {
    let home = Home::new("");
    let client = client_id();
    // The first run creates the identity and the grant and starts the daemon.
    home.setup(&client);
    let logged = home.setup_with(&client, &["-vv"]);
    let plain = home.setup(&client);
    assert_eq!(format!("{logged:?}"), format!("{plain:?}"));
}

fn id(n: u8) -> EndpointId {
    SecretKey::from_bytes(&[n; 32]).public()
}

fn cfg() -> &'static Path {
    Path::new("/x/irsync.toml")
}

fn config_from(text: &str) -> Config {
    Config::parse(text, Path::new("/x/irsync.toml")).unwrap()
}

/// The message of `text`, whatever its layout.
fn read(text: &str, path: &Path) -> Result<pb::Config, irsync::config::ConfigError> {
    reader::read(text, path).map(|r| r.config)
}

#[test]
fn client_name_rules() {
    let doc = read(
        &format!("[clients.myclient]\nclient_id = \"{}\"\n", id(1)),
        cfg(),
    )
    .unwrap();
    let check = |client, name| check_client_name(&doc, cfg(), client, name);
    assert!(check(id(1), "myclient").is_ok());
    assert!(check(id(2), "other").is_ok());
    let e = check(id(2), "myclient").unwrap_err().to_string();
    assert_eq!(
        e,
        format!(
            "the name \"myclient\" is already used on this server, as [clients.myclient] \
             for the client {}. If that client is gone, remove it there with \
             `irsync client remove myclient -f`; else rename this machine",
            id(1).fmt_short()
        )
    );
    for bad in ["", "all", "*", "a/b", "a:b"] {
        assert!(check(id(2), bad).is_err(), "{bad:?}");
    }
    // The names a client never sends are refused from a hand-run setup too.
    for bad in [
        "localhost",
        "LocalHost",
        "123",
        "tab\there",
        "a.b",
        " pad",
        "x\n[daemon]",
    ] {
        let e = check(id(2), bad).unwrap_err().to_string();
        assert!(
            e.contains("cannot be a name on this server"),
            "{bad:?}: {e}"
        );
    }
    // A client this server already has a name for is not being named.
    assert!(check(id(1), "a/b").is_ok());
}

/// The server dials a machine as `desktop`; that machine then comes as a
/// client called `desktop`. It is saved as a client beside the server:
/// the daemon to dial and the client id for grants, each in its table.
#[test]
fn a_server_of_the_same_name_is_no_obstacle() {
    let text = format!(
        "[servers.desktop]\nendpoint_id = \"{}\"\nssh = \"mike@desktop\"\n",
        id(5)
    );
    let mut doc = read(&text, cfg()).unwrap();
    assert!(grant_client(&mut doc, cfg(), id(2), "desktop").unwrap());
    let c = config_from(&write(&doc));
    let s = c.servers.get("desktop").unwrap();
    assert_eq!(s.endpoint_id, id(5));
    assert_eq!(s.ssh.as_deref(), Some("mike@desktop"));
    assert_eq!(c.clients.get("desktop").unwrap().client_id, id(2));
    assert_eq!((c.servers.entries.len(), c.clients.entries.len()), (1, 1));
    let t: toml::Table = write(&doc).parse().unwrap();
    assert_eq!(t["clients"]["desktop"]["all_paths"].as_str(), Some("rw"));
    assert_eq!(c.clients.resolve_peer("desktop").unwrap(), id(2));
}

/// A name that grants already name is a peer someone named by hand: a
/// client that calls itself the same must not get those grants.
#[test]
fn a_client_does_not_take_a_name_that_grants_name() {
    let text = format!(
        "[servers.desktop]\nendpoint_id = \"{}\"\n[exports.x]\npath = \"/x\"\nrw = [\"desktop\"]\n",
        id(5)
    );
    let mut doc = read(&text, cfg()).unwrap();
    let e = grant_client(&mut doc, cfg(), id(2), "desktop")
        .unwrap_err()
        .to_string();
    assert!(e.contains("[exports.x]") && e.contains("desktop"), "{e}");
    assert!(e.contains("no [clients.desktop] says who that is"), "{e}");
    assert!(e.contains("or name a server"), "{e}");
    assert_eq!(doc, read(&text, cfg()).unwrap());
}

#[test]
fn grants_the_name() {
    let mut doc = pb::Config::default();
    assert!(grant_client(&mut doc, cfg(), id(1), "myclient").unwrap());
    let t: toml::Table = write(&doc).parse().unwrap();
    assert_eq!(
        t["clients"]["myclient"]["client_id"].as_str(),
        Some(&*id(1).to_string())
    );
    assert!(t["clients"]["myclient"].get("endpoint_id").is_none());
    assert!(!t.contains_key("servers"));
    assert_eq!(t["clients"]["myclient"]["all_paths"].as_str(), Some("rw"));
    assert!(!t.contains_key("immediate"));
}

/// Whatever a client calls itself, the config stays valid TOML and the
/// name comes back as it went in.
#[test]
fn names_that_need_quoting_round_trip() {
    for name in [
        "my client",
        "say \"hi\"",
        "ü",
        "x = 1 [daemon]",
        "a]\\b",
        "'",
    ] {
        let mut doc = pb::Config::default();
        grant_client(&mut doc, cfg(), id(1), name).unwrap();
        let text = write(&doc);
        let t: toml::Table = text.parse().unwrap();
        assert_eq!(t.len(), 1, "{name:?}: {text}");
        assert!(
            t["clients"].as_table().unwrap().contains_key(name),
            "{name:?}: {text}"
        );
        assert_eq!(t["clients"][name]["all_paths"].as_str(), Some("rw"));
        // And irsync itself reads it back as that client.
        assert_eq!(
            config_from(&text).clients.get(name).unwrap().client_id,
            id(1)
        );
    }
}

#[test]
fn an_existing_all_paths_rw_counts() {
    let text = format!(
        "[clients.laptop]\nclient_id = \"{}\"\nall_paths = \"rw\"\n",
        id(1)
    );
    let mut doc = read(&text, cfg()).unwrap();
    assert!(!grant_client(&mut doc, cfg(), id(1), "myclient").unwrap());
    assert_eq!(doc, read(&text, cfg()).unwrap());
}

/// A name that belongs to another client is refused before anything on
/// the server changes: no identity, no grant, no daemon.
#[test]
fn a_taken_client_name_changes_nothing() {
    let home = Home::new("");
    let mut text = std::fs::read_to_string(home.config()).unwrap();
    text.push_str(&format!("[clients.myclient]\nclient_id = \"{}\"\n", id(1)));
    irsync::fs::private::write(home.config(), &text).unwrap();
    let out = home.try_setup(&id(2), "myclient", &[]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("[clients.myclient]"), "{err}");
    assert_eq!(std::fs::read_to_string(home.config()).unwrap(), text);
    assert!(probe(&home.state()).is_none());
    assert!(!home.path().join(".config/irsync/daemon_ed25519").exists());
    // Its own name: as before.
    assert_eq!(
        home.setup_named(&id(1), "myclient", &[]).grant,
        GrantOutcome::Added
    );
}

/// A grant that still names a client whose `[clients]` entry was removed
/// must not pass to the next client that calls itself that name.
#[test]
fn a_leftover_grant_name_is_not_taken_over() {
    for text in [
        "[immediate]\nrw = [\"myclient\"]\n",
        "[all]\nro = [\"myclient\"]\n",
        "[exports.photos]\npath = \"/p\"\nrw = [\"myclient\"]\n",
    ] {
        let mut doc = read(text, cfg()).unwrap();
        let e = grant_client(&mut doc, cfg(), id(2), "myclient")
            .unwrap_err()
            .to_string();
        assert!(e.contains("no [clients.myclient] says who that is"), "{e}");
        assert_eq!(doc, read(text, cfg()).unwrap());
    }
}

/// The name is checked against the config being edited, not against
/// what `[clients]` held before the lock was taken.
#[test]
fn a_name_taken_in_the_config_is_not_overwritten() {
    let text = format!("[clients.myclient]\nclient_id = \"{}\"\n", id(1));
    let mut doc = read(&text, cfg()).unwrap();
    assert!(grant_client(&mut doc, cfg(), id(2), "myclient").is_err());
    assert_eq!(doc, read(&text, cfg()).unwrap());
}

/// A recheck does not hand a removed client's leftover grant to whoever
/// has its name now, and changes nothing.
#[test]
fn recheck_does_not_revive_a_leftover_grant() {
    let home = Home::new("");
    let mut text = std::fs::read_to_string(home.config()).unwrap();
    text.push_str("[immediate]\nrw = [\"myclient\"]\n");
    irsync::fs::private::write(home.config(), &text).unwrap();
    for extra in [&["--recheck"][..], &[]] {
        let out = home.try_setup(&id(2), "myclient", extra);
        assert_eq!(out.status.code(), Some(1), "{extra:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("[immediate]"), "{err}");
        assert_eq!(std::fs::read_to_string(home.config()).unwrap(), text);
        assert!(probe(&home.state()).is_none());
    }
}
