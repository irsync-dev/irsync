//! `irsync ssh-setup`: every server-side step of SSH onboarding in one
//! command, run as the SSH user. It makes sure the daemon identity exists,
//! gives the client `all_paths = "rw"`, turns DHT publishing off when asked,
//! and gets this copy of irsync's daemon running, then reports what it did
//! as `key=value` lines.
//!
//! It stops or restarts only a daemon started from its own executable; any
//! other daemon is reported as foreign and left alone.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use iroh::{EndpointId, RelayUrl};
use std::net::SocketAddr;

use crate::authz::Access;
use crate::build_info::LONG_VERSION;
use crate::config::clients::Right;
use crate::config::file::Config;
use crate::config::names::short_host_name;
use crate::config::paths::Paths;
use crate::config::settings::Relay;
use crate::daemon_lock::{probe, LockInfo};
use crate::identity::{load_or_generate, parse_endpoint_id, IdentityError, PassphraseSource};
use crate::manage;
use irsync_proto::config as pb;

use super::service::{self, ServiceOutcome};

/// The first line of the report; text before it (a login shell's) is ignored.
pub const BEGIN: &str = "irsync-setup-begin";
/// The last line of the report.
pub const END: &str = "irsync-setup-end";

/// How long to wait for a daemon to stop or to start.
const DAEMON_WAIT: Duration = Duration::from_secs(10);

/// How long to wait for a running daemon to know its address (its home
/// relay, or with relays off a direct address).
const ADDR_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Created {
    Existing,
    Generated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantOutcome {
    Added,
    Already,
    /// `daemon.allow_all_paths` is off: nothing was granted.
    AllPathsOff,
    /// A recheck found no `all_paths = "rw"` or no `admin = "rw"` for the
    /// client and did not set it: the owner lowered or removed it.
    Missing,
}

/// Whether the daemon publishes its addresses in the mainline DHT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dht {
    On,
    Off,
    /// It was on; `--no-dht` turned it off in the config just now.
    TurnedOff,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonOutcome {
    /// This copy's daemon, at this version, was already running.
    Running,
    /// This copy's daemon was running and was told to re-read its settings
    /// (`SIGHUP`), because `ssh-setup` changed them.
    Reloaded,
    Started,
    /// This copy's daemon ran an older build; it was stopped and started again.
    Restarted {
        old_version: String,
    },
    /// A daemon from another executable holds the lock; it was left alone.
    Foreign {
        exe: PathBuf,
        version: String,
    },
}

/// What `ssh-setup` did, as the client reads it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupReport {
    pub version: String,
    /// The daemon's EndpointId.
    pub endpoint_id: EndpointId,
    pub identity: Created,
    pub grant: GrantOutcome,
    pub dht: Dht,
    pub daemon: DaemonOutcome,
    /// Where a daemon started by `ssh-setup` writes its output.
    pub log: PathBuf,
    /// The daemon's home relay, as it reported it.
    pub relay: Option<RelayUrl>,
    /// The daemon's direct addresses, as it reported them.
    pub addrs: Vec<SocketAddr>,
    /// What happened to the user service.
    pub service: ServiceOutcome,
}

impl SetupReport {
    /// The report between the [`BEGIN`] and [`END`] lines.
    pub fn to_lines(&self) -> String {
        let mut lines = vec![
            BEGIN.to_string(),
            format!("version={}", self.version),
            format!("endpoint_id={}", self.endpoint_id),
            format!(
                "identity={}",
                match self.identity {
                    Created::Existing => "existing",
                    Created::Generated => "generated",
                }
            ),
            format!(
                "grant={}",
                match self.grant {
                    GrantOutcome::Added => "added",
                    GrantOutcome::Already => "already",
                    GrantOutcome::AllPathsOff => "all_paths_off",
                    GrantOutcome::Missing => "missing",
                }
            ),
            format!(
                "dht={}",
                match self.dht {
                    Dht::On => "on",
                    Dht::Off => "off",
                    Dht::TurnedOff => "turned_off",
                }
            ),
        ];
        match &self.daemon {
            DaemonOutcome::Running => lines.push("daemon=running".into()),
            DaemonOutcome::Reloaded => lines.push("daemon=reloaded".into()),
            DaemonOutcome::Started => lines.push("daemon=started".into()),
            DaemonOutcome::Restarted { old_version } => {
                lines.push("daemon=restarted".into());
                lines.push(format!("daemon_old_version={old_version}"));
            }
            DaemonOutcome::Foreign { exe, version } => {
                lines.push("daemon=foreign".into());
                lines.push(format!("daemon_exe={}", exe.display()));
                lines.push(format!("daemon_version={version}"));
            }
        }
        lines.push(format!("log={}", self.log.display()));
        if let Some(r) = &self.relay {
            lines.push(format!("relay={r}"));
        }
        let addrs: Vec<String> = self.addrs.iter().map(|a| a.to_string()).collect();
        lines.push(format!("addrs={}", addrs.join(",")));
        match &self.service {
            ServiceOutcome::Systemd => lines.push("service=systemd".into()),
            ServiceOutcome::Launchd => lines.push("service=launchd".into()),
            ServiceOutcome::Skipped => lines.push("service=skipped".into()),
            ServiceOutcome::NotTouched => lines.push("service=not_touched".into()),
            ServiceOutcome::Unavailable => lines.push("service=unavailable".into()),
            ServiceOutcome::Failed(e) => {
                lines.push("service=failed".into());
                lines.push(format!("service_error={}", e.replace(['\r', '\n'], " ")));
            }
            ServiceOutcome::Foreign(f) => {
                lines.push("service=foreign".into());
                lines.push(format!("service_file={}", f.display()));
            }
        }
        lines.push(END.to_string());
        lines.join("\n") + "\n"
    }

    /// Reads the lines between [`BEGIN`] and [`END`] in `s`.
    pub fn parse(s: &str) -> Result<SetupReport, String> {
        let mut lines = s.lines().map(|l| l.trim_end_matches('\r'));
        if !lines.any(|l| l == BEGIN) {
            return Err(format!("no {BEGIN} line"));
        }
        let mut fields = std::collections::HashMap::new();
        let mut ended = false;
        for line in lines {
            if line == END {
                ended = true;
                break;
            }
            let (k, v) = line
                .split_once('=')
                .ok_or_else(|| format!("not a key=value line: {line:?}"))?;
            fields.insert(k, v);
        }
        if !ended {
            return Err(format!("no {END} line"));
        }
        let get = |k: &str| {
            fields
                .get(k)
                .map(|v| v.to_string())
                .ok_or_else(|| format!("missing {k}"))
        };
        let bad = |k: &str, v: &str| format!("unknown {k} value {v:?}");
        let endpoint_id =
            parse_endpoint_id(&get("endpoint_id")?).map_err(|e| format!("endpoint_id: {e}"))?;
        let identity = match get("identity")?.as_str() {
            "existing" => Created::Existing,
            "generated" => Created::Generated,
            v => return Err(bad("identity", v)),
        };
        let grant = match get("grant")?.as_str() {
            "added" => GrantOutcome::Added,
            "already" => GrantOutcome::Already,
            "all_paths_off" => GrantOutcome::AllPathsOff,
            "missing" => GrantOutcome::Missing,
            v => return Err(bad("grant", v)),
        };
        let dht = match get("dht")?.as_str() {
            "on" => Dht::On,
            "off" => Dht::Off,
            "turned_off" => Dht::TurnedOff,
            v => return Err(bad("dht", v)),
        };
        let daemon = match get("daemon")?.as_str() {
            "running" => DaemonOutcome::Running,
            "reloaded" => DaemonOutcome::Reloaded,
            "started" => DaemonOutcome::Started,
            "restarted" => DaemonOutcome::Restarted {
                old_version: get("daemon_old_version")?,
            },
            "foreign" => DaemonOutcome::Foreign {
                exe: PathBuf::from(get("daemon_exe")?),
                version: get("daemon_version")?,
            },
            v => return Err(bad("daemon", v)),
        };
        let service = match get("service")?.as_str() {
            "skipped" => ServiceOutcome::Skipped,
            "systemd" => ServiceOutcome::Systemd,
            "launchd" => ServiceOutcome::Launchd,
            "not_touched" => ServiceOutcome::NotTouched,
            "unavailable" => ServiceOutcome::Unavailable,
            "failed" => ServiceOutcome::Failed(
                fields
                    .get("service_error")
                    .map(|e| e.to_string())
                    .unwrap_or_default(),
            ),
            "foreign" => ServiceOutcome::Foreign(PathBuf::from(get("service_file")?)),
            v => return Err(bad("service", v)),
        };
        Ok(SetupReport {
            version: get("version")?,
            endpoint_id,
            identity,
            grant,
            dht,
            daemon,
            log: PathBuf::from(get("log")?),
            relay: match fields.get("relay") {
                Some(v) => Some(v.parse().map_err(|e| format!("relay: {e}"))?),
                None => None,
            },
            service,
            addrs: match fields.get("addrs") {
                Some(v) if !v.is_empty() => v
                    .split(',')
                    .map(|a| a.parse().map_err(|e| format!("addrs: {a:?}: {e}")))
                    .collect::<Result<_, String>>()?,
                _ => Vec::new(),
            },
        })
    }
}

/// Whether `name` may stand for `client` in the server config `msg` (the
/// file at `path`): the name of the client in `[clients]`. The name may be
/// free, and the client entry is made; a server of that name does not
/// stand in the way.
///
/// It is refused when it is not a valid name, when another client has it,
/// and when grants name it and `[clients]` has no such name: they are left
/// from a client that was removed, or name a server, and must not pass to
/// whoever has the name now. A client the server already has
/// a name for keeps that one, whatever it calls itself.
pub fn check_client_name(
    msg: &pb::Config,
    path: &Path,
    client: EndpointId,
    name: &str,
) -> anyhow::Result<()> {
    let Config { clients, .. } = Config::from_message(msg, path)?;
    if clients.alias_of(&client).is_some() {
        return Ok(());
    }
    // The same rule the client applies to its own name before it sends it.
    if short_host_name(name).as_deref() != Some(name) {
        bail!(
            "this client's host name {name:?} cannot be a name on this server \
             (it may not be empty, `localhost`, digits only, `all`, `*` or an \
             EndpointId, or contain `:`, `/`, `.` or control characters); \
             rename the client machine"
        );
    }
    if let Some(other) = clients.get(name) {
        bail!(
            "the name {name:?} is already used on this server, as [clients.{name}] \
             for the client {}. If that client is gone, remove it there with \
             `irsync client remove {name} -f`; else rename this machine",
            other.client_id.fmt_short()
        );
    }
    let lists = manage::grants::lists_naming(msg, name);
    if !lists.is_empty() {
        let lists: Vec<String> = lists.iter().map(|l| format!("[{l}]")).collect();
        let lists = lists.join(", ");
        bail!(
            "grants on this server ({lists}) name {name:?}, and no [clients.{name}] \
             says who that is. They are left from a client that was removed, or name \
             a server; take {name:?} out of those lists there, or rename this machine"
        );
    }
    Ok(())
}

/// Saves `client` as `[clients.name]` (unless it has a name there
/// already) and gives it `all_paths = "rw"` and `admin = "rw"`, after
/// [`check_client_name`] on `msg` itself. False when the client had both
/// already; the message is not changed then. The name
/// is only ever a label: the daemon decides access by the id it stands for.
pub fn grant_client(
    msg: &mut pb::Config,
    path: &Path,
    client: EndpointId,
    name: &str,
) -> anyhow::Result<bool> {
    check_client_name(msg, path, client, name)?;
    let clients = Config::from_message(msg, path)?.clients;
    let alias = match clients.alias_of(&client) {
        Some(a) => a.to_string(),
        None => {
            manage::clients::client_add(msg, name, client)?;
            name.to_string()
        }
    };
    let c = msg
        .clients
        .iter()
        .find(|c| c.name == alias)
        .context("the client entry just found is gone")?;
    let rw = Some(Access::ReadWrite);
    if Right::AllPaths.get(c) == rw && Right::Admin.get(c) == rw {
        return Ok(false);
    }
    manage::clients::client_set(msg, &alias, &[(Right::AllPaths, rw), (Right::Admin, rw)])?;
    Ok(true)
}

/// Does every server-side step for `client`: identity, grant, daemon.
/// `client_name`: the client's host name, the name it is saved and granted
/// under; refused, before anything is changed, when it cannot be a name
/// here or belongs to another client.
/// With `recheck` (a client checking a saved host that did not answer) a
/// missing `all_paths` right is set only when the daemon identity was just
/// generated, a rebuilt server; otherwise the owner removed it, and it is
/// reported as [`GrantOutcome::Missing`] instead.
/// With `no_dht`, `[daemon] dht` is set to `false` when it is on;
/// without it the setting is left as it is.
pub fn run(
    paths: &Paths,
    client: EndpointId,
    client_name: &str,
    recheck: bool,
    no_dht: bool,
    no_service: bool,
) -> anyhow::Result<SetupReport> {
    let Config { settings, .. } = Config::load(paths)?;

    // Before anything is changed: with `allow_all_paths` off nothing is
    // granted and the client is not named.
    if settings.daemon.allow_all_paths {
        let msg = manage::lock_config(&paths.config)?.load()?;
        check_client_name(&msg, &paths.config, client, client_name)
            .with_context(|| format!("in {}", paths.config.display()))?;
    }

    // 1. The daemon identity; no one is there to type a passphrase.
    let key = &settings.daemon.identity;
    let pass = PassphraseSource {
        allow_open: settings.unsafe_config_allowed,
        ..PassphraseSource::default()
    };
    let (secret, generated) = match load_or_generate(key, &pass, "irsync-daemon") {
        Ok(r) => r,
        Err(IdentityError::NoPassphraseSource(_) | IdentityError::PassphraseRequired) => bail!(
            "the daemon identity {} is encrypted: start the daemon by hand with `irsync daemon`",
            key.display()
        ),
        Err(e) => return Err(e).with_context(|| format!("identity {}", key.display())),
    };
    let identity = if generated {
        Created::Generated
    } else {
        Created::Existing
    };

    let home = usable_home(crate::config::paths::home_dir());
    let state = &settings.state_dir;
    let log = state.join("daemon.log");
    let exe = std::env::current_exe().context("finding this executable")?;
    // 2. The grant.
    let grant = if settings.daemon.allow_all_paths {
        let mut lock = manage::lock_config(&paths.config)?;
        let mut msg = lock.load()?;
        let before = msg.clone();
        // Checked again on the locked message: it may have changed.
        let added = grant_client(&mut msg, &paths.config, client, client_name)
            .with_context(|| format!("in {}", paths.config.display()))?;
        match (added, recheck && !generated) {
            (false, _) => {
                // The client already had `all_paths = "rw"` and `admin = "rw"`:
                // nothing changed.
                debug_assert_eq!(msg, before);
                GrantOutcome::Already
            }
            // Not saved: the edit is dropped with the lock.
            (true, true) => GrantOutcome::Missing,
            (true, false) => {
                lock.save(&mut msg)?;
                GrantOutcome::Added
            }
        }
    } else {
        GrantOutcome::AllPathsOff
    };

    // 3. DHT publishing: only ever turned off here, never back on.
    let dht = if !settings.daemon.discovery.dht {
        Dht::Off
    } else if no_dht {
        let mut lock = manage::lock_config(&paths.config)?;
        let mut msg = lock.load()?;
        let key = ["daemon", "dht"].map(String::from);
        manage::config::set(&mut msg, &key, manage::config::Value::Bool(false))
            .with_context(|| format!("in {}", paths.config.display()))?;
        lock.save(&mut msg)?;
        Dht::TurnedOff
    } else {
        Dht::On
    };

    // 4. The daemon, as a user service when there is
    // a service manager. A daemon that is not ours is left alone, and so is
    // the service then: it could not take the lock.
    let lock = probe(state);
    let ours = |i: &LockInfo| i.pid != 0 && i.exe == exe;
    // Where a service finds the config: always named, in full, since the
    // service manager's environment may differ from this session's.
    let config = std::path::absolute(&paths.config)
        .with_context(|| format!("finding {}", paths.config.display()))?;
    let reload = dht == Dht::TurnedOff;
    let bg = |lock| background(lock, &exe, paths, state, &log, reload, &ours);
    let (daemon, service) = if lock.as_ref().is_some_and(|i| !ours(i)) {
        (bg(lock)?, ServiceOutcome::NotTouched)
    } else if no_service {
        (bg(lock)?, ServiceOutcome::Skipped)
    } else {
        match service::detect().zip(home.as_deref()) {
            None => (bg(lock)?, ServiceOutcome::Unavailable),
            Some((m, h)) => {
                let file = service::unit_path(h, m);
                let text = std::fs::read_to_string(&file).ok();
                if service::classify(text.as_deref(), m, &exe, h) == service::Existing::Foreign {
                    (bg(lock)?, ServiceOutcome::Foreign(file))
                } else {
                    match as_service(m, h, lock, &exe, &config, state, &log, reload) {
                        Ok(d) => (
                            d,
                            match m {
                                service::Manager::Systemd => ServiceOutcome::Systemd,
                                service::Manager::Launchd => ServiceOutcome::Launchd,
                            },
                        ),
                        // As if there were no service manager.
                        Err(e) => (bg(probe(state))?, ServiceOutcome::Failed(e)),
                    }
                }
            }
        }
    };

    // 5. Where the daemon can be reached: a daemon that just started has
    // published nothing yet, so the client dials this first.
    let want_relay = settings.daemon.relay != Relay::Disabled;
    let reached = probe(state)
        .filter(|i| i.pid != 0)
        .and_then(|i| crate::daemon_addr::wait_for(state, i.pid, want_relay, ADDR_WAIT))
        .unwrap_or_default();

    Ok(SetupReport {
        version: LONG_VERSION.clone(),
        endpoint_id: secret.public(),
        identity,
        grant,
        dht,
        daemon,
        log,
        relay: reached.relay,
        addrs: reached.addrs,
        service,
    })
}

/// The daemon without a service: started in the background, in a session
/// of its own, when the lock is free. A daemon of `ours` at this version is
/// left running (and told to reload when `reload`); one at another
/// version is restarted; any other is left alone.
fn background(
    lock: Option<LockInfo>,
    exe: &Path,
    paths: &Paths,
    state: &Path,
    log: &Path,
    reload: bool,
    ours: &dyn Fn(&LockInfo) -> bool,
) -> anyhow::Result<DaemonOutcome> {
    Ok(match lock {
        None => {
            start_daemon(exe, paths, state, log)?;
            DaemonOutcome::Started
        }
        Some(i) if ours(&i) => {
            if i.version == *LONG_VERSION && i.exe == exe && reload {
                reload_daemon(i.pid)?;
                DaemonOutcome::Reloaded
            } else if i.version == *LONG_VERSION && i.exe == exe {
                DaemonOutcome::Running
            } else {
                stop_daemon(i.pid, state)?;
                start_daemon(exe, paths, state, log)?;
                DaemonOutcome::Restarted {
                    old_version: i.version,
                }
            }
        }
        Some(LockInfo { exe, version, .. }) => DaemonOutcome::Foreign { exe, version },
    })
}

/// The daemon as a user service of `m`. A service that runs this build is
/// left alone (and told to reload when `reload`). Otherwise the service is
/// prepared first, and only then is a daemon of ours that runs outside it
/// stopped, so a refusal (linger, say) leaves that daemon running. A
/// service that runs an older build is restarted. Waits for the lock as
/// [`start_daemon`] does; when it never comes, or the start fails, the
/// service is turned off again, so the background start that follows is
/// the only daemon. `lock`
/// is held by a daemon of ours, or by none.
#[allow(clippy::too_many_arguments)]
fn as_service(
    m: service::Manager,
    home: &Path,
    lock: Option<LockInfo>,
    exe: &Path,
    config: &Path,
    state: &Path,
    log: &Path,
    reload: bool,
) -> Result<DaemonOutcome, String> {
    let active = service::is_active(m);
    let current = |i: &LockInfo| i.exe == exe && i.version == *LONG_VERSION;
    if let Some(i) = lock.as_ref().filter(|i| active && current(i)) {
        if reload {
            reload_daemon(i.pid).map_err(|e| format!("{e:#}"))?;
            return Ok(DaemonOutcome::Reloaded);
        }
        return Ok(DaemonOutcome::Running);
    }
    service::prepare(m, home, exe, config, log)?;
    let outcome = match lock {
        Some(i) => {
            if !active {
                stop_daemon(i.pid, state).map_err(|e| format!("{e:#}"))?;
            }
            if current(&i) {
                // A background daemon of this build, moved into the service.
                DaemonOutcome::Started
            } else {
                DaemonOutcome::Restarted {
                    old_version: i.version,
                }
            }
        }
        None => DaemonOutcome::Started,
    };
    // Turned off again on a failure: `enable --now` may have enabled the
    // service before its start failed.
    let off = |mut e: String| {
        if let Err(off) = service::deactivate(m) {
            e.push_str(&format!(", and turning it off again failed: {off}"));
        }
        e
    };
    service::activate(m, home, active).map_err(off)?;
    let deadline = Instant::now() + DAEMON_WAIT;
    while !probe(state).is_some_and(|i| i.pid != 0 && current(&i)) {
        if Instant::now() >= deadline {
            return Err(off(format!(
                "the service did not start the daemon within {} s",
                DAEMON_WAIT.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(outcome)
}

/// The home directory for the user service: none unless it is absolute,
/// so nothing is ever looked up or written relative to the current
/// directory.
fn usable_home(home: Option<PathBuf>) -> Option<PathBuf> {
    home.filter(|h| h.is_absolute())
}

/// Tells this copy's daemon `pid` to re-read its settings.
fn reload_daemon(pid: u32) -> anyhow::Result<()> {
    crate::platform::daemonize::send_reload(pid)
}

/// Sends SIGTERM to this copy's daemon `pid` and waits for its lock to go.
fn stop_daemon(pid: u32, state: &Path) -> anyhow::Result<()> {
    crate::platform::daemonize::send_stop(pid)?;
    let deadline = Instant::now() + DAEMON_WAIT;
    while probe(state).is_some() {
        if Instant::now() >= deadline {
            bail!(
                "the old daemon (pid {pid}) did not stop within {} s",
                DAEMON_WAIT.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// The `exe daemon -vv` command line, without where its output goes.
fn daemon_command(exe: &Path, paths: &Paths) -> Command {
    let mut cmd = Command::new(exe);
    // `-vv`: info logs, as a service definition runs it; the default is warnings.
    cmd.args(["daemon", "-vv"]);
    // A long-lived background daemon must not quietly write TLS secrets or
    // a capture that were meant for one debugging run of the client.
    cmd.env_remove("SSLKEYLOGFILE").env_remove("IRSYNC_CAPTURE");
    if *paths != Paths::resolve(None) {
        cmd.arg("--config").arg(&paths.config);
    }
    cmd
}

/// Starts `exe daemon -vv` in its own session, output appended to `log`, and
/// waits until it holds the lock.
fn start_daemon(exe: &Path, paths: &Paths, state: &Path, log: &Path) -> anyhow::Result<()> {
    crate::fs::private::create_dir_all(state)
        .with_context(|| format!("creating {}", state.display()))?;
    // The log names files and peers: for the owner only.
    let out = crate::platform::daemonize::open_log(log)
        .with_context(|| format!("opening {}", log.display()))?;
    let start = out.metadata().map(|m| m.len()).unwrap_or(0);
    let mut cmd = daemon_command(exe, paths);
    cmd.stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out);
    crate::platform::daemonize::new_session(&mut cmd);
    let mut child = cmd
        .spawn()
        .with_context(|| format!("starting {} daemon", exe.display()))?;
    let pid = child.id();
    let deadline = Instant::now() + DAEMON_WAIT;
    loop {
        if probe(state).is_some_and(|i| i.pid == pid) {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            bail!(
                "the daemon exited ({status}) before it was ready; the end of {}:\n{}",
                log.display(),
                tail(log, start, 20)
            );
        }
        if Instant::now() >= deadline {
            bail!(
                "the daemon (pid {pid}) was not ready within {} s; see {}",
                DAEMON_WAIT.as_secs(),
                log.display()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The last `n` lines of `path` written after byte `from`.
fn tail(path: &Path, from: u64, n: usize) -> String {
    let mut text = String::new();
    if let Ok(mut f) = std::fs::File::open(path) {
        let _ = f.seek(SeekFrom::Start(from));
        let mut bytes = Vec::new();
        let _ = f.read_to_end(&mut bytes);
        text = String::from_utf8_lossy(&bytes).into_owned();
    }
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> SetupReport {
        SetupReport {
            version: "irsync 1.2.3 (abc 2026-11-10)".into(),
            endpoint_id: iroh::SecretKey::from_bytes(&[7; 32]).public(),
            identity: Created::Existing,
            grant: GrantOutcome::Already,
            dht: Dht::Off,
            daemon: DaemonOutcome::Running,
            log: PathBuf::from("/home/u/.local/state/irsync/daemon.log"),
            relay: None,
            addrs: Vec::new(),
            service: ServiceOutcome::Skipped,
        }
    }

    #[test]
    fn report_round_trips_each_service_outcome() {
        for service in [
            ServiceOutcome::Systemd,
            ServiceOutcome::Launchd,
            ServiceOutcome::Skipped,
            ServiceOutcome::NotTouched,
            ServiceOutcome::Unavailable,
            ServiceOutcome::Failed("linger: denied".into()),
            ServiceOutcome::Foreign(PathBuf::from("/h/.config/systemd/user/irsync.service")),
        ] {
            let mut r = report();
            r.service = service;
            assert_eq!(SetupReport::parse(&r.to_lines()).unwrap(), r);
        }
        let mut r = report();
        r.service = ServiceOutcome::Failed("first\nsecond".into());
        let text = r.to_lines();
        assert!(text.contains("\nservice_error=first second\n"), "{text}");
    }

    #[test]
    fn report_without_a_service_line_is_refused() {
        let text: String = report()
            .to_lines()
            .lines()
            .filter(|l| !l.starts_with("service"))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(SetupReport::parse(&text).unwrap_err(), "missing service");
    }

    /// A relative `$HOME` is no home: nothing is looked up or removed
    /// relative to the current directory.
    #[test]
    fn relative_home_is_no_home() {
        assert_eq!(usable_home(Some(PathBuf::from("relative/home"))), None);
        assert_eq!(usable_home(Some(PathBuf::from(""))), None);
        assert_eq!(usable_home(None), None);
        assert_eq!(
            usable_home(Some(PathBuf::from("/home/u"))),
            Some(PathBuf::from("/home/u"))
        );
    }

    #[test]
    fn daemon_never_inherits_capture_or_key_log() {
        // The daemon outlives this ssh session; a capture or TLS key log
        // meant for one debugging run must not follow it.
        let cmd = daemon_command(Path::new("irsync"), &Paths::resolve(None));
        let envs: Vec<_> = cmd.get_envs().collect();
        for var in ["SSLKEYLOGFILE", "IRSYNC_CAPTURE"] {
            assert!(
                envs.contains(&(std::ffi::OsStr::new(var), None)),
                "{var} is not removed: {envs:?}"
            );
        }
    }
}
