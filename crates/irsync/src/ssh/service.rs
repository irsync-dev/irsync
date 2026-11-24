//! The user service `irsync pair` makes of the daemon on the server, so it
//! starts again after a reboot: the files it writes, and the systemd or
//! launchd commands that load and start them.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The kind of user service manager. A Windows service is the third kind,
/// once the daemon runs on Windows (#277's design).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manager {
    Systemd,
    Launchd,
}

/// What `irsync pair` did about the user service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceOutcome {
    /// The systemd user service is set up and running.
    Systemd,
    /// The launchd agent is set up and running.
    Launchd,
    /// `--no-service`: nothing was done.
    Skipped,
    /// Another daemon holds the lock: the service was left alone.
    NotTouched,
    /// There is no service manager, or no user session to talk to it.
    Unavailable,
    /// The manager refused (linger, enable or start), with this error.
    Failed(String),
    /// A service file irsync did not write, and that does not start this
    /// binary, is there; it was left alone.
    Foreign(PathBuf),
}

/// A line in the files irsync writes (a `# ` comment in the unit, an XML
/// comment in the plist) that says they are its own.
pub const MARKER: &str = "Written by irsync pair";

/// Where the service file for `m` lives under `home`.
pub fn unit_path(home: &Path, m: Manager) -> PathBuf {
    match m {
        Manager::Systemd => home.join(".config/systemd/user/irsync.service"),
        Manager::Launchd => home.join("Library/LaunchAgents/org.irsync.daemon.plist"),
    }
}

/// The arguments the service runs `exe` with.
fn args(config: Option<&Path>) -> Vec<String> {
    let mut a = vec!["daemon".to_string(), "-vv".to_string()];
    if let Some(c) = config {
        a.push("--config".into());
        a.push(c.to_string_lossy().into_owned());
    }
    a
}

/// A systemd word: in double quotes, `\\` and `"` escaped, `%` doubled.
fn systemd_word(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            '$' => out.push_str("$$"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The service file for `m` that runs `exe daemon -vv` (with `--config`
/// when `config` is given). `exe` is written out in full, not as `%h` or
/// `$HOME`.
pub fn unit_text(m: Manager, exe: &Path, config: Option<&Path>) -> String {
    let exe = exe.to_string_lossy();
    let rest = args(config);
    match m {
        Manager::Systemd => {
            let mut words = vec![systemd_word(&exe)];
            words.extend(rest.iter().map(|a| systemd_word(a)));
            format!(
                "# {MARKER}\n[Unit]\nDescription=irsync daemon\n\n[Service]\n\
                 ExecStart={}\nRestart=on-failure\n\n[Install]\nWantedBy=default.target\n",
                words.join(" ")
            )
        }
        Manager::Launchd => {
            let strings: String = std::iter::once(exe.as_ref())
                .chain(rest.iter().map(|s| s.as_str()))
                .map(|s| format!("<string>{}</string>", xml_escape(s)))
                .collect();
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                 <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
                 \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
                 <!-- {MARKER} -->\n\
                 <plist version=\"1.0\"><dict>\n  \
                 <key>Label</key><string>org.irsync.daemon</string>\n  \
                 <key>ProgramArguments</key><array>{strings}</array>\n  \
                 <key>RunAtLoad</key><true/>\n  \
                 <key>KeepAlive</key><true/>\n\
                 </dict></plist>\n"
            )
        }
    }
}

/// What is at the service file's path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Existing {
    Absent,
    /// irsync wrote it (it has the [`MARKER`]).
    Ours,
    /// Someone else wrote it, and it starts this binary.
    UsesThisBinary,
    /// Someone else wrote it, and it starts something else.
    Foreign,
}

/// Looks at the text of the service file (None: no file) for `m`.
pub fn classify(text: Option<&str>, m: Manager, exe: &Path) -> Existing {
    let Some(text) = text else {
        return Existing::Absent;
    };
    if text.contains(MARKER) {
        return Existing::Ours;
    }
    let words = match m {
        Manager::Systemd => exec_start_words(text),
        Manager::Launchd => program_arguments(text),
    };
    match words.as_slice() {
        [first, second, ..] if Path::new(first) == exe && second == "daemon" => {
            Existing::UsesThisBinary
        }
        _ => Existing::Foreign,
    }
}

/// The first two words of the unit's `ExecStart=`, unquoted.
fn exec_start_words(text: &str) -> Vec<String> {
    let Some(value) = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("ExecStart="))
    else {
        return Vec::new();
    };
    // Prefixes that change how systemd runs the command.
    let value = value.trim_start_matches(['@', '-', ':', '+', '!']);
    let mut words = Vec::new();
    let mut chars = value.chars().peekable();
    while words.len() < 2 {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let mut w = String::new();
        let mut quoted = false;
        while let Some(&c) = chars.peek() {
            if !quoted && c.is_whitespace() {
                break;
            }
            chars.next();
            match c {
                '"' => quoted = !quoted,
                '\\' => {
                    if let Some(n) = chars.next() {
                        w.push(n);
                    }
                }
                '%' | '$' if chars.peek() == Some(&c) => {
                    chars.next();
                    w.push(c);
                }
                c => w.push(c),
            }
        }
        words.push(w);
    }
    words
}

/// The first two `<string>`s after `ProgramArguments` in a plist.
fn program_arguments(text: &str) -> Vec<String> {
    let Some(at) = text.find("<key>ProgramArguments</key>") else {
        return Vec::new();
    };
    let mut rest = &text[at..];
    let mut words = Vec::new();
    while words.len() < 2 {
        let Some(open) = rest.find("<string>") else {
            break;
        };
        let after = &rest[open + "<string>".len()..];
        let Some(close) = after.find("</string>") else {
            break;
        };
        words.push(xml_unescape(&after[..close]));
        rest = &after[close..];
    }
    words
}

/// The systemd unit's name, and the launchd job's label.
const UNIT: &str = "irsync";
const LABEL: &str = "org.irsync.daemon";

/// The service tool `name` (`systemctl`, `loginctl`, `launchctl`), if
/// there is one. A test-hooks build looks only in the directory
/// `IRSYNC_TEST_SERVICE_BIN_DIR` names, so a test never touches the real
/// service manager of the machine it runs on.
fn tool(name: &str) -> Option<PathBuf> {
    #[cfg(feature = "test-hooks")]
    {
        let dir = std::env::var_os("IRSYNC_TEST_SERVICE_BIN_DIR")?;
        let p = Path::new(&dir).join(name);
        p.is_file().then_some(p)
    }
    #[cfg(not(feature = "test-hooks"))]
    {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    }
}

/// Runs `name args`; its output, or why it failed, as one message.
fn run(name: &str, args: &[&str]) -> Result<String, String> {
    let shown = format!("{name} {}", args.join(" "));
    let exe = tool(name).ok_or_else(|| format!("{name} not found"))?;
    let out = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("`{shown}`: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        Err(if err.is_empty() {
            format!("`{shown}` failed ({})", out.status)
        } else {
            format!("`{shown}` failed: {err}")
        })
    }
}

/// The user service manager this session can talk to, if any: on Linux a
/// systemd user instance (`systemctl --user show-environment` works), on
/// macOS launchd.
pub fn detect() -> Option<Manager> {
    if cfg!(target_os = "linux") {
        run("systemctl", &["--user", "show-environment"])
            .ok()
            .map(|_| Manager::Systemd)
    } else if cfg!(target_os = "macos") {
        tool("launchctl").map(|_| Manager::Launchd)
    } else {
        None
    }
}

/// The launchd domains to try, in order: the GUI session of a user logged
/// in on the screen, else the user's background domain, which also serves
/// an ssh login.
fn launchd_domains() -> [String; 2] {
    let uid = uid();
    [format!("gui/{uid}"), format!("user/{uid}")]
}

/// This user's id, as the service managers name the user.
fn uid() -> u32 {
    #[cfg(unix)]
    {
        crate::platform::euid()
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// The domain the launchd job is loaded in, if it is loaded.
fn launchd_loaded() -> Option<String> {
    launchd_domains()
        .into_iter()
        .find(|d| run("launchctl", &["print", &format!("{d}/{LABEL}")]).is_ok())
}

/// Whether the service is running (systemd) or loaded (launchd).
pub fn is_active(m: Manager) -> bool {
    match m {
        Manager::Systemd => run("systemctl", &["--user", "is-active", "--quiet", UNIT]).is_ok(),
        Manager::Launchd => launchd_loaded().is_some(),
    }
}

/// Writes the service file for `exe` under `home`, unless one irsync did
/// not write is there; one that starts this binary is used as it is.
fn write_unit(m: Manager, home: &Path, exe: &Path, config: Option<&Path>) -> Result<(), String> {
    let path = unit_path(home, m);
    let old = std::fs::read_to_string(&path).ok();
    match classify(old.as_deref(), m, exe) {
        Existing::Absent | Existing::Ours => {}
        Existing::UsesThisBinary => return Ok(()),
        Existing::Foreign => {
            return Err(format!("{} was not written by irsync", path.display()));
        }
    }
    let text = unit_text(m, exe, config);
    if old.as_deref() == Some(text.as_str()) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        crate::fs::private::create_dir_all(dir)
            .map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    crate::fs::private::write(&path, text).map_err(|e| format!("writing {}: {e}", path.display()))
}

/// Gets the user service of `m` ready to run `exe daemon` (with
/// `--config config`), without starting it. On systemd, linger comes
/// first: without it the service would stop when the ssh session ends, so
/// nothing is written when it is refused. Then the service file is written
/// (when there is none, or irsync wrote it) and systemd re-reads it.
pub fn prepare(m: Manager, home: &Path, exe: &Path, config: Option<&Path>) -> Result<(), String> {
    match m {
        Manager::Systemd => {
            let uid = uid().to_string();
            let linger = run("loginctl", &["show-user", &uid, "-p", "Linger"])?;
            if !linger.lines().any(|l| l.trim() == "Linger=yes") {
                run("loginctl", &["enable-linger"])?;
            }
            write_unit(m, home, exe, config)?;
            run("systemctl", &["--user", "daemon-reload"])?;
        }
        Manager::Launchd => write_unit(m, home, exe, config)?,
    }
    Ok(())
}

/// Starts the service [`prepare`] made ready; with `restart`, a running
/// one is restarted so the new binary runs.
pub fn activate(m: Manager, home: &Path, restart: bool) -> Result<(), String> {
    match m {
        Manager::Systemd => {
            if restart {
                run("systemctl", &["--user", "restart", UNIT])?;
            } else {
                run("systemctl", &["--user", "enable", "--now", UNIT])?;
            }
        }
        Manager::Launchd => {
            let plist = unit_path(home, m);
            let plist = plist.to_string_lossy();
            let domain = match launchd_loaded() {
                Some(d) => d,
                None => {
                    let mut errors = Vec::new();
                    let mut loaded = None;
                    for d in launchd_domains() {
                        match run("launchctl", &["bootstrap", &d, &plist]) {
                            Ok(_) => {
                                loaded = Some(d);
                                break;
                            }
                            Err(e) => errors.push(e),
                        }
                    }
                    // Loaded by someone else in the meantime: not an error.
                    loaded
                        .or_else(launchd_loaded)
                        .ok_or_else(|| errors.join("; "))?
                }
            };
            if restart {
                run(
                    "launchctl",
                    &["kickstart", "-k", &format!("{domain}/{LABEL}")],
                )?;
            }
        }
    }
    Ok(())
}

/// Stops the service and keeps it from starting again (after a reboot
/// too), so a daemon started in the background is the only one. The
/// service file stays.
pub fn deactivate(m: Manager) -> Result<(), String> {
    match m {
        Manager::Systemd => run("systemctl", &["--user", "disable", "--now", UNIT]).map(|_| ()),
        Manager::Launchd => match launchd_loaded() {
            Some(d) => run("launchctl", &["bootout", &format!("{d}/{LABEL}")]).map(|_| ()),
            None => Ok(()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "/h/.local/bin/irsync";

    #[test]
    fn systemd_unit_runs_this_binary() {
        let t = unit_text(Manager::Systemd, Path::new(EXE), None);
        assert!(t.contains(MARKER), "{t}");
        assert!(
            t.contains("ExecStart=\"/h/.local/bin/irsync\" \"daemon\" \"-vv\"\n"),
            "{t}"
        );
        assert!(t.contains("Restart=on-failure"), "{t}");
        assert!(t.contains("WantedBy=default.target"), "{t}");
    }

    #[test]
    fn systemd_unit_quotes_odd_paths() {
        let t = unit_text(
            Manager::Systemd,
            Path::new("/home/a b/it's%/irsync"),
            Some(Path::new("/c d/irsync.toml")),
        );
        assert!(
            t.contains(
                "ExecStart=\"/home/a b/it's%%/irsync\" \"daemon\" \"-vv\" \"--config\" \"/c d/irsync.toml\"\n"
            ),
            "{t}"
        );
        let t = unit_text(Manager::Systemd, Path::new("/a\"b\\c/irsync"), None);
        assert!(t.contains("ExecStart=\"/a\\\"b\\\\c/irsync\" "), "{t}");
    }

    #[test]
    fn systemd_unit_escapes_dollar() {
        let exe = Path::new("/home/a$b/irsync");
        let t = unit_text(Manager::Systemd, exe, None);
        assert!(
            t.contains("ExecStart=\"/home/a$$b/irsync\" \"daemon\" \"-vv\"\n"),
            "{t}"
        );
        let hand = "[Service]\nExecStart=\"/home/a$$b/irsync\" daemon\n";
        assert_eq!(
            classify(Some(hand), Manager::Systemd, exe),
            Existing::UsesThisBinary
        );
    }

    #[test]
    fn launchd_plist_runs_this_binary() {
        let t = unit_text(Manager::Launchd, Path::new(EXE), None);
        assert!(t.contains("<key>Label</key><string>org.irsync.daemon</string>"));
        assert!(t.contains(
            "<string>/h/.local/bin/irsync</string><string>daemon</string><string>-vv</string>"
        ));
        assert!(t.contains("<key>RunAtLoad</key><true/>"));
        assert!(t.contains("<key>KeepAlive</key><true/>"));
        assert!(t.contains(MARKER));
        let t = unit_text(
            Manager::Launchd,
            Path::new("/a&b/irsync"),
            Some(Path::new("/c/x.toml")),
        );
        assert!(t.contains("<string>/a&amp;b/irsync</string>"), "{t}");
        assert!(
            t.contains("<string>--config</string><string>/c/x.toml</string>"),
            "{t}"
        );
    }

    #[test]
    fn unit_paths() {
        let h = Path::new("/h");
        assert_eq!(
            unit_path(h, Manager::Systemd),
            Path::new("/h/.config/systemd/user/irsync.service")
        );
        assert_eq!(
            unit_path(h, Manager::Launchd),
            Path::new("/h/Library/LaunchAgents/org.irsync.daemon.plist")
        );
    }

    #[test]
    fn classify_absent_and_ours() {
        let exe = Path::new(EXE);
        for m in [Manager::Systemd, Manager::Launchd] {
            assert_eq!(classify(None, m, exe), Existing::Absent);
            let ours = unit_text(m, exe, None);
            assert_eq!(classify(Some(&ours), m, exe), Existing::Ours);
            // Ours even when it starts another binary: irsync may replace it.
            let other = unit_text(m, Path::new("/usr/bin/irsync"), None);
            assert_eq!(classify(Some(&other), m, exe), Existing::Ours);
        }
    }

    #[test]
    fn classify_hand_written_systemd_unit() {
        let exe = Path::new(EXE);
        let mine = "[Service]\nExecStart=/h/.local/bin/irsync daemon -vv\nRestart=on-failure\n";
        assert_eq!(
            classify(Some(mine), Manager::Systemd, exe),
            Existing::UsesThisBinary
        );
        let quoted = "[Service]\nExecStart=\"/h/.local/bin/irsync\" \"daemon\"\n";
        assert_eq!(
            classify(Some(quoted), Manager::Systemd, exe),
            Existing::UsesThisBinary
        );
        let other = "[Service]\nExecStart=/usr/bin/irsync daemon\n";
        assert_eq!(
            classify(Some(other), Manager::Systemd, exe),
            Existing::Foreign
        );
        let not_daemon = "[Service]\nExecStart=/h/.local/bin/irsync serve\n";
        assert_eq!(
            classify(Some(not_daemon), Manager::Systemd, exe),
            Existing::Foreign
        );
        assert_eq!(classify(Some(""), Manager::Systemd, exe), Existing::Foreign);
    }

    #[test]
    fn classify_hand_written_plist() {
        let exe = Path::new(EXE);
        let mine = "<plist><dict><key>ProgramArguments</key><array>\
                    <string>/h/.local/bin/irsync</string><string>daemon</string>\
                    </array></dict></plist>";
        assert_eq!(
            classify(Some(mine), Manager::Launchd, exe),
            Existing::UsesThisBinary
        );
        let other = mine.replace("/h/.local", "/usr/local");
        assert_eq!(
            classify(Some(&other), Manager::Launchd, exe),
            Existing::Foreign
        );
    }
}
