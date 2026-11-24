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

/// The arguments the service runs `exe` with. The config is always
/// named: the service manager's environment (`XDG_CONFIG_HOME`, say) may
/// differ from the ssh session's that found it.
fn args(config: &Path) -> Vec<String> {
    vec![
        "daemon".to_string(),
        "-vv".to_string(),
        "--config".to_string(),
        config.to_string_lossy().into_owned(),
    ]
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

/// The service file for `m` that runs `exe daemon -vv --config config`.
/// `exe` and `config` are written out in full, not as `%h` or `$HOME`.
/// launchd keeps no output of its own, so the agent's goes to `log`.
pub fn unit_text(m: Manager, exe: &Path, config: &Path, log: &Path) -> String {
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
            let log = xml_escape(&log.to_string_lossy());
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                 <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
                 \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
                 <!-- {MARKER} -->\n\
                 <plist version=\"1.0\"><dict>\n  \
                 <key>Label</key><string>org.irsync.daemon</string>\n  \
                 <key>ProgramArguments</key><array>{strings}</array>\n  \
                 <key>RunAtLoad</key><true/>\n  \
                 <key>KeepAlive</key><true/>\n  \
                 <key>StandardOutPath</key><string>{log}</string>\n  \
                 <key>StandardErrorPath</key><string>{log}</string>\n\
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

/// Looks at the text of the service file (None: no file) for `m`. `home`
/// stands for a leading `%h`, `$HOME` or `${HOME}` in a systemd unit, as
/// in the unit irsync's own steps say to write.
pub fn classify(text: Option<&str>, m: Manager, exe: &Path, home: &Path) -> Existing {
    let Some(text) = text else {
        return Existing::Absent;
    };
    if text.contains(MARKER) {
        return Existing::Ours;
    }
    let words = match m {
        Manager::Systemd => exec_start_words(text, home),
        Manager::Launchd => program_arguments(text),
    };
    match words.as_slice() {
        [first, second, ..] if Path::new(first) == exe && second == "daemon" => {
            Existing::UsesThisBinary
        }
        _ => Existing::Foreign,
    }
}

/// The first two words of the unit's `ExecStart=`, unquoted, with a
/// leading `%h`, `$HOME` or `${HOME}` (not `%%h` or `$$HOME`) of a word
/// replaced by `home`, as systemd does for a user service.
fn exec_start_words(text: &str, home: &Path) -> Vec<String> {
    let Some(value) = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("ExecStart="))
    else {
        return Vec::new();
    };
    // Prefixes that change how systemd runs the command.
    let value = value.trim_start_matches(['@', '-', ':', '+', '!']);
    let home = home.to_string_lossy();
    let chars: Vec<char> = value.chars().collect();
    let at = |i: usize, s: &str| {
        s.chars()
            .enumerate()
            .all(|(k, c)| chars.get(i + k) == Some(&c))
    };
    let mut i = 0;
    let mut words = Vec::new();
    while words.len() < 2 {
        while chars.get(i).is_some_and(|c| c.is_whitespace()) {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let mut w = String::new();
        let mut quoted = false;
        // Past a leading `"` only: where a home prefix may stand.
        let mut start = true;
        while let Some(&c) = chars.get(i) {
            if !quoted && c.is_whitespace() {
                break;
            }
            if start {
                let prefix = ["%h", "${HOME}", "$HOME"].into_iter().find(|p| at(i, p));
                if let Some(p) = prefix {
                    w.push_str(&home);
                    i += p.chars().count();
                    start = false;
                    continue;
                }
            }
            i += 1;
            match c {
                '"' => {
                    quoted = !quoted;
                    continue;
                }
                '\\' => {
                    if let Some(&n) = chars.get(i) {
                        w.push(n);
                        i += 1;
                    }
                }
                '%' | '$' if chars.get(i) == Some(&c) => {
                    i += 1;
                    w.push(c);
                }
                c => w.push(c),
            }
            start = false;
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
    crate::platform::euid()
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
fn write_unit(
    m: Manager,
    home: &Path,
    exe: &Path,
    config: &Path,
    log: &Path,
) -> Result<(), String> {
    let path = unit_path(home, m);
    let old = std::fs::read_to_string(&path).ok();
    match classify(old.as_deref(), m, exe, home) {
        Existing::Absent | Existing::Ours => {}
        Existing::UsesThisBinary => return Ok(()),
        Existing::Foreign => {
            return Err(format!("{} was not written by irsync", path.display()));
        }
    }
    let text = unit_text(m, exe, config, log);
    if old.as_deref() == Some(text.as_str()) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        crate::fs::private::create_dir_all(dir)
            .map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    crate::fs::private::write(&path, text).map_err(|e| format!("writing {}: {e}", path.display()))
}

/// Gets the user service of `m` ready to run `exe daemon --config
/// config` (a launchd agent's output going to `log`), without starting it. On systemd, linger comes
/// first: without it the service would stop when the ssh session ends, so
/// nothing is written when it is refused. Then the service file is written
/// (when there is none, or irsync wrote it) and systemd re-reads it.
pub fn prepare(
    m: Manager,
    home: &Path,
    exe: &Path,
    config: &Path,
    log: &Path,
) -> Result<(), String> {
    match m {
        Manager::Systemd => {
            let uid = uid().to_string();
            let linger = run("loginctl", &["show-user", &uid, "-p", "Linger"])?;
            if !linger.lines().any(|l| l.trim() == "Linger=yes") {
                run("loginctl", &["enable-linger"])?;
            }
            write_unit(m, home, exe, config, log)?;
            run("systemctl", &["--user", "daemon-reload"])?;
        }
        Manager::Launchd => write_unit(m, home, exe, config, log)?,
    }
    Ok(())
}

/// Starts the service [`prepare`] made ready, and makes it start after a
/// reboot too; with `restart`, a running one is restarted so the new
/// binary runs.
pub fn activate(m: Manager, home: &Path, restart: bool) -> Result<(), String> {
    match m {
        Manager::Systemd => {
            if restart {
                // A running unit may have been disabled by hand.
                run("systemctl", &["--user", "enable", UNIT])?;
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
                    // Undoes a `disable` by [`deactivate`], so the agent
                    // loads again at login too.
                    for d in launchd_domains() {
                        let _ = run("launchctl", &["enable", &format!("{d}/{LABEL}")]);
                    }
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
/// service file stays. A launchd agent is also disabled, in every domain
/// that takes it: with `RunAtLoad` it would load again at the next login.
pub fn deactivate(m: Manager) -> Result<(), String> {
    match m {
        Manager::Systemd => run("systemctl", &["--user", "disable", "--now", UNIT]).map(|_| ()),
        Manager::Launchd => {
            if let Some(d) = launchd_loaded() {
                run("launchctl", &["bootout", &format!("{d}/{LABEL}")])?;
            }
            let mut errors = Vec::new();
            for d in launchd_domains() {
                if let Err(e) = run("launchctl", &["disable", &format!("{d}/{LABEL}")]) {
                    errors.push(e);
                }
            }
            if errors.len() == launchd_domains().len() {
                return Err(errors.join("; "));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "/h/.local/bin/irsync";
    const CONFIG: &str = "/h/.config/irsync/irsync.toml";
    const LOG: &str = "/h/.local/state/irsync/daemon.log";
    const HOME: &str = "/h";

    fn text(m: Manager, exe: &str) -> String {
        unit_text(m, Path::new(exe), Path::new(CONFIG), Path::new(LOG))
    }

    fn classify_as(t: &str, m: Manager) -> Existing {
        classify(Some(t), m, Path::new(EXE), Path::new(HOME))
    }

    #[test]
    fn systemd_unit_runs_this_binary() {
        let t = text(Manager::Systemd, EXE);
        assert!(t.contains(MARKER), "{t}");
        assert!(
            t.contains(
                "ExecStart=\"/h/.local/bin/irsync\" \"daemon\" \"-vv\" \"--config\" \
                 \"/h/.config/irsync/irsync.toml\"\n"
            ),
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
            Path::new("/c d/irsync.toml"),
            Path::new(LOG),
        );
        assert!(
            t.contains(
                "ExecStart=\"/home/a b/it's%%/irsync\" \"daemon\" \"-vv\" \"--config\" \"/c d/irsync.toml\"\n"
            ),
            "{t}"
        );
        let t = text(Manager::Systemd, "/a\"b\\c/irsync");
        assert!(t.contains("ExecStart=\"/a\\\"b\\\\c/irsync\" "), "{t}");
    }

    #[test]
    fn systemd_unit_escapes_dollar() {
        let exe = Path::new("/home/a$b/irsync");
        let t = unit_text(Manager::Systemd, exe, Path::new(CONFIG), Path::new(LOG));
        assert!(
            t.contains("ExecStart=\"/home/a$$b/irsync\" \"daemon\" \"-vv\" "),
            "{t}"
        );
        let hand = "[Service]\nExecStart=\"/home/a$$b/irsync\" daemon\n";
        assert_eq!(
            classify(Some(hand), Manager::Systemd, exe, Path::new(HOME)),
            Existing::UsesThisBinary
        );
    }

    #[test]
    fn launchd_plist_runs_this_binary() {
        let t = text(Manager::Launchd, EXE);
        assert!(t.contains("<key>Label</key><string>org.irsync.daemon</string>"));
        assert!(t.contains(
            "<string>/h/.local/bin/irsync</string><string>daemon</string><string>-vv</string>\
             <string>--config</string><string>/h/.config/irsync/irsync.toml</string>"
        ));
        assert!(t.contains("<key>RunAtLoad</key><true/>"));
        assert!(t.contains("<key>KeepAlive</key><true/>"));
        assert!(t.contains(MARKER));
        assert!(
            t.contains(
                "<key>StandardOutPath</key><string>/h/.local/state/irsync/daemon.log</string>"
            ),
            "{t}"
        );
        assert!(
            t.contains(
                "<key>StandardErrorPath</key><string>/h/.local/state/irsync/daemon.log</string>"
            ),
            "{t}"
        );
        let t = unit_text(
            Manager::Launchd,
            Path::new("/a&b/irsync"),
            Path::new("/c/x.toml"),
            Path::new("/s<1>/daemon.log"),
        );
        assert!(t.contains("<string>/a&amp;b/irsync</string>"), "{t}");
        assert!(
            t.contains("<string>--config</string><string>/c/x.toml</string>"),
            "{t}"
        );
        assert!(
            t.contains("<key>StandardOutPath</key><string>/s&lt;1&gt;/daemon.log</string>"),
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
            assert_eq!(classify(None, m, exe, Path::new(HOME)), Existing::Absent);
            assert_eq!(classify_as(&text(m, EXE), m), Existing::Ours);
            // Ours even when it starts another binary: irsync may replace it.
            assert_eq!(classify_as(&text(m, "/usr/bin/irsync"), m), Existing::Ours);
        }
    }

    #[test]
    fn classify_hand_written_systemd_unit() {
        let sd = Manager::Systemd;
        let mine = "[Service]\nExecStart=/h/.local/bin/irsync daemon -vv\nRestart=on-failure\n";
        assert_eq!(classify_as(mine, sd), Existing::UsesThisBinary);
        let quoted = "[Service]\nExecStart=\"/h/.local/bin/irsync\" \"daemon\"\n";
        assert_eq!(classify_as(quoted, sd), Existing::UsesThisBinary);
        let other = "[Service]\nExecStart=/usr/bin/irsync daemon\n";
        assert_eq!(classify_as(other, sd), Existing::Foreign);
        let not_daemon = "[Service]\nExecStart=/h/.local/bin/irsync serve\n";
        assert_eq!(classify_as(not_daemon, sd), Existing::Foreign);
        assert_eq!(classify_as("", sd), Existing::Foreign);
    }

    /// The unit irsync's own steps (and run-the-daemon.md) say to write
    /// names the binary under `%h`; systemd reads that as the home.
    #[test]
    fn classify_the_howto_unit_as_this_binary() {
        let sd = Manager::Systemd;
        let howto = "[Unit]\nDescription=irsync daemon\n[Service]\n\
                     ExecStart=%h/.local/bin/irsync daemon -vv\nRestart=on-failure\n\
                     [Install]\nWantedBy=default.target\n";
        assert_eq!(classify_as(howto, sd), Existing::UsesThisBinary);
        for line in [
            "ExecStart=$HOME/.local/bin/irsync daemon",
            "ExecStart=${HOME}/.local/bin/irsync daemon",
            "ExecStart=\"%h/.local/bin/irsync\" daemon",
        ] {
            assert_eq!(classify_as(line, sd), Existing::UsesThisBinary, "{line}");
        }
        // Escaped, or not at the start: not the home.
        for line in [
            "ExecStart=%%h/.local/bin/irsync daemon",
            "ExecStart=$$HOME/.local/bin/irsync daemon",
            "ExecStart=/x%h/.local/bin/irsync daemon",
        ] {
            assert_eq!(classify_as(line, sd), Existing::Foreign, "{line}");
        }
        // Another home: another binary.
        assert_eq!(
            classify(Some(howto), sd, Path::new(EXE), Path::new("/other")),
            Existing::Foreign
        );
    }

    #[test]
    fn classify_hand_written_plist() {
        let mine = "<plist><dict><key>ProgramArguments</key><array>\
                    <string>/h/.local/bin/irsync</string><string>daemon</string>\
                    </array></dict></plist>";
        assert_eq!(
            classify_as(mine, Manager::Launchd),
            Existing::UsesThisBinary
        );
        let other = mine.replace("/h/.local", "/usr/local");
        assert_eq!(classify_as(&other, Manager::Launchd), Existing::Foreign);
    }
}
