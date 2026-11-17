//! `irsync ssh-setup`: every server-side step of SSH onboarding in one
//! command, run as the SSH user. It makes sure the daemon identity exists,
//! grants the client immediate access, turns DHT publishing off when asked,
//! and gets this copy of irsync's daemon running, then reports what it did
//! as `key=value` lines.
//!
//! It stops or restarts only a daemon started from its own executable; any
//! other daemon is reported as foreign and left alone.

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use iroh::{EndpointId, RelayUrl};
use std::net::SocketAddr;

use crate::build::LONG_VERSION;
use crate::config::file::Config;
use crate::config::hosts::valid_alias;
use crate::config::paths::Paths;
use crate::config::settings::RelayMode;
use crate::daemon_lock::{probe, LockInfo};
use crate::identity::{load_or_generate, parse_endpoint_id, IdentityError, PassphraseSource};
use crate::manage::{self, PeerSpec};
use toml_edit::DocumentMut;

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
    /// `daemon.immediate` is off: nothing was granted.
    ImmediateOff,
    /// A recheck found no immediate grant for the client and did not add
    /// one: the owner removed it.
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
                    GrantOutcome::ImmediateOff => "immediate_off",
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
            "immediate_off" => GrantOutcome::ImmediateOff,
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

/// Whether `name` may stand for `client` in the server config `doc` (the
/// file at `path`). A host entry is a machine: `endpoint_id` is its daemon,
/// `client_id` the machine as a client. The name may be:
///
/// - free: the entry is made, with `client_id` only;
/// - an entry with no `client_id` that no grant names: this server dials
///   that machine's daemon by the name, and the client joins the entry.
///
/// It is refused when it is not a valid alias, when the entry already has
/// another client, and when grants name it without it being this client:
/// they are for a peer named by hand, or left from a client that was
/// removed, and must not pass to whoever has the name now. A client the
/// server already has a name for keeps that one, whatever it calls itself.
pub fn check_client_name(
    doc: &DocumentMut,
    path: &Path,
    client: EndpointId,
    name: &str,
) -> anyhow::Result<()> {
    let hosts = Config::parse(&doc.to_string(), path)?.hosts;
    if hosts.alias_of(&client).is_some() {
        return Ok(());
    }
    if !valid_alias(name) {
        bail!(
            "this client's host name {name:?} cannot be a name on this server \
             (it may not be empty, contain `:` or `/`, or be `all`, `*` or an \
             EndpointId); rename the client machine"
        );
    }
    let entry = hosts.get(name);
    if let Some((h, other)) = entry.and_then(|h| Some((h, h.client_id?))) {
        // The entry may also be how this server dials that machine: then
        // only its client id is to be replaced, not the host removed.
        let fix = if h.endpoint_id.is_some() {
            format!("replace it there with `irsync config set hosts.{name}.client_id {client}`")
        } else {
            format!("remove it there with `irsync host remove {name} -f`")
        };
        bail!(
            "the name {name:?} is already used on this server, as [hosts.{name}] \
             for the client {}. If that client is gone, {fix}; else rename this machine",
            other.fmt_short()
        );
    }
    let lists = manage::grants::lists_naming(doc, name);
    if !lists.is_empty() {
        let lists: Vec<String> = lists.iter().map(|l| format!("[{l}]")).collect();
        let lists = lists.join(", ");
        match entry {
            Some(h) => bail!(
                "grants on this server ({lists}) name {name:?}, which is [hosts.{name}] \
                 there, for {}. Those grants stay with that id. Rename this machine, \
                 or rename that entry there with `irsync host rename`",
                h.peer_id().fmt_short()
            ),
            None => bail!(
                "grants on this server still name {name:?} ({lists}), and no \
                 [hosts.{name}] says who that is. They are left from a client that \
                 was removed; take {name:?} out of those lists there, or rename \
                 this machine"
            ),
        }
    }
    Ok(())
}

/// Saves `client` as the `client_id` of `[hosts.name]` (unless it has a
/// name there already) and grants that name immediate access, after
/// [`check_client_name`] on `doc` itself. False when the client was
/// already granted, by a name or by its id; the `[hosts]` entry is written
/// all the same. The name is only ever a label: the daemon decides access
/// by the id it stands for.
pub fn grant_client(
    doc: &mut DocumentMut,
    path: &Path,
    client: EndpointId,
    name: &str,
) -> anyhow::Result<bool> {
    check_client_name(doc, path, client, name)?;
    let hosts = Config::parse(&doc.to_string(), path)?.hosts;
    let alias = match hosts.alias_of(&client) {
        Some(a) => a.to_string(),
        None => {
            manage::hosts::host_set_client(doc, name, client)?;
            name.to_string()
        }
    };
    let resolve = |s: &str| {
        if s == alias {
            return Some(client);
        }
        hosts.get(s).map(|h| h.peer_id())
    };
    let spec = PeerSpec::Alias(alias.clone());
    Ok(manage::grants::grant_immediate(doc, &spec, &resolve)?)
}

/// Does every server-side step for `client`: identity, grant, daemon.
/// `client_name`: the client's host name, the name it is saved and granted
/// under; refused, before anything is changed, when it cannot be a name
/// here or belongs to another client.
/// With `recheck` (a client checking a saved host that did not answer) a
/// missing immediate grant is added only when the daemon identity was just
/// generated, a rebuilt server; otherwise the owner removed it, and it is
/// reported as [`GrantOutcome::Missing`] instead.
/// With `no_dht`, `[daemon.discovery] dht` is set to `false` when it is on;
/// without it the setting is left as it is.
pub fn run(
    paths: &Paths,
    client: EndpointId,
    client_name: &str,
    recheck: bool,
    no_dht: bool,
) -> anyhow::Result<SetupReport> {
    let Config { settings, .. } = Config::load(paths)?;

    // Before anything is changed: with immediate mode off nothing is
    // granted and the client is not named.
    if settings.daemon.immediate {
        let doc = manage::lock_config(&paths.config)?.load()?;
        check_client_name(&doc, &paths.config, client, client_name)
            .with_context(|| format!("in {}", paths.config.display()))?;
    }

    // 1. The daemon identity; no one is there to type a passphrase.
    let key = &settings.daemon.identity;
    let (secret, generated) =
        match load_or_generate(key, &PassphraseSource::default(), "irsync-daemon") {
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

    // 2. The grant.
    let grant = if settings.daemon.immediate {
        let lock = manage::lock_config(&paths.config)?;
        let mut doc = lock.load()?;
        let before = doc.to_string();
        // Checked again on the locked document: it may have changed.
        let added = grant_client(&mut doc, &paths.config, client, client_name)
            .with_context(|| format!("in {}", paths.config.display()))?;
        match (added, recheck && !generated) {
            (false, _) => {
                // Granted by its bare id before it had a name: the name
                // is new.
                if doc.to_string() != before {
                    lock.save(&mut doc)?;
                }
                GrantOutcome::Already
            }
            // Not saved: the edit is dropped with the lock.
            (true, true) => GrantOutcome::Missing,
            (true, false) => {
                lock.save(&mut doc)?;
                GrantOutcome::Added
            }
        }
    } else {
        GrantOutcome::ImmediateOff
    };

    // 3. DHT publishing: only ever turned off here, never back on.
    let dht = if !settings.daemon.discovery.dht {
        Dht::Off
    } else if no_dht {
        let lock = manage::lock_config(&paths.config)?;
        let mut doc = lock.load()?;
        let key = ["daemon", "discovery", "dht"].map(String::from);
        manage::config::set(&mut doc, &key, toml_edit::Value::from(false))
            .with_context(|| format!("in {}", paths.config.display()))?;
        lock.save(&mut doc)?;
        Dht::TurnedOff
    } else {
        Dht::On
    };

    // 4. The daemon.
    let state = &settings.state_dir;
    let log = state.join("daemon.log");
    let exe = std::env::current_exe().context("finding this executable")?;
    let daemon = match probe(state) {
        None => {
            start_daemon(&exe, paths, state, &log)?;
            DaemonOutcome::Started
        }
        Some(LockInfo {
            pid,
            exe: held,
            version,
        }) if pid != 0 && held == exe => {
            if version == *LONG_VERSION && dht == Dht::TurnedOff {
                reload_daemon(pid)?;
                DaemonOutcome::Reloaded
            } else if version == *LONG_VERSION {
                DaemonOutcome::Running
            } else {
                stop_daemon(pid, state)?;
                start_daemon(&exe, paths, state, &log)?;
                DaemonOutcome::Restarted {
                    old_version: version,
                }
            }
        }
        Some(LockInfo { exe, version, .. }) => DaemonOutcome::Foreign { exe, version },
    };

    // 5. Where the daemon can be reached: a daemon that just started has
    // published nothing yet, so the client dials this first.
    let want_relay = settings.daemon.relay_mode != RelayMode::Disabled;
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
    })
}

/// Tells this copy's daemon `pid` to re-read its settings.
fn reload_daemon(pid: u32) -> anyhow::Result<()> {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    let raw = i32::try_from(pid).context("daemon pid out of range")?;
    kill(Pid::from_raw(raw), Signal::SIGHUP)
        .with_context(|| format!("telling the daemon (pid {pid}) to reload"))
}

/// Sends SIGTERM to this copy's daemon `pid` and waits for its lock to go.
fn stop_daemon(pid: u32, state: &Path) -> anyhow::Result<()> {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    let raw = i32::try_from(pid).context("daemon pid out of range")?;
    kill(Pid::from_raw(raw), Signal::SIGTERM)
        .with_context(|| format!("stopping the old daemon (pid {pid})"))?;
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
    std::fs::create_dir_all(state).with_context(|| format!("creating {}", state.display()))?;
    let out = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .with_context(|| format!("opening {}", log.display()))?;
    let start = out.metadata().map(|m| m.len()).unwrap_or(0);
    let mut cmd = daemon_command(exe, paths);
    cmd.stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out);
    // SAFETY: setsid is async-signal-safe and touches no Rust state.
    unsafe {
        cmd.pre_exec(|| {
            nix::unistd::setsid()
                .map(|_| ())
                .map_err(std::io::Error::from)
        });
    }
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
