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
        // Only macOS filesystems (APFS) refuse such names; on Linux any such
        // error is unexpected and must fail rather than skip silently.
        Err(e)
            if cfg!(target_os = "macos")
                && (matches!(
                    e.kind(),
                    std::io::ErrorKind::InvalidInput | std::io::ErrorKind::InvalidData
                ) || matches!(
                    e.raw_os_error(),
                    Some(x) if x == nix::libc::EILSEQ || x == nix::libc::EINVAL
                )) =>
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
        congestion: Default::default(),
    }
}

tokio::task_local! {
    /// Each stream's receive window for the endpoints bound inside
    /// [`with_stream_window`].
    static STREAM_WINDOW: u32;
}

/// Runs `f` with every endpoint it binds through [`loopback_endpoint`]
/// (a [`Harness`]'s daemon and clients among them) taking `window` bytes
/// per stream instead of the default 8 MiB.
pub async fn with_stream_window<F: std::future::Future>(window: u32, f: F) -> F::Output {
    STREAM_WINDOW.scope(window, f).await
}

/// The fault layer every endpoint bound inside [`with_netsim`] goes through.
#[cfg(feature = "netsim")]
#[derive(Clone)]
pub struct Netsim {
    pub board: std::sync::Arc<irsync::net::netsim::Switchboard>,
    pub profile: irsync::net::netsim::ProfileName,
    pub seed: u64,
}

#[cfg(feature = "netsim")]
tokio::task_local! {
    static NETSIM: Netsim;
}

/// Runs `f` with every endpoint it binds (a [`Harness`]'s daemon and
/// clients among them) on one in-process switchboard, through `profile`'s
/// faults. Under `#[tokio::test(start_paused = true)]` the run then skips
/// ahead whenever every task waits on a timer, so a sync that needs minutes
/// of network time takes seconds.
#[cfg(feature = "netsim")]
pub async fn with_netsim<F: std::future::Future>(
    profile: irsync::net::netsim::ProfileName,
    seed: u64,
    f: F,
) -> F::Output {
    let n = Netsim {
        board: irsync::net::netsim::Switchboard::new(),
        profile,
        seed,
    };
    NETSIM.scope(n, f).await
}

/// One offline endpoint for `side`: on the [`with_netsim`] switchboard when
/// inside one, else on loopback.
async fn endpoint_for(side: EndpointSide) -> Endpoint {
    #[cfg(feature = "netsim")]
    if let Ok(n) = NETSIM.try_with(Netsim::clone) {
        use irsync::net::netsim::{Side, Spec, WireChoice};
        // Each client endpoint gets its own fault stream.
        static STREAMS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let (side, stream) = match side {
            EndpointSide::Daemon => (Side::Daemon, 0),
            EndpointSide::Client => (
                Side::Client,
                STREAMS.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            ),
        };
        let spec = Spec {
            profile: n.profile,
            side,
            seed: n.seed,
            stream,
            acks: None,
        };
        return irsync::net::build_netsim_endpoint(
            offline_opts(),
            &spec,
            WireChoice::Memory(n.board),
        )
        .await
        .expect("bind netsim endpoint");
    }
    let _ = side;
    loopback_endpoint().await
}

/// Which end an endpoint is, for [`endpoint_for`].
#[derive(Clone, Copy)]
enum EndpointSide {
    Client,
    Daemon,
}

/// Binds one offline endpoint on loopback.
pub async fn loopback_endpoint() -> Endpoint {
    match STREAM_WINDOW.try_with(|w| *w) {
        Ok(w) => irsync::net::build_endpoint_with_stream_window(offline_opts(), w).await,
        Err(_) => build_endpoint(offline_opts()).await,
    }
    .expect("bind loopback endpoint")
}

/// Binds two offline endpoints on loopback. Dial one from the other with [`addr_of`].
pub async fn loopback_pair() -> (Endpoint, Endpoint) {
    (loopback_endpoint().await, loopback_endpoint().await)
}

/// [`addr_of`], or on a [`with_netsim`] switchboard the address of `ep`,
/// plugged in last.
fn dial_addr(ep: &Endpoint) -> EndpointAddr {
    #[cfg(feature = "netsim")]
    if let Ok(n) = NETSIM.try_with(Netsim::clone) {
        let port = n.board.last_port().expect("a port on the switchboard");
        let custom = irsync::net::netsim::custom_addr(port);
        return EndpointAddr::new(ep.id()).with_addrs([iroh::TransportAddr::Custom(custom)]);
    }
    addr_of(ep)
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
    /// Hooks for the daemon's side of every session (slow planner, in-flight
    /// cap, listing counters).
    pub hooks: std::sync::Arc<std::sync::Mutex<irsync::session::DaemonHooks>>,
    sender_parallel: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub _state: tempfile::TempDir,
}

impl DaemonHandle {
    /// Caps the streams a daemon sender runs at once. With every file in
    /// flight together, a byte-count fault would land with no file complete;
    /// bounded, files complete batch by batch before it fires.
    pub fn sender_parallel(&self, n: usize) {
        self.sender_parallel
            .store(n, std::sync::atomic::Ordering::SeqCst);
    }

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
                    fsync: false,
                    auto: false,
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
                    fsync: false,
                    auto: false,
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
    let ep = endpoint_for(EndpointSide::Daemon).await;
    let (addr, id) = (dial_addr(&ep), ep.id());
    let daemon = irsync::daemon::Daemon::with_home(&settings, authz, ep, home).expect("daemon");
    let hellos = daemon.hello_counter();
    let (sessions, hashed) = daemon.session_counters();
    let fault = daemon.fault_injector();
    let rerequests = daemon.rerequest_counter();
    let hosted = daemon.hosted_sessions();
    let events = daemon.receiver_events();
    let slow_finalize = daemon.slow_finalize();
    let sender_parallel = daemon.sender_parallel();
    let hooks = daemon.daemon_hooks();
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
        hooks,
        sender_parallel,
        _state: state,
    }
}

/// A client endpoint with its own fresh identity, offline on loopback.
pub async fn client_endpoint() -> Endpoint {
    endpoint_for(EndpointSide::Client).await
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
        Self::build(Some(base), |_| {}, |_| {}).await.0
    }

    /// A harness plus a second client endpoint the daemon also authorises.
    pub async fn with_guest() -> (Harness, Endpoint) {
        Self::build(None, |_| {}, |_| {}).await
    }

    /// A harness whose export is filled by `seed` before the daemon starts,
    /// with `warm_index` set on it.
    pub async fn warm(seed: impl FnOnce(&std::path::Path)) -> Harness {
        Self::build(None, |e| e.warm_index = true, seed).await.0
    }

    /// A harness whose export is first adjusted by `configure`.
    #[allow(dead_code)]
    pub async fn with_export(
        configure: impl FnOnce(&mut irsync::config::exports::Export),
    ) -> Harness {
        Self::build(None, configure, |_| {}).await.0
    }

    async fn build(
        base: Option<&std::path::Path>,
        configure: impl FnOnce(&mut irsync::config::exports::Export),
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
        let mut m = irsync::config::exports::Export {
            path: export.path().to_path_buf(),
            read_only: false,
            warm_index: false,
            fsync: false,
            auto: false,
            rw: Vec::new(),
            ro: Vec::new(),
        };
        configure(&mut m);
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
        self.run_reconnect_hello(opts, Self::hello(flow), ropts, local)
            .await
    }

    /// [`Self::run_reconnect`] with the caller's `hello` (its `session_id`
    /// is replaced on each redial).
    pub async fn run_reconnect_hello(
        &self,
        opts: &irsync::cli::SyncOptions,
        hello: irsync_proto::v1::Hello,
        ropts: irsync::session::reconnect::ReconnectOpts,
        local: irsync::session::LocalSide,
    ) -> (anyhow::Result<irsync::session::Outcome>, u32) {
        use irsync::session::reconnect::{run_with_reconnect, ConnState};
        let mut reconnects = 0;
        let out = run_with_reconnect(
            &self.client,
            self.daemon.addr.clone(),
            "daemon:m/",
            |id| {
                let mut h = hello.clone();
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

    /// Pushes the one file `src_file` to `dest_path` on the daemon (with a
    /// trailing `/` when `contents_only`): the session's root is the file's
    /// directory, and the CLI's filters apply on the client.
    pub async fn run_single_push(
        &self,
        opts: &irsync::cli::SyncOptions,
        src_file: &std::path::Path,
        dest_path: &[u8],
        contents_only: bool,
    ) -> anyhow::Result<irsync::session::Outcome> {
        let (parent, name) = irsync::session::single::split_last(src_file);
        let mut hello = Self::hello(Flow::Push);
        hello.path = dest_path.to_vec();
        hello.contents_only = contents_only;
        hello.single_file = true;
        hello.name = name.clone();
        let mut local = Self::side_for(&parent, self.client_state.path()).with_single(name);
        local.contents_only = false;
        local.filters = std::sync::Arc::new(irsync::filter::FilterSet::from_rules(&opts.filters)?);
        self.run_hello(opts, hello, local).await
    }

    /// What `remote_path` is on the daemon (one probe connection).
    pub async fn probe(
        &self,
        remote_path: &[u8],
    ) -> anyhow::Result<irsync_proto::v1::path_info::Kind> {
        let conn = self
            .client
            .connect(self.daemon.addr.clone(), irsync_proto::ALPN)
            .await?;
        let kind = irsync::session::single::probe(&conn, remote_path).await;
        conn.close(0u32.into(), b"done");
        kind
    }

    /// Pulls the one file at `remote_path` to `dest` (with a trailing `/`
    /// when `contents_only`), as the CLI does: probes first, so nothing is
    /// created locally unless the remote path is a file, then places it by
    /// rsync's rules and pulls into the directory that holds it.
    pub async fn run_single_pull(
        &self,
        opts: &irsync::cli::SyncOptions,
        remote_path: &[u8],
        dest: &std::path::Path,
        contents_only: bool,
    ) -> anyhow::Result<irsync::session::Outcome> {
        use irsync::session::single::{place_file, split_last};
        use irsync_proto::v1::path_info::Kind;
        use std::os::unix::ffi::OsStrExt;
        let kind = self.probe(remote_path).await?;
        anyhow::ensure!(kind == Kind::File, "remote path is {kind:?}, not a file");
        let remote = std::path::Path::new(std::ffi::OsStr::from_bytes(remote_path));
        let (_, basename) = split_last(remote);
        let placed = place_file(dest, contents_only, &basename, true)?;
        let mut hello = Self::hello(Flow::Pull);
        hello.path = remote_path.to_vec();
        hello.contents_only = false;
        hello.single_file = true;
        hello.name = placed.name.clone();
        let mut local =
            Self::side_for(&placed.root, self.client_state.path()).with_single(placed.name);
        local.contents_only = false;
        self.run_hello(opts, hello, local).await
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

/// Kills a daemon started with [`spawn_daemon_binary`] when the test ends.
pub struct DaemonGuard(pub std::process::Child);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Runs the built `irsync` with `home` as HOME (and the state under it),
/// bounded to 30 s so a protocol hang fails the test instead of blocking it.
pub fn irsync_bin(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    irsync_bin_in(home, None, args)
}

/// [`irsync_bin`] run from the directory `cwd` (`None`: the test's own).
pub fn irsync_bin_in(
    home: &std::path::Path,
    cwd: Option<&std::path::Path>,
    args: &[&str],
) -> std::process::Output {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_irsync"));
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let mut child = cmd
        .args(args)
        .env("HOME", home)
        .env("XDG_STATE_HOME", home.join("xdg"))
        // An unknown host is tried as an ssh destination; never reach out.
        .env("IRSYNC_RSH", "false")
        .env_remove("IRSYNC_PASSPHRASE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("irsync {args:?} did not finish within 30 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

/// A running `irsync daemon` process (see [`spawn_daemon_binary`]).
pub struct BinaryDaemon {
    pub guard: DaemonGuard,
    pub id: iroh::EndpointId,
    /// Where it listened at startup.
    pub listen: std::net::SocketAddr,
    /// Its stderr.
    pub log: std::path::PathBuf,
    stdout: std::sync::mpsc::Receiver<String>,
}

impl BinaryDaemon {
    pub fn pid(&self) -> u32 {
        self.guard.0.id()
    }

    /// Sends `SIGHUP`: the daemon re-reads its settings.
    pub fn reload(&self) {
        let status = std::process::Command::new("kill")
            .args(["-HUP", &self.pid().to_string()])
            .status()
            .unwrap();
        assert!(status.success(), "kill -HUP failed");
    }

    /// The next `listening on:` line the daemon prints (after a reload).
    pub fn next_listen(&self) -> std::net::SocketAddr {
        loop {
            let line = self
                .stdout
                .recv_timeout(Duration::from_secs(15))
                .expect("daemon printed no 'listening on:' line within 15 s");
            if let Some(rest) = line.strip_prefix("listening on: ") {
                return rest.trim().parse().unwrap();
            }
            assert!(
                line.starts_with("endpoint id: "),
                "unexpected stdout line {line:?}"
            );
        }
    }
}

/// Starts the built `irsync daemon` with `server_home` as HOME, isolated
/// (no relay, no lookup, no port mapper) on a loopback port, and waits for
/// it to say where it listens.
pub fn spawn_daemon_binary(server_home: &std::path::Path) -> BinaryDaemon {
    spawn_daemon_binary_env(server_home, &[])
}

/// [`spawn_daemon_binary`] with extra environment variables for the daemon.
pub fn spawn_daemon_binary_env(
    server_home: &std::path::Path,
    env: &[(&str, &str)],
) -> BinaryDaemon {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let cfg = server_home.join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join("irsync.toml"),
        "[daemon]\nrelay_mode = \"disabled\"\nportmap = false\n\
         [daemon.discovery]\ndht = false\ndns = false\nmdns = false\n",
    )
    .unwrap();
    let log = server_home.join("daemon.stderr");
    let child = Command::new(env!("CARGO_BIN_EXE_irsync"))
        .args(["daemon", "--listen", "127.0.0.1:0"])
        .env("HOME", server_home)
        .env("XDG_STATE_HOME", server_home.join("xdg"))
        .env_remove("IRSYNC_PASSPHRASE")
        .env_remove("RUST_LOG")
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(std::fs::File::create(&log).unwrap())
        .spawn()
        .unwrap();
    let mut daemon = DaemonGuard(child);
    let stdout = daemon.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let (mut id, mut listen) = (None, None);
    while listen.is_none() {
        let line = rx
            .recv_timeout(Duration::from_secs(15))
            .expect("daemon printed no 'listening on:' line within 15 s");
        if let Some(rest) = line.strip_prefix("endpoint id: ") {
            id = Some(irsync::identity::parse_endpoint_id(rest.trim()).unwrap());
        } else if let Some(rest) = line.strip_prefix("listening on: ") {
            listen = Some(rest.trim().parse::<std::net::SocketAddr>().unwrap());
        } else {
            panic!("unexpected stdout line {line:?}");
        }
    }
    BinaryDaemon {
        guard: daemon,
        id: id.unwrap(),
        listen: listen.unwrap(),
        log,
        stdout: rx,
    }
}
