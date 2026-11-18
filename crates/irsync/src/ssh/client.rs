//! The client side of SSH onboarding: drive the server through the system
//! `ssh` (or `--rsh` / `IRSYNC_RSH`). irsync has no SSH code of its own.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use iroh::EndpointId;

use super::release;
use super::setup::SetupReport;
use crate::build::{BuildInfo, LONG_VERSION};
use crate::config::hosts::short_host_name;

/// irsync's own copy on the server, relative to the remote home.
pub const REMOTE_BIN: &str = ".local/share/irsync/bin/irsync";

/// Where the book explains the setup without ssh.
const BY_HAND_URL: &str =
    "https://irsync.org/book/tutorial/first-sync.html#set-the-server-up-by-hand";

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
    /// an EndpointId and a hex-encoded name; it never contains user input. It is sh syntax, and
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
    /// The server's short host name (`uname -n`), when it can be an alias.
    pub hostname: Option<String>,
}

/// Map `uname -sm` output to the names `std::env::consts` uses.
pub fn parse_uname(line: &str) -> (Option<&'static str>, Option<&'static str>) {
    let mut it = line.split_ascii_whitespace();
    let os = match it.next() {
        Some("Linux") => Some("linux"),
        Some("Darwin") => Some("macos"),
        Some("FreeBSD") => Some("freebsd"),
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
        "echo {PROBE_MARKER}; id -u; uname -sm; uname -n 2>/dev/null || echo; \
         \"$HOME/{REMOTE_BIN}\" --version 2>/dev/null || true"
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
    let hostname = lines.next().and_then(short_host_name);
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
        hostname,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    Current,
    Copied {
        replaced: Option<String>,
    },
    /// The release binary for `platform` ("linux aarch64"), downloaded
    /// here and copied over.
    Downloaded {
        replaced: Option<String>,
        platform: String,
    },
}

/// Make sure the server has this exact build: copy this binary over if the
/// platforms match, else the release binary for the server's platform.
pub fn ensure_installed(rsh: &Rsh, dest: &str, probe: &Probe) -> Result<Install> {
    // `--version` prints the program name, then LONG_VERSION.
    if probe.installed.as_deref() == Some(format!("irsync {}", *LONG_VERSION).as_str()) {
        return Ok(Install::Current);
    }
    let (my_os, my_arch) = (std::env::consts::OS, std::env::consts::ARCH);
    if probe.os != Some(my_os) || probe.arch != Some(my_arch) {
        return install_release(rsh, dest, probe);
    }
    let exe = exe_path()?;
    match upload(rsh, dest, &exe)? {
        Upload::Done => {}
        Upload::Short => bail!("{}", cut_short(dest)),
        Upload::NotThisVersion | Upload::DoesNotRun => bail!(
            "the copied irsync does not run on {dest}. Nothing was changed there. \
             Install irsync on the server and set it up by hand: {BY_HAND_URL}"
        ),
    }
    Ok(Install::Copied {
        replaced: probe.installed.clone(),
    })
}

/// The server's OS or CPU is not ours, so our own binary is no use there:
/// download the release binary for this build's tag and copy that over.
/// Only a release build can; see [`release::release_tag`].
fn install_release(rsh: &Rsh, dest: &str, probe: &Probe) -> Result<Install> {
    let (my_os, my_arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let os = probe.os.unwrap_or("an unknown OS");
    let arch = probe.arch.unwrap_or("an unknown CPU");
    let by_hand =
        format!("Install irsync on both machines and set the server up by hand: {BY_HAND_URL}");
    let Some(target) = release::target_for(probe.os, probe.arch) else {
        bail!(
            "{dest} runs {os} {arch}, this irsync is {my_os} {my_arch}, and there is no \
             release binary for that platform. {by_hand}"
        );
    };
    let Some(tag) = release_tag() else {
        bail!(
            "{dest} runs {os} {arch}, this irsync is {my_os} {my_arch} and a development \
             build ({}). Only release builds install on another OS or CPU. {by_hand}",
            *LONG_VERSION
        );
    };
    eprintln!("irsync: downloading irsync {tag} for {os} {arch}");
    let fetched = release::fetch(&tag, target)?;
    match upload(rsh, dest, fetched.bin())? {
        Upload::Done => {}
        Upload::Short => bail!("{}", cut_short(dest)),
        Upload::NotThisVersion => bail!(
            "release {tag} is not this build ({}): this irsync is a development build with \
             a release's version number, so it cannot install on {dest}. Nothing was \
             changed there.",
            *LONG_VERSION
        ),
        Upload::DoesNotRun => bail!(
            "the release binary for {os} {arch} does not run on {dest} (is the home \
             directory there mounted noexec?). Nothing was changed there. {by_hand}"
        ),
    }
    Ok(Install::Downloaded {
        replaced: probe.installed.clone(),
        platform: format!("{os} {arch}"),
    })
}

/// The release tag of this build, or `None` for a development build. A
/// test that points the download at its own files always gets a tag: test
/// builds come from a tree with uncommitted changes.
fn release_tag() -> Option<String> {
    let info = BuildInfo::current();
    #[cfg(feature = "test-hooks")]
    if release::test_url().is_some() {
        return Some(format!("v{}", info.version));
    }
    release::release_tag(&info)
}

fn cut_short(dest: &str) -> String {
    format!("the copy of irsync on {dest} was cut short; nothing was changed, try again")
}

/// What the upload command reported.
#[derive(Debug, PartialEq, Eq)]
enum Upload {
    Done,
    /// Fewer bytes arrived than were sent.
    Short,
    /// The file arrived whole and runs on the server, but reports another
    /// version than this build's.
    NotThisVersion,
    /// The file arrived whole, but on the server it prints no version: it
    /// does not run there (wrong libc, a `noexec` home).
    DoesNotRun,
}

/// Send `local` to the server as the new irsync. It replaces the old one
/// only if the size matches and its `--version` is ours; otherwise the
/// temp file is removed and nothing changes.
fn upload(rsh: &Rsh, dest: &str, local: &Path) -> Result<Upload> {
    let file =
        std::fs::File::open(local).with_context(|| format!("cannot read {}", local.display()))?;
    let len = file.metadata()?.len();
    let dir = REMOTE_BIN.rsplit_once('/').map_or("", |(d, _)| d);
    let tmp = format!("\"$HOME/{REMOTE_BIN}.tmp.$$\"");
    // `--version` prints the program name, then LONG_VERSION. The text sits
    // in double quotes in the remote sh command.
    let want = format!("irsync {}", *LONG_VERSION);
    debug_assert!(!want.contains(['"', '$', '`']), "{want}");
    // Exit 3 marks a short upload, exit 4 a binary that reports another
    // version, exit 5 one that prints no version at all (it does not run);
    // the temp file is removed first.
    let cmd = format!(
        "mkdir -p \"$HOME/{dir}\" && cat > {tmp} \
         && {{ [ \"$(wc -c < {tmp})\" -eq {len} ] || {{ rm -f {tmp}; exit 3; }}; }} \
         && chmod 755 {tmp} \
         && {{ v=$({tmp} --version 2>/dev/null | head -n 1); \
         [ -n \"$v\" ] || {{ rm -f {tmp}; exit 5; }}; \
         [ \"$v\" = \"{want}\" ] || {{ rm -f {tmp}; exit 4; }}; }} \
         && mv -f {tmp} \"$HOME/{REMOTE_BIN}\""
    );
    let status = rsh
        .command(dest, &cmd)
        .stdin(Stdio::from(file))
        .status()
        .with_context(|| format!("cannot run {}", rsh.argv[0]))?;
    match status.code() {
        Some(0) => Ok(Upload::Done),
        Some(3) => Ok(Upload::Short),
        Some(4) => Ok(Upload::NotThisVersion),
        Some(5) => Ok(Upload::DoesNotRun),
        _ => bail!("copying irsync to {dest} failed ({status})"),
    }
}

/// This machine's short host name, the name the server will know this
/// client by. A test-hooks build takes it from `IRSYNC_TEST_HOSTNAME`.
pub fn client_host_name() -> Result<String> {
    let raw = raw_host_name()?;
    short_host_name(&raw).ok_or_else(|| {
        anyhow!(
            "this machine's host name ({raw:?}) cannot be used as its name on the \
             server; give this machine a host name of its own"
        )
    })
}

fn raw_host_name() -> Result<String> {
    #[cfg(feature = "test-hooks")]
    if let Ok(name) = std::env::var("IRSYNC_TEST_HOSTNAME") {
        return Ok(name);
    }
    let name = nix::unistd::gethostname().context("cannot read this machine's host name")?;
    Ok(name.to_string_lossy().into_owned())
}

/// `name` as hex digits, for a command line that several shells read.
fn encode_name(name: &str) -> String {
    name.bytes().map(|b| format!("{b:02x}")).collect()
}

/// The name [`encode_name`] wrote.
pub fn decode_name(hex: &str) -> Result<String> {
    if hex.is_empty() || !hex.len().is_multiple_of(2) || !hex.is_ascii() {
        bail!("not a hex-encoded name: {hex:?}");
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
        .collect::<std::result::Result<Vec<u8>, _>>()
        .map_err(|_| anyhow!("not a hex-encoded name: {hex:?}"))?;
    String::from_utf8(bytes).map_err(|_| anyhow!("the name in {hex:?} is not UTF-8"))
}

/// The `ssh-setup` command line for the server. `client_name` is whatever
/// this machine is called, so it goes as hex: the login shell there may be
/// sh, fish or tcsh, and none of them may read any of it as syntax.
pub fn setup_command(client: EndpointId, client_name: &str, recheck: bool, no_dht: bool) -> String {
    let mut cmd = format!(
        "\"$HOME/{REMOTE_BIN}\" ssh-setup --client {client} --client-name-hex {}",
        encode_name(client_name)
    );
    if recheck {
        cmd.push_str(" --recheck");
    }
    if no_dht {
        cmd.push_str(" --no-dht");
    }
    cmd
}

/// Run `ssh-setup` on the server and parse what it reports; `recheck`
/// passes `--recheck` (a saved host: never add back a removed grant),
/// `no_dht` passes `--no-dht` (turn the daemon's DHT publishing off).
pub fn run_setup(
    rsh: &Rsh,
    dest: &str,
    client: EndpointId,
    client_name: &str,
    recheck: bool,
    no_dht: bool,
) -> Result<SetupReport> {
    let cmd = setup_command(client, client_name, recheck, no_dht);
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
