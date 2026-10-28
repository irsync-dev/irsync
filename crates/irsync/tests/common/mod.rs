//! Shared helpers for integration tests that need real iroh endpoints.
#![allow(dead_code)]

/// Creates a file named with raw non-UTF-8 `name` bytes in `dir`. Returns `None`
/// (after a note on stderr) when the filesystem refuses such names (e.g. APFS),
/// so callers can skip; panics on any other error.
pub fn try_create_non_utf8(dir: &std::path::Path, name: &[u8]) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let p = dir.join(std::ffi::OsStr::from_bytes(name));
    match std::fs::write(&p, b"x") {
        Ok(()) => Some(p),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::InvalidInput | std::io::ErrorKind::InvalidData
            ) || matches!(
                e.raw_os_error(),
                Some(x) if x == nix::libc::EILSEQ || x == nix::libc::EINVAL
            ) =>
        {
            eprintln!("skipping non-UTF-8 name: filesystem refused");
            None
        }
        Err(e) => panic!("creating non-UTF-8 name failed: {e}"),
    }
}

use std::future::Future;
use std::time::Duration;

use iroh::{Endpoint, EndpointAddr, SecretKey};
use irsync::net::{build_endpoint, EndpointOpts, RelayChoice};

/// Awaits `fut`, panicking after 10 s so a regression fails instead of hanging.
pub async fn within<F: Future>(fut: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(10), fut)
        .await
        .expect("timed out after 10 s")
}

/// Options for a fully offline endpoint: no relay, no address lookup, loopback bind.
pub fn offline_opts() -> EndpointOpts {
    EndpointOpts {
        secret: SecretKey::generate(),
        relay: RelayChoice::Disabled,
        dht: false,
        dns: false,
        mdns: false,
        publish: false,
        bind_addrs: vec!["127.0.0.1:0".parse().unwrap()],
        portmap: false,
    }
}

/// Binds one offline endpoint on loopback.
pub async fn loopback_endpoint() -> Endpoint {
    build_endpoint(offline_opts())
        .await
        .expect("bind loopback endpoint")
}

/// Binds two offline endpoints on loopback. Dial one from the other with [`addr_of`].
pub async fn loopback_pair() -> (Endpoint, Endpoint) {
    (loopback_endpoint().await, loopback_endpoint().await)
}

/// The endpoint's id plus its direct (loopback) addresses: enough to dial it offline.
pub fn addr_of(ep: &Endpoint) -> EndpointAddr {
    let addr = ep.addr();
    assert!(
        addr.ip_addrs().next().is_some(),
        "endpoint has no direct addresses"
    );
    assert!(
        addr.ip_addrs().all(|a| a.ip().is_loopback()),
        "offline endpoint bound beyond loopback: {addr:?}"
    );
    addr
}

/// Connects `client` to `server` over loopback and returns both ends of the connection
/// as `(client_side, server_side)`.
pub async fn connect_pair(
    client: &Endpoint,
    server: &Endpoint,
) -> (iroh::endpoint::Connection, iroh::endpoint::Connection) {
    let addr = addr_of(server);
    let accept = async {
        server
            .accept()
            .await
            .expect("incoming connection")
            .await
            .expect("server handshake")
    };
    let connect = async {
        client
            .connect(addr, irsync_proto::ALPN)
            .await
            .expect("client connect")
    };
    let (c, s) = within(async { tokio::join!(connect, accept) }).await;
    (c, s)
}

/// A running in-process daemon: its dialable address, its id, and the task running it.
pub struct DaemonHandle {
    pub addr: EndpointAddr,
    pub id: iroh::EndpointId,
    pub task: tokio::task::JoinHandle<anyhow::Result<()>>,
    pub shutdown: tokio_util::sync::CancellationToken,
    pub hellos: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Sessions the daemon has finished (successfully or not).
    pub sessions: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Files the daemon's hashers read, summed over finished sessions.
    pub hashed: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Drops a connection once the daemon's data byte count crosses a limit.
    pub fault: std::sync::Arc<irsync::session::FaultInjector>,
    /// `NeedChunks` re-requests sent by the daemon's receivers.
    pub rerequests: std::sync::Arc<std::sync::atomic::AtomicU64>,
    hosted: std::sync::Arc<std::sync::Mutex<Vec<[u8; 16]>>>,
    /// The daemon receivers' event log (`delete:<path>`, `plan:<path>`).
    pub events: irsync::session::EventLog,
    /// Set to make every daemon receiver's finalize take that much longer.
    pub slow_finalize: std::sync::Arc<std::sync::Mutex<Option<Duration>>>,
    pub _state: tempfile::TempDir,
}

impl DaemonHandle {
    /// Ids of the sessions the daemon has hosted (fresh or resumed), in order, deduplicated.
    pub fn session_ids(&self) -> Vec<[u8; 16]> {
        let mut v: Vec<[u8; 16]> = Vec::new();
        for id in self.hosted.lock().unwrap().iter() {
            if !v.contains(id) {
                v.push(*id);
            }
        }
        v
    }

    /// Waits until every connection that sent a Hello has been fully handled.
    pub async fn settle(&self) {
        use std::sync::atomic::Ordering::SeqCst;
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while self.sessions.load(SeqCst) < self.hellos.load(SeqCst) {
            assert!(
                std::time::Instant::now() < deadline,
                "daemon sessions did not finish"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

/// Starts a daemon on a loopback endpoint serving `exports` (name -> (path, read_only)).
pub async fn spawn_daemon(
    exports: Vec<(&str, std::path::PathBuf, bool)>,
    authz: irsync::authz::MemoryAuthorizer,
) -> DaemonHandle {
    use irsync::config::exports::Export;
    let exports = exports
        .into_iter()
        .map(|(name, path, read_only)| {
            (
                name,
                Export {
                    path,
                    read_only,
                    warm_index: false,
                    rw: Vec::new(),
                    ro: Vec::new(),
                },
            )
        })
        .collect();
    spawn_daemon_exports(exports, authz).await
}

/// As [`spawn_daemon`], with `home` as the directory relative remote paths
/// resolve against.
pub async fn spawn_daemon_home(
    exports: Vec<(&str, std::path::PathBuf, bool)>,
    authz: irsync::authz::MemoryAuthorizer,
    home: Option<std::path::PathBuf>,
) -> DaemonHandle {
    use irsync::config::exports::Export;
    let map = exports
        .into_iter()
        .map(|(name, path, read_only)| {
            (
                name.to_string(),
                Export {
                    path,
                    read_only,
                    warm_index: false,
                    rw: Vec::new(),
                    ro: Vec::new(),
                },
            )
        })
        .collect();
    spawn_daemon_full(
        std::sync::Arc::new(authz.with_exports(map)),
        tempfile::tempdir().unwrap(),
        home,
    )
    .await
}

/// Starts a daemon on a loopback endpoint serving `exports` as configured.
pub async fn spawn_daemon_exports(
    exports: Vec<(&str, irsync::config::exports::Export)>,
    authz: irsync::authz::MemoryAuthorizer,
) -> DaemonHandle {
    let map = exports
        .into_iter()
        .map(|(name, export)| (name.to_string(), export))
        .collect();
    spawn_daemon_with(std::sync::Arc::new(authz.with_exports(map))).await
}

/// Starts a daemon on a loopback endpoint whose exports and grants come from `authz`.
pub async fn spawn_daemon_with(
    authz: std::sync::Arc<dyn irsync::authz::Authorizer>,
) -> DaemonHandle {
    spawn_daemon_with_state(authz, tempfile::tempdir().unwrap()).await
}

/// As [`spawn_daemon_with`], keeping its state (indexes) in `state`.
pub async fn spawn_daemon_with_state(
    authz: std::sync::Arc<dyn irsync::authz::Authorizer>,
    state: tempfile::TempDir,
) -> DaemonHandle {
    spawn_daemon_full(authz, state, dirs::home_dir()).await
}

/// As [`spawn_daemon_with_state`], with the daemon's home directory.
pub async fn spawn_daemon_full(
    authz: std::sync::Arc<dyn irsync::authz::Authorizer>,
    state: tempfile::TempDir,
    home: Option<std::path::PathBuf>,
) -> DaemonHandle {
    use irsync::config::settings::Settings;
    let settings = Settings {
        state_dir: state.path().to_path_buf(),
        ..Settings::default()
    };
    let ep = loopback_endpoint().await;
    let (addr, id) = (addr_of(&ep), ep.id());
    let daemon = irsync::daemon::Daemon::with_home(&settings, authz, ep, home).expect("daemon");
    let hellos = daemon.hello_counter();
    let (sessions, hashed) = daemon.session_counters();
    let fault = daemon.fault_injector();
    let rerequests = daemon.rerequest_counter();
    let hosted = daemon.hosted_sessions();
    let events = daemon.receiver_events();
    let slow_finalize = daemon.slow_finalize();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let task = tokio::spawn(daemon.run(shutdown.clone()));
    DaemonHandle {
        addr,
        id,
        task,
        shutdown,
        hellos,
        sessions,
        hashed,
        fault,
        rerequests,
        hosted,
        events,
        slow_finalize,
        _state: state,
    }
}

/// A client endpoint with its own fresh identity, offline on loopback.
pub async fn client_endpoint() -> Endpoint {
    loopback_endpoint().await
}

// ---------------------------------------------------------------------------
// End-to-end sync helpers.

/// Which way the data flows in a [`Harness`] run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Client sends: local dir -> daemon export.
    Push,
    /// Daemon sends: daemon export -> local dir.
    Pull,
}

/// `irsync -a` options as the CLI would build them.
pub fn archive_opts() -> irsync::cli::SyncOptions {
    let args = irsync::cli::Args::try_parse_from(["irsync", "-a", "src/", "host:m/"]).unwrap();
    irsync::cli::SyncOptions::from_args(&args)
}

/// A daemon serving export `m` (a temp dir) to one authorised client, plus the
/// client's own local dir and state dir. Runs can be repeated.
pub struct Harness {
    pub daemon: DaemonHandle,
    pub client: Endpoint,
    pub export: tempfile::TempDir,
    pub local: tempfile::TempDir,
    pub client_state: tempfile::TempDir,
}

impl Harness {
    pub async fn new() -> Harness {
        Self::with_guest().await.0
    }

    /// A harness whose export and local dirs are created under `base`
    /// (a filesystem with properties the default temp dir may lack).
    pub async fn new_in(base: &std::path::Path) -> Harness {
        Self::build(Some(base), false, |_| {}).await.0
    }

    /// A harness plus a second client endpoint the daemon also authorises.
    pub async fn with_guest() -> (Harness, Endpoint) {
        Self::build(None, false, |_| {}).await
    }

    /// A harness whose export is filled by `seed` before the daemon starts,
    /// with `warm_index` set on it.
    pub async fn warm(seed: impl FnOnce(&std::path::Path)) -> Harness {
        Self::build(None, true, seed).await.0
    }

    async fn build(
        base: Option<&std::path::Path>,
        warm_index: bool,
        seed: impl FnOnce(&std::path::Path),
    ) -> (Harness, Endpoint) {
        let tmp = || match base {
            Some(b) => tempfile::tempdir_in(b).unwrap(),
            None => tempfile::tempdir().unwrap(),
        };
        use irsync::authz::{Access, Grants, MemoryAuthorizer};
        let client = client_endpoint().await;
        let guest = client_endpoint().await;
        let export = tmp();
        seed(export.path());
        let grants = Grants {
            all: Some(Access::ReadWrite),
            ..Grants::default()
        };
        let authz = MemoryAuthorizer::single(client.id(), grants.clone()).with(guest.id(), grants);
        let m = irsync::config::exports::Export {
            path: export.path().to_path_buf(),
            read_only: false,
            warm_index,
            rw: Vec::new(),
            ro: Vec::new(),
        };
        let daemon = spawn_daemon_exports(vec![("m", m)], authz).await;
        let h = Harness {
            daemon,
            client,
            export,
            local: tmp(),
            client_state: tempfile::tempdir().unwrap(),
        };
        (h, guest)
    }

    pub fn src(&self, flow: Flow) -> &std::path::Path {
        match flow {
            Flow::Push => self.local.path(),
            Flow::Pull => self.export.path(),
        }
    }

    pub fn dst(&self, flow: Flow) -> &std::path::Path {
        match flow {
            Flow::Push => self.export.path(),
            Flow::Pull => self.local.path(),
        }
    }

    /// Hello for the whole export, contents only (`host:m/`).
    pub fn hello(flow: Flow) -> irsync_proto::v1::Hello {
        use irsync_proto::chunk::CHUNK_PARAMS;
        use irsync_proto::v1::hello::Direction;
        irsync_proto::v1::Hello {
            protocol_version: irsync_proto::PROTOCOL_VERSION,
            session_id: vec![],
            path: b"m".to_vec(),
            direction: match flow {
                Flow::Push => Direction::ClientSends,
                Flow::Pull => Direction::DaemonSends,
            } as i32,
            chunk_min: CHUNK_PARAMS.min,
            chunk_avg: CHUNK_PARAMS.avg,
            chunk_max: CHUNK_PARAMS.max,
            contents_only: true,
            ..Default::default()
        }
    }

    /// The client's side of a session over the local dir (contents only).
    pub fn local_side(&self) -> irsync::session::LocalSide {
        Self::side_for(self.local.path(), self.client_state.path())
    }

    /// A client side over `dir` (contents only) with its index under `state`.
    pub fn side_for(dir: &std::path::Path, state: &std::path::Path) -> irsync::session::LocalSide {
        let index = irsync::index::Index::open(state, dir)
            .expect("open client index")
            .into_handle();
        irsync::session::LocalSide::new(
            dir.to_path_buf(),
            true,
            index,
            std::sync::Arc::new(irsync::filter::FilterSet::new(vec![])),
        )
    }

    /// One `run_with_reconnect` (a client "process"); returns its result and
    /// how many times it reconnected. Waits until the daemon has finished
    /// with every connection.
    pub async fn run_reconnect(
        &self,
        opts: &irsync::cli::SyncOptions,
        flow: Flow,
        ropts: irsync::session::reconnect::ReconnectOpts,
        local: irsync::session::LocalSide,
    ) -> (anyhow::Result<irsync::session::Outcome>, u32) {
        use irsync::session::reconnect::{run_with_reconnect, ConnState};
        let mut reconnects = 0;
        let out = run_with_reconnect(
            &self.client,
            self.daemon.addr.clone(),
            |id| {
                let mut h = Self::hello(flow);
                h.session_id = id.map(|i| i.to_vec()).unwrap_or_default();
                h
            },
            local,
            opts,
            ropts,
            |s| {
                if matches!(s, ConnState::Reconnecting { .. }) {
                    reconnects += 1;
                }
            },
        )
        .await;
        self.daemon.settle().await;
        (out, reconnects)
    }

    /// One complete session; waits until the daemon has finished its side too.
    pub async fn run(
        &self,
        opts: &irsync::cli::SyncOptions,
        flow: Flow,
    ) -> anyhow::Result<irsync::session::Outcome> {
        self.run_side(opts, flow, self.local_side()).await
    }

    pub async fn run_side(
        &self,
        opts: &irsync::cli::SyncOptions,
        flow: Flow,
        local: irsync::session::LocalSide,
    ) -> anyhow::Result<irsync::session::Outcome> {
        self.run_hello(opts, Self::hello(flow), local).await
    }

    /// Like [`Harness::run_side`] with a caller-built `Hello` (e.g. a sub-path).
    pub async fn run_hello(
        &self,
        opts: &irsync::cli::SyncOptions,
        hello: irsync_proto::v1::Hello,
        local: irsync::session::LocalSide,
    ) -> anyhow::Result<irsync::session::Outcome> {
        use std::sync::atomic::Ordering;
        let before = self.daemon.sessions.load(Ordering::SeqCst);
        let conn = self
            .client
            .connect(self.daemon.addr.clone(), irsync_proto::ALPN)
            .await?;
        let out = irsync::session::engine::run_client(conn.clone(), hello, local, opts).await;
        conn.close(0u32.into(), b"done");
        // The daemon's side ends right after ours; wait for its bookkeeping.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while self.daemon.sessions.load(Ordering::SeqCst) == before {
            assert!(
                std::time::Instant::now() < deadline,
                "daemon session did not finish"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        out
    }

    /// The daemon's index for export `m` (a second connection to its database).
    pub fn daemon_index(&self) -> irsync::index::Index {
        irsync::index::Index::open(self.daemon._state.path(), self.export.path())
            .expect("open daemon index")
    }

    /// Files the daemon's hashers have read so far (all finished sessions).
    pub fn daemon_files_hashed(&self) -> u64 {
        self.daemon.hashed.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Builds the `src` and `dst` trees, then runs one session in direction `flow`.
pub async fn sync_trees(
    src: impl FnOnce(&std::path::Path),
    dst: impl FnOnce(&std::path::Path),
    opts: &irsync::cli::SyncOptions,
    flow: Flow,
) -> (irsync::session::Outcome, Harness) {
    let h = Harness::new().await;
    src(h.src(flow));
    dst(h.dst(flow));
    let out = h.run(opts, flow).await.expect("sync session");
    (out, h)
}

/// Asserts both trees hold the same names, kinds, bytes and link targets, plus
/// mtimes (when `preserve.times`) and permission bits (when `preserve.perms`).
pub fn assert_trees_equal(
    a: &std::path::Path,
    b: &std::path::Path,
    preserve: &irsync::cli::Preserve,
) {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let (ma, mb) = (
        std::fs::symlink_metadata(a).unwrap(),
        std::fs::symlink_metadata(b).unwrap(),
    );
    let (ta, tb) = (ma.file_type(), mb.file_type());
    let kind = |t: std::fs::FileType| {
        if t.is_dir() {
            "dir"
        } else if t.is_symlink() {
            "symlink"
        } else if t.is_file() {
            "file"
        } else if t.is_fifo() {
            "fifo"
        } else {
            "other"
        }
    };
    assert_eq!(kind(ta), kind(tb), "kind differs: {a:?} vs {b:?}");
    if ta.is_symlink() {
        assert_eq!(
            std::fs::read_link(a).unwrap(),
            std::fs::read_link(b).unwrap(),
            "link target differs: {a:?}"
        );
    } else if preserve.perms {
        assert_eq!(
            ma.mode() & 0o7777,
            mb.mode() & 0o7777,
            "mode differs: {a:?}"
        );
    }
    if preserve.times {
        assert_eq!(
            (ma.mtime(), ma.mtime_nsec()),
            (mb.mtime(), mb.mtime_nsec()),
            "mtime differs: {a:?} vs {b:?}"
        );
    }
    if ta.is_file() {
        assert!(
            std::fs::read(a).unwrap() == std::fs::read(b).unwrap(),
            "contents differ: {a:?} vs {b:?}"
        );
    }
    if ta.is_dir() {
        let names = |d: &std::path::Path| {
            let mut v: Vec<std::ffi::OsString> = std::fs::read_dir(d)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            v.sort();
            v
        };
        let (na, nb) = (names(a), names(b));
        assert_eq!(na, nb, "entries differ: {a:?} vs {b:?}");
        for n in na {
            assert_trees_equal(&a.join(&n), &b.join(&n), preserve);
        }
    }
}
