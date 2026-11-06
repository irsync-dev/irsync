//! The long-running side: accepts connections, checks the peer's key before reading
//! anything, resolves and validates the requested export, then hands off to a session.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result};
use iroh::endpoint::{Connection, Incoming, SendStream};
use iroh::{Endpoint, SecretKey};
use irsync_proto::chunk::CHUNK_PARAMS;
use irsync_proto::v1::{
    error::Code, export_entry, hello::Direction, Error, ExportEntry, ExportList, Hello,
};
use irsync_proto::PROTOCOL_VERSION;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::authz::{Access, Authorizer, FileAuthorizer, Grants, Snapshot};
use crate::config::exports::Export;
use crate::config::file::Config;
use crate::config::paths::Paths;
use crate::config::settings::{listen_sockets, DaemonSettings, Settings};
use crate::filter::FilterSet;
use crate::fs::paths::{escape_path, RelPath};
use crate::identity::PassphraseSource;
use crate::index::{Index, IndexHandle};
use crate::net::addr::HostPort;
use crate::net::frame::{read_msg, write_msg};
use crate::net::{build_endpoint, daemon_endpoint_opts, RelayChoice};
use crate::resolve::{resolve, ResolveError, Resolved};
use crate::scan::WalkOpts;
use crate::session::engine::{serve_daemon, Place};
#[cfg(feature = "test-hooks")]
use crate::session::FaultInjector;
use crate::session::{DaemonSessions, HASH_WORKERS};
use irsync_proto::v1::SessionPlan;
use prost::Message;

/// How often each export is garbage-collected, and the delay before the first
/// run (so startup stays quick).
const GC_INTERVAL: Duration = Duration::from_secs(3600);
const GC_FIRST_RUN: Duration = Duration::from_secs(60);

/// How long a client may take to open the control stream and send `Hello`,
/// and to open the listing stream after `HelloAck`.
pub(crate) const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

struct Shared {
    /// Consulted afresh on every connection.
    authz: Arc<dyn Authorizer>,
    state_dir: PathBuf,
    /// The home directory relative paths resolve against; `None` = unknown.
    home: Option<PathBuf>,
    /// One open index per export path, opened on first use and kept (with
    /// its GC and warm-up tasks) even if the export is later removed.
    indexes: Mutex<HashMap<PathBuf, IndexHandle>>,
    /// Cancels the per-index background tasks; cancelled when `run` ends.
    tasks: CancellationToken,
    /// Age (seconds) after which dormant sessions and abandoned partial
    /// files are collected; read at every collection, so a reload applies.
    dormant_secs: AtomicU64,
    /// Hosted sessions (one connection per session id) and test hooks;
    /// cloned into each connection, so a reload's `idle_timeout` applies to
    /// the connections accepted after it.
    hosting: Mutex<DaemonSessions>,
    #[cfg(feature = "test-hooks")]
    hellos: Arc<std::sync::atomic::AtomicU64>,
    /// Finished sessions, and files their hashers read.
    #[cfg(feature = "test-hooks")]
    sessions: Arc<std::sync::atomic::AtomicU64>,
    #[cfg(feature = "test-hooks")]
    hashed: Arc<std::sync::atomic::AtomicU64>,
}

impl Shared {
    fn hosting(&self) -> DaemonSessions {
        self.hosting
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn dormant(&self) -> Duration {
        Duration::from_secs(self.dormant_secs.load(Ordering::Relaxed))
    }

    /// The `[daemon]` settings that apply without a new endpoint.
    fn apply(&self, d: &DaemonSettings) {
        self.hosting
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .idle_timeout = Some(d.idle_timeout);
        self.dormant_secs
            .store(d.dormant_session_timeout.as_secs(), Ordering::Relaxed);
    }
}

/// How a running daemon re-reads its settings (`SIGHUP`): where the config
/// is, how to unlock the identity, what the command line fixed, and whom to
/// tell about the endpoint after every bind.
pub struct Reloader {
    pub paths: Paths,
    pub pass: PassphraseSource,
    /// `--listen` from the command line: kept over whatever the file says.
    pub listen: Vec<HostPort>,
    /// Signalled to reload.
    pub notify: Arc<Notify>,
    /// Called with every endpoint the daemon starts accepting on.
    pub announce: Arc<dyn Fn(&Endpoint) + Send + Sync>,
}

pub struct Daemon {
    endpoint: Endpoint,
    shared: Arc<Shared>,
    /// Indexes opened by `new` whose background tasks `run` starts.
    startup: Vec<(String, Export, IndexHandle)>,
    reload: Option<Reloader>,
}

/// Counts the connection handlers an endpoint has running.
struct Live(Arc<AtomicUsize>);

impl Live {
    fn enter(count: &Arc<AtomicUsize>) -> Live {
        count.fetch_add(1, Ordering::SeqCst);
        Live(count.clone())
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Daemon {
    /// Wraps `endpoint` and opens the index (under `settings.state_dir`) of
    /// every export `authz` lists now; exports added later are opened by
    /// their first connection.
    pub fn new(
        settings: &Settings,
        authz: Arc<dyn Authorizer>,
        endpoint: Endpoint,
    ) -> Result<Daemon> {
        Self::with_home(settings, authz, endpoint, dirs::home_dir())
    }

    /// As [`Daemon::new`], with `home` as the directory relative remote
    /// paths resolve against (`None`: such paths are unresolvable).
    pub fn with_home(
        settings: &Settings,
        authz: Arc<dyn Authorizer>,
        endpoint: Endpoint,
        home: Option<PathBuf>,
    ) -> Result<Daemon> {
        let snap = authz.snapshot()?;
        let shared = Arc::new(Shared {
            authz,
            state_dir: settings.state_dir.clone(),
            home,
            indexes: Mutex::new(HashMap::new()),
            tasks: CancellationToken::new(),
            dormant_secs: AtomicU64::new(settings.daemon.dormant_session_timeout.as_secs()),
            hosting: Mutex::new(DaemonSessions {
                idle_timeout: Some(settings.daemon.idle_timeout),
                #[cfg(feature = "test-hooks")]
                fault: Some(Arc::default()),
                ..DaemonSessions::default()
            }),
            #[cfg(feature = "test-hooks")]
            hellos: Default::default(),
            #[cfg(feature = "test-hooks")]
            sessions: Default::default(),
            #[cfg(feature = "test-hooks")]
            hashed: Default::default(),
        });
        // Opened here, before anyone else can open the same files, and
        // fatally: a daemon that cannot index its exports does not start.
        let mut startup = Vec::new();
        for (name, export) in snap.exports {
            let (index, fresh) = open_index(&shared, &name, &export)?;
            if fresh {
                startup.push((name, export, index));
            }
        }
        Ok(Daemon {
            endpoint,
            shared,
            startup,
            reload: None,
        })
    }

    /// Makes the daemon re-read its settings when `reload.notify` is signalled.
    pub fn with_reload(mut self, reload: Reloader) -> Daemon {
        self.reload = Some(reload);
        self
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
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

    /// The fault injector every session of this daemon consults.
    #[cfg(feature = "test-hooks")]
    pub fn fault_injector(&self) -> Arc<FaultInjector> {
        self.shared.hosting().fault.unwrap_or_default()
    }

    /// `NeedChunks` re-requests sent by this daemon's receivers.
    #[cfg(feature = "test-hooks")]
    pub fn rerequest_counter(&self) -> Arc<std::sync::atomic::AtomicU64> {
        self.shared.hosting().rerequests
    }

    /// The event log every daemon receiver writes to.
    #[cfg(feature = "test-hooks")]
    pub fn receiver_events(&self) -> crate::session::EventLog {
        self.shared.hosting().events
    }

    /// Ids of every session hosted so far (fresh or resumed), in order.
    #[cfg(feature = "test-hooks")]
    pub fn hosted_sessions(&self) -> Arc<std::sync::Mutex<Vec<[u8; 16]>>> {
        self.shared.hosting().registry.hosted.clone()
    }

    /// Set to make every daemon receiver's finalize take that much longer.
    #[cfg(feature = "test-hooks")]
    pub fn slow_finalize(&self) -> Arc<std::sync::Mutex<Option<Duration>>> {
        self.shared.hosting().slow_finalize
    }

    /// Hooks for the daemon's side of every session.
    #[cfg(feature = "test-hooks")]
    pub fn daemon_hooks(&self) -> Arc<std::sync::Mutex<crate::session::DaemonHooks>> {
        self.shared.hosting().daemon_hooks
    }

    /// The streams a daemon sender runs at once; zero keeps the default.
    #[cfg(feature = "test-hooks")]
    pub fn sender_parallel(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        self.shared.hosting().sender_parallel
    }

    pub async fn run(mut self, shutdown: CancellationToken) -> Result<()> {
        let _tasks = self.shared.tasks.clone().drop_guard();
        for (name, export, index) in &self.startup {
            start_tasks(&self.shared, name, export, index.clone());
        }
        let mut live = Arc::new(AtomicUsize::new(0));
        // Each session's span carries it; a reload may change it.
        let mut local_id: Arc<str> = self.endpoint.id().to_string().into();
        loop {
            let reload = async {
                match &self.reload {
                    Some(r) => r.notify.notified().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = shutdown.cancelled() => break,
                inc = self.endpoint.accept() => {
                    let Some(inc) = inc else { break };
                    let shared = self.shared.clone();
                    let local_id = local_id.clone();
                    let live = Live::enter(&live);
                    tokio::spawn(async move {
                        let _live = live;
                        if let Err(e) = handle(shared, inc, local_id).await {
                            tracing::warn!("connection failed: {e:#}");
                        }
                    });
                }
                _ = reload => match self.rebind().await {
                    Ok(endpoint) => {
                        let old = std::mem::replace(&mut self.endpoint, endpoint);
                        local_id = self.endpoint.id().to_string().into();
                        let old_live = std::mem::replace(&mut live, Arc::new(AtomicUsize::new(0)));
                        tokio::spawn(retire(old, old_live, self.shared.tasks.clone()));
                    }
                    Err(e) => tracing::warn!("reload failed, keeping the current settings: {e:#}"),
                },
            }
        }
        self.endpoint.close().await;
        Ok(())
    }

    /// Re-reads the config and binds a new endpoint from its `[daemon]`
    /// table, applying the settings that need no endpoint on the way.
    async fn rebind(&self) -> Result<Endpoint> {
        let r = self.reload.as_ref().context("no reload source")?;
        tracing::info!(config = %r.paths.config.display(), "reloading settings");
        let mut settings = Config::load(&r.paths)?.settings;
        if !r.listen.is_empty() {
            settings.daemon.listen = r.listen.clone();
        }
        let endpoint = endpoint_from_settings(&settings, &r.pass).await?;
        if endpoint.id() != self.endpoint.id() {
            tracing::warn!(
                old = %self.endpoint.id(),
                new = %endpoint.id(),
                "the daemon identity changed: clients must use the new id"
            );
        }
        if settings.state_dir != self.shared.state_dir {
            tracing::warn!(
                "state_dir changed to {}; it takes effect at the next start",
                settings.state_dir.display()
            );
        }
        self.shared.apply(&settings.daemon);
        (r.announce)(&endpoint);
        tracing::info!("settings reloaded");
        Ok(endpoint)
    }
}

/// An endpoint a reload replaced: refuses what still arrives on it and
/// closes once the connections it accepted have ended, or at shutdown.
async fn retire(endpoint: Endpoint, live: Arc<AtomicUsize>, stop: CancellationToken) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = stop.cancelled() => break,
            inc = endpoint.accept() => match inc {
                Some(inc) => {
                    tracing::info!("refusing a connection to a retired endpoint");
                    inc.refuse();
                }
                None => break,
            },
            _ = tick.tick() => {
                if live.load(Ordering::SeqCst) == 0 {
                    break;
                }
            }
        }
    }
    tracing::info!("closing the retired endpoint");
    endpoint.close().await;
}

/// The index for `export`, opened on first use, which also starts its
/// background tasks.
fn index_for(shared: &Arc<Shared>, name: &str, export: &Export) -> Result<IndexHandle> {
    let (index, fresh) = open_index(shared, name, export)?;
    if fresh {
        start_tasks(shared, name, export, index.clone());
    }
    Ok(index)
}

/// The cached index for `export.path`, or a newly opened one (`true`).
fn open_index(shared: &Shared, name: &str, export: &Export) -> Result<(IndexHandle, bool)> {
    let mut indexes = shared
        .indexes
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some(ix) = indexes.get(&export.path) {
        return Ok((ix.clone(), false));
    }
    let index = Index::open(&shared.state_dir, &export.path)
        .with_context(|| format!("open index for export {name}"))?
        .into_handle();
    indexes.insert(export.path.clone(), index.clone());
    Ok((index, true))
}

/// Starts the GC loop of `index` and, with `warm_index`, its warm-up, both
/// stopped by `shared.tasks`.
fn start_tasks(shared: &Arc<Shared>, name: &str, export: &Export, index: IndexHandle) {
    let (name, root) = (name.to_string(), export.path.clone());
    if export.warm_index {
        let (index, name, root, stop) = (
            index.clone(),
            name.clone(),
            root.clone(),
            shared.tasks.clone(),
        );
        tokio::spawn(async move {
            let warm = crate::warm::index_root(index, &root, HASH_WORKERS, false);
            tokio::select! {
                _ = stop.cancelled() => {}
                r = warm => match r {
                    Ok(r) => tracing::info!(
                        export = %name,
                        examined = r.examined,
                        hashed = r.hashed,
                        bytes = r.bytes,
                        errors = r.errors.len(),
                        "index warmed"
                    ),
                    Err(e) => tracing::warn!(export = %name, "index warm-up failed: {e}"),
                },
            }
        });
    }
    let (shared, stop, gc_index) = (shared.clone(), shared.tasks.clone(), index);
    tokio::spawn(async move {
        let first = tokio::time::Instant::now() + GC_FIRST_RUN;
        let mut tick = tokio::time::interval_at(first, GC_INTERVAL);
        loop {
            tokio::select! {
                _ = stop.cancelled() => break,
                _ = tick.tick() => {}
            }
            // Walks the export only when a day has passed or
            // collectable (dormant) partial records exist.
            let dormant = shared.dormant();
            match gc_if_due(gc_index.clone(), &root, dormant, dormant).await {
                Ok(Some(r)) => tracing::info!(
                    export = %name,
                    parts_removed = r.parts_removed.len(),
                    parts_kept = r.parts_kept,
                    sessions_removed = r.sessions_removed,
                    entries_removed = r.entries_removed,
                    "gc finished"
                ),
                Ok(None) => tracing::debug!(export = %name, "gc not due"),
                Err(e) => tracing::warn!(export = %name, "gc failed: {e}"),
            }
        }
    });
}

async fn handle(shared: Arc<Shared>, incoming: Incoming, local_id: Arc<str>) -> Result<()> {
    let conn = incoming.await.context("handshake")?;
    // Authorise on the verified key, against a fresh read of the grants,
    // before touching any stream. An unreadable config fails closed.
    let id = conn.remote_id();
    let snap = match shared.authz.snapshot() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(peer = %id.fmt_short(), "{e:#}");
            conn.close(1u32.into(), b"unauthorized");
            return Ok(());
        }
    };
    let grants = snap.grants(&id);
    if grants.is_empty() {
        tracing::info!(peer = %id.fmt_short(), "rejecting unauthorized key");
        conn.close(1u32.into(), b"unauthorized");
        return Ok(());
    }
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

    if hello.direction == Direction::ListExports as i32 {
        if let Err((code, message)) = version_check(&hello) {
            tracing::info!(peer = %id.fmt_short(), ?code, %message, "rejecting listing");
            return reject(&conn, &mut send, code, message).await;
        }
        tracing::info!(peer = %id.fmt_short(), "listing exports");
        write_msg(&mut send, &list_exports_for(&snap, &grants)).await?;
        send.finish()?;
        let _ = tokio::time::timeout(Duration::from_secs(5), send.stopped()).await;
        conn.close(0u32.into(), b"done");
        return Ok(());
    }

    match validate(&snap, &grants, shared.home.as_deref(), &hello) {
        Ok((resolved, rel)) => {
            let root = resolved.export.path.clone();
            let name = resolved.name.to_string();
            let index = match index_for(&shared, &name, resolved.export) {
                Ok(ix) => ix,
                Err(e) => {
                    tracing::warn!(export = %name, "cannot open index: {e:#}");
                    let message = format!("cannot open index for export {name:?}: {e:#}");
                    let res = reject(&conn, &mut send, Code::Internal, message).await;
                    #[cfg(feature = "test-hooks")]
                    shared
                        .sessions
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    return res;
                }
            };
            let hosting = shared.hosting();
            let place = Place {
                root,
                export: name,
                rel,
            };
            let res = serve_daemon(conn, send, recv, place, hello, index, hosting, &local_id).await;
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
            let res = reject(&conn, &mut send, code, message).await;
            #[cfg(feature = "test-hooks")]
            shared
                .sessions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            res
        }
    }
}

/// The exports `grants` cover, by name, with the real path and the
/// effective access (`read_only` caps a read-write grant).
fn list_exports_for(snap: &Snapshot, grants: &Grants) -> ExportList {
    use std::os::unix::ffi::OsStrExt;
    let exports = snap
        .exports
        .iter()
        .filter_map(|(name, e)| {
            let access = grants.access(name)?;
            let access = if e.read_only || access == Access::ReadOnly {
                export_entry::Access::Ro
            } else {
                export_entry::Access::Rw
            };
            Some(ExportEntry {
                name: name.clone(),
                path: e.path.as_os_str().as_bytes().to_vec(),
                access: access as i32,
            })
        })
        .collect();
    ExportList { exports }
}

fn version_check(hello: &Hello) -> Result<(), (Code, String)> {
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
    Ok(())
}

fn validate<'a>(
    snap: &'a Snapshot,
    grants: &Grants,
    home: Option<&Path>,
    hello: &Hello,
) -> Result<(Resolved<'a>, RelPath), (Code, String)> {
    version_check(hello)?;
    // The path is resolved (export name, home, absolute) and the export
    // containing it chosen before anything is said about it. The reply for
    // a path this key may not use is the same whether the path is outside
    // every export, inside an export the key has no grant for, or missing:
    // a key must not be able to probe the daemon's layout. Only a key that
    // may use the containing export learns that the path is invalid or
    // that the export is read-only.
    let no_access = || {
        (
            Code::Unauthorized,
            format!("no access to {}", escape_path(&hello.path)),
        )
    };
    let resolved = match resolve(&hello.path, home, &snap.exports, grants) {
        Err(ResolveError::BadPath) => return Err((Code::BadPath, "invalid path".into())),
        Ok(None) => return Err(no_access()),
        Ok(Some(r)) => r,
    };
    let rel = if hello.name.is_empty() {
        resolved.rel.clone()
    } else {
        resolved
            .rel
            .join(&hello.name)
            .map_err(|_| (Code::BadPath, "invalid name".into()))?
    };
    match Direction::try_from(hello.direction) {
        Ok(Direction::ClientSends) => {
            if resolved.export.read_only || !resolved.access.permits(true) {
                return Err((Code::ReadOnly, "export is read-only for this key".into()));
            }
        }
        Ok(Direction::DaemonSends) => {}
        // A listing never reaches here; no dedicated code for a malformed
        // direction either: UNKNOWN is the honest answer.
        Ok(Direction::ListExports) | Err(_) => return Err((Code::Unknown, "bad direction".into())),
    }
    Ok((resolved, rel))
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

fn load_or_generate_identity(path: &Path, pass: &PassphraseSource) -> Result<SecretKey> {
    let (secret, generated) = crate::identity::load_or_generate(path, pass, "irsync-daemon")
        .with_context(|| format!("identity {}", path.display()))?;
    if generated {
        tracing::info!(path = %path.display(), "generated daemon identity");
    }
    Ok(secret)
}

/// The daemon's endpoint for `settings`: its identity (generated when
/// missing), listen sockets, relay, discovery and port mapping.
async fn endpoint_from_settings(settings: &Settings, pass: &PassphraseSource) -> Result<Endpoint> {
    let secret = load_or_generate_identity(&settings.daemon.identity, pass)?;
    let bind_addrs = listen_sockets(&settings.daemon.listen).await?;
    let opts = daemon_endpoint_opts(&settings.daemon, secret, bind_addrs);
    if settings.daemon.discovery.dht && opts.relay == RelayChoice::Disabled {
        tracing::warn!(
            "dht is enabled but relays are disabled: DHT records will carry no addresses"
        );
    }
    Ok(build_endpoint(opts).await?)
}

/// Runs the daemon: hosts, exports and grants are re-read from the config
/// file (and any split file beside it) on every connection; the `[daemon]`
/// settings are re-read and applied when `reload` is signalled (`SIGHUP`).
/// `listen` is the command line's override of the file's `listen`; `debug`
/// is the debug port's bound address, announced with the listening ones.
pub async fn run_from_settings(
    paths: &Paths,
    pass: &PassphraseSource,
    listen: Vec<HostPort>,
    shutdown: CancellationToken,
    reload: Arc<Notify>,
    debug: Option<SocketAddr>,
) -> Result<()> {
    let mut settings = Config::load(paths)?.settings;
    if !listen.is_empty() {
        settings.daemon.listen = listen.clone();
    }
    let authz = Arc::new(FileAuthorizer::new(paths.clone()));
    let snap = authz.snapshot()?;
    if snap.exports.is_empty() {
        tracing::warn!(
            "no exports defined in {}; run irsync export add",
            snap.exports_file.display()
        );
    }
    let endpoint = endpoint_from_settings(&settings, pass).await?;
    // Printed only once startup can no longer fail (and again after every
    // reload): a parent process that reads these lines may dial at once.
    let announce: Arc<dyn Fn(&Endpoint) + Send + Sync> = Arc::new(move |ep: &Endpoint| {
        println!("endpoint id: {}", ep.id());
        for addr in listen_addrs(ep) {
            println!("listening on: {addr}");
        }
        if let Some(addr) = debug {
            println!("debug port: {addr}");
        }
    });
    let daemon = Daemon::new(&settings, authz, endpoint)?.with_reload(Reloader {
        paths: paths.clone(),
        pass: pass.clone(),
        listen,
        notify: reload,
        announce: announce.clone(),
    });
    announce(daemon.endpoint());
    daemon.run(shutdown).await
}

/// The sockets the daemon announces: the fault layer's when it is on.
fn listen_addrs(ep: &Endpoint) -> Vec<SocketAddr> {
    #[cfg(feature = "netsim")]
    if let Some(a) = crate::net::netsim::bound_addr() {
        return vec![a.into()];
    }
    ep.bound_sockets()
}

/// What a [`gc`] run removed (or, on a dry run, would remove).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
    /// Relative paths of the removed `.irsync.<name>.part` files.
    pub parts_removed: Vec<RelPath>,
    /// Stale-looking partial records kept because a session still references them.
    pub parts_kept: usize,
    /// Sessions removed (on a dry run: that would be removed).
    pub sessions_removed: u32,
    /// Index entries of regular files that are gone or were replaced
    /// (another inode or size) since they were indexed, removed with their
    /// chunk rows (on a dry run: that would be removed).
    pub entries_removed: usize,
    /// Stale records whose in-place `.part` file does not exist (for example
    /// it lives in a `--partial-dir`, which records do not remember). The
    /// record is dropped; the file, if any, is collected as an orphan later.
    pub rows_without_file: usize,
    /// Paths that could not be removed or walked; the pass carries on.
    pub errors: Vec<(RelPath, String)>,
}

fn io_err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

/// True for the exact name pattern the receiver gives its partial files.
fn is_part_name(name: &[u8]) -> bool {
    name.len() > ".irsync.".len() + ".part".len()
        && name.starts_with(b".irsync.")
        && name.ends_with(b".part")
}

/// The `.part` file the receiver keeps for destination `rel`. The name may
/// be shortened (see [`crate::recv::temp_name`]), so there is no inverse:
/// the GC maps every known destination forward and compares.
fn part_of(rel: &RelPath) -> Option<RelPath> {
    let b = rel.as_bytes();
    let i = b.iter().rposition(|&c| c == b'/').map_or(0, |i| i + 1);
    let mut p = b[..i].to_vec();
    p.extend_from_slice(&crate::recv::temp_name(&b[i..], b".part"));
    RelPath::new(&p).ok()
}

/// What [`remove_part`] did.
#[derive(Debug, PartialEq, Eq)]
enum Removal {
    Removed,
    Missing,
    /// Not ours to remove: wrong name, not a regular file, or touched since.
    Skipped,
}

/// Removes `root/rel` if its name is a part name, it is a regular file
/// (never follows a symlink) and its mtime is not newer than `cutoff`. With
/// `dry_run` only classifies.
fn remove_part(root: &Path, rel: &RelPath, cutoff: i64, dry_run: bool) -> io::Result<Removal> {
    use std::os::unix::fs::MetadataExt;
    if !rel.components().last().is_some_and(is_part_name) {
        return Ok(Removal::Skipped);
    }
    let full = root.join(rel.to_os());
    match std::fs::symlink_metadata(&full) {
        Ok(m) if m.file_type().is_file() && m.mtime() <= cutoff => {
            if !dry_run {
                std::fs::remove_file(&full)?;
            }
            Ok(Removal::Removed)
        }
        Ok(_) => Ok(Removal::Skipped),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Removal::Missing),
        Err(e) => Err(e),
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Garbage-collects resume state under `root`: abandoned `.part` files and
/// their `partials` rows, then dormant sessions, all older than `older_than`.
/// A part is only removed when no session plan references its path and its
/// record (or, for an orphan without a record, its mtime) is stale. With
/// `dry_run` nothing is changed. Parts are collected before sessions, so a
/// part freed by an expiring session goes on the next run.
pub async fn gc(
    index: IndexHandle,
    root: &Path,
    older_than: Duration,
    dry_run: bool,
) -> io::Result<GcReport> {
    let cutoff = unix_now().saturating_sub(older_than.as_secs().min(i64::MAX as u64) as i64);

    // Orphan candidates: stale `.part` files on disk (the walk never follows symlinks).
    let (mut rx, summary) = crate::scan::walk(
        root.to_path_buf(),
        Arc::new(FilterSet::new(vec![])),
        WalkOpts {
            follow_root_symlink: false,
            threads: 2,
            max_depth: None,
        },
        None,
    );
    let mut on_disk = Vec::new();
    // Every regular file the walk saw, by inode and size.
    let mut seen = std::collections::HashMap::new();
    while let Some(ev) = rx.recv().await {
        // Gaps are in the summary's errors.
        let crate::scan::ScanEvent::Entry(item) = ev else {
            continue;
        };
        let is_reg = item.entry.kind() == irsync_proto::v1::file_entry::Kind::Regular;
        let named = item.path.components().last().is_some_and(is_part_name);
        if is_reg && named && item.entry.mtime_sec <= cutoff {
            on_disk.push(item.path);
        } else if is_reg {
            seen.insert(item.path, (item.entry.inode, item.entry.size));
        }
    }
    let mut walk_errors = Vec::new();
    if let Ok(sum) = summary.await {
        walk_errors = sum.errors;
    }

    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || -> io::Result<GcReport> {
        let mut ix = index.lock().map_err(|_| io_err("index lock poisoned"))?;
        let mut referenced: Vec<Vec<u8>> = Vec::new();
        for row in ix.all_sessions().map_err(io_err)? {
            let plan = SessionPlan::decode(&row.plan[..]).map_err(io_err)?;
            referenced.extend(plan.entries.into_iter().map(|e| e.path));
        }
        // The `.part` of every referenced path, so a part on disk can be
        // matched without inverting its (possibly shortened) name.
        let referenced_parts: Vec<Vec<u8>> = referenced
            .iter()
            .filter_map(|p| RelPath::new(p).ok())
            .filter_map(|p| part_of(&p))
            .map(|p| p.as_bytes().to_vec())
            .collect();
        // Plan paths are relative to the session root, which may sit below the
        // index root: match on a whole-component suffix to stay conservative.
        let suffix_of = |rel: &RelPath, list: &[Vec<u8>]| {
            let b = rel.as_bytes();
            list.iter().any(|p| {
                !p.is_empty()
                    && (b == &p[..]
                        || (b.len() > p.len()
                            && b.ends_with(p)
                            && b[b.len() - p.len() - 1] == b'/'))
            })
        };
        let is_ref = |rel: &RelPath| suffix_of(rel, &referenced);
        let mut report = GcReport::default();
        let mut handled = std::collections::HashSet::new();

        // Stale records, with their part files. The index lock is held, so a
        // live receiver cannot refresh a record between check and removal.
        for dest in ix.partials_older_than(older_than).map_err(io_err)? {
            if is_ref(&dest) {
                report.parts_kept += 1;
                continue;
            }
            let Some(part) = part_of(&dest) else { continue };
            handled.insert(part.clone());
            match remove_part(&root, &part, cutoff, dry_run) {
                Ok(Removal::Removed) => report.parts_removed.push(part),
                Ok(Removal::Missing) => report.rows_without_file += 1,
                Ok(Removal::Skipped) => {
                    report.parts_kept += 1;
                    continue;
                }
                Err(e) => {
                    tracing::warn!(path = %part.to_os().to_string_lossy(), "gc: {e}");
                    report.errors.push((part, e.to_string()));
                    continue;
                }
            }
            if !dry_run {
                ix.remove_partial(&dest).map_err(io_err)?;
            }
        }
        // Orphans: stale files with no record. A part with a record of any
        // age is not an orphan (the stale ones were handled above).
        let recorded: std::collections::HashSet<RelPath> = ix
            .partial_paths()
            .map_err(io_err)?
            .iter()
            .filter_map(part_of)
            .collect();
        for part in on_disk {
            if handled.contains(&part) || recorded.contains(&part) {
                continue;
            }
            if is_ref(&part) || suffix_of(&part, &referenced_parts) {
                report.parts_kept += 1;
                continue;
            }
            match remove_part(&root, &part, cutoff, dry_run) {
                Ok(Removal::Removed) => report.parts_removed.push(part),
                Ok(Removal::Missing | Removal::Skipped) => {}
                Err(e) => {
                    tracing::warn!(path = %part.to_os().to_string_lossy(), "gc: {e}");
                    report.errors.push((part, e.to_string()));
                }
            }
        }
        report.parts_removed.sort();
        report.sessions_removed = if dry_run {
            ix.count_sessions_older_than(older_than).map_err(io_err)?
        } else {
            ix.delete_sessions_older_than(older_than)
                .map_err(io_err)?
                .len() as u32
        };
        report.entries_removed = prune_entries(&mut ix, &root, &seen, &walk_errors, dry_run)?;
        report.errors.extend(walk_errors);
        if !dry_run {
            ix.set_last_gc_at(unix_now()).map_err(io_err)?;
        }
        Ok(report)
    })
    .await
    .map_err(io_err)?
}

/// Removes the index entries of regular files that are gone or were
/// replaced (another inode or size), unless `dry_run`; returns how many.
/// `seen` is what the walk found. A path under one of the walk's `errors`
/// may only be unreadable, so it stays; one the walk missed or saw
/// differently may have been placed since, so it is looked at once more
/// (the index lock is held: a receiver cannot index it meanwhile). A walk
/// that found no file at all prunes nothing: the root may be a mount point
/// whose filesystem is not mounted.
fn prune_entries(
    ix: &mut crate::index::Index,
    root: &Path,
    seen: &std::collections::HashMap<RelPath, (u64, u64)>,
    errors: &[(RelPath, String)],
    dry_run: bool,
) -> io::Result<usize> {
    use std::os::unix::fs::MetadataExt;
    if seen.is_empty() {
        return Ok(0);
    }
    let regular = irsync_proto::v1::file_entry::Kind::Regular as i32;
    let unreadable = |p: &RelPath| {
        errors.iter().any(|(e, _)| {
            let (e, p) = (e.as_bytes(), p.as_bytes());
            e.is_empty() || p == e || (p.starts_with(e) && p.get(e.len()) == Some(&b'/'))
        })
    };
    let mut removed = 0;
    for (path, kind, inode, size) in ix.entry_identities().map_err(io_err)? {
        if kind != regular || seen.get(&path) == Some(&(inode, size)) || unreadable(&path) {
            continue;
        }
        let here = std::fs::symlink_metadata(root.join(path.to_os()))
            .ok()
            .filter(|m| m.is_file())
            .map(|m| (m.ino(), m.len()));
        if here == Some((inode, size)) {
            continue;
        }
        removed += 1;
        if !dry_run {
            ix.remove_entry(&path).map_err(io_err)?;
        }
    }
    Ok(removed)
}

/// A full GC walk is due at most this often, unless stale partials wait.
pub const GC_EVERY: Duration = Duration::from_secs(24 * 3600);

/// Whether a GC walk is due: never walked, the last walk is [`GC_EVERY`]
/// old, or partial records have gone stale since (Ruling 32).
pub fn gc_due(last_gc_at: Option<i64>, now: i64, stale_partials: bool) -> bool {
    stale_partials || last_gc_at.is_none_or(|t| now.saturating_sub(t) >= GC_EVERY.as_secs() as i64)
}

/// Runs [`gc`] only when [`gc_due`] says so; `stale_after` is the age at
/// which a partial record counts as stale. `None` when skipped.
pub async fn gc_if_due(
    index: IndexHandle,
    root: &Path,
    older_than: Duration,
    stale_after: Duration,
) -> io::Result<Option<GcReport>> {
    let ix = index.clone();
    let due = tokio::task::spawn_blocking(move || -> io::Result<bool> {
        let ix = ix.lock().map_err(|_| io_err("index lock poisoned"))?;
        let last = ix.last_gc_at().map_err(io_err)?;
        let stale = !ix
            .partials_older_than(stale_after)
            .map_err(io_err)?
            .is_empty();
        Ok(gc_due(last, unix_now(), stale))
    })
    .await
    .map_err(io_err)??;
    if !due {
        return Ok(None);
    }
    gc(index, root, older_than, false).await.map(Some)
}
