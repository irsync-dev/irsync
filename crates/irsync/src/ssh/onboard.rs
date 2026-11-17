//! The client's first run against a host it does not know: set the server
//! up over SSH, save a host alias for it, and say what was done. It never
//! asks first; it acts, then reports.

use anyhow::Context;
use iroh::EndpointId;

use super::client::{ensure_installed, probe, run_setup, Install, Rsh, REMOTE_BIN};
use super::setup::{Created, DaemonOutcome, Dht, GrantOutcome, SetupReport};
use crate::build::LONG_VERSION;
use crate::config::hosts::valid_alias;
use crate::config::paths::Paths;
use crate::manage::{self, hosts::HostFlags};

/// Why a root account is refused, on the client and by `ssh-setup` on the
/// server.
pub const ROOT_ACCOUNT: &str = "irsync does not set up a daemon as root";

/// Where the book explains running the daemon under its own account.
const RUN_THE_DAEMON_URL: &str = "https://irsync.org/book/how-to/run-the-daemon.html";

/// What onboarding `alias` did.
#[derive(Debug, Clone)]
pub struct Onboarded {
    pub alias: String,
    /// The server daemon's EndpointId, now saved under `alias`.
    pub endpoint_id: EndpointId,
    /// The id `alias` pointed at before, when it changed.
    pub previous_id: Option<EndpointId>,
    /// `alias` was saved before this run (with `previous_id` or the same id).
    pub alias_existed: bool,
    pub report: SetupReport,
    pub install: Install,
    /// The server's OS (`std::env::consts::OS` names), from the probe.
    pub os: Option<&'static str>,
    /// This client's EndpointId, the one `ssh-setup` granted.
    pub client: EndpointId,
}

impl Onboarded {
    /// Where the daemon said it can be reached, to dial alongside the
    /// lookups: a daemon that just started has published nothing yet.
    pub fn hints(&self) -> Vec<iroh::TransportAddr> {
        let r = &self.report;
        r.relay
            .iter()
            .cloned()
            .map(iroh::TransportAddr::Relay)
            .chain(r.addrs.iter().copied().map(iroh::TransportAddr::Ip))
            .collect()
    }
}

/// Sets up `dest` (an ssh destination) for `client`: probe, install this
/// build, run `ssh-setup`, then save `alias` with `ssh = dest`. A first
/// run passes the same text for both. `recheck`: `alias` is a saved host
/// being checked, so `ssh-setup` must not add back a removed grant.
/// `no_dht`: turn the daemon's DHT publishing off.
pub fn onboard(
    paths: &Paths,
    rsh: &Rsh,
    alias: &str,
    dest: &str,
    client: EndpointId,
    recheck: bool,
    no_dht: bool,
) -> anyhow::Result<Onboarded> {
    check(alias, dest)?;
    let p = probe(rsh, dest)?;
    // Before anything is installed or changed on the server.
    if p.root {
        anyhow::bail!(
            "the ssh account on {dest} is root, and {ROOT_ACCOUNT}.\n  \
             Use another account, or set the daemon up by hand:\n  \
             {RUN_THE_DAEMON_URL}"
        );
    }
    let install = ensure_installed(rsh, dest, &p)?;
    let report = run_setup(rsh, dest, client, recheck, no_dht)?;
    let lock = manage::lock_config(&paths.config)?;
    let mut doc = lock.load()?;
    let flags = HostFlags {
        ssh: Some(dest.to_string()),
        ..HostFlags::default()
    };
    let alias_existed = doc
        .get("hosts")
        .and_then(|h| h.get(alias))
        .is_some_and(toml_edit::Item::is_table_like);
    let previous_id = manage::hosts::host_add(&mut doc, alias, report.endpoint_id, &flags)
        .with_context(|| format!("saving host alias {alias}"))?;
    lock.save(&mut doc)?;
    Ok(Onboarded {
        alias: alias.to_string(),
        endpoint_id: report.endpoint_id,
        previous_id,
        alias_existed,
        install,
        os: p.os,
        client,
        report,
    })
}

/// Whether `alias` can be saved and `dest` is a host name ssh will not
/// take for an option. Checked before any ssh call, so the server is
/// never changed for an alias that can't be saved.
pub fn check(alias: &str, dest: &str) -> anyhow::Result<()> {
    if dest.starts_with('-') {
        anyhow::bail!("{dest:?} is not a host name");
    }
    if !valid_alias(alias) {
        return Err(manage::ManageError::BadAlias(alias.to_string()).into());
    }
    Ok(())
}

/// The report, one line each, without the `irsync: ` prefix. The service
/// how-to is separate: see [`howto`].
pub fn summary(o: &Onboarded) -> Vec<String> {
    let dest = &o.alias;
    let bin = format!("~/{REMOTE_BIN}");
    let version = &*LONG_VERSION;
    let r = &o.report;
    let mut out = vec![format!("set up {dest} over ssh:")];
    let (replaced, note) = match &o.install {
        Install::Current => (None, None),
        Install::Copied { replaced, .. } => (Some(replaced), None),
        Install::Downloaded {
            replaced, platform, ..
        } => (
            Some(replaced),
            Some(format!(" (release binary for {platform})")),
        ),
    };
    let note = note.unwrap_or_default();
    match replaced {
        None => {}
        Some(None) => out.push(format!("  installed irsync {version} at {bin}{note}")),
        Some(Some(old)) => {
            let old = old.strip_prefix("irsync ").unwrap_or(old);
            out.push(format!(
                "  replaced irsync {old} with {version} at {bin}{note}"
            ));
        }
    }
    if r.identity == Created::Generated {
        out.push("  generated the daemon identity".into());
    }
    match r.grant {
        GrantOutcome::Added => out.push(format!(
            "  granted this client ({}) access to any path",
            short(&o.client)
        )),
        GrantOutcome::Already => {}
        GrantOutcome::Missing => out.push(format!(
            "  this client ({}) has no immediate grant on {dest} any more; \
             left it that way",
            short(&o.client)
        )),
        GrantOutcome::ImmediateOff => out.push(format!(
            "  immediate mode is off on {dest}: only exports granted to this client are reachable"
        )),
    }
    if r.dht == Dht::TurnedOff {
        out.push(format!(
            "  turned off DHT publishing on {dest} ([daemon.discovery] dht = false)"
        ));
    }
    match &r.daemon {
        DaemonOutcome::Running => {}
        DaemonOutcome::Reloaded => {
            out.push("  told the daemon to reload its settings".into());
        }
        DaemonOutcome::Started => {
            out.push(format!("  started the daemon (log: {})", r.log.display()));
        }
        DaemonOutcome::Restarted { old_version } => {
            out.push(format!("  restarted the daemon (was {old_version})"));
        }
        DaemonOutcome::Foreign { exe, version } => {
            out.push(format!(
                "  another irsync daemon is running there ({}, {version}); left it alone",
                exe.display()
            ));
            out.push(
                "  the alias points at this config's daemon identity; if that daemon uses \
                 another config, syncs may fail"
                    .into(),
            );
        }
    }
    match o.previous_id {
        // Already saved with this id (a restart): nothing to say.
        None if o.alias_existed => {}
        None => out.push(format!(
            "  saved host alias {dest} -> {}",
            short(&o.endpoint_id)
        )),
        Some(old) => out.push(format!(
            "  updated host alias {dest}: {} -> {}",
            short(&old),
            short(&o.endpoint_id)
        )),
    }
    out
}

fn short(id: &EndpointId) -> String {
    id.fmt_short().to_string()
}

/// After a daemon was started or restarted: how to make it a user service
/// on the server's OS. Printed without a prefix so it can be pasted. Empty
/// otherwise.
pub fn howto(o: &Onboarded) -> Vec<String> {
    if !matches!(
        o.report.daemon,
        DaemonOutcome::Started | DaemonOutcome::Restarted { .. }
    ) {
        return Vec::new();
    }
    // The daemon irsync started holds this lock (beside its log); the
    // service can't start until that daemon is stopped.
    let lock = o.report.log.with_file_name("daemon.lock");
    // The service starts only once the old daemon has let go of the lock.
    let stop = [
        format!(
            "  pid=\"$(sed -n 's/^pid=//p' {})\"",
            shell_path(&lock.to_string_lossy())
        ),
        "  kill \"$pid\"".to_string(),
        "  while kill -0 \"$pid\" 2>/dev/null; do sleep 0.2; done".to_string(),
    ];
    let (file, body, start): (&str, &[&str], &[&str]) = match o.os {
        Some("macos") => (
            "~/Library/LaunchAgents/org.irsync.daemon.plist (with your home directory for $HOME)",
            &[
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
                "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">",
                "<plist version=\"1.0\"><dict>",
                "  <key>Label</key><string>org.irsync.daemon</string>",
                "  <key>ProgramArguments</key><array><string>$HOME/.local/share/irsync/bin/irsync</string><string>daemon</string><string>-vv</string></array>",
                "  <key>RunAtLoad</key><true/>",
                "  <key>KeepAlive</key><true/>",
                "</dict></plist>",
            ],
            &["  launchctl load -w ~/Library/LaunchAgents/org.irsync.daemon.plist"],
        ),
        _ => (
            "~/.config/systemd/user/irsync.service",
            &[
                "[Unit]",
                "Description=irsync daemon",
                "[Service]",
                "ExecStart=%h/.local/share/irsync/bin/irsync daemon -vv",
                "Restart=on-failure",
                "[Install]",
                "WantedBy=default.target",
            ],
            &[
                "  systemctl --user enable --now irsync",
                "  loginctl enable-linger $USER",
            ],
        ),
    };
    let mut out = vec![
        "To keep the daemon running after a reboot, make it a user service.".to_string(),
        format!("Put this in {file}:"),
    ];
    out.extend(body.iter().map(|l| l.to_string()));
    out.push("Then stop the daemon irsync started and let the service take over:".into());
    out.extend(stop);
    out.extend(start.iter().map(|l| l.to_string()));
    out
}

/// `path` for a shell command: under `~` when it is the default state dir
/// (`$HOME/.local/state/irsync`), else as is, quoted when it holds anything
/// but plain path characters.
fn shell_path(path: &str) -> String {
    const DEFAULT: &str = "/.local/state/irsync/daemon.lock";
    if path.starts_with('/') && path.ends_with(DEFAULT) {
        return format!("~{DEFAULT}");
    }
    let plain = path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+,@".contains(c));
    if plain {
        path.to_string()
    } else {
        format!("'{}'", path.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::ssh::payload::Contents;

    /// Neither `fileset` nor the man pages.
    const ALONE: Contents = Contents {
        fileset: false,
        man: false,
    };

    fn id(b: u8) -> EndpointId {
        iroh::SecretKey::from_bytes(&[b; 32]).public()
    }

    /// ssh would read `-oProxyCommand=…` as an option: such a destination
    /// is refused before any ssh call, as alias or as `ssh` value.
    #[test]
    fn dash_destination_is_not_a_host_name() {
        for (alias, dest) in [("-oProxyCommand=x", "-oProxyCommand=x"), ("nas", "-x")] {
            let e = check(alias, dest).unwrap_err();
            assert_eq!(e.to_string(), format!("{dest:?} is not a host name"));
        }
        check("nas", "me@myserver").unwrap();
    }

    fn onboarded(install: Install, daemon: DaemonOutcome, os: Option<&'static str>) -> Onboarded {
        Onboarded {
            alias: "myserver".into(),
            endpoint_id: id(2),
            previous_id: Some(id(3)),
            alias_existed: true,
            report: SetupReport {
                version: LONG_VERSION.clone(),
                endpoint_id: id(2),
                identity: Created::Existing,
                grant: GrantOutcome::Already,
                dht: Dht::On,
                daemon,
                log: PathBuf::from("/s/daemon.log"),
                relay: None,
                addrs: Vec::new(),
            },
            install,
            os,
            client: id(1),
        }
    }

    #[test]
    fn downloaded_says_which_platform() {
        let o = onboarded(
            Install::Downloaded {
                replaced: None,
                platform: "linux aarch64".into(),
                contents: ALONE,
            },
            DaemonOutcome::Started,
            Some("linux"),
        );
        let s = summary(&o);
        let want = format!(
            "  installed irsync {} at ~/{REMOTE_BIN} (release binary for linux aarch64)",
            *LONG_VERSION
        );
        assert!(s.contains(&want), "{s:?}");
        let o = onboarded(
            Install::Downloaded {
                replaced: Some("irsync 0.0.1 (old)".into()),
                platform: "linux aarch64".into(),
                contents: ALONE,
            },
            DaemonOutcome::Started,
            Some("linux"),
        );
        let s = summary(&o);
        let want = format!(
            "  replaced irsync 0.0.1 (old) with {} at ~/{REMOTE_BIN} (release binary for linux aarch64)",
            *LONG_VERSION
        );
        assert!(s.contains(&want), "{s:?}");
    }

    #[test]
    fn replaced_restarted_on_macos() {
        let o = onboarded(
            Install::Copied {
                replaced: Some("irsync 0.0.1 (old)".into()),
                contents: ALONE,
            },
            DaemonOutcome::Restarted {
                old_version: "0.0.1 (old)".into(),
            },
            Some("macos"),
        );
        let s = summary(&o).join("\n");
        assert!(s.contains(&format!(
            "  replaced irsync 0.0.1 (old) with {} at ~/.local/bin/irsync",
            *LONG_VERSION
        )));
        assert!(s.contains("  restarted the daemon (was 0.0.1 (old))"));
        assert!(s.contains(&format!(
            "  updated host alias myserver: {} -> {}",
            id(3).fmt_short(),
            id(2).fmt_short()
        )));
        assert!(!s.contains("generated the daemon identity"));
        assert!(!s.contains("granted"));
        assert!(!s.contains("user service"));
        let h = howto(&o);
        let text = h.join("\n");
        assert!(text.contains("org.irsync.daemon.plist"));
        assert!(text.contains("<key>KeepAlive</key>"));
        assert!(!text.contains("systemctl"));
        // The plist body starts at column 0 so it can be pasted as a file.
        assert!(h.iter().any(|l| l.starts_with("<?xml ")));
        at(&h, "  <key>ProgramArguments</key><array><string>$HOME/.local/share/irsync/bin/irsync</string><string>daemon</string><string>-vv</string></array>");
        let pid = at(&h, "  pid=\"$(sed -n 's/^pid=//p' /s/daemon.lock)\"");
        let wait = at(
            &h,
            "  while kill -0 \"$pid\" 2>/dev/null; do sleep 0.2; done",
        );
        assert!(at(&h, "  kill \"$pid\"") < wait);
        assert!(pid < wait);
        assert!(
            wait < at(
                &h,
                "  launchctl load -w ~/Library/LaunchAgents/org.irsync.daemon.plist"
            )
        );
    }

    /// The index of the line equal to `want`.
    fn at(lines: &[String], want: &str) -> usize {
        lines
            .iter()
            .position(|l| l == want)
            .unwrap_or_else(|| panic!("no line {want:?} in {lines:#?}"))
    }

    #[test]
    fn started_on_linux_stops_then_hands_over() {
        let mut o = onboarded(Install::Current, DaemonOutcome::Started, Some("linux"));
        o.report.log = PathBuf::from("/home/u/.local/state/irsync/daemon.log");
        let h = howto(&o);
        assert_eq!(
            h[0],
            "To keep the daemon running after a reboot, make it a user service."
        );
        assert_eq!(h[1], "Put this in ~/.config/systemd/user/irsync.service:");
        at(&h, "ExecStart=%h/.local/share/irsync/bin/irsync daemon -vv");
        at(&h, "Restart=on-failure");
        let pid = at(
            &h,
            "  pid=\"$(sed -n 's/^pid=//p' ~/.local/state/irsync/daemon.lock)\"",
        );
        let wait = at(
            &h,
            "  while kill -0 \"$pid\" 2>/dev/null; do sleep 0.2; done",
        );
        assert!(at(&h, "  kill \"$pid\"") < wait);
        let enable = at(&h, "  systemctl --user enable --now irsync");
        assert!(pid < wait && wait < enable);
        assert!(enable < at(&h, "  loginctl enable-linger $USER"));
        assert!(!h.join("\n").contains("launchctl"));
        // Odd paths are quoted for the shell.
        o.report.log = PathBuf::from("/srv/my state/daemon.log");
        at(
            &howto(&o),
            "  pid=\"$(sed -n 's/^pid=//p' '/srv/my state/daemon.lock')\"",
        );
    }

    #[test]
    fn foreign_daemon_gets_no_howto() {
        let o = onboarded(
            Install::Current,
            DaemonOutcome::Foreign {
                exe: "/usr/bin/irsync".into(),
                version: "0.0.9 (x)".into(),
            },
            Some("linux"),
        );
        let s = summary(&o);
        assert_eq!(
            s[1],
            "  another irsync daemon is running there (/usr/bin/irsync, 0.0.9 (x)); left it alone"
        );
        assert!(s[2].starts_with("  the alias points at this config's daemon identity"));
        assert!(!s.join("\n").contains("installed"));
        assert!(howto(&o).is_empty());
    }

    /// `--no-remote-dht` on a running daemon: the setting line, then the
    /// reload; no how-to, since the daemon kept running.
    #[test]
    fn dht_turned_off_and_reloaded() {
        let mut o = onboarded(Install::Current, DaemonOutcome::Reloaded, Some("linux"));
        o.report.dht = Dht::TurnedOff;
        let s = summary(&o);
        let off = at(
            &s,
            "  turned off DHT publishing on myserver ([daemon.discovery] dht = false)",
        );
        assert!(off < at(&s, "  told the daemon to reload its settings"));
        assert!(howto(&o).is_empty());
        o.report.dht = Dht::Off;
        assert!(!summary(&o).join("\n").contains("DHT"));
    }

    /// `saved` for a new alias, `updated` for a changed id, and nothing
    /// about the alias when it existed with the same id (a plain restart).
    #[test]
    fn alias_line_says_saved_updated_or_nothing() {
        let mut o = onboarded(Install::Current, DaemonOutcome::Started, Some("linux"));
        o.previous_id = None;
        o.alias_existed = false;
        let s = summary(&o).join("\n");
        assert!(
            s.contains(&format!(
                "  saved host alias myserver -> {}",
                id(2).fmt_short()
            )),
            "{s}"
        );
        o.alias_existed = true;
        let s = summary(&o);
        assert!(!s.join("\n").contains("host alias"), "{s:#?}");
        assert_eq!(
            s.last().unwrap(),
            "  started the daemon (log: /s/daemon.log)"
        );
        o.previous_id = Some(id(3));
        let s = summary(&o).join("\n");
        assert!(
            s.contains(&format!(
                "  updated host alias myserver: {} -> {}",
                id(3).fmt_short(),
                id(2).fmt_short()
            )),
            "{s}"
        );
    }
}
