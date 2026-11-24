//! The user-service files `irsync pair` writes on the server, so the daemon
//! starts again after a reboot. Nothing here runs a command or writes a file.

use std::path::{Path, PathBuf};

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
                '%' if chars.peek() == Some(&'%') => {
                    chars.next();
                    w.push('%');
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
