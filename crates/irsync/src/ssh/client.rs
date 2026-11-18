//! The client side of SSH onboarding: drive the server through the system
//! `ssh` (or `--rsh` / `IRSYNC_RSH`). irsync has no SSH code of its own.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use iroh::EndpointId;

use super::onboard::SetupFlags;
use super::payload::{Contents, Packed, Staged};
use super::release;
use super::setup::SetupReport;
use crate::build::{BuildInfo, LONG_VERSION};
use crate::config::hosts::short_host_name;

/// irsync on the server, relative to the remote home: where the release
/// installer puts it.
pub const REMOTE_BIN: &str = ".local/bin/irsync";

/// Where irsync put its copy on the server before it used the installer.
pub const OLD_REMOTE_BIN: &str = ".local/share/irsync/bin/irsync";

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
    /// also captures stderr; `ensure_installed` feeds the packed files on stdin.
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
    // Each answer has its own label, so one that comes back empty (a
    // missing or silent command) cannot be taken for the next.
    let cmd = format!(
        "echo {PROBE_MARKER}; echo \"u=$(id -u 2>/dev/null)\"; \
         echo \"s=$(uname -sm 2>/dev/null)\"; echo \"n=$(uname -n 2>/dev/null)\"; \
         echo \"v=$(\"$HOME/{REMOTE_BIN}\" --version 2>/dev/null)\""
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
    // The first line with each label; a multi-line answer only counts
    // for its first line.
    let reply = &all[at + 1..];
    let answer = |label: &str| {
        reply
            .iter()
            .find_map(|l| l.strip_prefix(label))
            .map(str::trim)
            .unwrap_or("")
    };
    let root = answer("u=") == "0";
    let (os, arch) = parse_uname(answer("s="));
    let hostname = short_host_name(answer("n="));
    let installed = Some(answer("v="))
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
    /// This machine's own files, copied over.
    Copied {
        replaced: Option<String>,
        contents: Contents,
    },
    /// The release files for `platform` ("linux aarch64"), downloaded
    /// here and copied over.
    Downloaded {
        replaced: Option<String>,
        platform: String,
        contents: Contents,
    },
}

/// Make sure the server has this exact build, installed by the release
/// installer into `~/.local`. A release build sends the release files for
/// the server's platform; on the same OS and CPU it falls back to this
/// machine's own files when there are no release files for this build. A
/// development build sends its own files, and only to the same platform.
///
/// `no_install` copies nothing: the irsync already in `~/.local/bin` must be
/// this exact build, or this fails and the server is left as it was.
pub fn ensure_installed(rsh: &Rsh, dest: &str, probe: &Probe, no_install: bool) -> Result<Install> {
    // `--version` prints the program name, then LONG_VERSION.
    if probe.installed.as_deref() == Some(format!("irsync {}", *LONG_VERSION).as_str()) {
        return Ok(Install::Current);
    }
    if no_install {
        let found = match &probe.installed {
            Some(v) => format!("found {v}"),
            None => "found none".to_string(),
        };
        bail!(
            "--no-install: {dest} has no irsync {} at ~/.local/bin/irsync ({found}). \
             Install that version there, or run without --no-install (or set \
             ssh_install = true in the config). Nothing was changed.",
            *LONG_VERSION
        );
    }
    let (my_os, my_arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let same = probe.os == Some(my_os) && probe.arch == Some(my_arch);
    let target = release::target_for(probe.os, probe.arch);
    if let (Some(target), Some(tag)) = (target, release_tag()) {
        match install_release(rsh, dest, probe, target, &tag, same)? {
            Ok(install) => return Ok(install),
            Err(reason) => eprintln!(
                "irsync: no release files for this build ({reason}); \
                 installing this machine's own files"
            ),
        }
    }
    if same {
        return install_local(rsh, dest, probe);
    }
    let os = probe.os.unwrap_or("an unknown OS");
    let arch = probe.arch.unwrap_or("an unknown CPU");
    let by_hand =
        format!("Install irsync on both machines and set the server up by hand: {BY_HAND_URL}");
    if target.is_none() {
        bail!(
            "{dest} runs {os} {arch}, this irsync is {my_os} {my_arch}, and there is no \
             release binary for that platform. {by_hand}"
        );
    }
    bail!(
        "{dest} runs {os} {arch}, this irsync is {my_os} {my_arch} and a development \
         build ({}). Only release builds install on another OS or CPU. {by_hand}",
        *LONG_VERSION
    )
}

/// This machine's own files, for a server of the same OS and CPU.
fn install_local(rsh: &Rsh, dest: &str, probe: &Probe) -> Result<Install> {
    let staged = Staged::from_local(&exe_path()?)?;
    let contents = staged.contents();
    match install_remote(rsh, dest, &staged.pack()?)? {
        Upload::Done => {}
        Upload::NotThisVersion | Upload::DoesNotRun => bail!(
            "the copied irsync does not run on {dest}. Nothing was changed there. \
             Install irsync on the server and set it up by hand: {BY_HAND_URL}"
        ),
        other => bail!("{}", other.failure(dest)),
    }
    Ok(Install::Copied {
        replaced: probe.installed.clone(),
        contents,
    })
}

/// Download the release files of `tag` for `target` and install them.
/// `same`: the server has this machine's OS and CPU, so a missing download
/// or a release of another build is no error; the inner `Err` says why,
/// and the caller sends this machine's own files instead.
fn install_release(
    rsh: &Rsh,
    dest: &str,
    probe: &Probe,
    target: &str,
    tag: &str,
    same: bool,
) -> Result<std::result::Result<Install, String>> {
    let os = probe.os.unwrap_or("an unknown OS");
    let arch = probe.arch.unwrap_or("an unknown CPU");
    let by_hand =
        format!("Install irsync on both machines and set the server up by hand: {BY_HAND_URL}");
    eprintln!("irsync: downloading irsync {tag} for {os} {arch}");
    let fetched = match release::fetch(tag, target) {
        Ok(fetched) => fetched,
        Err(e) if same => {
            return Ok(Err(format!(
                "could not download release {tag}: {}",
                e.root_cause()
            )))
        }
        Err(e) => return Err(e),
    };
    let staged = Staged::from_release(fetched)?;
    let contents = staged.contents();
    match install_remote(rsh, dest, &staged.pack()?)? {
        Upload::Done => {}
        Upload::NotThisVersion if same => {
            return Ok(Err(format!("release {tag} is another build")))
        }
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
        other => bail!("{}", other.failure(dest)),
    }
    Ok(Ok(Install::Downloaded {
        replaced: probe.installed.clone(),
        platform: format!("{os} {arch}"),
        contents,
    }))
}

/// The release tag of this build, or `None` for a development build. A
/// test build is a release build only when a test points the download at
/// its own files, so no test ever downloads from GitHub.
fn release_tag() -> Option<String> {
    let info = BuildInfo::current();
    #[cfg(feature = "test-hooks")]
    let tag = release::test_url().map(|_| format!("v{}", info.version));
    #[cfg(not(feature = "test-hooks"))]
    let tag = release::release_tag(&info);
    tag
}

/// What the install command reported.
#[derive(Debug, PartialEq, Eq)]
enum Upload {
    Done,
    /// Fewer bytes arrived than were sent.
    Short,
    /// The files arrived whole and irsync runs on the server, but reports
    /// another version than this build's.
    NotThisVersion,
    /// The files arrived whole, but on the server irsync prints no
    /// version: it does not run there (wrong libc, a `noexec` home).
    DoesNotRun,
    /// The installer script failed; it said why on stderr.
    InstallerFailed,
    /// No work directory, or the files could not be stored or unpacked.
    CannotUnpack,
}

impl Upload {
    /// The message for the outcomes every caller treats alike.
    fn failure(&self, dest: &str) -> String {
        match self {
            Upload::Short => {
                format!(
                    "the copy of irsync on {dest} was cut short; nothing was changed, try again"
                )
            }
            Upload::InstallerFailed => {
                format!("the installer failed on {dest}; see its message above")
            }
            Upload::CannotUnpack => format!(
                "could not unpack irsync on {dest} (it needs tar and a writable home \
                 directory). Nothing was changed there."
            ),
            other => format!("installing irsync on {dest} failed ({other:?})"),
        }
    }
}

/// Send `packed` to the server and run its installer there. Nothing is
/// installed unless every byte arrived and the unpacked irsync's
/// `--version` is ours. The work directory in the home is removed on
/// every exit, a signal included.
fn install_remote(rsh: &Rsh, dest: &str, packed: &Packed) -> Result<Upload> {
    let file = std::fs::File::open(packed.path()).context("cannot read the packed files")?;
    let len = packed.len();
    // `--version` prints the program name, then LONG_VERSION. The text sits
    // in double quotes in the remote sh command.
    let want = format!("irsync {}", *LONG_VERSION);
    debug_assert!(!want.contains(['"', '$', '`']), "{want}");
    // Exit 3 marks a short upload, 4 an irsync that reports another
    // version, 5 one that prints no version at all (it does not run), 6 a
    // failed installer, 7 files that could not be stored or unpacked.
    // `sh_quote` takes no `'`, so the EXIT trap calls a function.
    let cmd = format!(
        "w=; clean() {{ [ -z \"$w\" ] || rm -rf \"$w\"; }}; \
         trap clean EXIT; trap \"exit 129\" HUP; trap \"exit 130\" INT; trap \"exit 143\" TERM; \
         w=$(mktemp -d \"$HOME/.irsync-install.XXXXXX\") || exit 7; \
         cat > \"$w/in.tar\" || exit 7; \
         [ \"$(wc -c < \"$w/in.tar\")\" -eq {len} ] || exit 3; \
         tar -xf \"$w/in.tar\" -C \"$w\" || exit 7; \
         v=$(\"$w/files/irsync\" --version 2>/dev/null | head -n 1); \
         [ -n \"$v\" ] || exit 5; \
         [ \"$v\" = \"{want}\" ] || exit 4; \
         sh \"$w/installer.sh\" --from \"$w/files\" || exit 6"
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
        Some(6) => Ok(Upload::InstallerFailed),
        Some(7) => Ok(Upload::CannotUnpack),
        _ => bail!("installing irsync on {dest} failed ({status})"),
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
pub fn setup_command(client: EndpointId, client_name: &str, flags: SetupFlags) -> String {
    let mut cmd = format!(
        "\"$HOME/{REMOTE_BIN}\" ssh-setup --client {client} --client-name-hex {}",
        encode_name(client_name)
    );
    if flags.recheck {
        cmd.push_str(" --recheck");
    }
    if flags.no_dht {
        cmd.push_str(" --no-dht");
    }
    if flags.no_install {
        cmd.push_str(" --keep-old");
    }
    cmd
}

/// Run `ssh-setup` on the server and parse what it reports; `recheck`
/// passes `--recheck` (a saved host: never add back a removed grant),
/// `no_dht` passes `--no-dht` (turn the daemon's DHT publishing off),
/// `no_install` passes `--keep-old` (nothing was installed this run).
pub fn run_setup(
    rsh: &Rsh,
    dest: &str,
    client: EndpointId,
    client_name: &str,
    flags: SetupFlags,
) -> Result<SetupReport> {
    let cmd = setup_command(client, client_name, flags);
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
