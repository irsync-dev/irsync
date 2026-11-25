//! Shared helpers for integration tests that need real iroh endpoints.
#![allow(dead_code)]

pub mod bin;
pub use bin::{irsync_command, StateHome};
pub mod fixture;

/// Creates a file named with raw non-UTF-8 `name` bytes in `dir`. Returns `None`
/// (after a note on stderr) when the filesystem refuses such names, so callers
/// can skip: APFS on macOS, and on Linux ZFS with `utf8only=on`, which answers
/// `EILSEQ`. Panics on any other error, and on a Linux refusal when `CI` is
/// set: the runners' filesystem takes such names, so a skip there would hide
/// the tests.
#[cfg(unix)]
pub fn try_create_non_utf8(dir: &std::path::Path, name: &[u8]) -> Option<std::path::PathBuf> {
    use irsync::platform::testutil::{errno, name_from_bytes};
    let p = dir.join(name_from_bytes(name));
    match std::fs::write(&p, b"x") {
        Ok(()) => Some(p),
        Err(e)
            if cfg!(target_os = "macos")
                && (matches!(
                    e.kind(),
                    std::io::ErrorKind::InvalidInput | std::io::ErrorKind::InvalidData
                ) || matches!(
                    e.raw_os_error(),
                    Some(x) if x == errno::EILSEQ || x == errno::EINVAL
                )) =>
        {
            eprintln!("skipping non-UTF-8 name: filesystem refused");
            None
        }
        Err(e) if e.raw_os_error() == Some(errno::EILSEQ) && std::env::var_os("CI").is_none() => {
            eprintln!("skipping non-UTF-8 name: filesystem refused (ZFS with utf8only=on?)");
            None
        }
        Err(e) => panic!("creating non-UTF-8 name failed: {e}"),
    }
}

/// Whether this process may make symlinks. Always on Unix; on Windows it
/// needs the privilege or Developer Mode, and a test that needs symlinks
/// returns early when not (a line that starts `skipping:` says so). With
/// `CI` set, not being able to is a failure.
pub fn symlinks_available() -> bool {
    #[cfg(unix)]
    let available = true;
    #[cfg(windows)]
    let available = irsync::platform::testutil::symlinks_available();
    available
}

/// Removes the link at `link` itself, never what it leads to. Windows
/// makes a link to a directory as a directory, removed as one; Unix
/// removes any link as a file.
pub fn remove_link(link: &std::path::Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        let t = std::fs::symlink_metadata(link)?.file_type();
        if t.is_dir() || t.is_symlink_dir() {
            return std::fs::remove_dir(link);
        }
    }
    std::fs::remove_file(link)
}

/// Opens `p` for reading and writing as another program would, sharing
/// what `share` says (1 read, 2 write, 4 delete).
#[cfg(windows)]
pub fn open_sharing(p: &std::path::Path, share: u32) -> std::fs::File {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(share)
        .open(p)
        .unwrap()
}

/// A Windows name from its UTF-16 units, which may hold a lone surrogate.
#[cfg(windows)]
pub fn name_from_units(units: &[u16]) -> std::ffi::OsString {
    use std::os::windows::ffi::OsStringExt;
    std::ffi::OsString::from_wide(units)
}

/// A drive letter made with `subst` (no privilege needed), removed again
/// when dropped.
#[cfg(windows)]
pub struct Subst(pub char);

#[cfg(windows)]
impl Drop for Subst {
    /// Removes the drive, once: a letter given back is free for another
    /// test to take, so it is never removed a second time.
    fn drop(&mut self) {
        let _ = std::process::Command::new("subst")
            .arg(format!("{}:", self.0))
            .arg("/d")
            .output();
    }
}

/// A free drive letter `subst` makes for `dir`, if one takes it (a letter
/// held by a mapped network drive that is away is refused); removed when
/// the result is dropped.
#[cfg(windows)]
pub fn subst_of(dir: &std::path::Path) -> Option<Subst> {
    ('D'..='Z').rev().find_map(|l| {
        if std::fs::metadata(format!(r"{l}:\")).is_ok() {
            return None;
        }
        let made = std::process::Command::new("subst")
            .arg(format!("{l}:"))
            .arg(dir)
            .output()
            .ok()?;
        made.status.success().then(|| Subst(l))
    })
}

use std::future::Future;
use std::time::Duration;

use iroh::{Endpoint, EndpointAddr, SecretKey};
use irsync::config::settings::Relay;
use irsync::net::{build_endpoint, EndpointOpts};

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
        relay: Relay::Disabled,
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
    // Walk threads would let the paused clock skip ahead under load.
    irsync::scan::clock_hold::enable();
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
        use irsync::net::netsim::{Spec, WireChoice};
        use irsync::side::Side;
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
    pub hooks: std::sync::Arc<std::sync::Mutex<irsync::session::SideHooks>>,
    sender_parallel: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// The limit on one key's dormant sessions per export; zero is none.
    pub max_dormant: std::sync::Arc<std::sync::atomic::AtomicU32>,
    pub _state: tempfile::TempDir,
    /// The daemon's home, when the harness made one.
    _home: Option<tempfile::TempDir>,
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
                    index_at_start: false,
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
                    index_at_start: false,
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
    // A home of its own: a relative path or an `all_paths` push resolves
    // there, never in the real home.
    let home = tempfile::tempdir().unwrap();
    let mut d = spawn_daemon_full(authz, state, Some(home.path().to_path_buf())).await;
    d._home = Some(home);
    d
}

/// As [`spawn_daemon_with_state`], with the daemon's home directory.
pub async fn spawn_daemon_full(
    authz: std::sync::Arc<dyn irsync::authz::Authorizer>,
    state: tempfile::TempDir,
    home: Option<std::path::PathBuf>,
) -> DaemonHandle {
    spawn_daemon_settings(authz, state, home, Default::default()).await
}

/// As [`spawn_daemon_full`], with the `[daemon]` settings `daemon`.
pub async fn spawn_daemon_settings(
    authz: std::sync::Arc<dyn irsync::authz::Authorizer>,
    state: tempfile::TempDir,
    home: Option<std::path::PathBuf>,
    daemon: irsync::config::settings::DaemonSettings,
) -> DaemonHandle {
    use irsync::config::settings::Settings;
    let settings = Settings {
        state_dir: state.path().to_path_buf(),
        daemon,
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
    let max_dormant = daemon.max_dormant_sessions();
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
        max_dormant,
        _state: state,
        _home: None,
    }
}

/// An offline loopback endpoint that offers the classical key exchange
/// groups only, as an irsync built on ring does.
pub async fn classical_endpoint() -> Endpoint {
    use rustls::crypto::aws_lc_rs::{self, kx_group};
    let mut provider = aws_lc_rs::default_provider();
    provider.kx_groups = vec![kx_group::X25519, kx_group::SECP256R1, kx_group::SECP384R1];
    Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .crypto_provider(std::sync::Arc::new(provider))
        .alpns(vec![irsync_proto::ALPN.to_vec()])
        .clear_ip_transports()
        .bind_addr("127.0.0.1:0")
        .unwrap()
        .bind()
        .await
        .unwrap()
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
pub fn archive_opts() -> irsync::options::SyncOptions {
    let args = irsync::cli::Args::try_parse_from(["irsync", "-a", "src/", "host:m/"]).unwrap();
    irsync::options::SyncOptions::from_args(&args)
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
        Self::build(Some(base), |_| {}, |_| {}, |_| Vec::new())
            .await
            .0
    }

    /// A harness plus a second client endpoint the daemon also authorises.
    pub async fn with_guest() -> (Harness, Endpoint) {
        Self::build(None, |_| {}, |_| {}, |_| Vec::new()).await
    }

    /// A harness whose export is filled by `seed` before the daemon starts,
    /// with `index_at_start` set on it.
    pub async fn warm(seed: impl FnOnce(&std::path::Path)) -> Harness {
        Self::build(None, |e| e.index_at_start = true, seed, |_| Vec::new())
            .await
            .0
    }

    /// A harness whose export is first adjusted by `configure`.
    #[allow(dead_code)]
    pub async fn with_export(
        configure: impl FnOnce(&mut irsync::config::exports::Export),
    ) -> Harness {
        Self::build(None, configure, |_| {}, |_| Vec::new()).await.0
    }

    /// A harness whose export `m` holds a second export, `name`, at the
    /// directory `dir` below it (made here), adjusted by `configure`.
    #[allow(dead_code)]
    pub async fn with_inner_export(
        name: &'static str,
        dir: &'static str,
        configure: impl FnOnce(&mut irsync::config::exports::Export),
    ) -> Harness {
        Self::with_other_export(
            name,
            move |m| {
                let path = m.join(dir);
                std::fs::create_dir_all(&path).unwrap();
                path
            },
            configure,
        )
        .await
    }

    /// A harness with a second export, `name`, at the path `path` gives for
    /// the root of `m` (`path` makes what it wants made), adjusted by
    /// `configure`.
    #[allow(dead_code)]
    pub async fn with_other_export(
        name: &'static str,
        path: impl FnOnce(&std::path::Path) -> std::path::PathBuf,
        configure: impl FnOnce(&mut irsync::config::exports::Export),
    ) -> Harness {
        Self::build(
            None,
            |_| {},
            |_| {},
            move |m| {
                let path = path(m);
                let mut e = irsync::config::exports::Export {
                    path,
                    read_only: false,
                    index_at_start: false,
                    fsync: false,
                    auto: false,
                    rw: Vec::new(),
                    ro: Vec::new(),
                };
                configure(&mut e);
                vec![(name, e)]
            },
        )
        .await
        .0
    }

    async fn build(
        base: Option<&std::path::Path>,
        configure: impl FnOnce(&mut irsync::config::exports::Export),
        seed: impl FnOnce(&std::path::Path),
        extra: impl FnOnce(&std::path::Path) -> Vec<(&'static str, irsync::config::exports::Export)>,
    ) -> (Harness, Endpoint) {
        let tmp = || match base {
            Some(b) => tempfile::tempdir_in(b).unwrap(),
            None => tempfile::tempdir().unwrap(),
        };
        use irsync::authz::{Access, MemoryAuthorizer, Permissions};
        let client = client_endpoint().await;
        let guest = client_endpoint().await;
        let export = tmp();
        seed(export.path());
        let grants = Permissions {
            all_exports: Some(Access::ReadWrite),
            ..Permissions::default()
        };
        let authz = MemoryAuthorizer::single(client.id(), grants.clone()).with(guest.id(), grants);
        let mut m = irsync::config::exports::Export {
            path: export.path().to_path_buf(),
            read_only: false,
            index_at_start: false,
            fsync: false,
            auto: false,
            rw: Vec::new(),
            ro: Vec::new(),
        };
        configure(&mut m);
        let mut exports = vec![("m", m)];
        exports.extend(extra(export.path()));
        let daemon = spawn_daemon_exports(exports, authz).await;
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
    pub fn hello(flow: Flow) -> irsync::session::SyncHello {
        use irsync::options::Direction;
        irsync::session::SyncHello::new(
            match flow {
                Flow::Push => Direction::ClientSends,
                Flow::Pull => Direction::DaemonSends,
            },
            irsync_proto::v1::SyncRequest {
                path: b"m".to_vec(),
                contents_only: true,
                ..Default::default()
            },
        )
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
        opts: &irsync::options::SyncOptions,
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
        opts: &irsync::options::SyncOptions,
        hello: irsync::session::SyncHello,
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
                h.req.session_id = id.map(|i| i.to_vec()).unwrap_or_default();
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
        opts: &irsync::options::SyncOptions,
        src_file: &std::path::Path,
        dest_path: &[u8],
        contents_only: bool,
    ) -> anyhow::Result<irsync::session::Outcome> {
        let (parent, name) = irsync::session::single::split_last(src_file);
        let mut hello = Self::hello(Flow::Push);
        hello.req.path = dest_path.to_vec();
        hello.req.contents_only = contents_only;
        hello.req.single_file = true;
        hello.req.name = name.clone();
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
        let kind = irsync::session::single::probe(&conn, remote_path, None).await;
        conn.close(0u32.into(), b"done");
        kind
    }

    /// Pulls the one file at `remote_path` to `dest` (with a trailing `/`
    /// when `contents_only`), as the CLI does: probes first, so nothing is
    /// created locally unless the remote path is a file, then places it by
    /// rsync's rules and pulls into the directory that holds it.
    pub async fn run_single_pull(
        &self,
        opts: &irsync::options::SyncOptions,
        remote_path: &[u8],
        dest: &std::path::Path,
        contents_only: bool,
    ) -> anyhow::Result<irsync::session::Outcome> {
        use irsync::session::single::{place_file, split_last};
        use irsync_proto::v1::path_info::Kind;
        let kind = self.probe(remote_path).await?;
        anyhow::ensure!(kind == Kind::File, "remote path is {kind:?}, not a file");
        #[cfg(unix)]
        let remote = std::path::Path::new(irsync::platform::testutil::name_from_bytes(remote_path));
        // Windows names are UTF-16: the stored bytes (WTF-8) as an owned name.
        #[cfg(windows)]
        let remote_os = irsync::platform::bytes_to_os(remote_path)?;
        #[cfg(windows)]
        let remote = std::path::Path::new(&remote_os);
        let (_, basename) = split_last(remote);
        let placed = place_file(dest, contents_only, &basename)?;
        let mut hello = Self::hello(Flow::Pull);
        hello.req.path = remote_path.to_vec();
        hello.req.contents_only = false;
        hello.req.single_file = true;
        hello.req.name = placed.name.clone();
        let mut local =
            Self::side_for(&placed.root, self.client_state.path()).with_single(placed.name);
        local.contents_only = false;
        self.run_hello(opts, hello, local).await
    }

    /// One complete session; waits until the daemon has finished its side too.
    pub async fn run(
        &self,
        opts: &irsync::options::SyncOptions,
        flow: Flow,
    ) -> anyhow::Result<irsync::session::Outcome> {
        self.run_side(opts, flow, self.local_side()).await
    }

    pub async fn run_side(
        &self,
        opts: &irsync::options::SyncOptions,
        flow: Flow,
        local: irsync::session::LocalSide,
    ) -> anyhow::Result<irsync::session::Outcome> {
        self.run_hello(opts, Self::hello(flow), local).await
    }

    /// Like [`Harness::run_side`] with a caller-built `Hello` (e.g. a sub-path).
    pub async fn run_hello(
        &self,
        opts: &irsync::options::SyncOptions,
        hello: irsync::session::SyncHello,
        local: irsync::session::LocalSide,
    ) -> anyhow::Result<irsync::session::Outcome> {
        use std::sync::atomic::Ordering;
        let before = self.daemon.sessions.load(Ordering::SeqCst);
        let conn = self
            .client
            .connect(self.daemon.addr.clone(), irsync_proto::ALPN)
            .await?;
        irsync::net::tap::ConnTap::of(&conn).set_local_id(self.client.id());
        let out = irsync::session::engine::run_client(conn.clone(), hello, local, opts).await;
        conn.close(0u32.into(), b"done");
        // The daemon's side ends after ours, once it has written what it
        // queued for its index: seconds for a large tree on a busy disk
        // (over 10 s for two 20,000-file pulls on a CI runner).
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while self.daemon.sessions.load(Ordering::SeqCst) == before {
            assert!(
                std::time::Instant::now() < deadline,
                "daemon session did not finish (client side: {:?})",
                out.as_ref().map(|_| "ok")
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

/// Sets the permission bits of `p` (`chmod`, which follows a link), for a
/// test whose setup gives a file a mode but is not about modes. Windows
/// has none: nothing is done there. On Unix this is
/// `platform::testutil::set_mode`.
pub fn set_mode_where_modes_exist(p: &std::path::Path, mode: u32) -> std::io::Result<()> {
    irsync::platform::set_mode_path_following(p, mode)
}

/// The process's real user id, as a clone folder's name holds it. Windows
/// has none: 0 there, the id irsync gives itself (`platform::euid`).
pub fn real_uid() -> u32 {
    #[cfg(unix)]
    let uid = irsync::platform::testutil::real_uid();
    #[cfg(windows)]
    let uid = irsync::platform::euid();
    uid
}

/// Sets the modification time of `p`, a file or a directory, a final link
/// followed. On Unix through `p` opened for reading (`futimens`); Windows
/// sets a time only through a handle opened to write attributes.
pub fn set_modified(p: &std::path::Path, t: std::time::SystemTime) -> std::io::Result<()> {
    #[cfg(unix)]
    let file = std::fs::File::open(p)?;
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_WRITE_ATTRIBUTES,
        };
        std::fs::OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(p)?
    };
    file.set_modified(t)
}

/// Builds the `src` and `dst` trees, then runs one session in direction `flow`.
pub async fn sync_trees(
    src: impl FnOnce(&std::path::Path),
    dst: impl FnOnce(&std::path::Path),
    opts: &irsync::options::SyncOptions,
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
    preserve: &irsync::options::Preserve,
) {
    #[cfg(unix)]
    use irsync::platform::testutil::is_fifo;
    use irsync::platform::{mode_of, mtime_of};
    // Windows has no FIFOs.
    #[cfg(windows)]
    let is_fifo = |_: std::fs::FileType| false;
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
        } else if is_fifo(t) {
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
            mode_of(&ma) & 0o7777,
            mode_of(&mb) & 0o7777,
            "mode differs: {a:?}"
        );
    }
    if preserve.times {
        assert_eq!(
            mtime_of(&ma),
            mtime_of(&mb),
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
    irsync_bin_env(home, cwd, &[], args)
}

/// [`irsync_bin_in`] with extra environment variables.
pub fn irsync_bin_env(
    home: &std::path::Path,
    cwd: Option<&std::path::Path>,
    env: &[(&str, &std::path::Path)],
    args: &[&str],
) -> std::process::Output {
    use std::process::Stdio;
    let mut cmd = irsync_command(home);
    cmd.envs(env.iter().copied());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let mut child = cmd
        .args(args)
        .state_home(home.join("xdg"))
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

    /// Ends the daemon and waits for it: on Unix with `SIGTERM`, as a
    /// service manager does; on Windows it is killed. Its protocol capture
    /// is written out after every batch, so it holds what the daemon did.
    pub fn stop(mut self) {
        #[cfg(unix)]
        {
            let status = std::process::Command::new("kill")
                .args(["-TERM", &self.pid().to_string()])
                .status()
                .unwrap();
            assert!(status.success(), "kill -TERM failed");
        }
        #[cfg(windows)]
        bin::stop_child(&mut self.guard.0);
        let _ = self.guard.0.wait();
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
    use std::process::Stdio;
    let cfg = server_home.join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    irsync::fs::private::write(
        cfg.join("irsync.toml"),
        "[daemon]\nrelay = \"disabled\"\nportmap = false\n\
         dht = false\ndns = false\nmdns = false\n",
    )
    .unwrap();
    let log = server_home.join("daemon.stderr");
    let child = irsync_command(server_home)
        .args(["daemon", "-vv", "--listen", "127.0.0.1:0"])
        .state_home(server_home.join("xdg"))
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

/// Sets (or, with `None`, removes) `all_paths` of the client with id `id`
/// in the config under `home`, as `irsync client set` does. A client the
/// config does not know yet is saved as `[clients.granted]` first.
pub fn set_all_paths(home: &std::path::Path, id: &str, access: Option<irsync::authz::Access>) {
    set_right(home, id, irsync::config::clients::Right::AllPaths, access);
}

/// Like [`set_all_paths`], for the `admin` right.
pub fn set_admin(home: &std::path::Path, id: &str, access: Option<irsync::authz::Access>) {
    set_right(home, id, irsync::config::clients::Right::Admin, access);
}

/// Runs the built binary, as the server's owner would, so the save sees
/// the test's HOME and state dir and never the developer's own.
fn set_right(
    home: &std::path::Path,
    id: &str,
    right: irsync::config::clients::Right,
    access: Option<irsync::authz::Access>,
) {
    let path = home.join(".config/irsync/irsync.toml");
    // Reading does not look for a daemon; only the save does.
    let msg = irsync::manage::load_message(&path).unwrap();
    let parsed = irsync::identity::parse_endpoint_id(id).unwrap();
    let known = msg
        .clients
        .iter()
        .find(|c| irsync::manage::id_of(&c.client_id) == Some(parsed))
        .map(|c| c.name.clone());
    let run = |args: &[&str]| {
        let o = irsync_bin(home, args);
        assert!(
            o.status.success(),
            "irsync {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    };
    let name = match known {
        Some(name) => name,
        None => {
            run(&["client", "add", "granted", id]);
            "granted".to_string()
        }
    };
    let value = access.map_or("none", irsync::authz::Access::as_str);
    run(&["client", "set", &name, &format!("{}={value}", right.key())]);
}
