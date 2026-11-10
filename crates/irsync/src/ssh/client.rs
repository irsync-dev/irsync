//! The client side of SSH onboarding: drive the server through the system
//! `ssh` (or `--rsh` / `IRSYNC_RSH`). irsync has no SSH code of its own.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use iroh::EndpointId;

use super::setup::SetupReport;
use crate::build::LONG_VERSION;

/// irsync's own copy on the server, relative to the remote home.
pub const REMOTE_BIN: &str = ".local/share/irsync/bin/irsync";

/// Printed first by the probe so a login banner before it can be ignored.
const PROBE_MARKER: &str = "irsync-probe";

#[cfg(feature = "test-hooks")]
static EXE_OVERRIDE: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Copy this binary instead of `current_exe()` (tests run as another binary).
#[cfg(feature = "test-hooks")]
pub fn set_exe_override(path: PathBuf) {
    *EXE_OVERRIDE.lock().unwrap() = Some(path);
}

fn exe_path() -> Result<PathBuf> {
    #[cfg(feature = "test-hooks")]
    if let Some(p) = EXE_OVERRIDE.lock().unwrap().clone() {
        return Ok(p);
    }
    std::env::current_exe().context("cannot find this irsync binary")
}

/// The remote shell command line, split like rsync's `-e`.
#[derive(Debug, Clone)]
pub struct Rsh {
    argv: Vec<String>,
}

impl Rsh {
    /// The flag, else `IRSYNC_RSH`, else `ssh`.
    pub fn from_opts(flag: Option<&str>) -> Rsh {
        let env = std::env::var("IRSYNC_RSH").ok();
        let text = flag
            .map(str::to_owned)
            .or(env)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "ssh".to_owned());
        Rsh {
            argv: text.split_ascii_whitespace().map(str::to_owned).collect(),
        }
    }

    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    /// `argv… dest remote_cmd`. The child inherits stdin, stdout and stderr
    /// unless the caller overrides them: `probe` and `ensure_installed` leave
    /// stderr on the terminal so password and host-key prompts and ssh's own
    /// errors show live; `probe` and `run_setup` capture stdout; `run_setup`
    /// also captures stderr; `ensure_installed` feeds the binary on stdin.
    /// `remote_cmd` is fixed text built by irsync, plus
    /// an EndpointId; it never contains user input. It is sh syntax, and
    /// the login shell may be fish or tcsh, so it is handed to `/bin/sh`
    /// whole: `exec /bin/sh -c '…'` reads the same in all of them.
    fn command(&self, dest: &str, remote_cmd: &str) -> Command {
        let mut c = Command::new(&self.argv[0]);
        c.args(&self.argv[1..])
            .arg(dest)
            .arg(format!("exec /bin/sh -c {}", sh_quote(remote_cmd)));
        c
    }
}

/// `s` in single quotes for sh. fish and tcsh read it the same as long as
/// it holds no `\` or `'` (fish treats `\\` and `\'` specially inside
/// quotes, and irsync's remote commands use neither).
fn sh_quote(s: &str) -> String {
    debug_assert!(!s.contains(['\\', '\'']), "{s}");
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub os: Option<&'static str>,
    pub arch: Option<&'static str>,
    /// First line of `~/REMOTE_BIN --version`, if any.
    pub installed: Option<String>,
    /// The ssh user is root (`id -u` is 0).
    pub root: bool,
}

/// Map `uname -sm` output to the names `std::env::consts` uses.
pub fn parse_uname(line: &str) -> (Option<&'static str>, Option<&'static str>) {
    let mut it = line.split_ascii_whitespace();
    let os = match it.next() {
        Some("Linux") => Some("linux"),
        Some("Darwin") => Some("macos"),
        _ => None,
    };
    let arch = match it.next() {
        Some("x86_64" | "amd64") => Some("x86_64"),
        Some("aarch64" | "arm64") => Some("aarch64"),
        _ => None,
    };
    (os, arch)
}

/// One SSH call: the server's platform and the installed version.
pub fn probe(rsh: &Rsh, dest: &str) -> Result<Probe> {
    // The marker lets us skip any banner a noisy login shell prints first.
    let cmd = format!(
        "echo {PROBE_MARKER}; id -u; uname -sm; \"$HOME/{REMOTE_BIN}\" --version 2>/dev/null || true"
    );
    let out = rsh
        .command(dest, &cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("cannot run {}", rsh.argv[0]))?;
    if !out.status.success() {
        bail!("cannot reach {dest} over {} ({})", rsh.argv[0], out.status);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let all: Vec<&str> = text.lines().collect();
    let Some(at) = all.iter().rposition(|l| l.trim() == PROBE_MARKER) else {
        bail!(
            "unexpected reply from {dest} (no probe marker): {}",
            text.trim()
        );
    };
    let mut lines = all[at + 1..].iter().copied();
    let root = lines.next().map(str::trim) == Some("0");
    let (os, arch) = parse_uname(lines.next().unwrap_or(""));
    let installed = lines
        .next()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned);
    Ok(Probe {
        os,
        arch,
        installed,
        root,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    Current,
    Copied { replaced: Option<String> },
}

/// Make sure the server has this exact build, copying it over if the
/// platforms match.
pub fn ensure_installed(rsh: &Rsh, dest: &str, probe: &Probe) -> Result<Install> {
    // `--version` prints the program name, then LONG_VERSION.
    if probe.installed.as_deref() == Some(format!("irsync {}", *LONG_VERSION).as_str()) {
        return Ok(Install::Current);
    }
    let (my_os, my_arch) = (std::env::consts::OS, std::env::consts::ARCH);
    if probe.os != Some(my_os) || probe.arch != Some(my_arch) {
        bail!(
            "{dest} runs {} {}, this irsync is {my_os} {my_arch}: install irsync on {dest} \
             by hand (downloading builds for other platforms is not supported yet)",
            probe.os.unwrap_or("an unknown OS"),
            probe.arch.unwrap_or("an unknown CPU"),
        );
    }
    let exe = exe_path()?;
    let file =
        std::fs::File::open(&exe).with_context(|| format!("cannot read {}", exe.display()))?;
    let len = file.metadata()?.len();
    let dir = REMOTE_BIN.rsplit_once('/').map_or("", |(d, _)| d);
    let tmp = format!("\"$HOME/{REMOTE_BIN}.tmp.$$\"");
    // Exit 3 marks a short upload; the temp file is removed first.
    let cmd = format!(
        "mkdir -p \"$HOME/{dir}\" && cat > {tmp} \
         && {{ [ \"$(wc -c < {tmp})\" -eq {len} ] || {{ rm -f {tmp}; exit 3; }}; }} \
         && chmod 755 {tmp} && mv -f {tmp} \"$HOME/{REMOTE_BIN}\""
    );
    let status = rsh
        .command(dest, &cmd)
        .stdin(Stdio::from(file))
        .status()
        .with_context(|| format!("cannot run {}", rsh.argv[0]))?;
    if status.code() == Some(3) {
        bail!("the copy of irsync on {dest} was cut short; nothing was changed, try again");
    }
    if !status.success() {
        bail!("copying irsync to {dest} failed ({status})");
    }
    Ok(Install::Copied {
        replaced: probe.installed.clone(),
    })
}

/// Run `ssh-setup` on the server and parse what it reports; `recheck`
/// passes `--recheck` (a saved host: never add back a removed grant),
/// `no_dht` passes `--no-dht` (turn the daemon's DHT publishing off).
pub fn run_setup(
    rsh: &Rsh,
    dest: &str,
    client: EndpointId,
    recheck: bool,
    no_dht: bool,
) -> Result<SetupReport> {
    let mut flags = String::new();
    if recheck {
        flags.push_str(" --recheck");
    }
    if no_dht {
        flags.push_str(" --no-dht");
    }
    let cmd = format!("\"$HOME/{REMOTE_BIN}\" ssh-setup --client {client}{flags}");
    let out = rsh
        .command(dest, &cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("cannot run {}", rsh.argv[0]))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("setup on {dest} failed ({}): {}", out.status, err.trim());
    }
    SetupReport::parse(&String::from_utf8_lossy(&out.stdout))
        .map_err(|e| anyhow!("unreadable setup report from {dest}: {e}"))
}
