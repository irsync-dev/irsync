//! The long-running side: accepts connections, checks the peer's key before reading
//! anything, resolves and validates the requested module, then hands off to a session.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use iroh::endpoint::{Connection, Incoming, SendStream};
use iroh::{Endpoint, SecretKey};
use irsync_proto::chunk::CHUNK_PARAMS;
use irsync_proto::v1::{error::Code, hello::Direction, Error, Hello};
use irsync_proto::PROTOCOL_VERSION;
use tokio_util::sync::CancellationToken;

use crate::authz::{Authorizer, FileAuthorizer, Grants};
use crate::config::daemon::{DaemonConfig, Module, RelayMode};
use crate::fs::paths::is_contained;
use crate::identity::{generate_openssh, load_secret_key};
use crate::index::{Index, IndexHandle};
use crate::net::frame::{read_msg, write_msg};
use crate::net::{build_endpoint, EndpointOpts, RelayChoice};
use crate::session::engine::serve_daemon;

const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

struct Shared {
    authz: Arc<dyn Authorizer>,
    modules: BTreeMap<String, Module>,
    indexes: HashMap<String, IndexHandle>,
    #[cfg(feature = "test-hooks")]
    hellos: Arc<std::sync::atomic::AtomicU64>,
    /// Finished sessions, and files their hashers read.
    #[cfg(feature = "test-hooks")]
    sessions: Arc<std::sync::atomic::AtomicU64>,
    #[cfg(feature = "test-hooks")]
    hashed: Arc<std::sync::atomic::AtomicU64>,
}

pub struct Daemon {
    endpoint: Endpoint,
    shared: Arc<Shared>,
}

impl Daemon {
    /// Opens one index per module (under `cfg.state_dir`) and wraps `endpoint`.
    pub fn new(
        cfg: DaemonConfig,
        authz: Arc<dyn Authorizer>,
        endpoint: Endpoint,
    ) -> Result<Daemon> {
        let mut indexes = HashMap::new();
        for (name, m) in &cfg.modules {
            let ix = Index::open(&cfg.state_dir, &m.path)
                .with_context(|| format!("open index for module {name}"))?;
            indexes.insert(name.clone(), ix.into_handle());
        }
        Ok(Daemon {
            endpoint,
            shared: Arc::new(Shared {
                authz,
                modules: cfg.modules,
                indexes,
                #[cfg(feature = "test-hooks")]
                hellos: Default::default(),
                #[cfg(feature = "test-hooks")]
                sessions: Default::default(),
                #[cfg(feature = "test-hooks")]
                hashed: Default::default(),
            }),
        })
    }

    /// Counter of successfully decoded Hello messages.
    #[cfg(feature = "test-hooks")]
    pub fn hello_counter(&self) -> Arc<std::sync::atomic::AtomicU64> {
        self.shared.hellos.clone()
    }

    /// Counters of finished sessions and of files their hashers read. The
    /// session counter moves after the hash counter is updated.
    #[cfg(feature = "test-hooks")]
    pub fn session_counters(
        &self,
    ) -> (
        Arc<std::sync::atomic::AtomicU64>,
        Arc<std::sync::atomic::AtomicU64>,
    ) {
        (self.shared.sessions.clone(), self.shared.hashed.clone())
    }

    pub async fn run(self, shutdown: CancellationToken) -> Result<()> {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                inc = self.endpoint.accept() => {
                    let Some(inc) = inc else { break };
                    let shared = self.shared.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle(shared, inc).await {
                            tracing::warn!("connection failed: {e:#}");
                        }
                    });
                }
            }
        }
        self.endpoint.close().await;
        Ok(())
    }
}

async fn handle(shared: Arc<Shared>, incoming: Incoming) -> Result<()> {
    let conn = incoming.await.context("handshake")?;
    // Authorise on the verified key before touching any stream.
    let id = conn.remote_id();
    let grants = match shared.authz.grants(&id) {
        Some(g) if !g.is_empty() => g,
        _ => {
            tracing::info!(peer = %id.fmt_short(), "rejecting unauthorized key");
            conn.close(1u32.into(), b"unauthorized");
            return Ok(());
        }
    };
    let first = tokio::time::timeout(HELLO_TIMEOUT, async {
        let (send, mut recv) = conn.accept_bi().await.context("accept control stream")?;
        let hello: Hello = read_msg(&mut recv)
            .await
            .context("read Hello")?
            .context("peer closed before Hello")?;
        anyhow::Ok((send, recv, hello))
    })
    .await;
    let Ok(first) = first else {
        conn.close(2u32.into(), b"hello timeout");
        anyhow::bail!("timed out waiting for Hello");
    };
    let (mut send, recv, hello) = first?;
    #[cfg(feature = "test-hooks")]
    shared
        .hellos
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    match validate(&shared, &grants, &hello) {
        Ok(module) => {
            let root = module.path.clone();
            let index = shared
                .indexes
                .get(&hello.module)
                .context("module has no index")?
                .clone();
            let res = serve_daemon(conn, send, recv, root, hello, index).await;
            #[cfg(feature = "test-hooks")]
            {
                use std::sync::atomic::Ordering::SeqCst;
                if let Ok(o) = &res {
                    shared.hashed.fetch_add(o.files_hashed, SeqCst);
                }
                shared.sessions.fetch_add(1, SeqCst);
            }
            res.map(|_| ())
        }
        Err((code, message)) => {
            tracing::info!(peer = %id.fmt_short(), ?code, %message, "rejecting session");
            reject(&conn, &mut send, code, message).await
        }
    }
}

fn validate<'a>(
    shared: &'a Shared,
    grants: &Grants,
    hello: &Hello,
) -> Result<&'a Module, (Code, String)> {
    if hello.protocol_version != PROTOCOL_VERSION {
        return Err((
            Code::VersionMismatch,
            format!(
                "protocol version {} unsupported (daemon speaks {PROTOCOL_VERSION})",
                hello.protocol_version
            ),
        ));
    }
    if (hello.chunk_min, hello.chunk_avg, hello.chunk_max)
        != (CHUNK_PARAMS.min, CHUNK_PARAMS.avg, CHUNK_PARAMS.max)
    {
        return Err((Code::VersionMismatch, "chunk params mismatch".into()));
    }
    let Some(module) = shared.modules.get(&hello.module) else {
        return Err((
            Code::NoSuchModule,
            format!("no such module {:?}", hello.module),
        ));
    };
    if !is_contained(&module.path, &hello.path) {
        return Err((Code::BadPath, "path escapes module".into()));
    }
    match Direction::try_from(hello.direction) {
        Ok(Direction::ClientSends) => {
            if module.read_only || !grants.allows(&hello.module, true) {
                return Err((Code::ReadOnly, "module is read-only for this key".into()));
            }
        }
        Ok(Direction::DaemonSends) => {
            if !grants.allows(&hello.module, false) {
                return Err((Code::Unauthorized, "no access to module".into()));
            }
        }
        // No dedicated code for a malformed direction; UNKNOWN is the honest answer.
        Err(_) => return Err((Code::Unknown, "bad direction".into())),
    }
    Ok(module)
}

async fn reject(
    conn: &Connection,
    send: &mut SendStream,
    code: Code,
    message: String,
) -> Result<()> {
    write_msg(
        send,
        &Error {
            code: code as i32,
            message,
        },
    )
    .await?;
    send.finish()?;
    // Wait (bounded) for the peer to read the error before closing the connection.
    let _ = tokio::time::timeout(Duration::from_secs(5), send.stopped()).await;
    conn.close(2u32.into(), b"rejected");
    Ok(())
}

fn load_or_generate_identity(path: &Path) -> Result<SecretKey> {
    if !path.exists() {
        tracing::info!(path = %path.display(), "generating daemon identity");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        return Ok(generate_openssh(path, "irsync-daemon")?);
    }
    Ok(load_secret_key(path, None)?)
}

/// The daemon's EndpointId as a string, generating the identity if missing.
pub fn show_id(cfg: &DaemonConfig) -> Result<String> {
    Ok(load_or_generate_identity(&cfg.identity)?
        .public()
        .to_string())
}

pub async fn run_from_config(cfg: DaemonConfig, shutdown: CancellationToken) -> Result<()> {
    let secret = load_or_generate_identity(&cfg.identity)?;
    let authz = Arc::new(FileAuthorizer::load(&cfg.authorized_keys)?);
    let relay = match (&cfg.relay_mode, &cfg.relay) {
        (RelayMode::Disabled, _) => RelayChoice::Disabled,
        (_, Some(url)) => RelayChoice::Custom(url.clone()),
        _ => RelayChoice::N0Default,
    };
    if cfg.discovery.dht && relay == RelayChoice::Disabled {
        tracing::warn!(
            "dht is enabled but relays are disabled: DHT records will carry no addresses"
        );
    }
    let endpoint = build_endpoint(EndpointOpts {
        secret,
        relay,
        dht: cfg.discovery.dht,
        dns: cfg.discovery.dns,
        mdns: cfg.discovery.mdns,
        publish: true,
        bind_addr: None,
    })
    .await?;
    println!("endpoint id: {}", endpoint.id());
    Daemon::new(cfg, authz, endpoint)?.run(shutdown).await
}
