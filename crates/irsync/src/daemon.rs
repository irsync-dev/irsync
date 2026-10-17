//! The long-running side: accepts connections, checks the peer's key before reading
//! anything, resolves and validates the requested module, then hands off to a session.

use std::collections::{BTreeMap, HashMap};
use std::io;
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
use crate::filter::FilterSet;
use crate::fs::paths::{is_contained, RelPath};
use crate::identity::{generate_openssh, load_secret_key};
use crate::index::{Index, IndexHandle};
use crate::net::frame::{read_msg, write_msg};
use crate::net::{build_endpoint, EndpointOpts, RelayChoice};
use crate::scan::WalkOpts;
use crate::session::engine::serve_daemon;
use crate::session::DaemonSessions;
#[cfg(feature = "test-hooks")]
use crate::session::FaultInjector;
use irsync_proto::v1::SessionPlan;
use prost::Message;

/// How often each module is garbage-collected, and the delay before the first
/// run (so startup stays quick).
const GC_INTERVAL: Duration = Duration::from_secs(3600);
const GC_FIRST_RUN: Duration = Duration::from_secs(60);

const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

struct Shared {
    authz: Arc<dyn Authorizer>,
    modules: BTreeMap<String, Module>,
    indexes: HashMap<String, IndexHandle>,
    /// Age after which dormant sessions and abandoned partial files are collected.
    dormant: Duration,
    /// Hosted sessions (one connection per session id) and test hooks.
    hosting: DaemonSessions,
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
                dormant: cfg.dormant_session_timeout,
                hosting: DaemonSessions {
                    #[cfg(feature = "test-hooks")]
                    fault: Some(Arc::default()),
                    ..DaemonSessions::default()
                },
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

    /// The fault injector every session of this daemon consults.
    #[cfg(feature = "test-hooks")]
    pub fn fault_injector(&self) -> Arc<FaultInjector> {
        self.shared.hosting.fault.clone().unwrap_or_default()
    }

    /// `NeedChunks` re-requests sent by this daemon's receivers.
    #[cfg(feature = "test-hooks")]
    pub fn rerequest_counter(&self) -> Arc<std::sync::atomic::AtomicU64> {
        self.shared.hosting.rerequests.clone()
    }

    /// The event log every daemon receiver writes to.
    #[cfg(feature = "test-hooks")]
    pub fn receiver_events(&self) -> crate::session::EventLog {
        self.shared.hosting.events.clone()
    }

    /// Ids of every session hosted so far (fresh or resumed), in order.
    #[cfg(feature = "test-hooks")]
    pub fn hosted_sessions(&self) -> Arc<std::sync::Mutex<Vec<[u8; 16]>>> {
        self.shared.hosting.registry.hosted.clone()
    }

    pub async fn run(self, shutdown: CancellationToken) -> Result<()> {
        for (name, module) in &self.shared.modules {
            let Some(index) = self.shared.indexes.get(name).cloned() else {
                continue;
            };
            let (name, root) = (name.clone(), module.path.clone());
            let (dormant, stop) = (self.shared.dormant, shutdown.clone());
            tokio::spawn(async move {
                let first = tokio::time::Instant::now() + GC_FIRST_RUN;
                let mut tick = tokio::time::interval_at(first, GC_INTERVAL);
                loop {
                    tokio::select! {
                        _ = stop.cancelled() => break,
                        _ = tick.tick() => {}
                    }
                    match gc(index.clone(), &root, dormant, false).await {
                        Ok(r) => tracing::info!(
                            module = %name,
                            parts_removed = r.parts_removed.len(),
                            parts_kept = r.parts_kept,
                            sessions_removed = r.sessions_removed,
                            "gc finished"
                        ),
                        Err(e) => tracing::warn!(module = %name, "gc failed: {e}"),
                    }
                }
            });
        }
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
            let hosting = shared.hosting.clone();
            let res = serve_daemon(conn, send, recv, root, hello, index, hosting).await;
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

/// What a [`gc`] run removed (or, on a dry run, would remove).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
    /// Relative paths of the removed `.irsync.<name>.part` files.
    pub parts_removed: Vec<RelPath>,
    /// Stale-looking partial records kept because a session still references them.
    pub parts_kept: usize,
    /// Sessions removed (on a dry run: that would be removed).
    pub sessions_removed: u32,
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

/// The `.part` file the receiver keeps for destination `rel`.
fn part_of(rel: &RelPath) -> Option<RelPath> {
    let b = rel.as_bytes();
    let i = b.iter().rposition(|&c| c == b'/').map_or(0, |i| i + 1);
    let mut p = b[..i].to_vec();
    p.extend_from_slice(b".irsync.");
    p.extend_from_slice(&b[i..]);
    p.extend_from_slice(b".part");
    RelPath::new(&p).ok()
}

/// The destination a `.part` path belongs to.
fn dest_of(part: &RelPath) -> Option<RelPath> {
    let b = part.as_bytes();
    let i = b.iter().rposition(|&c| c == b'/').map_or(0, |i| i + 1);
    let name = &b[i..];
    if !is_part_name(name) {
        return None;
    }
    let mut d = b[..i].to_vec();
    d.extend_from_slice(&name[".irsync.".len()..name.len() - ".part".len()]);
    RelPath::new(&d).ok()
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
        },
    );
    let mut on_disk = Vec::new();
    while let Some(item) = rx.recv().await {
        let is_reg = item.entry.kind() == irsync_proto::v1::file_entry::Kind::Regular;
        let named = item.path.components().last().is_some_and(is_part_name);
        if is_reg && named && item.entry.mtime_sec <= cutoff {
            on_disk.push(item.path);
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
        // Plan paths are relative to the session root, which may sit below the
        // index root: match on a whole-component suffix to stay conservative.
        let is_ref = |rel: &RelPath| {
            let b = rel.as_bytes();
            referenced.iter().any(|p| {
                !p.is_empty()
                    && (b == &p[..]
                        || (b.len() > p.len()
                            && b.ends_with(p)
                            && b[b.len() - p.len() - 1] == b'/'))
            })
        };
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
        // Orphans: stale files with no record.
        for part in on_disk {
            if handled.contains(&part) {
                continue;
            }
            let Some(dest) = dest_of(&part) else { continue };
            if ix.partial(&dest).map_err(io_err)?.is_some() {
                continue;
            }
            if is_ref(&dest) || is_ref(&part) {
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
        report.errors.extend(walk_errors);
        Ok(report)
    })
    .await
    .map_err(io_err)?
}
