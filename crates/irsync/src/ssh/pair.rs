//! `irsync pair HOST`: which server a pairing is for.

use anyhow::bail;

use crate::config::clients::Clients;
use crate::config::servers::Servers;
use crate::identity::parse_endpoint_id;

/// What the host is to the saved config, before the pairing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairKind {
    /// Not saved: a first pairing; the server picks the names.
    First,
    /// Saved as a client only, with no server of that name: its daemon
    /// was never set up from here. The server is saved under the same
    /// name, beside the client.
    ClientOnly,
    /// Saved as a server: check it again, do not re-grant.
    Saved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairTarget {
    /// The name used from here on: the saved server when an address found
    /// one, else the name as typed.
    pub host: String,
    /// What to pass to ssh: the saved `ssh` value, else the name.
    pub dest: String,
    /// The name to save the server under, when it is known before the
    /// pairing: a saved server, or a client only (which keeps its name).
    pub alias: Option<String>,
    pub kind: PairKind,
    /// Set when an address found a saved server by its `ssh` value: what
    /// was typed.
    pub found_as: Option<String>,
}

/// Decides what `pair` does for `host` as typed. One trailing `:` is
/// dropped (`myserver:` is a habit from `HOST:`).
pub fn target(servers: &Servers, clients: &Clients, host: &str) -> anyhow::Result<PairTarget> {
    let typed = host.strip_suffix(':').unwrap_or(host);
    let is_id = parse_endpoint_id(typed).is_ok();
    // Not a name and not an id: a saved server may have been set up by
    // this very ssh destination (an address, say). Then it is that server.
    let by_ssh = if servers.get(typed).is_none() && clients.get(typed).is_none() && !is_id {
        match servers.by_ssh(typed) {
            Ok(alias) => alias.map(str::to_string),
            Err(aliases) => bail!(
                "{typed} is the ssh destination of several saved servers with \
                 different ids ({}); name one of them, or change an `ssh` \
                 value in {}",
                aliases.join(", "),
                servers.path.display()
            ),
        }
    } else {
        None
    };
    let found_as = by_ssh.as_ref().map(|_| typed.to_string());
    let name = by_ssh.as_deref().unwrap_or(typed);
    let saved = servers.get(name);
    if saved.is_none() && is_id {
        bail!("pair needs a server name or an ssh destination, not an EndpointId");
    }
    let kind = match saved {
        Some(_) => PairKind::Saved,
        None if clients.get(name).is_some() => PairKind::ClientOnly,
        None => PairKind::First,
    };
    let dest = saved
        .and_then(|h| h.ssh.clone())
        .unwrap_or_else(|| name.to_string());
    Ok(PairTarget {
        host: name.to_string(),
        dest,
        alias: (kind != PairKind::First).then(|| name.to_string()),
        kind,
        found_as,
    })
}

/// The last line of a pairing.
pub fn paired_line(alias: &str) -> String {
    format!("paired with {alias}; syncs go over iroh, not ssh")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> String {
        iroh::SecretKey::from_bytes(&[7; 32]).public().to_string()
    }

    fn target_in(toml: &str, host: &str) -> anyhow::Result<PairTarget> {
        let path = std::path::Path::new("irsync.toml");
        let c = crate::config::file::Config::parse(toml, path).unwrap();
        target(&c.servers, &c.clients, host)
    }

    #[test]
    fn unknown_name_is_a_first_pairing() {
        let t = target_in("", "myserver").unwrap();
        assert_eq!(t.dest, "myserver");
        assert_eq!(t.alias, None);
        assert_eq!(t.kind, PairKind::First);
        assert_eq!(t.found_as, None);
    }

    #[test]
    fn trailing_colon_is_dropped() {
        let t = target_in("", "myserver:").unwrap();
        assert_eq!(t.host, "myserver");
        assert_eq!(t.dest, "myserver");
    }

    #[test]
    fn saved_host_uses_its_ssh_value() {
        let h = format!(
            "config_version = 2\n[servers.myserver]\nendpoint_id = \"{}\"\nssh = \"mike@nas\"\n",
            id()
        );
        let t = target_in(&h, "myserver").unwrap();
        assert_eq!(t.dest, "mike@nas");
        assert_eq!(t.alias.as_deref(), Some("myserver"));
        assert_eq!(t.kind, PairKind::Saved);
    }

    #[test]
    fn address_finds_the_saved_host() {
        let h = format!(
            "config_version = 2\n[servers.myserver]\nendpoint_id = \"{}\"\nssh = \"mike@nas\"\n",
            id()
        );
        let t = target_in(&h, "mike@nas").unwrap();
        assert_eq!(t.host, "myserver");
        assert_eq!(t.found_as.as_deref(), Some("mike@nas"));
        assert_eq!(t.kind, PairKind::Saved);
    }

    #[test]
    fn client_only_entry_is_client_only() {
        let h = format!(
            "config_version = 2\n[clients.myserver]\nclient_id = \"{}\"\n",
            id()
        );
        let t = target_in(&h, "myserver").unwrap();
        assert_eq!(t.kind, PairKind::ClientOnly);
        assert_eq!(t.alias.as_deref(), Some("myserver"));
    }

    #[test]
    fn endpoint_id_is_refused() {
        let e = target_in("", &id()).unwrap_err().to_string();
        assert_eq!(
            e,
            "pair needs a server name or an ssh destination, not an EndpointId"
        );
    }
}
