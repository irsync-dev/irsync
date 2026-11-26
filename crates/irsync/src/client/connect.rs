//! Reaching a server: the configuration, the client identity and the
//! endpoint a sync, a listing or `remote` dials from.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use iroh::{EndpointAddr, EndpointId, TransportAddr};

use crate::config::clients::Clients;
use crate::config::dial::{Dial, DialFlags};
use crate::config::file::Config;
use crate::config::paths::{state_dir, Paths};
use crate::config::servers::{ServerEntry, Servers};
use crate::config::settings::{PqKeyExchange, Settings};
use crate::identity::{load_client_identity, parse_endpoint_id, KeySource, PassphraseSource};
use crate::net::{build_endpoint, client_endpoint_opts};
use crate::options::SyncOptions;
use crate::session::reconnect::{dial_once, never_reached, ReconnectOpts, PAIRED_FIRST_DIAL};

/// The daemon's EndpointId: a server's name, else the host string itself
/// must be an EndpointId. `clients` is only for the message about a name
/// that is a client's and no server's.
pub(crate) fn resolve_target(
    servers: &Servers,
    clients: &Clients,
    host: &str,
) -> anyhow::Result<EndpointId> {
    if let Some(entry) = servers.get(host) {
        return Ok(entry.endpoint_id);
    }
    if clients.get(host).is_some() {
        bail!(
            "{host} is saved in {} as a client only: it has no endpoint_id to dial",
            servers.path.display()
        );
    }
    parse_endpoint_id(host).map_err(|_| {
        let missing = if servers.path.exists() {
            ""
        } else {
            "; no such file"
        };
        anyhow::anyhow!(
            "{host:?} is not a server in {} and not an EndpointId{missing}",
            servers.path.display()
        )
    })
}

/// A client endpoint ready to dial `host`, plus what the session setup
/// still needs from the configuration.
pub struct Connected {
    pub endpoint: iroh::Endpoint,
    pub target: EndpointAddr,
    pub host_entry: Option<ServerEntry>,
    /// The settings of the dial, decided.
    pub dial: Dial,
    /// The saved alias `host_entry` is under: the host as typed, or the
    /// alias a first run saved it as.
    pub alias: Option<String>,
    /// `opts` with `parallel` and `state_dir` filled in from the config.
    pub opts: SyncOptions,
}

impl Connected {
    /// The name of a server `irsync pair` saved (one with an `ssh` value).
    pub fn paired(&self) -> Option<&str> {
        self.host_entry
            .as_ref()
            .filter(|h| h.ssh.is_some())
            .and(self.alias.as_deref())
    }

    /// The bound on each dial before the daemon has answered: see
    /// [`PAIRED_FIRST_DIAL`] for a paired server, else iroh's own.
    pub(crate) fn first_dial(&self) -> Option<std::time::Duration> {
        first_dial_limit(self.paired().map(|_| PAIRED_FIRST_DIAL))
    }

    /// How a session or a one-request exchange with this server dials and
    /// redials, from the dial's settings.
    pub(crate) fn reconnect_opts(&self) -> ReconnectOpts {
        ReconnectOpts {
            max: self.dial.reconnect,
            dial_retries: self.dial.dial_retries,
            first_dial_timeout: self.first_dial(),
            require_pq: self.dial.pq_key_exchange == PqKeyExchange::Require,
            ..ReconnectOpts::default()
        }
    }
}

/// Resolves `host`, the direct addresses, the client identity and the
/// endpoint, the same way for a sync, a listing and `remote`. `say`
/// prints a line for the user (above a live progress block).
pub async fn connect(
    host: &str,
    opts: &SyncOptions,
    say: &dyn Fn(&str),
) -> anyhow::Result<Connected> {
    let paths = Paths::resolve(opts.config.as_deref());
    let config = Config::load(&paths)?;
    let pass = PassphraseSource::from_env(
        opts.passphrase_file.clone(),
        config.settings.unsafe_config_allowed,
    );
    // An address that is the `ssh` value of a saved server finds that
    // server: no ssh is needed for that.
    let by_ssh = crate::ssh::pair::saved_by_ssh(&config.servers, &config.clients, host)?;
    if let Some(alias) = &by_ssh {
        say(&format!("irsync: {host} is saved as {alias}"));
    }
    let host = by_ssh.as_deref().unwrap_or(host);
    // A sync never sets a server up: it says how to.
    if config.servers.get(host).is_none() && parse_endpoint_id(host).is_err() {
        if config.clients.get(host).is_some() {
            bail!(crate::session::reconnect::client_only(host));
        }
        bail!(crate::session::reconnect::not_paired(host, &paths.config));
    }
    let Config {
        settings,
        servers,
        clients,
        ..
    } = config;
    let host_entry = servers.get(host).cloned();
    let target = resolve_target(&servers, &clients, host)?;
    let mut opts = opts.clone();
    opts.state_dir = Some(settings.state_dir.clone());
    let dial = Dial::resolve(&dial_flags(&opts)?, host_entry.as_ref(), &settings.defaults)
        .map_err(anyhow::Error::msg)?;
    opts.parallel = Some(dial.parallel);
    opts.idle_timeout = Some(dial.idle_timeout).filter(|t| !t.is_zero());
    // Any direct address means isolated mode: dial those and nothing else.
    // Resolved before the identity is loaded so a bad address never prompts
    // for a passphrase.
    let addresses = &dial.address;
    let mut resolved = Vec::new();
    for entry in addresses {
        let addrs = entry
            .resolve()
            .await
            .with_context(|| format!("resolving address {entry}"))?;
        if addrs.is_empty() {
            bail!("address {entry} resolved to no addresses");
        }
        resolved.extend(addrs);
    }
    let secret = load_client_key(&opts, &settings, &pass)?;
    if !addresses.is_empty() {
        tracing::info!(
            addresses = ?resolved,
            "isolated mode: dialling directly, relay and lookups off"
        );
    }
    let mut endpoint_opts = client_endpoint_opts(secret, &dial);
    if !addresses.is_empty() {
        // Bound to every interface, the client would offer the daemon all
        // of them for extra paths; bound to the source of the dial, only
        // that one.
        endpoint_opts.bind_addrs = crate::net::addr::source_binds(&resolved);
        tracing::debug!(binds = ?endpoint_opts.bind_addrs, "isolated mode: binding");
    }
    let endpoint = build_endpoint(endpoint_opts).await?;
    // Direct addresses replace everything; else the daemon's own report,
    // when there is one, goes along with the lookups.
    let target = if addresses.is_empty() {
        EndpointAddr::from_parts(target, Vec::<TransportAddr>::new())
    } else {
        EndpointAddr::from_parts(target, dial_addrs(resolved)?)
    };
    Ok(Connected {
        endpoint,
        target,
        alias: host_entry.is_some().then(|| host.to_string()),
        host_entry,
        dial,
        opts,
    })
}

/// What the command line says about a dial.
fn dial_flags(opts: &SyncOptions) -> anyhow::Result<DialFlags> {
    Ok(DialFlags {
        parallel: opts.parallel,
        reconnect: opts.reconnect,
        dial_retries: opts.dial_retries,
        relay: opts.relay.clone(),
        no_dht: opts.no_dht,
        no_dns: opts.no_dns,
        no_mdns: opts.no_mdns,
        pq_key_exchange: opts.pq_key_exchange,
        idle_timeout: opts.idle_timeout,
        address: opts.addresses.clone(),
    })
}

/// The bound on each dial until the server has answered once: `usual`
/// (`None`: iroh's own handshake timeout, about 60 s). A test-hooks build
/// takes `IRSYNC_TEST_DIAL_SECS` seconds instead, when that is set, so a
/// test of a host that does not answer need not wait the real time out.
fn first_dial_limit(usual: Option<std::time::Duration>) -> Option<std::time::Duration> {
    #[cfg(feature = "test-hooks")]
    if let Some(secs) = std::env::var("IRSYNC_TEST_DIAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
    {
        return Some(std::time::Duration::from_secs(secs));
    }
    usual
}

/// The client identity for a sync, with a stderr notice when it was generated.
fn load_client_key(
    opts: &SyncOptions,
    settings: &Settings,
    pass: &PassphraseSource,
) -> anyhow::Result<iroh::SecretKey> {
    client_key(opts.identity.as_deref(), settings, pass)
}

/// As [`load_client_key`], for the identity file named on the command line.
pub fn client_key(
    explicit: Option<&Path>,
    settings: &Settings,
    pass: &PassphraseSource,
) -> anyhow::Result<iroh::SecretKey> {
    let (secret, source) = client_identity(explicit, settings, pass)?;
    if let KeySource::Generated(path) = &source {
        eprintln!("irsync: generated identity {}", path.display());
        eprintln!("{}", crate::identity::public_key_line(&secret, "irsync"));
        eprintln!("{}", secret.public());
    }
    Ok(secret)
}

/// The transport addresses to dial: the fault layer's custom ones when it is on.
fn dial_addrs(resolved: Vec<std::net::SocketAddr>) -> anyhow::Result<Vec<TransportAddr>> {
    #[cfg(feature = "netsim")]
    if crate::net::netsim::active()?.is_some() {
        return resolved
            .into_iter()
            .map(|a| match a {
                std::net::SocketAddr::V4(v4) => {
                    Ok(TransportAddr::Custom(crate::net::netsim::custom_addr(v4)))
                }
                std::net::SocketAddr::V6(_) => bail!("netsim dials IPv4 addresses only, got {a}"),
            })
            .collect();
    }
    Ok(resolved.into_iter().map(TransportAddr::Ip).collect())
}

/// A throwaway state directory a listing swaps in for the configured one.
static CLIENT_STATE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// The client's state directory: a listing's throwaway one, else the
/// config's `state_dir`, else the default.
pub(super) fn client_state_dir(opts: &SyncOptions) -> PathBuf {
    CLIENT_STATE
        .get()
        .or(opts.state_dir.as_ref())
        .cloned()
        .unwrap_or_else(state_dir)
}

/// Swaps in `dir` for the configured state directory for the rest of the
/// process: a listing keeps its index in a throwaway one.
pub fn use_state_dir(dir: PathBuf) {
    let _ = CLIENT_STATE.set(dir);
}

/// The client identity: `explicit` (`--identity`) or the config's `identity`
/// when set, else the default key, else `~/.ssh/id_ed25519`, else a key
/// generated at the default path. Says where it came from.
pub fn client_identity(
    explicit: Option<&std::path::Path>,
    settings: &crate::config::settings::Settings,
    pass: &PassphraseSource,
) -> anyhow::Result<(iroh::SecretKey, KeySource)> {
    let (path, ssh) = match explicit {
        Some(p) => (p, None),
        None => (
            settings.identity.as_path(),
            settings.ssh_identity.as_deref(),
        ),
    };
    // Name the file actually read: with nothing at `path`, the ssh key.
    let key = crate::identity::client_key_path(path, ssh);
    let (secret, source) = load_client_identity(path, ssh, pass, "irsync")
        .with_context(|| format!("loading identity {}", key.display()))?;
    if let KeySource::Ssh(p) = &source {
        tracing::info!(path = %p.display(), "using the ssh key as the client identity");
    }
    Ok((secret, source))
}

/// Dials the daemon of `c` once, bounded as a first dial is, and checks the
/// post-quantum key exchange when it is required. For the one-shot exchanges
/// (a listing, `remote`); the caller closes the connection.
pub async fn dial(c: &Connected) -> anyhow::Result<iroh::endpoint::Connection> {
    let conn = dial_once(&c.endpoint, &c.target, c.first_dial())
        .await
        .map_err(|msg| never_reached(anyhow::anyhow!(msg), c.paired()))?;
    crate::session::reconnect::check_pq_key_exchange(
        &conn,
        c.dial.pq_key_exchange == PqKeyExchange::Require,
        &c.target.id,
    )?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A name saved as a client only cannot be dialled. A sync stops before
    /// this and names `irsync pair`, so this is the error for a config that
    /// changed in between.
    #[test]
    fn a_client_only_host_is_no_target() {
        let key = |n: u8| iroh::SecretKey::from_bytes(&[n; 32]).public();
        let path = Path::new("/c/irsync.toml");
        let text = format!(
            "[clients.phone]\nclient_id = \"{}\"\n\
             [servers.box]\nendpoint_id = \"{}\"\n[clients.box]\nclient_id = \"{}\"\n",
            key(1),
            key(2),
            key(3)
        );
        let Config {
            servers, clients, ..
        } = Config::parse(&text, path).unwrap();
        let target = |host: &str| resolve_target(&servers, &clients, host);
        let e = target("phone").unwrap_err().to_string();
        assert_eq!(
            e,
            "phone is saved in /c/irsync.toml as a client only: it has no endpoint_id to dial"
        );
        // The daemon's id is the one dialled, not the client's.
        assert_eq!(target("box").unwrap(), key(2));
        assert_eq!(target(&key(4).to_string()).unwrap(), key(4));
        assert!(target("nobody").is_err());
    }
}
