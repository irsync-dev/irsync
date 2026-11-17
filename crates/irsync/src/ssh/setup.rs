//! `irsync ssh-setup`: every server-side step of SSH onboarding in one
//! command, run as the SSH user. It makes sure the daemon identity exists,
//! grants the client immediate access, turns DHT publishing off when asked,
//! and gets this copy of irsync's daemon running, then reports what it did
//! as `key=value` lines.
//!
//! It stops or restarts only a daemon started from its own executable, or
//! from the old install path when no service file starts that; any other
//! daemon is reported as foreign and left alone.

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
use crate::config::paths::Paths;
use crate::config::settings::RelayMode;
use crate::daemon_lock::{probe, LockInfo};
use crate::identity::{load_or_generate, parse_endpoint_id, IdentityError, PassphraseSource};
use crate::manage::{self, PeerSpec};

use super::client::OLD_REMOTE_BIN;
use super::legacy;

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

/// The old ssh install, `~/.local/share/irsync/bin/irsync`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OldBin {
    /// There is none.
    Absent,
    /// It is still there on purpose: a service file starts it, nothing was
    /// installed this run (`--keep-old`), `ssh-setup` itself runs from it,
    /// or a daemon whose owner is unknown holds the lock.
    Kept,
    /// It was removed, and its `bin` directory if that was then empty.
    Removed,
    /// Removing it failed, with this error; it is still there.
    RemoveFailed(String),
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
    /// Service files that still start the old binary (or cannot be read).
    pub stale_services: Vec<PathBuf>,
    pub old_bin: OldBin,
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
        for f in &self.stale_services {
            lines.push(format!("stale_service={}", f.display()));
        }
        match &self.old_bin {
            OldBin::Absent => lines.push("old_bin=absent".into()),
            OldBin::Kept => lines.push("old_bin=kept".into()),
            OldBin::Removed => lines.push("old_bin=removed".into()),
            OldBin::RemoveFailed(e) => {
                lines.push("old_bin=failed".into());
                lines.push(format!("old_bin_error={}", e.replace(['\r', '\n'], " ")));
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
        // The one key that can repeat.
        let mut stale_services = Vec::new();
        let mut ended = false;
        for line in lines {
            if line == END {
                ended = true;
                break;
            }
            let (k, v) = line
                .split_once('=')
                .ok_or_else(|| format!("not a key=value line: {line:?}"))?;
            if k == "stale_service" {
                stale_services.push(PathBuf::from(v));
            } else {
                fields.insert(k, v);
            }
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
        // An older server sends no `old_bin` line.
        let old_bin = match fields.get("old_bin").copied() {
            None | Some("absent") => OldBin::Absent,
            Some("kept") => OldBin::Kept,
            Some("removed") => OldBin::Removed,
            Some("failed") => OldBin::RemoveFailed(
                fields
                    .get("old_bin_error")
                    .map(|e| e.to_string())
                    .unwrap_or_default(),
            ),
            Some(v) => return Err(bad("old_bin", v)),
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
            stale_services,
            old_bin,
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

/// Does every server-side step for `client`: identity, grant, daemon.
/// With `recheck` (a client checking a saved host that did not answer) a
/// missing immediate grant is added only when the daemon identity was just
/// generated, a rebuilt server; otherwise the owner removed it, and it is
/// reported as [`GrantOutcome::Missing`] instead.
/// With `no_dht`, `[daemon.discovery] dht` is set to `false` when it is on;
/// without it the setting is left as it is.
/// A daemon running from the old install path is moved to this copy unless
/// a service file still starts it; the old binary is then removed, unless
/// `keep_old` (nothing was installed this run) or a service file needs it.
pub fn run(
    paths: &Paths,
    client: EndpointId,
    recheck: bool,
    no_dht: bool,
    keep_old: bool,
) -> anyhow::Result<SetupReport> {
    let Config {
        settings, hosts, ..
    } = Config::load(paths)?;

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
        let spec = PeerSpec::Id {
            id: client,
            comment: None,
        };
        let resolve = |s: &str| hosts.get(s).map(|h| h.endpoint_id);
        let added = manage::grants::grant_immediate(&mut doc, &spec, &resolve)
            .with_context(|| format!("in {}", paths.config.display()))?;
        match (added, recheck && !generated) {
            (false, _) => GrantOutcome::Already,
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

    // 4. The daemon. One running from the old install path is ours too,
    // unless a service file starts it: stopping it would fight systemd or
    // launchd.
    let home = usable_home(dirs::home_dir());
    let stale = home
        .as_deref()
        .map(legacy::stale_services)
        .unwrap_or_default();
    let is_old_bin = |p: &Path| home.as_deref().is_some_and(|h| legacy::is_old_bin(h, p));
    let state = &settings.state_dir;
    let log = state.join("daemon.log");
    let exe = std::env::current_exe().context("finding this executable")?;
    let lock = probe(state);
    // Held, but by whom is not known: it may be a daemon from the old path
    // that is still starting.
    let owner_unknown = lock.as_ref().is_some_and(|i| i.pid == 0);
    let daemon = match lock {
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
        Some(LockInfo {
            pid,
            exe: held,
            version,
        }) if pid != 0 && stale.is_empty() && is_old_bin(&held) => {
            stop_daemon(pid, state)?;
            start_daemon(&exe, paths, state, &log)?;
            DaemonOutcome::Restarted {
                old_version: version,
            }
        }
        Some(LockInfo { exe, version, .. }) => DaemonOutcome::Foreign { exe, version },
    };

    // 5. The old binary, now that no daemon of ours runs from it.
    let keep = !stale.is_empty() || keep_old || owner_unknown || is_old_bin(&exe);
    let old_bin = old_bin_step(home.as_deref(), keep);

    // 6. Where the daemon can be reached: a daemon that just started has
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
        stale_services: stale,
        old_bin,
    })
}

/// The home directory to look for the old install in: none unless it is
/// absolute, so nothing is ever looked up or removed relative to the
/// current directory.
fn usable_home(home: Option<PathBuf>) -> Option<PathBuf> {
    home.filter(|h| h.is_absolute())
}

/// What happens to the old binary under `home`: removed unless `keep`.
/// A failed removal does not fail the setup; it is reported.
fn old_bin_step(home: Option<&Path>, keep: bool) -> OldBin {
    let Some(h) = home else {
        return OldBin::Absent;
    };
    if h.join(OLD_REMOTE_BIN).symlink_metadata().is_err() {
        OldBin::Absent
    } else if keep {
        OldBin::Kept
    } else {
        match legacy::remove_old_bin(h) {
            Ok(_) => OldBin::Removed,
            Err(e) => OldBin::RemoveFailed(e.to_string()),
        }
    }
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
            stale_services: Vec::new(),
            old_bin: OldBin::Absent,
        }
    }

    #[test]
    fn report_round_trips_stale_and_old_bin() {
        for old_bin in [
            OldBin::Absent,
            OldBin::Kept,
            OldBin::Removed,
            OldBin::RemoveFailed("Permission denied (os error 13)".into()),
        ] {
            let mut r = report();
            r.old_bin = old_bin;
            r.stale_services = vec![
                PathBuf::from("/home/u/.config/systemd/user/irsync.service"),
                PathBuf::from("/home/u/Library/LaunchAgents/org.irsync.daemon.plist"),
            ];
            let text = r.to_lines();
            assert_eq!(SetupReport::parse(&text).unwrap(), r, "{text}");
            r.stale_services.clear();
            let text = r.to_lines();
            assert_eq!(SetupReport::parse(&text).unwrap(), r, "{text}");
        }
    }

    /// The error is one line in the report; a `failed` with no error line
    /// still parses.
    #[test]
    fn failed_removal_error_is_one_line() {
        let mut r = report();
        r.old_bin = OldBin::RemoveFailed("first\nsecond".into());
        let text = r.to_lines();
        assert!(text.contains("\nold_bin_error=first second\n"), "{text}");
        assert_eq!(
            SetupReport::parse(&text).unwrap().old_bin,
            OldBin::RemoveFailed("first second".into())
        );
        let text: String = text
            .lines()
            .filter(|l| !l.starts_with("old_bin_error="))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(
            SetupReport::parse(&text).unwrap().old_bin,
            OldBin::RemoveFailed(String::new())
        );
    }

    /// A home with the old binary in it.
    fn home_with_old_bin() -> (tempfile::TempDir, PathBuf) {
        let home = tempfile::tempdir().unwrap();
        let old = home.path().join(OLD_REMOTE_BIN);
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, "old").unwrap();
        (home, old)
    }

    #[test]
    fn old_bin_step_removes_or_keeps() {
        let (home, old) = home_with_old_bin();
        // `keep` covers a stale service file, `--keep-old`, this executable
        // being the old one, and a lock whose owner is unknown.
        assert_eq!(old_bin_step(Some(home.path()), true), OldBin::Kept);
        assert!(old.exists());
        assert_eq!(old_bin_step(Some(home.path()), false), OldBin::Removed);
        assert!(!old.exists());
        assert_eq!(old_bin_step(Some(home.path()), false), OldBin::Absent);
        assert_eq!(old_bin_step(None, false), OldBin::Absent);
    }

    #[test]
    fn failed_removal_is_reported_not_kept() {
        if nix::unistd::geteuid().is_root() {
            // root may remove anything.
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let (home, old) = home_with_old_bin();
        let bin = old.parent().unwrap();
        std::fs::set_permissions(bin, std::fs::Permissions::from_mode(0o555)).unwrap();
        let got = old_bin_step(Some(home.path()), false);
        // Writable again, so the temp dir can be cleaned up.
        std::fs::set_permissions(bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            matches!(&got, OldBin::RemoveFailed(e) if !e.is_empty()),
            "{got:?}"
        );
        assert!(old.exists());
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

    /// A server with an older irsync sends neither line.
    #[test]
    fn report_without_the_new_lines_parses() {
        let mut r = report();
        r.old_bin = OldBin::Removed;
        r.stale_services = vec![PathBuf::from("/x/irsync.service")];
        let text: String = r
            .to_lines()
            .lines()
            .filter(|l| !l.starts_with("stale_service=") && !l.starts_with("old_bin="))
            .map(|l| format!("{l}\n"))
            .collect();
        let parsed = SetupReport::parse(&text).unwrap();
        assert_eq!(parsed.stale_services, Vec::<PathBuf>::new());
        assert_eq!(parsed.old_bin, OldBin::Absent);
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
