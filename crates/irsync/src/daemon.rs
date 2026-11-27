//! The long-running side: accepts connections, checks the peer's key before reading
//! anything, resolves and validates the requested export, then hands off to a session.

use std::collections::{BTreeMap, HashMap};
use std::io::{self, IsTerminal};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result};
use iroh::endpoint::{Connection, Incoming, SendStream};
use iroh::{Endpoint, EndpointId, SecretKey};
use irsync_proto::chunk::CHUNK_PARAMS;
use irsync_proto::v1::{
    error::Code, export_entry, hello, hello_reply, Error, ExportEntry, ExportList, Hello,
    HelloReply, PathInfo, SyncRequest,
};
use irsync_proto::PROTOCOL_VERSION;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::authz::{Access, Authorizer, AuthzError, FileAuthorizer, Permissions, Snapshot};
use crate::autoexport::{auto_export_dir, check_root, AutoExportError};
use crate::config::exports::Export;
use crate::config::file::Config;
use crate::config::paths::Paths;
use crate::config::settings::{
    listen_sockets, DaemonSettings, PqKeyExchange, Relay, Settings, TelemetrySettings,
};
use crate::fs::paths::{escape_path, RelPath};
use crate::gc::{gc_if_due_skipping, nested_roots};
use crate::identity::PassphraseSource;
use crate::index::{Index, IndexHandle};
use crate::net::addr::HostPort;
use crate::net::frame::{read_msg_tapped, write_msg_tapped};
use crate::net::tap::{ConnTap, StreamKind as TapStream, StreamTap};
use crate::net::{build_endpoint, daemon_endpoint_opts, finish_answer};
use crate::options::Direction;
use crate::resolve::{containing, resolve, resolve_path, ResolveError, Resolved};
use crate::session::engine::{serve_daemon, Place, HELLO_TIMEOUT};
use crate::session::single::{is_one_component, place_single, probe_kind};
#[cfg(feature = "test-hooks")]
use crate::session::FaultInjector;
use crate::session::{DaemonSessions, SyncHello, HASH_WORKERS};

mod admin;
mod indexing;
mod inner;
pub use admin::{ADMIN_IDLE, ADMIN_MAX_CHANGES, ADMIN_MAX_PENDING, ADMIN_MAX_REQUEST};
use indexing::StartedBy;

/// How often each export is garbage-collected, and the delay before the first
/// run (so startup stays quick).
const GC_INTERVAL: Duration = Duration::from_secs(3600);
const GC_FIRST_RUN: Duration = Duration::from_secs(60);

struct Shared {
    /// Consulted afresh on every connection.
    authz: Arc<dyn Authorizer>,
    state_dir: PathBuf,
    /// The home directory relative paths resolve against; `None` = unknown.
    home: Option<PathBuf>,
    /// One open index per export path, opened on first use and kept (with
    /// its GC and index-at-start tasks) even if the export is later removed.
    indexes: Mutex<HashMap<PathBuf, IndexHandle>>,
    /// Cancels the per-index background tasks; cancelled when `run` ends.
    tasks: CancellationToken,
    /// The indexing runs since the daemon started, the latest per path.
    runs: indexing::Runs,
    /// Age (seconds) after which dormant sessions and abandoned partial
    /// files are collected; read at every collection, so a reload applies.
    dormant_secs: AtomicU64,
    /// Hosted sessions (one connection per session id) and test hooks;
    /// cloned into each connection, so a reload's `idle_timeout` applies to
    /// the connections accepted after it.
    hosting: Mutex<DaemonSessions>,
    /// `pq_key_exchange = "require"`; read at every connection, so a
    /// reload applies.
    require_pq: std::sync::atomic::AtomicBool,
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
        self.hosting
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .max_dormant_sessions
            .store(d.max_dormant_sessions_per_key, Ordering::Relaxed);
        self.dormant_secs
            .store(d.dormant_session_timeout.as_secs(), Ordering::Relaxed);
        self.require_pq.store(
            d.pq_key_exchange == PqKeyExchange::Require,
            Ordering::Relaxed,
        );
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
    /// The settings read once at start, which a reload cannot change.
    started: StartOnly,
}

/// The settings a daemon reads once, at start: a reload that finds them
/// changed says that they take effect at the next start.
#[derive(Debug, Clone, PartialEq)]
struct StartOnly {
    state_dir: PathBuf,
    debug_addr: Option<SocketAddr>,
    telemetry: TelemetrySettings,
}

impl StartOnly {
    fn of(settings: &Settings) -> StartOnly {
        StartOnly {
            state_dir: settings.state_dir.clone(),
            debug_addr: settings.daemon.debug_addr,
            telemetry: settings.telemetry.clone(),
        }
    }

    /// A warning for each setting `now` changes from these.
    fn changed_in(&self, now: &Settings) -> Vec<String> {
        let now = StartOnly::of(now);
        let mut changed = Vec::new();
        if now.state_dir != self.state_dir {
            changed.push(format!("state_dir changed to {}", now.state_dir.display()));
        }
        if now.debug_addr != self.debug_addr {
            changed.push(match now.debug_addr {
                Some(a) => format!("debug_addr changed to {a}"),
                None => "debug_addr was removed".to_string(),
            });
        }
        if now.telemetry != self.telemetry {
            changed.push("[telemetry] changed".to_string());
        }
        changed
            .into_iter()
            .map(|c| format!("{c}; it takes effect at the next start"))
            .collect()
    }
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
        Self::with_home(settings, authz, endpoint, crate::config::paths::home_dir())
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
            runs: indexing::Runs::default(),
            dormant_secs: AtomicU64::new(settings.daemon.dormant_session_timeout.as_secs()),
            hosting: Mutex::new(DaemonSessions {
                idle_timeout: Some(settings.daemon.idle_timeout),
                max_dormant_sessions: Arc::new(settings.daemon.max_dormant_sessions_per_key.into()),
                #[cfg(feature = "test-hooks")]
                fault: Some(Arc::default()),
                ..DaemonSessions::default()
            }),
            require_pq: (settings.daemon.pq_key_exchange == PqKeyExchange::Require).into(),
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
            started: StartOnly::of(settings),
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

    /// The limit on one key's dormant sessions per export; zero is none.
    #[cfg(feature = "test-hooks")]
    pub fn max_dormant_sessions(&self) -> Arc<std::sync::atomic::AtomicU32> {
        self.shared.hosting().max_dormant_sessions
    }

    /// Set to make every daemon receiver's finalize take that much longer.
    #[cfg(feature = "test-hooks")]
    pub fn slow_finalize(&self) -> Arc<std::sync::Mutex<Option<Duration>>> {
        self.shared.hosting().slow_finalize
    }

    /// Hooks for the daemon's side of every session.
    #[cfg(feature = "test-hooks")]
    pub fn daemon_hooks(&self) -> Arc<std::sync::Mutex<crate::session::SideHooks>> {
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
                    let local = self.endpoint.id();
                    let live = Live::enter(&live);
                    tokio::spawn(async move {
                        let _live = live;
                        if let Err(e) = handle(shared, inc, local, local_id).await {
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
        let pass = PassphraseSource {
            allow_open: settings.unsafe_config_allowed,
            ..r.pass.clone()
        };
        let endpoint = endpoint_from_settings(&settings, &pass).await?;
        if endpoint.id() != self.endpoint.id() {
            tracing::warn!(
                old = %self.endpoint.id(),
                new = %endpoint.id(),
                "the daemon identity changed: clients must use the new id"
            );
        }
        for warning in self.started.changed_in(&settings) {
            tracing::warn!("{warning}");
        }
        self.shared.apply(&settings.daemon);
        // Log first, announce last: the `listening on:` line tells a watcher
        // the reload is done, so everything the reload logs must precede it.
        tracing::info!("settings reloaded");
        (r.announce)(&endpoint);
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

/// Starts hashing `export`'s folder into `index` in the background, as
/// `by` asked, stopped by `shared.tasks`, and records the run in
/// `shared.runs`; its status. `Err` with the run in progress when the
/// folder is being indexed already: no second run is started. The run's
/// end is logged with its counts, and each file it could not read with
/// its path.
fn spawn_run(
    shared: &Arc<Shared>,
    name: &str,
    export: &Export,
    index: IndexHandle,
    by: StartedBy,
) -> Result<irsync_proto::v1::IndexRun, Box<irsync_proto::v1::IndexRun>> {
    let root = export.path.clone();
    let (progress, status) = shared.runs.claim(&root, name, by)?;
    let (shared, name, stop) = (shared.clone(), name.to_string(), shared.tasks.clone());
    tokio::spawn(async move {
        // Ends the run however this task ends, a panic included.
        let end = indexing::Finish::new(&shared.runs, &root);
        let indexing =
            crate::warm::index_root_tracked(index, &root, HASH_WORKERS, false, &progress);
        let result = tokio::select! {
            _ = stop.cancelled() => return,
            r = indexing => r,
        };
        match &result {
            Ok(r) => {
                for (p, msg) in &r.errors {
                    let file = root.join(p.to_os()).display().to_string();
                    tracing::warn!(export = %name, "{}", indexing::failed_line(&file, msg));
                }
                tracing::info!(
                    export = %name,
                    examined = r.examined,
                    hashed = r.hashed,
                    bytes = r.bytes,
                    errors = r.errors.len(),
                    "export indexed"
                );
            }
            Err(e) => tracing::warn!(export = %name, "indexing the export failed: {e}"),
        }
        end.finish(&result);
    });
    Ok(status)
}

/// Starts the GC loop of `index` and, with `index_at_start`, the hashing of
/// the export's files into it, both stopped by `shared.tasks`.
fn start_tasks(shared: &Arc<Shared>, name: &str, export: &Export, index: IndexHandle) {
    let (name, root) = (name.to_string(), export.path.clone());
    if export.index_at_start {
        // A run already going (asked for by a client) is the one that counts.
        let _ = spawn_run(shared, &name, export, index.clone(), StartedBy::AtStart);
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
            // partial records have become collectable (dormant) since.
            let dormant = shared.dormant();
            // Other exports inside this one collect their own state. An
            // unreadable config skips this round: it may hide one.
            let skip = match shared.authz.snapshot() {
                Ok(snap) => nested_roots(&root, snap.exports.values().map(|e| e.path.as_path())),
                Err(e) => {
                    tracing::warn!(export = %name, "gc skipped: {e:#}");
                    continue;
                }
            };
            match gc_if_due_skipping(gc_index.clone(), &root, dormant, dormant, &skip).await {
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

async fn handle(
    shared: Arc<Shared>,
    incoming: Incoming,
    local: EndpointId,
    local_id: Arc<str>,
) -> Result<()> {
    let conn = incoming.await.context("handshake")?;
    let conn_tap = ConnTap::of(&conn);
    conn_tap.set_local_id(local);
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
    let grants = snap.permissions(&id);
    if grants.is_empty() {
        tracing::info!(peer = %id.fmt_short(), "rejecting unauthorized key");
        conn.close(1u32.into(), b"unauthorized");
        return Ok(());
    }
    let key_exchange = crate::net::key_exchange(&conn);
    let key_exchange = key_exchange.as_deref().unwrap_or("unknown");
    // After the grant check, so a stranger learns nothing about the setting.
    if shared.require_pq.load(Ordering::Relaxed) && !crate::net::has_pq_key_exchange(&conn) {
        tracing::warn!(
            peer = %id.fmt_short(),
            key_exchange,
            "rejecting a connection without post-quantum key exchange (pq_key_exchange = \"require\")"
        );
        let (code, reason) = crate::net::PQ_REQUIRED_CLOSE;
        conn.close(code.into(), reason);
        return Ok(());
    }
    tracing::info!(peer = %id.fmt_short(), key_exchange, "connection accepted");
    let first = tokio::time::timeout(HELLO_TIMEOUT, async {
        let (send, mut recv) = conn.accept_bi().await.context("accept control stream")?;
        let tap = conn_tap.stream(TapStream::Control, u64::from(recv.id()));
        let hello: Hello = read_msg_tapped(&mut recv, &tap)
            .await
            .context("read Hello")?
            .context("peer closed before Hello")?;
        anyhow::Ok((send, recv, tap, hello))
    })
    .await;
    let Ok(first) = first else {
        conn.close(2u32.into(), b"hello timeout");
        anyhow::bail!("timed out waiting for Hello");
    };
    let (mut send, recv, tap, hello) = first?;
    if let Err((code, message)) = protocol_check(&hello) {
        tracing::info!(peer = %id.fmt_short(), ?code, %message, "rejecting connection");
        return reject(&conn, &mut send, &tap, code, message).await;
    }
    match &hello.request {
        None => {
            let message = "a Hello must hold a request".to_string();
            tracing::info!(peer = %id.fmt_short(), %message, "rejecting connection");
            return reject(&conn, &mut send, &tap, Code::BadRequest, message).await;
        }
        Some(hello::Request::Admin(_)) => {
            return admin::serve_admin(shared, &snap, conn, local, id, send, recv, tap).await;
        }
        Some(hello::Request::ListExports(_)) => {
            tracing::info!(peer = %id.fmt_short(), "listing exports");
            let list = list_exports_for(&snap, &grants);
            return answer(&conn, &mut send, &tap, hello_reply::Reply::Exports(list)).await;
        }
        Some(hello::Request::Probe(p)) => {
            let target = Target {
                push: false,
                path: &p.path,
                name: b"",
                single_file: false,
                contents_only: false,
            };
            let mut fresh = None;
            let kind = match admit_target(&shared, &snap, &mut fresh, &grants, id, &target).await {
                Ok((resolved, rel)) => {
                    let root = resolved.export.path.clone();
                    tokio::task::spawn_blocking(move || probe_kind(&root, &rel))
                        .await
                        .map_err(|e| (Code::Internal, format!("probing the path failed: {e}")))
                }
                Err(e) => Err(e),
            };
            return match kind {
                Ok(kind) => {
                    tracing::info!(peer = %id.fmt_short(), ?kind, "probed path");
                    let info = PathInfo { kind: kind as i32 };
                    answer(&conn, &mut send, &tap, hello_reply::Reply::PathInfo(info)).await
                }
                Err((code, message)) => {
                    tracing::info!(peer = %id.fmt_short(), ?code, %message, "rejecting probe");
                    reject(&conn, &mut send, &tap, code, message).await
                }
            };
        }
        Some(hello::Request::Push(_) | hello::Request::Pull(_)) => {}
    }
    let hello = SyncHello::from_hello(hello).expect("a push or a pull");
    // Only session hellos: a listing or a probe never bumps `sessions`, so
    // counting it would leave the tests' `settle` waiting forever.
    #[cfg(feature = "test-hooks")]
    shared
        .hellos
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let req = &hello.req;
    let target = Target {
        push: hello.direction == Direction::ClientSends,
        path: &req.path,
        name: &req.name,
        single_file: req.single_file,
        contents_only: req.contents_only,
    };
    // The snapshot taken after adding an automatic export; the session's
    // `Resolved` borrows from it.
    let mut fresh = None;
    let validated = match sync_check(req) {
        Ok(()) => admit_target(&shared, &snap, &mut fresh, &grants, id, &target).await,
        Err(e) => Err(e),
    };
    let validated = match validated {
        Ok((resolved, rel)) if req.single_file => {
            let (root, hello) = (resolved.export.path.clone(), hello.clone());
            tokio::task::spawn_blocking(move || place_single(&root, &rel, &hello))
                .await
                .unwrap_or_else(|e| Err((Code::Internal, format!("placing the file failed: {e}"))))
                .map(|(rel, single)| (resolved, rel, Some(single)))
        }
        Ok((resolved, rel)) => Ok((resolved, rel, None)),
        Err(e) => Err(e),
    };
    match validated {
        Ok((resolved, rel, single)) => {
            let root = resolved.export.path.clone();
            let name = resolved.name.to_string();
            let index = match index_for(&shared, &name, resolved.export) {
                Ok(ix) => ix,
                Err(e) => {
                    tracing::warn!(export = %name, "cannot open index: {e:#}");
                    let message = format!("cannot open index for export {name:?}: {e:#}");
                    let res = reject(&conn, &mut send, &tap, Code::Internal, message).await;
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
                fsync: resolved.export.fsync,
                single,
            };
            let hello = hello.clone();
            let res = serve_daemon(
                conn, send, recv, tap, place, hello, index, hosting, &local_id,
            )
            .await;
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
            let res = reject(&conn, &mut send, &tap, code, message).await;
            #[cfg(feature = "test-hooks")]
            shared
                .sessions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            res
        }
    }
}

/// What a `Hello` asks of a path: a push's or a pull's, or a probe's, which
/// is resolved and checked as a pull of its path.
struct Target<'h> {
    push: bool,
    path: &'h [u8],
    /// Joined below the resolved path; with `single_file`, the file's name,
    /// which `place_single` places.
    name: &'h [u8],
    single_file: bool,
    contents_only: bool,
}

/// Resolves `t` for the key with `grants` and admits it, or says why not.
/// For a key with `all_paths` whose path no export it may use contains,
/// an automatic export is added first ([`serve_all_paths`]); `fresh` then
/// holds the snapshot that has it, which the result borrows.
async fn admit_target<'a>(
    shared: &Arc<Shared>,
    snap: &'a Snapshot,
    fresh: &'a mut Option<Snapshot>,
    grants: &Permissions,
    id: iroh::EndpointId,
    t: &Target<'_>,
) -> Result<(Resolved<'a>, RelPath), (Code, String)> {
    let (resolved, rel) = match validate(snap, grants, shared.home.as_deref(), t)? {
        Some(v) => v,
        None if grants.all_paths.is_some() => {
            let (s, abs) = serve_all_paths(shared, snap, id, t).await?;
            let fresh: &'a Snapshot = fresh.insert(s);
            let grants = fresh.permissions(&id);
            match containing(&abs, &fresh.exports, &grants, None) {
                Some(resolved) => admit(resolved, t)?,
                None => return Err(no_access(t.path)),
            }
        }
        None => return Err(no_access(t.path)),
    };
    inner_export_check(snap, grants, id, t, resolved, rel).await
}

/// A request must not reach another export's directory through an outer
/// export, in whatever spelling (see [`inner::inner_export`]): on a
/// volume that ignores case, `/srv/RO` is the export `/srv/ro` too, but
/// only `/srv` matches it by name. Every kind of session passes here, a
/// probe too.
async fn inner_export_check<'a>(
    snap: &Snapshot,
    grants: &Permissions,
    id: iroh::EndpointId,
    t: &Target<'_>,
    resolved: Resolved<'a>,
    rel: RelPath,
) -> Result<(Resolved<'a>, RelPath), (Code, String)> {
    // Only exports this peer has a grant for, as `containing` chooses
    // among: a grant on an outer export reaches everything below it, and
    // the reply never names an export the peer may not see. (An automatic
    // export just added is the chosen one.)
    let others: Vec<(String, PathBuf)> = snap
        .exports
        .iter()
        .filter(|(n, e)| n.as_str() != resolved.name && grants.access_to(n, e).is_some())
        .map(|(n, e)| (n.clone(), e.path.clone()))
        .collect();
    let (root, r) = (resolved.export.path.clone(), rel.clone());
    let auto = resolved.export.auto;
    let crossed = tokio::task::spawn_blocking(move || {
        if auto {
            inner::auto_export_reaches(&root, &r, &others)
        } else {
            inner::inner_export(&root, &r, &others)
        }
    })
    .await;
    match crossed {
        Ok(found) => check_answer(t.path, id, found).map(|()| (resolved, rel)),
        Err(e) => {
            tracing::warn!(peer = %id.fmt_short(), "checking for an inner export: {e}");
            Err((Code::Internal, "checking the path failed".into()))
        }
    }
}

/// The answer to a request from the check for another export
/// ([`inner::inner_export`], [`inner::auto_export_reaches`]): `Ok` to go
/// ahead; a request that reaches export `name` is refused naming it; a
/// check that could not finish refuses too, it never allows.
fn check_answer(
    path: &[u8],
    peer: iroh::EndpointId,
    found: io::Result<Option<String>>,
) -> Result<(), (Code, String)> {
    match found {
        Ok(Some(name)) => Err(inside_export(path, &name)),
        Ok(None) => Ok(()),
        Err(e) => {
            tracing::warn!(peer = %peer.fmt_short(), "checking for an inner export: {e}");
            Err((Code::Internal, format!("checking the path failed: {e}")))
        }
    }
}

/// Writes `reply` as the answer to `Hello` and closes the connection once
/// the client has it ([`finish_answer`]): a listing's or a probe's, which
/// ends the connection.
async fn answer(
    conn: &Connection,
    send: &mut SendStream,
    tap: &StreamTap,
    reply: hello_reply::Reply,
) -> Result<()> {
    let reply = HelloReply { reply: Some(reply) };
    write_msg_tapped(send, &reply, tap).await?;
    finish_answer(conn, send, tap, 0, b"done").await
}

/// The exports `grants` cover, by name, with the real path and the
/// effective access (`read_only` caps a read-write grant).
fn list_exports_for(snap: &Snapshot, grants: &Permissions) -> ExportList {
    let exports = snap
        .exports
        .iter()
        .filter_map(|(name, e)| {
            let access = grants.access_to(name, e)?;
            let access = if e.read_only || access == Access::ReadOnly {
                export_entry::Access::Ro
            } else {
                export_entry::Access::Rw
            };
            Some(ExportEntry {
                name: name.clone(),
                path: crate::platform::os_to_bytes(e.path.as_os_str()),
                access: access as i32,
            })
        })
        .collect();
    ExportList { exports }
}

/// The protocol version: all a listing, a probe or an admin session needs
/// to agree on.
fn protocol_check(hello: &Hello) -> Result<(), (Code, String)> {
    if hello.protocol_version != PROTOCOL_VERSION {
        return Err((
            Code::VersionMismatch,
            format!(
                "protocol version {} unsupported (daemon speaks {PROTOCOL_VERSION})",
                hello.protocol_version
            ),
        ));
    }
    Ok(())
}

/// What a push or a pull must agree on besides the protocol version: the
/// chunking parameters, and the preserve flags every client sends.
fn sync_check(req: &SyncRequest) -> Result<(), (Code, String)> {
    if (req.chunk_min, req.chunk_avg, req.chunk_max)
        != (CHUNK_PARAMS.min, CHUNK_PARAMS.avg, CHUNK_PARAMS.max)
    {
        return Err((Code::VersionMismatch, "chunk params mismatch".into()));
    }
    if req.preserve.is_none() {
        return Err((
            Code::VersionMismatch,
            "a Hello for a push or a pull must hold the preserve flags".into(),
        ));
    }
    Ok(())
}

/// The reply for a path this key may not use. It is the same whether the
/// path is outside every export, inside an export the key has no grant
/// for, or missing: a key must not be able to probe the daemon's layout.
fn no_access(path: &[u8]) -> (Code, String) {
    (
        Code::Unauthorized,
        format!("no access to {}", escape_path(path)),
    )
}

/// `Ok(None)` when no export the key may use contains the path: the caller
/// replies [`no_access`], or, for a key with `all_paths`, adds an
/// automatic export ([`serve_all_paths`]).
fn validate<'a>(
    snap: &'a Snapshot,
    grants: &Permissions,
    home: Option<&Path>,
    t: &Target<'_>,
) -> Result<Option<(Resolved<'a>, RelPath)>, (Code, String)> {
    // The path is resolved (export name, home, absolute) and the export
    // containing it chosen before anything is said about it. Only a key
    // that may use the containing export learns that the path is invalid
    // or that the export is read-only.
    match resolve(t.path, home, &snap.exports, grants) {
        Err(ResolveError::BadPath) => Err((Code::BadPath, "invalid path".into())),
        Ok(None) => Ok(None),
        Ok(Some(r)) => admit(r, t).map(Some),
    }
}

/// For a key with `all_paths`, whose path no export it may use contains:
/// adds an automatic export for the path ([`auto_export_dir`]) that lists the
/// key with its `all_paths` level, and returns a snapshot that has it,
/// with the path made canonical to resolve against it. A push by a key
/// whose `all_paths` is `ro` is refused first, so nothing is created for
/// it. Such a key can reach the server over SSH anyway, so a missing path
/// is reported as such. The filesystem and config work runs off the async
/// runtime: it may wait for the config lock.
async fn serve_all_paths(
    shared: &Arc<Shared>,
    snap: &Snapshot,
    id: iroh::EndpointId,
    t: &Target<'_>,
) -> Result<(Snapshot, PathBuf), (Code, String)> {
    let push = t.push;
    // An export's short name stands for its path only when this client may
    // use that export; any other name is a directory under the home, so
    // the client cannot reach (and auto-export) another client's export.
    let grants = snap.permissions(&id);
    let Some(level) = grants.all_paths else {
        return Err(no_access(t.path));
    };
    let usable: BTreeMap<String, Export> = snap
        .exports
        .iter()
        .filter(|(name, e)| grants.access_to(name, e).is_some())
        .map(|(name, e)| (name.clone(), e.clone()))
        .collect();
    let abs = match resolve_path(t.path, shared.home.as_deref(), &usable) {
        Ok(Some((abs, _))) => abs,
        Ok(None) => return Err(no_access(t.path)),
        Err(ResolveError::BadPath) => return Err((Code::BadPath, "invalid path".into())),
    };
    // Another spelling of a path an export this peer has a grant for
    // holds: the request's deepest existing part is resolved to its real
    // place, and the ordinary exact match runs once more on that real path.
    // If such an export holds it, the request is served through that export
    // under its rules, as if the peer had typed the real spelling. It
    // cannot widen access: the peer gets what that spelling gives it.
    let base = abs.clone();
    let real = tokio::task::spawn_blocking(move || inner::real_place(&base))
        .await
        .unwrap_or_else(|e| Err(io::Error::other(e.to_string())));
    match real {
        Ok(Some((real, missing))) => {
            let real = real.join(missing);
            if containing(&real, &snap.exports, &grants, None).is_some() {
                return Ok((snap.clone(), real));
            }
        }
        Ok(None) => {}
        Err(e) => check_answer(t.path, id, Err(e))?,
    }
    // Before `auto_export_dir`, which makes a missing push target. The reply is
    // the one for a push into a read-only export.
    if push && !level.permits(true) {
        return Err(read_only());
    }
    // Then a granted export at, above or on the way to `abs` in another
    // spelling refuses the request, before the automatic route makes a
    // directory or adds an export (see `inner::auto_export_reaches`). The
    // name a push joins below the path (`admit`) is part of the way, so a
    // request that the check after `admit` would refuse adds nothing
    // either.
    let others: Vec<(String, PathBuf)> = usable
        .iter()
        .map(|(n, e)| (n.clone(), e.path.clone()))
        .collect();
    let base = abs.clone();
    let below = (!t.single_file)
        .then(|| RelPath::new(t.name).ok())
        .flatten()
        .unwrap_or_else(RelPath::root);
    let reached =
        tokio::task::spawn_blocking(move || inner::auto_export_reaches(&base, &below, &others))
            .await;
    match reached {
        Ok(found) => check_answer(t.path, id, found)?,
        Err(e) => {
            tracing::warn!(peer = %id.fmt_short(), "checking for an inner export: {e}");
            return Err((Code::Internal, "checking the path failed".into()));
        }
    }
    // A single file pushed to a name, not into a directory.
    let file_dest = t.single_file && push && !t.contents_only;
    let (shared, path) = (shared.clone(), t.path.to_vec());
    tokio::task::spawn_blocking(move || {
        add_auto_export(&shared, abs, push, file_dest, id, level, &path)
    })
    .await
    .unwrap_or_else(|e| Err((Code::Internal, format!("adding the export failed: {e}"))))
}

/// The blocking part of [`serve_all_paths`] for `abs`, the resolved form
/// of `path` (as typed, for messages); the export lists `id` with
/// `level`. `file_dest`: see [`auto_export_dir`].
fn add_auto_export(
    shared: &Shared,
    abs: PathBuf,
    push: bool,
    file_dest: bool,
    id: iroh::EndpointId,
    level: Access,
    path: &[u8],
) -> Result<(Snapshot, PathBuf), (Code, String)> {
    let refuse = |e: AutoExportError| match e {
        AutoExportError::NotFound => (
            Code::BadPath,
            format!("no such file or directory: {}", escape_path(path)),
        ),
        e => (Code::BadPath, e.to_string()),
    };
    let root = auto_export_dir(&abs, push, file_dest).map_err(refuse)?;
    // A symlink may lead to `/` or to a name that is not UTF-8.
    let canonical = std::fs::canonicalize(&root).map_err(|_| refuse(AutoExportError::NotFound))?;
    check_root(&canonical).map_err(refuse)?;
    let abs = match abs.strip_prefix(&root) {
        Ok(rest) if !rest.as_os_str().is_empty() => canonical.join(rest),
        _ => canonical.clone(),
    };
    let internal = |e: AuthzError| {
        tracing::warn!(peer = %id.fmt_short(), path = %canonical.display(), "{e:#}");
        (Code::Internal, format!("{e:#}"))
    };
    shared
        .authz
        .add_auto_export(&canonical, id, level)
        .map_err(internal)?;
    let fresh = shared.authz.snapshot().map_err(internal)?;
    Ok((fresh, abs))
}

/// The reply for a request that reaches the granted export `name` in
/// another spelling than its own.
fn inside_export(path: &[u8], name: &str) -> (Code, String) {
    (
        Code::BadPath,
        format!(
            "{} is inside export {name}; ask for it by that export",
            escape_path(path)
        ),
    )
}

/// The reply for a push this key may not make.
fn read_only() -> (Code, String) {
    (Code::ReadOnly, "export is read-only for this key".into())
}

/// The session `resolved` stands for, when `t` may have it: the name
/// joined below the path, and no push into a read-only export. A probe is
/// checked as a pull of its path.
fn admit<'a>(
    resolved: Resolved<'a>,
    t: &Target<'_>,
) -> Result<(Resolved<'a>, RelPath), (Code, String)> {
    // A single file's name is placed by `place_single`, not joined here.
    let rel = if t.single_file {
        if !is_one_component(t.name) {
            return Err((Code::BadPath, "invalid name".into()));
        }
        resolved.rel.clone()
    } else if t.name.is_empty() {
        resolved.rel.clone()
    } else {
        resolved
            .rel
            .join(t.name)
            .map_err(|_| (Code::BadPath, "invalid name".into()))?
    };
    if t.push && (resolved.export.read_only || !resolved.access.permits(true)) {
        return Err(read_only());
    }
    Ok((resolved, rel))
}

async fn reject(
    conn: &Connection,
    send: &mut SendStream,
    tap: &StreamTap,
    code: Code,
    message: String,
) -> Result<()> {
    let refusal = HelloReply {
        reply: Some(hello_reply::Reply::Error(Error {
            code: code as i32,
            message,
        })),
    };
    write_msg_tapped(send, &refusal, tap).await?;
    finish_answer(conn, send, tap, 2, b"rejected").await
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
    if settings.daemon.discovery.dht && opts.relay == Relay::Disabled {
        tracing::warn!(
            "dht is enabled but relays are disabled: DHT records will carry no addresses"
        );
    }
    Ok(build_endpoint(opts).await?)
}

/// Runs the daemon: clients, exports and grants are re-read from the config
/// file on every connection; the `[daemon]` settings are re-read and
/// applied when `reload` is signalled (`SIGHUP`).
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
    let Config {
        mut settings,
        exports,
        clients,
        ..
    } = Config::load(paths)?;
    if !listen.is_empty() {
        settings.daemon.listen = listen.clone();
    }
    let pass = &PassphraseSource {
        allow_open: settings.unsafe_config_allowed,
        ..pass.clone()
    };
    // With --passphrase-file and no key the daemon refuses below; refuse
    // here instead, before the lock writes anything.
    let identity = &settings.daemon.identity;
    crate::identity::refuse_to_generate_with_passphrase_file(identity, pass)
        .with_context(|| format!("identity {}", identity.display()))?;
    // Held until this function returns; a second daemon on the same state
    // dir fails here, before it binds anything.
    let _lock = crate::daemon_lock::acquire(&settings.state_dir)?;
    let authz = Arc::new(FileAuthorizer::new(paths.clone()));
    // Clients with `all_paths` get their exports added as they connect.
    let all_paths =
        settings.daemon.allow_all_paths && clients.entries.values().any(|c| c.all_paths.is_some());
    if exports.exports.is_empty() && !all_paths {
        tracing::warn!(
            "no exports defined in {}; run irsync export add",
            exports.path.display()
        );
    }
    let endpoint = endpoint_from_settings(&settings, pass).await?;
    // Printed only once startup can no longer fail (and again after every
    // reload): a parent process that reads these lines may dial at once.
    // `daemon.addr` follows each endpoint's address for `ssh-setup`.
    let state = settings.state_dir.clone();
    // The daemon logs warnings by default, so a terminal gets a greeting.
    if std::io::stdout().is_terminal() {
        print!(
            "{}",
            startup_banner(crate::build_info::LONG_VERSION.as_str(), &settings)
        );
    }
    let announce: Arc<dyn Fn(&Endpoint) + Send + Sync> = Arc::new(move |ep: &Endpoint| {
        crate::daemon_addr::spawn_writer(state.clone(), ep.clone());
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
    #[cfg(feature = "test-hooks")]
    {
        daemon.daemon_hooks().lock().unwrap().no_change_checks =
            crate::session::no_change_checks_from_env();
    }
    announce(daemon.endpoint());
    daemon.run(shutdown).await
}

/// What the daemon prints first when stdout is a terminal: its version and
/// which network services it uses.
pub(crate) fn startup_banner(version: &str, settings: &Settings) -> String {
    let d = &settings.daemon;
    let relay = match &d.relay {
        Relay::Default => "default (n0)".to_string(),
        r => r.to_string(),
    };
    let on: Vec<&str> = [
        ("dns", d.discovery.dns),
        ("dht", d.discovery.dht),
        ("mdns", d.discovery.mdns),
    ]
    .into_iter()
    .filter_map(|(n, e)| e.then_some(n))
    .collect();
    let discovery = if on.is_empty() {
        "none".to_string()
    } else {
        on.join(", ")
    };
    format!("irsync daemon {version} started\n  relay: {relay}\n  discovery: {discovery}\n")
}

/// The sockets the daemon announces: the fault layer's when it is on.
fn listen_addrs(ep: &Endpoint) -> Vec<SocketAddr> {
    #[cfg(feature = "netsim")]
    if let Some(a) = crate::net::netsim::bound_addr() {
        return vec![a.into()];
    }
    ep.bound_sockets()
}

#[cfg(test)]
mod banner_tests {
    use super::*;

    #[test]
    fn a_reload_names_each_setting_that_waits_for_a_restart() {
        let before = Settings::default();
        let started = StartOnly::of(&before);
        assert!(started.changed_in(&before).is_empty());
        let mut after = before.clone();
        after.state_dir = PathBuf::from("/elsewhere");
        after.daemon.debug_addr = Some(SocketAddr::from(([127, 0, 0, 1], 9464)));
        after.telemetry.otlp_endpoint = Some("http://c:4318".parse().unwrap());
        // A setting a reload applies is not named.
        after.daemon.portmap = !before.daemon.portmap;
        assert_eq!(
            started.changed_in(&after),
            [
                "state_dir changed to /elsewhere; it takes effect at the next start",
                "debug_addr changed to 127.0.0.1:9464; it takes effect at the next start",
                "[telemetry] changed; it takes effect at the next start",
            ]
        );
        let started = StartOnly::of(&after);
        let mut removed = after.clone();
        removed.daemon.debug_addr = None;
        assert_eq!(
            started.changed_in(&removed),
            ["debug_addr was removed; it takes effect at the next start"]
        );
    }

    #[test]
    fn banner_names_version_and_network() {
        let mut settings = Settings::default();
        settings.daemon.relay = Relay::Disabled;
        settings.daemon.discovery.dht = false;
        let b = startup_banner("9.9.9", &settings);
        assert!(b.starts_with("irsync daemon 9.9.9 started\n"));
        assert!(b.contains("relay: disabled"));
        assert!(b.contains("discovery: dns, mdns"));
    }
}
