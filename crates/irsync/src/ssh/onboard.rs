//! The client's first run against a host it does not know: set the server
//! up over SSH, save a host alias for it, and say what was done. It never
//! asks first; it acts, then reports.

use anyhow::Context;
use iroh::EndpointId;

use super::client::{ensure_installed, probe, run_setup, Install, Rsh, REMOTE_BIN};
use super::setup::{Created, DaemonOutcome, GrantOutcome, SetupReport};
use crate::build::LONG_VERSION;
use crate::config::paths::Paths;
use crate::manage::{self, hosts::HostFlags};

/// What onboarding `alias` did.
#[derive(Debug, Clone)]
pub struct Onboarded {
    pub alias: String,
    /// The server daemon's EndpointId, now saved under `alias`.
    pub endpoint_id: EndpointId,
    /// The id `alias` pointed at before, when it changed.
    pub previous_id: Option<EndpointId>,
    pub report: SetupReport,
    pub install: Install,
    /// The server's OS (`std::env::consts::OS` names), from the probe.
    pub os: Option<&'static str>,
    /// This client's EndpointId, the one `ssh-setup` granted.
    pub client: EndpointId,
}

/// Sets up `dest` (an ssh destination, also the alias) for `client`:
/// probe, install this build, run `ssh-setup`, then save the alias with
/// `ssh = dest`.
pub fn onboard(
    paths: &Paths,
    rsh: &Rsh,
    dest: &str,
    client: EndpointId,
) -> anyhow::Result<Onboarded> {
    // A split hosts file can't take the alias: fail before touching the server.
    manage::ensure_editable(paths, manage::Table::Hosts)?;
    let p = probe(rsh, dest)?;
    let install = ensure_installed(rsh, dest, &p)?;
    let report = run_setup(rsh, dest, client)?;
    let lock = manage::lock_config(&paths.config)?;
    let mut doc = lock.load()?;
    let flags = HostFlags {
        ssh: Some(dest.to_string()),
        ..HostFlags::default()
    };
    let previous_id = manage::hosts::host_add(&mut doc, dest, report.endpoint_id, &flags)
        .with_context(|| format!("saving host alias {dest}"))?;
    lock.save(&doc)?;
    Ok(Onboarded {
        alias: dest.to_string(),
        endpoint_id: report.endpoint_id,
        previous_id,
        install,
        os: p.os,
        client,
        report,
    })
}

/// The report, one line each, without the `irsync: ` prefix.
pub fn summary(o: &Onboarded) -> Vec<String> {
    let dest = &o.alias;
    let bin = format!("~/{REMOTE_BIN}");
    let version = &*LONG_VERSION;
    let r = &o.report;
    let mut out = vec![format!("set up {dest} over ssh:")];
    match &o.install {
        Install::Current => {}
        Install::Copied { replaced: None } => {
            out.push(format!("  installed irsync {version} at {bin}"));
        }
        Install::Copied {
            replaced: Some(old),
        } => {
            let old = old.strip_prefix("irsync ").unwrap_or(old);
            out.push(format!("  replaced irsync {old} with {version} at {bin}"));
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
        GrantOutcome::ImmediateOff => out.push(format!(
            "  immediate mode is off on {dest}: only exports granted to this client are reachable"
        )),
    }
    let started = match &r.daemon {
        DaemonOutcome::Running => false,
        DaemonOutcome::Started => {
            out.push(format!("  started the daemon (log: {})", r.log.display()));
            true
        }
        DaemonOutcome::Restarted { old_version } => {
            out.push(format!("  restarted the daemon (was {old_version})"));
            true
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
            false
        }
    };
    match o.previous_id {
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
    if started {
        out.extend(service_howto(o.os));
    }
    out
}

fn short(id: &EndpointId) -> String {
    id.fmt_short().to_string()
}

/// How to make the daemon a user service on `os`.
fn service_howto(os: Option<&str>) -> Vec<String> {
    let mut out = vec!["To keep the daemon running after a reboot, make it a user service:".into()];
    let lines: &[&str] = match os {
        Some("macos") => &[
            "  put this in ~/Library/LaunchAgents/org.irsync.daemon.plist (with your home directory for $HOME):",
            "    <?xml version=\"1.0\" encoding=\"UTF-8\"?>",
            "    <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">",
            "    <plist version=\"1.0\"><dict>",
            "      <key>Label</key><string>org.irsync.daemon</string>",
            "      <key>ProgramArguments</key><array><string>$HOME/.local/share/irsync/bin/irsync</string><string>daemon</string></array>",
            "      <key>RunAtLoad</key><true/>",
            "      <key>KeepAlive</key><true/>",
            "    </dict></plist>",
            "  then run:",
            "    launchctl load -w ~/Library/LaunchAgents/org.irsync.daemon.plist",
        ],
        _ => &[
            "  put this in ~/.config/systemd/user/irsync.service:",
            "    [Unit]",
            "    Description=irsync daemon",
            "    [Service]",
            "    ExecStart=%h/.local/share/irsync/bin/irsync daemon",
            "    [Install]",
            "    WantedBy=default.target",
            "  then run:",
            "    systemctl --user enable --now irsync",
            "    loginctl enable-linger $USER",
        ],
    };
    out.extend(lines.iter().map(|l| l.to_string()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn id(b: u8) -> EndpointId {
        iroh::SecretKey::from_bytes(&[b; 32]).public()
    }

    fn onboarded(install: Install, daemon: DaemonOutcome, os: Option<&'static str>) -> Onboarded {
        Onboarded {
            alias: "myserver".into(),
            endpoint_id: id(2),
            previous_id: Some(id(3)),
            report: SetupReport {
                version: LONG_VERSION.clone(),
                endpoint_id: id(2),
                identity: Created::Existing,
                grant: GrantOutcome::Already,
                daemon,
                log: PathBuf::from("/s/daemon.log"),
            },
            install,
            os,
            client: id(1),
        }
    }

    #[test]
    fn replaced_restarted_on_macos() {
        let o = onboarded(
            Install::Copied {
                replaced: Some("irsync 0.0.1 (old)".into()),
            },
            DaemonOutcome::Restarted {
                old_version: "0.0.1 (old)".into(),
            },
            Some("macos"),
        );
        let s = summary(&o).join("\n");
        assert!(s.contains(&format!(
            "  replaced irsync 0.0.1 (old) with {} at ~/.local/share/irsync/bin/irsync",
            *LONG_VERSION
        )));
        assert!(s.contains("  restarted the daemon (was 0.0.1 (old))"));
        assert!(s.contains(&format!(
            "  updated host alias myserver: {} -> {}",
            id(3).fmt_short(),
            id(2).fmt_short()
        )));
        assert!(s.contains("org.irsync.daemon.plist"));
        assert!(s.contains("<key>KeepAlive</key>"));
        assert!(!s.contains("systemctl"));
        assert!(!s.contains("generated the daemon identity"));
        assert!(!s.contains("granted"));
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
        assert!(!s.join("\n").contains("user service"));
    }
}
