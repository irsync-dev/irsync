//! The isolated `irsync daemon`: its config, the client key, spawning it,
//! parsing its startup lines, logging its stderr and stopping it.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

use crate::process;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;

/// How many trailing lines of the daemon's stderr an error message carries.
const TAIL_LINES: usize = 20;
/// How much of the end of the log file `tail` reads to find them.
const TAIL_BYTES: u64 = 64 * 1024;
/// How often `start` and `stop` look at the child while waiting.
const POLL: Duration = Duration::from_millis(100);
/// How long, after the child exits, its last stdout lines and stderr may
/// take to arrive before we stop waiting for them.
const DRAIN: Duration = Duration::from_secs(1);

pub struct DaemonConfig<'a> {
    pub config_dir: &'a Path,
    pub export_path: &'a Path,
    /// The client's EndpointId, granted read-write on the export.
    pub client_id: &'a str,
}

/// The files [`write_config`] wrote.
#[derive(Debug, Clone, PartialEq)]
pub struct DaemonFiles {
    pub config: PathBuf,
    pub exports: PathBuf,
}

/// A path as a quoted TOML string.
fn toml_path(p: &Path) -> Result<String> {
    let s = p
        .to_str()
        .ok_or_else(|| anyhow!("path is not UTF-8: {}", p.display()))?;
    Ok(toml::Value::String(s.to_string()).to_string())
}

/// Writes `<config_dir>/irsync.toml` and `<config_dir>/exports.toml`
/// (export `data`, read-write for the client), creates `<config_dir>/state`,
/// and returns both paths.
pub fn write_config(c: &DaemonConfig) -> Result<DaemonFiles> {
    let dir = c.config_dir;
    let state = dir.join("state");
    std::fs::create_dir_all(&state).with_context(|| format!("creating {}", state.display()))?;
    let settings = format!(
        "state_dir = {}\n\
         [daemon]\n\
         identity = {}\n\
         relay_mode = \"disabled\"\n\
         portmap = false\n\
         [daemon.discovery]\n\
         dht = false\n\
         dns = false\n\
         mdns = false\n",
        toml_path(&state)?,
        toml_path(&dir.join("daemon_ed25519"))?,
    );
    let config = dir.join("irsync.toml");
    std::fs::write(&config, settings).with_context(|| format!("writing {}", config.display()))?;
    let exports = dir.join("exports.toml");
    std::fs::write(
        &exports,
        format!(
            "[data]\npath = {}\nrw = [{}]\n",
            toml_path(c.export_path)?,
            toml::Value::String(c.client_id.to_string())
        ),
    )
    .with_context(|| format!("writing {}", exports.display()))?;
    Ok(DaemonFiles { config, exports })
}

/// Runs `irsync keygen --out <config_dir>/id_ed25519`; returns the private
/// key path, the public key line (stdout line 1) and the EndpointId (line 2).
pub fn client_keygen(irsync: &Path, config_dir: &Path) -> Result<(PathBuf, String, String)> {
    let key = config_dir.join("id_ed25519");
    let out = process::output(
        Command::new(irsync)
            .args(["keygen", "--out"])
            .arg(&key)
            .env_remove("IRSYNC_PASSPHRASE")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .with_context(|| format!("cannot run {} keygen", irsync.display()))?;
    if !out.status.success() {
        bail!(
            "irsync keygen failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let stdout = String::from_utf8(out.stdout).context("irsync keygen: stdout is not UTF-8")?;
    let mut lines = stdout.lines().map(str::trim);
    let pub_line = lines
        .next()
        .filter(|l| !l.is_empty())
        .ok_or_else(|| anyhow!("irsync keygen printed no public key line"))?
        .to_string();
    let id = lines
        .next()
        .filter(|l| !l.is_empty())
        .ok_or_else(|| anyhow!("irsync keygen printed no endpoint id line"))?
        .to_string();
    Ok((key, pub_line, id))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Startup {
    pub endpoint_id: String,
    pub address: SocketAddr,
}

/// The startup line grammar: `endpoint id: <id>`, then `listening on: <addr>`.
#[derive(Default)]
struct StartupParser {
    endpoint_id: Option<String>,
}

impl StartupParser {
    fn feed(&mut self, line: &str) -> Result<Option<Startup>> {
        if let Some(id) = line.strip_prefix("endpoint id: ") {
            if self.endpoint_id.is_some() {
                bail!("daemon printed a second endpoint id line: {line:?}");
            }
            self.endpoint_id = Some(id.trim().to_string());
            return Ok(None);
        }
        if let Some(addr) = line.strip_prefix("listening on: ") {
            let Some(endpoint_id) = self.endpoint_id.take() else {
                bail!("daemon printed {line:?} before its endpoint id");
            };
            let address = addr
                .trim()
                .parse()
                .with_context(|| format!("daemon printed a bad address: {line:?}"))?;
            return Ok(Some(Startup {
                endpoint_id,
                address,
            }));
        }
        bail!("unexpected daemon stdout line: {line:?}")
    }
}

/// Reads lines until a `listening on:` line follows an `endpoint id:` line;
/// an unexpected line, EOF, or `timeout` is an error.
pub fn parse_startup(lines: &mpsc::Receiver<String>, timeout: Duration) -> Result<Startup> {
    let deadline = Instant::now() + timeout;
    let mut parser = StartupParser::default();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match lines.recv_timeout(remaining) {
            Ok(line) => {
                if let Some(s) = parser.feed(&line)? {
                    return Ok(s);
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                bail!("daemon did not print its startup lines within {timeout:?}")
            }
            Err(RecvTimeoutError::Disconnected) => {
                bail!("daemon stdout ended before its startup lines")
            }
        }
    }
}

/// The daemon log file with a shared writer so markers and the copied
/// stderr interleave in one file.
#[derive(Debug, Clone)]
pub struct DaemonLog {
    file: Arc<Mutex<File>>,
    path: PathBuf,
}

impl DaemonLog {
    pub fn create(path: &Path) -> Result<DaemonLog> {
        let f = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!("creating {}", path.display()))?;
        Ok(DaemonLog {
            file: Arc::new(Mutex::new(f)),
            path: path.to_path_buf(),
        })
    }

    /// Where the log is, for error messages.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes `=== <text> ===\n` and flushes.
    pub fn marker(&self, text: &str) -> Result<()> {
        self.write_all(format!("=== {text} ===\n").as_bytes())
    }

    fn write_all(&self, bytes: &[u8]) -> Result<()> {
        let mut f = self.file.lock().unwrap_or_else(|e| e.into_inner());
        f.write_all(bytes)?;
        f.flush()?;
        Ok(())
    }

    /// The last `n` lines written so far, looking only at the file's last
    /// `TAIL_BYTES`.
    fn tail(&self, n: usize) -> String {
        let mut f = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let mut buf = Vec::new();
        let mut start = 0;
        let read = f
            .seek(SeekFrom::End(0))
            .and_then(|len| {
                start = len.saturating_sub(TAIL_BYTES);
                f.seek(SeekFrom::Start(start))
            })
            .and_then(|_| f.read_to_end(&mut buf));
        // A full read leaves the cursor at the end, where the next write belongs.
        if read.is_err() {
            let _ = f.seek(SeekFrom::End(0));
            return String::new();
        }
        let text = String::from_utf8_lossy(&buf);
        let mut lines: Vec<&str> = text.lines().collect();
        if start > 0 && !lines.is_empty() {
            lines.remove(0); // cut mid-line by the byte window
        }
        lines[lines.len().saturating_sub(n)..].join("\n")
    }
}

/// Joins `h` if it finishes within `bound`; otherwise leaves it detached, so
/// a grandchild holding the daemon's stderr open cannot hang `stop`.
fn join_bounded(h: JoinHandle<()>, bound: Duration) {
    let deadline = Instant::now() + bound;
    while !h.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if h.is_finished() {
        let _ = h.join();
    }
}

/// Copies `r` line by line into `log` until EOF or a read error, each line
/// written whole so a marker never splits one.
fn copy_into(log: &DaemonLog, r: impl Read) {
    let mut r = BufReader::new(r);
    let mut line = Vec::new();
    loop {
        line.clear();
        match r.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                if !line.ends_with(b"\n") {
                    line.push(b'\n');
                }
                if log.write_all(&line).is_err() {
                    return;
                }
            }
        }
    }
}

#[derive(Debug)]
pub struct Daemon {
    child: Child,
    pub startup: Startup,
    log: DaemonLog,
    copier: Option<JoinHandle<()>>,
}

/// Why the wait for the startup lines ended without them.
enum StartFailure {
    Exited(ExitStatus),
    Other(anyhow::Error),
}

impl Daemon {
    /// Spawns `<irsync> daemon --config <config> --exports <exports>
    /// --listen 127.0.0.1:0` and
    /// waits up to `timeout` for its startup lines. A child that exits first
    /// is reported at once with its status and the tail of its stderr.
    pub fn start(
        irsync: &Path,
        files: &DaemonFiles,
        log: DaemonLog,
        log_level: &str,
        timeout: Duration,
    ) -> Result<Daemon> {
        let mut child = process::spawn(
            Command::new(irsync)
                .arg("daemon")
                .arg("--config")
                .arg(&files.config)
                .arg("--exports")
                .arg(&files.exports)
                .args(["--listen", "127.0.0.1:0"])
                .env("RUST_LOG", log_level)
                .env_remove("IRSYNC_PASSPHRASE")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .with_context(|| format!("cannot run {} daemon", irsync.display()))?;

        let stderr = child.stderr.take().expect("stderr is piped");
        let copier_log = log.clone();
        let copier = thread::spawn(move || copy_into(&copier_log, stderr));

        let stdout = child.stdout.take().expect("stdout is piped");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            // Keep draining after startup so the daemon never blocks on a
            // full pipe; sends fail harmlessly once the receiver is gone.
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                let _ = tx.send(line);
            }
        });

        match wait_for_startup(&mut child, &rx, timeout) {
            Ok(startup) => Ok(Daemon {
                child,
                startup,
                log,
                copier: Some(copier),
            }),
            Err(StartFailure::Exited(status)) => {
                join_bounded(copier, DRAIN);
                Err(anyhow!(
                    "daemon exited before startup ({status}); see {}; stderr:\n{}",
                    log.path().display(),
                    log.tail(TAIL_LINES)
                ))
            }
            Err(StartFailure::Other(e)) => {
                let _ = child.kill();
                let _ = child.wait();
                join_bounded(copier, DRAIN);
                Err(e.context(format!(
                    "daemon failed to start; see {}; stderr:\n{}",
                    log.path().display(),
                    log.tail(TAIL_LINES)
                )))
            }
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Writes a marker line into the daemon log.
    pub fn marker(&self, text: &str) -> Result<()> {
        self.log.marker(text)
    }

    /// An error if the child has already exited.
    pub fn check_alive(&mut self) -> Result<()> {
        match self.child.try_wait().context("checking on the daemon")? {
            None => Ok(()),
            Some(status) => bail!(
                "daemon exited ({status}); see {}; stderr:\n{}",
                self.log.path().display(),
                self.log.tail(TAIL_LINES)
            ),
        }
    }

    /// The daemon log's path, for error messages.
    pub fn log_path(&self) -> &Path {
        self.log.path()
    }

    /// SIGINT, wait up to `grace`, then SIGKILL; joins the copier (bounded). Idempotent.
    pub fn stop(&mut self, grace: Duration) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = kill(Pid::from_raw(self.child.id() as i32), Signal::SIGINT);
            let deadline = Instant::now() + grace;
            while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
                thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
            }
            if matches!(self.child.try_wait(), Ok(None)) {
                let _ = self.child.kill();
            }
            let _ = self.child.wait();
        }
        if let Some(copier) = self.copier.take() {
            join_bounded(copier, DRAIN);
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop(Duration::from_secs(10));
    }
}

/// `parse_startup` that also polls the child, so an early exit ends the
/// wait at once rather than at the timeout.
fn wait_for_startup(
    child: &mut Child,
    lines: &mpsc::Receiver<String>,
    timeout: Duration,
) -> std::result::Result<Startup, StartFailure> {
    let deadline = Instant::now() + timeout;
    let mut parser = StartupParser::default();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return drain_after_exit(&mut parser, lines, status),
            Ok(None) => {}
            Err(e) => return Err(StartFailure::Other(e.into())),
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(StartFailure::Other(anyhow!(
                "daemon did not print its startup lines within {timeout:?}"
            )));
        }
        match lines.recv_timeout(POLL.min(remaining)) {
            Ok(line) => match parser.feed(&line) {
                Ok(Some(s)) => return Ok(s),
                Ok(None) => {}
                Err(e) => return Err(StartFailure::Other(e)),
            },
            Err(RecvTimeoutError::Timeout) => {}
            // Stdout closed: the child is most likely exiting; let the next
            // `try_wait` see it, without spinning on the closed channel.
            Err(RecvTimeoutError::Disconnected) => thread::sleep(POLL.min(remaining)),
        }
    }
}

/// The child has exited, but the lines it printed first may still be in
/// the channel. A daemon that started and then died has started: return
/// its startup so `check_alive` reports the death. Bounded by `DRAIN` in
/// case a grandchild holds stdout open.
fn drain_after_exit(
    parser: &mut StartupParser,
    lines: &mpsc::Receiver<String>,
    status: ExitStatus,
) -> std::result::Result<Startup, StartFailure> {
    let deadline = Instant::now() + DRAIN;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match lines.recv_timeout(remaining) {
            Ok(line) => match parser.feed(&line) {
                Ok(Some(s)) => return Ok(s),
                Ok(None) => {}
                Err(e) => return Err(StartFailure::Other(e)),
            },
            Err(_) => return Err(StartFailure::Exited(status)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    fn channel_of(lines: &[&str]) -> mpsc::Receiver<String> {
        let (tx, rx) = mpsc::channel();
        for l in lines {
            tx.send((*l).to_string()).unwrap();
        }
        rx
    }

    fn dev_null() -> DaemonFiles {
        DaemonFiles {
            config: PathBuf::from("/dev/null"),
            exports: PathBuf::from("/dev/null"),
        }
    }

    fn chmod_755(p: &Path) {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn write_config_writes_settings_and_exports() {
        let t = tempfile::tempdir().unwrap();
        let cfg = t.path().join("cfg");
        std::fs::create_dir(&cfg).unwrap();
        let export = t.path().join("mod \"q\"");
        let files = write_config(&DaemonConfig {
            config_dir: &cfg,
            export_path: &export,
            client_id: "abc",
        })
        .unwrap();
        assert_eq!(
            files,
            DaemonFiles {
                config: cfg.join("irsync.toml"),
                exports: cfg.join("exports.toml"),
            }
        );
        let read =
            |p: &Path| -> toml::Table { std::fs::read_to_string(p).unwrap().parse().unwrap() };
        let s = |p: PathBuf| toml::Value::String(p.to_str().unwrap().to_string());
        let parsed = read(&files.config);
        assert_eq!(parsed["state_dir"], s(cfg.join("state")));
        let d = &parsed["daemon"];
        assert_eq!(d["identity"], s(cfg.join("daemon_ed25519")));
        assert_eq!(d["relay_mode"].as_str(), Some("disabled"));
        assert_eq!(d["portmap"].as_bool(), Some(false));
        for k in ["dht", "dns", "mdns"] {
            assert_eq!(d["discovery"][k].as_bool(), Some(false));
        }
        let exports = read(&files.exports);
        assert_eq!(exports["data"]["path"], s(export));
        assert_eq!(
            exports["data"]["rw"],
            toml::Value::Array(vec![toml::Value::String("abc".into())])
        );
        assert!(cfg.join("state").is_dir());
    }

    #[test]
    fn parse_startup_reads_id_then_address() {
        let rx = channel_of(&["endpoint id: abc123", "listening on: 127.0.0.1:43210"]);
        let s = parse_startup(&rx, Duration::from_secs(1)).unwrap();
        assert_eq!(s.endpoint_id, "abc123");
        assert_eq!(s.address, "127.0.0.1:43210".parse::<SocketAddr>().unwrap());
    }

    #[test]
    fn parse_startup_rejects_address_before_id_unknown_lines_and_eof() {
        assert!(parse_startup(
            &channel_of(&["listening on: 127.0.0.1:1"]),
            Duration::from_secs(1)
        )
        .is_err());
        assert!(parse_startup(
            &channel_of(&["endpoint id: a", "hello"]),
            Duration::from_secs(1)
        )
        .is_err());
        assert!(parse_startup(&channel_of(&["endpoint id: a"]), Duration::from_secs(1)).is_err());
        // EOF
    }

    #[test]
    fn parse_startup_times_out() {
        let (_tx, rx) = mpsc::channel::<String>(); // keep the sender alive so it is a timeout, not EOF
        let t = Instant::now();
        assert!(parse_startup(&rx, Duration::from_millis(200)).is_err());
        assert!(t.elapsed() >= Duration::from_millis(200));
    }

    #[test]
    fn markers_and_copied_stderr_share_one_file() {
        let t = tempfile::tempdir().unwrap();
        let log = DaemonLog::create(&t.path().join("d.log")).unwrap();
        log.marker("ping").unwrap();
        // copy a fake stderr through the same writer the copier uses
        copy_into(&log, std::io::Cursor::new(b"line one\n".to_vec()));
        log.marker("iteration 1").unwrap();
        assert_eq!(
            std::fs::read_to_string(t.path().join("d.log")).unwrap(),
            "=== ping ===\nline one\n=== iteration 1 ===\n"
        );
    }

    #[test]
    fn daemon_that_exits_early_is_reported_without_waiting() {
        // A fake "irsync" that ignores its arguments, prints to stderr and exits 3.
        let t = tempfile::tempdir().unwrap();
        let fake = t.path().join("irsync");
        std::fs::write(&fake, "#!/bin/sh\necho 'config error' >&2\nexit 3\n").unwrap();
        chmod_755(&fake);
        let log = DaemonLog::create(&t.path().join("d.log")).unwrap();
        let started = Instant::now();
        let err = Daemon::start(&fake, &dev_null(), log, "debug", Duration::from_secs(15))
            .unwrap_err()
            .to_string();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(
            err.contains("exit") && err.contains("config error"),
            "{err}"
        );
        assert!(
            err.contains(&t.path().join("d.log").display().to_string()),
            "{err}"
        );
    }

    #[test]
    fn stop_kills_a_child_that_ignores_sigint() {
        let t = tempfile::tempdir().unwrap();
        let fake = t.path().join("irsync");
        std::fs::write(
            &fake,
            "#!/bin/sh\ntrap '' INT\necho 'endpoint id: x'\necho 'listening on: 127.0.0.1:1'\nwhile :; do sleep 1; done\n",
        )
        .unwrap();
        chmod_755(&fake);
        let log = DaemonLog::create(&t.path().join("d.log")).unwrap();
        let mut d =
            Daemon::start(&fake, &dev_null(), log, "debug", Duration::from_secs(15)).unwrap();
        let pid = d.pid();
        d.stop(Duration::from_millis(500));
        assert!(nix::sys::signal::kill(Pid::from_raw(pid as i32), None).is_err());
        // gone
    }

    #[test]
    fn startup_lines_already_sent_win_over_an_exit() {
        // The child has exited and both lines wait in the channel: the
        // daemon did start, so this is a startup, not "exited before startup".
        let mut child = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
        child.wait().unwrap();
        let rx = channel_of(&["endpoint id: x", "listening on: 127.0.0.1:1"]);
        let s = wait_for_startup(&mut child, &rx, Duration::from_secs(5))
            .unwrap_or_else(|_| panic!("startup reported as a failure"));
        assert_eq!(s.endpoint_id, "x");
    }

    #[test]
    fn daemon_that_starts_then_exits_is_started_then_dead() {
        let t = tempfile::tempdir().unwrap();
        let fake = t.path().join("irsync");
        std::fs::write(
            &fake,
            "#!/bin/sh\necho 'endpoint id: x'\necho 'listening on: 127.0.0.1:1'\nexit 0\n",
        )
        .unwrap();
        chmod_755(&fake);
        let log = DaemonLog::create(&t.path().join("d.log")).unwrap();
        let mut d =
            Daemon::start(&fake, &dev_null(), log, "debug", Duration::from_secs(15)).unwrap();
        assert_eq!(d.startup.endpoint_id, "x");
        let deadline = Instant::now() + Duration::from_secs(5);
        let err = loop {
            match d.check_alive() {
                Ok(()) => {}
                Err(e) => break e.to_string(),
            }
            assert!(Instant::now() < deadline, "daemon never reported dead");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(
            err.contains(&t.path().join("d.log").display().to_string()),
            "{err}"
        );
        assert_eq!(d.log_path(), t.path().join("d.log"));
    }
}
