//! The client side of `irsync pair`: set the server up over ssh, save a
//! server entry for it, and say what was done. It never asks first; it
//! acts, then reports.

use anyhow::Context;
use iroh::EndpointId;

use super::client::{client_host_name, ensure_installed, probe, run_setup, Install, Rsh};
use super::payload::Contents;
use super::service::ServiceOutcome;
use super::setup::{Created, DaemonOutcome, Dht, GrantOutcome, SetupReport};
use crate::build::LONG_VERSION;
use crate::config::names::valid_alias;
use crate::config::paths::Paths;
use crate::manage::{self, servers::ServerFlags};
use irsync_proto::config as pb;

/// Why a root account is refused, on the client and by `ssh-setup` on the
/// server.
pub const ROOT_ACCOUNT: &str = "irsync does not set up a daemon as root";

/// Where the book explains running the daemon under its own account.
const RUN_THE_DAEMON_URL: &str = "https://irsync.org/book/how-to/run-the-daemon";

/// One server that onboarding wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedAlias {
    pub alias: String,
    /// The id it pointed at before, when it changed.
    pub previous_id: Option<EndpointId>,
    /// It was saved before this run (with `previous_id` or the same id).
    pub existed: bool,
    /// The `ssh` value it had and keeps, when this run reached the host by
    /// an address: a name, or another spelling of that address. The
    /// address typed goes into `ssh_aliases`.
    pub kept_ssh: Option<String>,
}

/// What the host given to `irsync pair` is, as an ssh destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestKind {
    /// An IP address, with or without `user@`.
    Ip {
        v6: bool,
    },
    /// `user@host`, with `host` not an IP address.
    UserHost {
        host: String,
    },
    Plain,
}

pub fn dest_kind(dest: &str) -> DestKind {
    let (user, host) = match dest.rsplit_once('@') {
        Some((_, h)) => (true, h),
        None => (false, dest),
    };
    // A link-local IPv6 address may carry a zone: `fe80::1%eth0`.
    let bare = host.split_once('%').map_or(host, |(a, _)| a);
    match bare.parse::<std::net::IpAddr>() {
        Ok(ip) => DestKind::Ip { v6: ip.is_ipv6() },
        Err(_) if user => DestKind::UserHost {
            host: host.to_string(),
        },
        Err(_) => DestKind::Plain,
    }
}

/// The address in `dest`, when it is one (see [`DestKind::Ip`]).
fn ip_of(dest: &str) -> Option<std::net::IpAddr> {
    let host = dest.rsplit_once('@').map_or(dest, |(_, h)| h);
    host.split_once('%').map_or(host, |(a, _)| a).parse().ok()
}

/// Whether a first run to `dest` can go ahead, before any ssh call: ssh
/// must not take `dest` for an option, and `dest` must be an IP address
/// or a name that can be saved as an alias.
pub fn check_dest(dest: &str) -> anyhow::Result<()> {
    if dest.starts_with('-') {
        anyhow::bail!("{dest:?} is not a host name");
    }
    if !matches!(dest_kind(dest), DestKind::Ip { .. }) && !valid_alias(dest) {
        return Err(manage::ManageError::BadAlias(dest.to_string()).into());
    }
    Ok(())
}

/// [`check_dest`], and after the probe: an IPv6 address cannot be an
/// alias, so the server must have given a host name (`server_name`) to
/// save it under. Checked before anything on the server is changed.
pub fn check_first(dest: &str, server_name: Option<&str>) -> anyhow::Result<()> {
    check_dest(dest)?;
    if dest_kind(dest) == (DestKind::Ip { v6: true }) && server_name.is_none() {
        anyhow::bail!(
            "{dest} gave no host name to save it under, and an IPv6 address \
             cannot be a server name.\n  Set a host name on that machine, or add \
             the server by hand with `irsync server add`"
        );
    }
    Ok(())
}

/// The aliases a first run to `dest` saves for the daemon `id`, the main
/// one first. An IP address goes under the server's host name
/// (`server_name`), with `-2`, `-3`, ... when that name belongs to another
/// daemon; without a name an IPv4 address is its own alias. `user@host`
/// also gets the plain `host` when no alias has that name yet (the first
/// one wins). `existing`: the id an alias is saved with.
pub fn choose_aliases(
    dest: &str,
    server_name: Option<&str>,
    id: EndpointId,
    existing: &dyn Fn(&str) -> Option<EndpointId>,
) -> Vec<String> {
    match (dest_kind(dest), server_name) {
        (DestKind::Ip { .. }, Some(name)) => {
            let free = |a: &str| existing(a).is_none_or(|old| old == id);
            let mut alias = name.to_string();
            let mut n = 2;
            while !free(&alias) {
                alias = format!("{name}-{n}");
                n += 1;
            }
            vec![alias]
        }
        (DestKind::UserHost { host }, _) if valid_alias(&host) && existing(&host).is_none() => {
            vec![dest.to_string(), host]
        }
        _ => vec![dest.to_string()],
    }
}

/// What onboarding `alias` did.
#[derive(Debug, Clone)]
pub struct Onboarded {
    pub alias: String,
    /// The server daemon's EndpointId, now saved under `alias`.
    pub endpoint_id: EndpointId,
    /// Every alias this run saved or checked, `alias` first.
    pub saved: Vec<SavedAlias>,
    pub report: SetupReport,
    pub install: Install,
    /// The server's OS (`std::env::consts::OS` names), from the probe.
    pub os: Option<&'static str>,
    /// This client's EndpointId, the one `ssh-setup` granted.
    pub client: EndpointId,
    /// This client's host name, the name the server saved it under.
    pub client_name: String,
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

/// What an onboarding run is asked to do beyond the defaults.
#[derive(Debug, Clone, Copy, Default)]
pub struct SetupFlags {
    /// `alias` is a saved host being checked, so `ssh-setup` must not add
    /// back a removed grant.
    pub recheck: bool,
    /// Turn the daemon's DHT publishing off.
    pub no_dht: bool,
    /// Copy nothing to the server: use the irsync already there.
    pub no_install: bool,
    /// Start the daemon in the background, not as a user service.
    pub no_service: bool,
}

/// Sets up `dest` (an ssh destination) for `client`: probe, install this
/// build, run `ssh-setup`, then save the host with `ssh = dest`.
/// `alias`: a saved host being checked, the only entry updated; `None` on
/// a first run, which saves what [`choose_aliases`] says. `flags` says
/// what else to do.
pub fn onboard(
    paths: &Paths,
    rsh: &Rsh,
    alias: Option<&str>,
    dest: &str,
    client: EndpointId,
    flags: SetupFlags,
) -> anyhow::Result<Onboarded> {
    match alias {
        Some(a) => check(a, dest)?,
        None => check_dest(dest)?,
    }
    // The server knows this client by its host name: no name, no setup.
    let client_name = client_host_name()?;
    let p = probe(rsh, dest)?;
    // Before anything is installed or changed on the server.
    if p.root {
        anyhow::bail!(
            "the ssh account on {dest} is root, and {ROOT_ACCOUNT}.\n  \
             Use another account, or set the daemon up by hand:\n  \
             {RUN_THE_DAEMON_URL}"
        );
    }
    if alias.is_none() {
        check_first(dest, p.hostname.as_deref())?;
    }
    let install = ensure_installed(rsh, dest, &p, flags.no_install)?;
    // The setup is run by the irsync that was just put there, so a setup
    // that fails cannot leave the server without that install: say so.
    let report = run_setup(rsh, dest, client, &client_name, flags).map_err(|e| {
        if install == Install::Current {
            e
        } else {
            anyhow::anyhow!("{e:#}\n  irsync was installed on {dest} before this; that stays")
        }
    })?;
    let mut lock = manage::lock_config(&paths.config)?;
    let mut msg = lock.load()?;
    let id = report.endpoint_id;
    let aliases = match alias {
        Some(a) => vec![a.to_string()],
        None => choose_aliases(dest, p.hostname.as_deref(), id, &|a| saved_id(&msg, a)),
    };
    let typed_ip = ip_of(dest);
    let mut saved = Vec::new();
    for alias in aliases {
        // Saved with a daemon id before: an entry that was a client only
        // is saved as a host now.
        let before = saved_id(&msg, &alias);
        let existed = before.is_some();
        // An address goes stale, a name does not: the same daemon, saved
        // with a name to reach it by, keeps that name. It also keeps
        // another spelling of this address (`u@1.2.3.4` for `1.2.3.4`).
        // Only an address that differs is replaced: the old one is stale.
        let kept_ssh = (typed_ip.is_some() && before == Some(id))
            .then(|| saved_ssh(&msg, &alias))
            .flatten()
            .filter(|old| old != dest && ip_of(old).is_none_or(|ip| Some(ip) == typed_ip));
        let server_flags = ServerFlags {
            ssh: kept_ssh.is_none().then(|| dest.to_string()),
            ..ServerFlags::default()
        };
        let previous_id = manage::servers::server_add(&mut msg, &alias, id, &server_flags)
            .with_context(|| format!("saving server {alias}"))?;
        if kept_ssh.is_some() {
            // So that typing this address again finds the host without ssh.
            manage::servers::server_add_ssh_alias(&mut msg, &alias, dest);
        }
        saved.push(SavedAlias {
            alias,
            previous_id,
            existed,
            kept_ssh,
        });
    }
    // A check that found a new daemon (the server was rebuilt): the other
    // names saved for that daemon through this ssh destination follow.
    if let (Some(_), Some(old)) = (alias, saved[0].previous_id) {
        for other in aliases_of(&msg, old, dest) {
            manage::servers::server_add(&mut msg, &other, id, &ServerFlags::default())
                .with_context(|| format!("saving server {other}"))?;
            saved.push(SavedAlias {
                alias: other,
                previous_id: Some(old),
                existed: true,
                kept_ssh: None,
            });
        }
    }
    lock.save(&mut msg)?;
    Ok(Onboarded {
        alias: saved[0].alias.clone(),
        endpoint_id: id,
        saved,
        install,
        os: p.os,
        client,
        client_name,
        report,
    })
}

/// The server `alias` of `msg`.
fn saved_host<'a>(msg: &'a pb::Config, alias: &str) -> Option<&'a pb::ServerEntry> {
    msg.servers.iter().find(|s| s.name == alias)
}

/// The id `[servers.alias]` holds in `msg`.
fn saved_id(msg: &pb::Config, alias: &str) -> Option<EndpointId> {
    manage::id_of(&saved_host(msg, alias)?.endpoint_id)
}

/// The names of the servers in `msg` saved with the daemon `id` and
/// `ssh = dest`.
fn aliases_of(msg: &pb::Config, id: EndpointId, dest: &str) -> Vec<String> {
    msg.servers
        .iter()
        .filter(|s| manage::id_of(&s.endpoint_id) == Some(id) && s.ssh.as_deref() == Some(dest))
        .map(|s| s.name.clone())
        .collect()
}

/// The `ssh` value `[servers.alias]` holds in `msg`.
fn saved_ssh(msg: &pb::Config, alias: &str) -> Option<String> {
    saved_host(msg, alias)?.ssh.clone()
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
    let dir = "~/.local/bin";
    let version = &*LONG_VERSION;
    let r = &o.report;
    let mut out = vec![format!("set up {dest} over ssh:")];
    let (replaced, note, contents) = match &o.install {
        Install::Current => (None, String::new(), None),
        Install::Copied { replaced, contents } => (Some(replaced), String::new(), Some(contents)),
        Install::Downloaded {
            replaced,
            platform,
            contents,
        } => (
            Some(replaced),
            format!(" (release files for {platform})"),
            Some(contents),
        ),
    };
    if let (Some(replaced), Some(contents)) = (replaced, contents) {
        let what = contents_phrase(contents, &note);
        match replaced {
            None => out.push(format!("  installed irsync {version} in {dir}{what}")),
            Some(old) => {
                let old = old.strip_prefix("irsync ").unwrap_or(old);
                out.push(format!(
                    "  replaced irsync {old} with {version} in {dir}{what}"
                ));
            }
        }
    }
    if r.identity == Created::Generated {
        out.push("  generated the daemon identity".into());
    }
    match r.grant {
        GrantOutcome::Added => out.push(format!(
            "  granted this client ({}, {}) access to any path",
            o.client_name,
            short(&o.client)
        )),
        GrantOutcome::Already => {}
        GrantOutcome::Missing => out.push(format!(
            "  this client's ({}) all_paths on {dest} was lowered or removed; \
             it is left as it is",
            short(&o.client)
        )),
        GrantOutcome::AllPathsOff => out.push(format!(
            "  allow_all_paths is off on {dest}: only exports granted to this client are reachable"
        )),
    }
    if r.dht == Dht::TurnedOff {
        out.push(format!(
            "  turned off DHT publishing on {dest} ([daemon] dht = false)"
        ));
    }
    match &r.daemon {
        DaemonOutcome::Running => {}
        DaemonOutcome::Reloaded => {
            out.push("  told the daemon to reload its settings".into());
        }
        // A service's output goes to the service manager, not to that log.
        DaemonOutcome::Started
            if matches!(r.service, ServiceOutcome::Systemd | ServiceOutcome::Launchd) =>
        {
            out.push("  started the daemon".into());
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
            out.push(format!(
                "  server {dest} is saved with the daemon identity of this config; if that \
                 daemon uses another config, syncs may fail"
            ));
        }
    }
    if let Some(line) = service_line(&r.service, &r.log) {
        out.push(line);
    }
    for a in &o.saved {
        let alias = &a.alias;
        match a.previous_id {
            None if a.existed => {
                // Already saved with this id (a restart): nothing to say,
                // unless the host was reached another way than it is saved.
                if let Some(ssh) = &a.kept_ssh {
                    out.push(format!("  server {alias} keeps ssh = {ssh}"));
                }
            }
            None => out.push(format!(
                "  saved server {alias} -> {}",
                short(&o.endpoint_id)
            )),
            Some(old) => out.push(format!(
                "  updated server {alias}: {} -> {}",
                short(&old),
                short(&o.endpoint_id)
            )),
        }
    }
    out
}

/// How the daemon runs, after the daemon line; none when the service was
/// not touched. `log`: where a launchd agent's output goes.
fn service_line(s: &ServiceOutcome, log: &std::path::Path) -> Option<String> {
    const UNTIL: &str = "the daemon runs in the background until the server reboots";
    Some(match s {
        ServiceOutcome::Systemd => "  the daemon runs as a systemd user service (irsync.service), \
                                    also after a reboot"
            .into(),
        ServiceOutcome::Launchd => format!(
            "  the daemon runs as a launchd agent (org.irsync.daemon, log: {}); after a \
             reboot it starts when you log in on the Mac",
            log.display()
        ),
        ServiceOutcome::Skipped => format!("  {UNTIL} (--no-service)"),
        ServiceOutcome::Unavailable => {
            format!("  no systemd or launchd user session there: {UNTIL}")
        }
        ServiceOutcome::Failed(e) => format!(
            "  warning: could not make the daemon a user service: {}; it runs in the \
             background until the server reboots",
            e.replace(['\r', '\n'], " ")
        ),
        ServiceOutcome::Foreign(p) => format!(
            "  {} was not written by irsync; left it alone. The daemon runs in the \
             background until the server reboots",
            home_tilde(p)
        ),
        ServiceOutcome::NotTouched => return None,
    })
}

/// What an install put beside `irsync`, as the tail of the install line:
/// `, with fileset and the manual pages` and so on, then `note`.
fn contents_phrase(c: &Contents, note: &str) -> String {
    const WHY: &str = "this machine has none beside its irsync";
    match (c.fileset, c.man) {
        (true, true) => format!(", with fileset and the manual pages{note}"),
        (true, false) => format!(", with fileset (no manual pages: {WHY}){note}"),
        (false, true) => format!(", with the manual pages (no fileset: {WHY}){note}"),
        (false, false) => format!(" (no fileset, no manual pages: {WHY}){note}"),
    }
}

/// A service file's path with the home part shown as `~`. The two files
/// the setup looks at have fixed places under the home directory.
fn home_tilde(path: &std::path::Path) -> String {
    let p = path.to_string_lossy();
    for tail in [
        "/.config/systemd/user/irsync.service",
        "/Library/LaunchAgents/org.irsync.daemon.plist",
    ] {
        if p.ends_with(tail) {
            return format!("~{tail}");
        }
    }
    p.into_owned()
}

fn short(id: &EndpointId) -> String {
    id.fmt_short().to_string()
}

/// After a daemon was started or restarted in the background because
/// irsync could not make it a user service: how to do that by hand on the
/// server's OS. Printed without a prefix so it can be pasted. Empty
/// otherwise.
pub fn howto(o: &Onboarded) -> Vec<String> {
    let by_hand = matches!(
        o.report.service,
        ServiceOutcome::Unavailable | ServiceOutcome::Failed(_) | ServiceOutcome::Foreign(_)
    );
    if !by_hand
        || !matches!(
            o.report.daemon,
            DaemonOutcome::Started | DaemonOutcome::Restarted { .. }
        )
    {
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
                "  <key>ProgramArguments</key><array><string>$HOME/.local/bin/irsync</string><string>daemon</string><string>-vv</string></array>",
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
                "ExecStart=%h/.local/bin/irsync daemon -vv",
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

    #[test]
    fn chooses_aliases() {
        let none = |_: &str| None;
        let pick = |dest, name, ex: &dyn Fn(&str) -> Option<EndpointId>| {
            choose_aliases(dest, name, id(1), ex)
        };
        assert_eq!(pick("myserver", None, &none), ["myserver"]);
        assert_eq!(
            pick("mike@myserver", Some("x"), &none),
            ["mike@myserver", "myserver"]
        );
        // First one wins: plain name taken, by anyone.
        let taken = |a: &str| (a == "myserver").then(|| id(2));
        assert_eq!(pick("bob@myserver", None, &taken), ["bob@myserver"]);
        let own = |a: &str| (a == "myserver").then(|| id(1));
        assert_eq!(pick("bob@myserver", None, &own), ["bob@myserver"]);
        // IP addresses take the server's name.
        assert_eq!(pick("192.168.1.5", Some("myserver"), &none), ["myserver"]);
        assert_eq!(
            pick("mike@192.168.1.5", Some("myserver"), &none),
            ["myserver"]
        );
        assert_eq!(pick("fe80::1", Some("myserver"), &none), ["myserver"]);
        assert_eq!(pick("fe80::1%eth0", Some("myserver"), &none), ["myserver"]);
        assert_eq!(pick("192.168.1.5", Some("myserver"), &own), ["myserver"]);
        assert_eq!(
            pick("192.168.1.5", Some("myserver"), &taken),
            ["myserver-2"]
        );
        let two = |a: &str| match a {
            "myserver" => Some(id(2)),
            "myserver-2" => Some(id(1)),
            _ => None,
        };
        assert_eq!(pick("192.168.1.5", Some("myserver"), &two), ["myserver-2"]);
        assert_eq!(pick("192.168.1.5", None, &none), ["192.168.1.5"]);
    }

    #[test]
    fn first_run_checks() {
        assert!(check_first("192.168.1.5", None).is_ok());
        assert!(check_first("fe80::1", Some("myserver")).is_ok());
        let e = check_first("mike@fe80::1", None).unwrap_err().to_string();
        assert!(e.contains("host name"), "{e}");
        assert!(check_first("-oProxyCommand=x", None).is_err());
        assert!(check_first("all", None).is_err());
    }

    /// One line for each alias saved or changed; none for one that was
    /// already saved with this id.
    #[test]
    fn summary_lists_every_saved_alias() {
        let mut o = onboarded(Install::Current, DaemonOutcome::Running, Some("linux"));
        o.alias = "u@myserver".into();
        o.saved = vec![
            SavedAlias {
                alias: "u@myserver".into(),
                previous_id: None,
                existed: true,
                kept_ssh: None,
            },
            SavedAlias {
                alias: "myserver".into(),
                previous_id: None,
                existed: false,
                kept_ssh: None,
            },
        ];
        let s = summary(&o).join("\n");
        assert!(!s.contains("alias u@myserver"), "{s}");
        assert!(
            s.contains(&format!("  saved server myserver -> {}", id(2).fmt_short())),
            "{s}"
        );
    }

    fn onboarded(install: Install, daemon: DaemonOutcome, os: Option<&'static str>) -> Onboarded {
        Onboarded {
            alias: "myserver".into(),
            endpoint_id: id(2),
            saved: vec![SavedAlias {
                alias: "myserver".into(),
                previous_id: Some(id(3)),
                existed: true,
                kept_ssh: None,
            }],
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
                // Says nothing, so each test adds only the lines it checks.
                service: ServiceOutcome::NotTouched,
            },
            install,
            os,
            client: id(1),
            client_name: "myclient".into(),
        }
    }

    const ALL: Contents = Contents {
        fileset: true,
        man: true,
    };

    #[test]
    fn downloaded_says_which_platform() {
        let o = onboarded(
            Install::Downloaded {
                replaced: None,
                platform: "linux aarch64".into(),
                contents: ALL,
            },
            DaemonOutcome::Started,
            Some("linux"),
        );
        let s = summary(&o);
        let want = format!(
            "  installed irsync {} in ~/.local/bin, with fileset and the manual pages (release files for linux aarch64)",
            *LONG_VERSION
        );
        assert!(s.contains(&want), "{s:?}");
        let o = onboarded(
            Install::Downloaded {
                replaced: Some("irsync 0.0.1 (old)".into()),
                platform: "linux aarch64".into(),
                contents: ALL,
            },
            DaemonOutcome::Started,
            Some("linux"),
        );
        let s = summary(&o);
        let want = format!(
            "  replaced irsync 0.0.1 (old) with {} in ~/.local/bin, with fileset and the manual pages (release files for linux aarch64)",
            *LONG_VERSION
        );
        assert!(s.contains(&want), "{s:?}");
    }

    /// A copy of this machine's files says what it left out, and why.
    #[test]
    fn copied_says_what_is_missing() {
        let why = "this machine has none beside its irsync";
        let v = &*LONG_VERSION;
        for (contents, want) in [
            (ALL, format!("  installed irsync {v} in ~/.local/bin, with fileset and the manual pages")),
            (ALONE, format!("  installed irsync {v} in ~/.local/bin (no fileset, no manual pages: {why})")),
            (
                Contents { fileset: true, man: false },
                format!("  installed irsync {v} in ~/.local/bin, with fileset (no manual pages: {why})"),
            ),
            (
                Contents { fileset: false, man: true },
                format!("  installed irsync {v} in ~/.local/bin, with the manual pages (no fileset: {why})"),
            ),
        ] {
            let o = onboarded(
                Install::Copied { replaced: None, contents },
                DaemonOutcome::Started,
                Some("linux"),
            );
            let s = summary(&o);
            assert!(s.contains(&want), "{want}\n{s:#?}");
        }
    }

    #[test]
    fn replaced_restarted_on_macos() {
        let mut o = onboarded(
            Install::Copied {
                replaced: Some("irsync 0.0.1 (old)".into()),
                contents: ALL,
            },
            DaemonOutcome::Restarted {
                old_version: "0.0.1 (old)".into(),
            },
            Some("macos"),
        );
        o.report.service = ServiceOutcome::Unavailable;
        let s = summary(&o).join("\n");
        assert!(s.contains(&format!(
            "  replaced irsync 0.0.1 (old) with {} in ~/.local/bin, with fileset and the manual pages",
            *LONG_VERSION
        )));
        assert!(s.contains("  restarted the daemon (was 0.0.1 (old))"));
        assert!(s.contains(&format!(
            "  updated server myserver: {} -> {}",
            id(3).fmt_short(),
            id(2).fmt_short()
        )));
        assert!(!s.contains("generated the daemon identity"));
        assert!(!s.contains("granted"));
        let h = howto(&o);
        let text = h.join("\n");
        assert!(text.contains("org.irsync.daemon.plist"));
        assert!(text.contains("<key>KeepAlive</key>"));
        assert!(!text.contains("systemctl"));
        // The plist body starts at column 0 so it can be pasted as a file.
        assert!(h.iter().any(|l| l.starts_with("<?xml ")));
        at(&h, "  <key>ProgramArguments</key><array><string>$HOME/.local/bin/irsync</string><string>daemon</string><string>-vv</string></array>");
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
        o.report.service = ServiceOutcome::Unavailable;
        o.report.log = PathBuf::from("/home/u/.local/state/irsync/daemon.log");
        let h = howto(&o);
        assert_eq!(
            h[0],
            "To keep the daemon running after a reboot, make it a user service."
        );
        assert_eq!(h[1], "Put this in ~/.config/systemd/user/irsync.service:");
        at(&h, "ExecStart=%h/.local/bin/irsync daemon -vv");
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
        assert!(
            s[2].starts_with("  server myserver is saved with the daemon identity of this config")
        );
        assert!(!s.join("\n").contains("installed"));
        assert!(howto(&o).is_empty());
    }

    /// `pair --server-no-dht` on a running daemon: the setting line, then the
    /// reload; no how-to, since the daemon kept running.
    #[test]
    fn dht_turned_off_and_reloaded() {
        let mut o = onboarded(Install::Current, DaemonOutcome::Reloaded, Some("linux"));
        o.report.dht = Dht::TurnedOff;
        let s = summary(&o);
        let off = at(
            &s,
            "  turned off DHT publishing on myserver ([daemon] dht = false)",
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
        o.saved[0].previous_id = None;
        o.saved[0].existed = false;
        let s = summary(&o).join("\n");
        assert!(
            s.contains(&format!("  saved server myserver -> {}", id(2).fmt_short())),
            "{s}"
        );
        o.saved[0].existed = true;
        let s = summary(&o);
        let all = s.join("\n");
        assert!(
            !all.contains("saved server") && !all.contains("updated server"),
            "{s:#?}"
        );
        assert_eq!(
            s.last().unwrap(),
            "  started the daemon (log: /s/daemon.log)"
        );
        o.saved[0].previous_id = Some(id(3));
        let s = summary(&o).join("\n");
        assert!(
            s.contains(&format!(
                "  updated server myserver: {} -> {}",
                id(3).fmt_short(),
                id(2).fmt_short()
            )),
            "{s}"
        );
    }

    /// One line after the daemon line says how the daemon runs.
    #[test]
    fn summary_names_the_service() {
        let mut o = onboarded(Install::Current, DaemonOutcome::Started, Some("linux"));
        o.saved[0].existed = true;
        o.saved[0].previous_id = None;
        for (service, want) in [
            (
                ServiceOutcome::Systemd,
                Some("  the daemon runs as a systemd user service (irsync.service), also after a reboot".to_string()),
            ),
            (
                ServiceOutcome::Launchd,
                Some("  the daemon runs as a launchd agent (org.irsync.daemon, log: /s/daemon.log); after a reboot it starts when you log in on the Mac".to_string()),
            ),
            (
                ServiceOutcome::Skipped,
                Some("  the daemon runs in the background until the server reboots (--no-service)".to_string()),
            ),
            (
                ServiceOutcome::Unavailable,
                Some("  no systemd or launchd user session there: the daemon runs in the background until the server reboots".to_string()),
            ),
            (
                ServiceOutcome::Failed("linger: access denied".into()),
                Some("  warning: could not make the daemon a user service: linger: access denied; it runs in the background until the server reboots".to_string()),
            ),
            (
                ServiceOutcome::Foreign(PathBuf::from("/home/u/.config/systemd/user/irsync.service")),
                Some("  ~/.config/systemd/user/irsync.service was not written by irsync; left it alone. The daemon runs in the background until the server reboots".to_string()),
            ),
            (ServiceOutcome::NotTouched, None),
        ] {
            o.report.service = service.clone();
            let s = summary(&o);
            // A service's output does not go to the log.
            let daemon = match service {
                ServiceOutcome::Systemd | ServiceOutcome::Launchd => at(&s, "  started the daemon"),
                _ => at(&s, "  started the daemon (log: /s/daemon.log)"),
            };
            match want {
                Some(want) => {
                    assert_eq!(s.len(), daemon + 2, "{s:#?}");
                    assert_eq!(s[daemon + 1], want);
                }
                None => assert_eq!(s.len(), daemon + 1, "{s:#?}"),
            }
        }
    }

    /// A multi-line error is one line of the summary.
    #[test]
    fn failed_service_error_is_one_line() {
        let mut o = onboarded(Install::Current, DaemonOutcome::Started, Some("linux"));
        o.report.service = ServiceOutcome::Failed("first\nsecond".into());
        at(
            &summary(&o),
            "  warning: could not make the daemon a user service: first second; it runs in the background until the server reboots",
        );
    }

    /// The manual steps only when irsync could not make the service itself.
    #[test]
    fn howto_only_without_a_service() {
        let mut o = onboarded(Install::Current, DaemonOutcome::Started, Some("linux"));
        for (service, steps) in [
            (ServiceOutcome::Systemd, false),
            (ServiceOutcome::Launchd, false),
            (ServiceOutcome::Skipped, false),
            (ServiceOutcome::NotTouched, false),
            (ServiceOutcome::Unavailable, true),
            (ServiceOutcome::Failed("no".into()), true),
            (
                ServiceOutcome::Foreign(PathBuf::from("/h/.config/systemd/user/irsync.service")),
                true,
            ),
        ] {
            o.report.service = service.clone();
            assert_eq!(!howto(&o).is_empty(), steps, "{service:?}");
        }
    }
}
