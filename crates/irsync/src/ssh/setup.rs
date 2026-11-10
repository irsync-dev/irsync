//! `irsync ssh-setup`: every server-side step of SSH onboarding in one
//! command, run as the SSH user. It makes sure the daemon identity exists,
//! grants the client immediate access and gets this copy of irsync's daemon
//! running, then reports what it did as `key=value` lines.
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
use iroh::EndpointId;

use crate::build::LONG_VERSION;
use crate::config::file::Config;
use crate::config::paths::Paths;
use crate::daemon_lock::{probe, LockInfo};
use crate::identity::{load_or_generate, parse_endpoint_id, IdentityError, PassphraseSource};
use crate::manage::{self, PeerSpec};

/// The first line of the report; text before it (a login shell's) is ignored.
pub const BEGIN: &str = "irsync-setup-begin";
/// The last line of the report.
pub const END: &str = "irsync-setup-end";

/// How long to wait for a daemon to stop or to start.
const DAEMON_WAIT: Duration = Duration::from_secs(10);

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonOutcome {
    /// This copy's daemon, at this version, was already running.
    Running,
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
    pub daemon: DaemonOutcome,
    /// Where a daemon started by `ssh-setup` writes its output.
    pub log: PathBuf,
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
        ];
        match &self.daemon {
            DaemonOutcome::Running => lines.push("daemon=running".into()),
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
        let daemon = match get("daemon")?.as_str() {
            "running" => DaemonOutcome::Running,
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
            daemon,
            log: PathBuf::from(get("log")?),
        })
    }
}

/// Does every server-side step for `client`: identity, grant, daemon.
/// With `recheck` (a client checking a saved host that did not answer) a
/// missing immediate grant is added only when the daemon identity was just
/// generated, a rebuilt server; otherwise the owner removed it, and it is
/// reported as [`GrantOutcome::Missing`] instead.
pub fn run(paths: &Paths, client: EndpointId, recheck: bool) -> anyhow::Result<SetupReport> {
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
        manage::ensure_editable(paths, manage::Table::Exports)?;
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
                lock.save(&doc)?;
                GrantOutcome::Added
            }
        }
    } else {
        GrantOutcome::ImmediateOff
    };

    // 3. The daemon.
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
            if version == *LONG_VERSION {
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

    Ok(SetupReport {
        version: LONG_VERSION.clone(),
        endpoint_id: secret.public(),
        identity,
        grant,
        daemon,
        log,
    })
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

/// Starts `exe daemon` in its own session, output appended to `log`, and
/// waits until it holds the lock.
fn start_daemon(exe: &Path, paths: &Paths, state: &Path, log: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(state).with_context(|| format!("creating {}", state.display()))?;
    let out = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .with_context(|| format!("opening {}", log.display()))?;
    let start = out.metadata().map(|m| m.len()).unwrap_or(0);
    let mut cmd = Command::new(exe);
    cmd.arg("daemon");
    if *paths != Paths::resolve(None) {
        cmd.arg("--config").arg(&paths.config);
    }
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
