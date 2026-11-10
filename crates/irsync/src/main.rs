use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context};
use iroh::{EndpointAddr, EndpointId, TransportAddr};
use irsync::build::BuildInfo;
use irsync::cli::location::Location;
use irsync::cli::progress::render::Geometry;
use irsync::cli::progress::screen::{LogWriter, Screen, WriterTerm};
use irsync::cli::progress::summary::SessionSummary;
use irsync::cli::progress::Reporter;
use irsync::cli::{
    validate, Args, Command, ConfigArgs, ConfigCmd, DaemonArgs, Direction, ExportArgs, ExportCmd,
    FilesArgs, GrantArgs, GrantCmd, HostAddArgs, HostArgs, HostCmd, IdArgs, IdCmd, IndexArgs,
    LogArgs, SyncOptions,
};
use irsync::config::file::Config;
use irsync::config::hosts::Hosts;
use irsync::config::paths::{state_dir, Paths};
use irsync::config::settings::Settings;
use irsync::filter::FilterSet;
use irsync::fs::paths::escape_path;
use irsync::identity::{
    load_client_identity, load_or_generate, parse_endpoint_id, KeySource, PassphraseSource,
};
use irsync::index::Index;
use irsync::manage::{self, PeerSpec};
use irsync::net::{build_endpoint, client_endpoint_opts};
use irsync::session::engine::list_exports;
use irsync::session::reconnect::{
    describe_dial, explain, run_with_reconnect, ConnState, ReconnectOpts,
};
use irsync::session::{resolve_roots, Event, EventBus, LocalSide, Outcome, Party, HASH_WORKERS};
use irsync::telemetry::init::trace_files_from_env;
use irsync::telemetry::meter::Usage;
use irsync::telemetry::{self, OtlpConfig, Side, Telemetry, TelemetryConfig};
use irsync_proto::chunk::CHUNK_PARAMS;
use irsync_proto::v1::{export_entry, hello, Hello};
use irsync_proto::PROTOCOL_VERSION;

fn main() {
    let args = Args::parse();
    // The receiver holds a descriptor per file in flight; see `fs::rlimit`.
    let _ = irsync::fs::rlimit::raise_nofile();
    if args.cmd.is_none() && args.sync.paths.len() == 1 {
        let opts = SyncOptions::from_args(&args);
        std::process::exit(run_list(args, &opts));
    }
    match &args.cmd {
        Some(Command::Daemon(d)) => std::process::exit(run_daemon(d)),
        Some(Command::Gc {
            dry_run,
            log,
            files,
        }) => std::process::exit(run_gc(*dry_run, *log, files)),
        Some(Command::Index(ix)) => std::process::exit(run_index(ix)),
        Some(Command::Host(h)) => std::process::exit(run_host(h)),
        Some(Command::Export(e)) => std::process::exit(run_export(e)),
        Some(Command::Grant(g)) => std::process::exit(run_grant(g)),
        Some(Command::Id(i)) => std::process::exit(run_id(i)),
        Some(Command::Config(c)) => std::process::exit(run_config(c)),
        Some(Command::Show {
            passphrase_file,
            files,
        }) => std::process::exit(run_show(files, passphrase_file.clone())),
        None => {
            let opts = SyncOptions::from_args(&args);
            std::process::exit(run_sync(&args, &opts));
        }
    }
}

/// The client's log filter for `-q` and `-v`, unless `RUST_LOG` is set.
fn client_log_default(verbose: u8, quiet: bool) -> &'static str {
    match (quiet, verbose) {
        (true, _) => "error",
        (false, 0) => "warn",
        (false, 1) => "warn,irsync=info",
        (false, 2) => "info,irsync=debug",
        _ => "debug,irsync=trace",
    }
}

/// The daemon's log filter for `-q` and `-v`, unless `RUST_LOG` is set.
fn daemon_log_default(verbose: u8, quiet: bool) -> &'static str {
    match (quiet, verbose) {
        (true, _) => "error",
        (false, 0) => "info",
        (false, 1) => "debug",
        _ => "trace",
    }
}

/// Logging alone, for the commands without telemetry.
fn init_logging(verbose: u8, quiet: bool, writer: LogWriter) {
    let cfg = TelemetryConfig::logs_only(Side::Client, client_log_default(verbose, quiet));
    init_telemetry(cfg, writer);
}

/// Installs the subscriber and recorder for `cfg` (see [`telemetry::init`]).
fn init_telemetry<W>(cfg: TelemetryConfig, writer: W) -> Telemetry
where
    W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + Send + Sync + 'static,
{
    let t = telemetry::init(cfg, writer);
    BuildInfo::current().log_startup();
    t
}

/// The settings for telemetry, read before anything else: a broken config
/// gives the defaults here and fails later where it is used.
fn early_settings(config: Option<&std::path::Path>) -> Settings {
    Config::load(&Paths::resolve(config))
        .map(|c| c.settings)
        .unwrap_or_default()
}

/// Exit codes: 0 ok, 1 configuration, operand or connection errors, 23
/// partial transfer, 25 deletions stopped at `--max-delete` (23 and 25 as in
/// rsync; a command line clap cannot parse has already exited 2).
fn run_sync(args: &Args, opts: &SyncOptions) -> i32 {
    let log = LogWriter::new();
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("irsync: {e}");
            return 1;
        }
    };
    let settings = early_settings(opts.config.as_deref());
    let mut telemetry = {
        let _rt = rt.enter();
        init_telemetry(
            TelemetryConfig {
                side: Side::Client,
                log_default: client_log_default(opts.verbose, opts.quiet).to_string(),
                summary: opts.verbose >= 1 && !opts.quiet,
                debug_addr: opts.debug_addr,
                otlp: OtlpConfig::resolve(&settings.telemetry),
                trace_files: trace_files_from_env(),
            },
            log.clone(),
        )
    };
    let mut debug_port = None;
    let (result, reporter, elapsed) = rt.block_on(async {
        match telemetry.start().await {
            Ok(bound) => debug_port = bound,
            Err(e) => {
                if let Some(a) = opts.debug_addr {
                    eprintln!("irsync: debug port {a}: {e}");
                }
            }
        }
        let bus = EventBus::default();
        // `-q` silences everything but errors. On a terminal the live
        // progress block is always drawn; `TERM=dumb` is not a terminal.
        let tty =
            std::io::stdout().is_terminal() && std::env::var("TERM").map_or(true, |t| t != "dumb");
        let task = (!opts.quiet).then(|| {
            let screen = if tty {
                Screen::tty(
                    Box::new(WriterTerm::new(Box::new(std::io::stdout()), || {
                        console::Term::stdout().size()
                    })),
                    Geometry::detect,
                )
            } else {
                Screen::plain(Box::new(std::io::stdout()))
            };
            // Log lines now print above the block.
            log.attach(screen.multi());
            // A bad command line fails in `sync` below with the same error.
            let source = match direction_of(args) {
                Ok(Direction::DaemonSends) => Party::Peer,
                Ok(Direction::ClientSends) | Err(_) => Party::Local,
            };
            let r =
                Reporter::with_screen(screen, opts.verbose, opts.stats, opts.itemize, opts.dry_run)
                    .with_delete_mode(opts.delete)
                    .with_source(source);
            tokio::spawn(r.run(bus.subscribe(), None))
        });
        let start = Instant::now();
        let result = sync(args, opts, &bus, &log).await;
        // Every session has dropped its sender clone: the reporter drains and stops.
        drop(bus);
        let reporter = match task {
            Some(t) => t.await.ok(),
            None => None,
        };
        let elapsed = start.elapsed();
        (result, reporter, elapsed)
    });
    // The block goes before anything is printed on stderr: indicatif leaves
    // the cursor at the end of its last line, so a line printed under a live
    // block lands on it and the next repaint (or the final clear) erases it.
    let mut reporter = reporter;
    if let Some(r) = &mut reporter {
        r.clear();
    }
    let code = match result {
        Ok(Totals {
            outcome,
            sessions,
            failed,
        }) => {
            for (path, msg) in &outcome.errors {
                eprintln!("irsync: {}: {msg}", path.to_os().to_string_lossy());
            }
            if outcome.max_delete_reached {
                eprintln!("irsync: deletions stopped due to the --max-delete limit");
            }
            if let Some(mut r) = reporter {
                r.finish(&outcome, elapsed);
            }
            combine_exit_codes(&[outcome], failed, sessions)
        }
        Err(e) => {
            eprintln!("irsync: {e:#}");
            1
        }
    };
    rt.block_on(async {
        telemetry.flush();
        if let (Some(addr), false) = (debug_port, opts.debug_no_wait) {
            // Listen before saying so: a signal sent on seeing the line ends the wait.
            let signals = exit_signals();
            eprintln!("irsync: debug port open at {addr}; Ctrl-C to exit");
            wait_for_signal(signals).await;
        }
        telemetry.shutdown().await;
    });
    code
}

/// SIGINT's and SIGHUP's streams.
type ExitSignals = (tokio::signal::unix::Signal, tokio::signal::unix::Signal);

/// Listens for SIGINT and SIGHUP; `None` when that cannot be set up.
fn exit_signals() -> Option<ExitSignals> {
    use tokio::signal::unix::{signal, SignalKind};
    Some((
        signal(SignalKind::interrupt()).ok()?,
        signal(SignalKind::hangup()).ok()?,
    ))
}

/// Returns on SIGINT or SIGHUP from `signals`; at once when there are none.
async fn wait_for_signal(signals: Option<ExitSignals>) {
    let Some((mut int, mut hup)) = signals else {
        return;
    };
    tokio::select! {
        _ = int.recv() => {}
        _ = hup.recv() => {}
    }
}

/// The process exit code for `total` sessions of which `failed` errored:
/// all failed = 1 (nothing got through); otherwise 25 (deletions stopped at
/// `--max-delete`) outranks 23 (partial transfer, per-file errors or some
/// sources failed) outranks 0.
fn combine_exit_codes(outcomes: &[Outcome], failed: usize, total: usize) -> i32 {
    if failed > 0 && failed >= total {
        1
    } else if outcomes.iter().any(|o| o.max_delete_reached) {
        25
    } else if failed > 0 || outcomes.iter().any(|o| !o.errors.is_empty()) {
        23
    } else {
        0
    }
}

/// Partial files and sessions untouched for this long are collected.
const CLIENT_GC_AGE: std::time::Duration = std::time::Duration::from_secs(7 * 86400);
/// A partial record untouched this long makes the next run's GC walk due.
const CLIENT_GC_STALE: std::time::Duration = std::time::Duration::from_secs(3600);

fn run_gc(dry_run: bool, log: LogArgs, files: &FilesArgs) -> i32 {
    init_logging(log.verbose, log.quiet, LogWriter::new());
    let result = (|| -> anyhow::Result<()> {
        let dir = Config::load(&file_paths(files))?.settings.state_dir;
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async {
            let (mut parts, mut sessions, mut entries) = (0usize, 0u32, 0usize);
            let Ok(rd) = std::fs::read_dir(dir.join("index")) else {
                if !log.quiet {
                    println!(
                        "0 part file(s), 0 session(s), 0 index entries {}",
                        verb(dry_run)
                    );
                }
                return Ok(());
            };
            let mut files: Vec<PathBuf> = rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "sqlite"))
                .collect();
            files.sort();
            for file in files {
                let root = match Index::root_of_file(&file) {
                    Ok(Some(r)) => r,
                    Ok(None) => continue,
                    Err(e) => {
                        eprintln!("irsync: {}: {e}", file.display());
                        continue;
                    }
                };
                let index = match Index::open(&dir, &root) {
                    Ok(i) => i.into_handle(),
                    Err(e) => {
                        eprintln!("irsync: {}: {e}", root.display());
                        continue;
                    }
                };
                match irsync::daemon::gc(index, &root, CLIENT_GC_AGE, dry_run).await {
                    Ok(r) => {
                        for p in &r.parts_removed {
                            if !log.quiet {
                                println!("{}", root.join(p.to_os()).display());
                            }
                        }
                        for (p, msg) in &r.errors {
                            eprintln!("irsync: {}: {msg}", root.join(p.to_os()).display());
                        }
                        parts += r.parts_removed.len();
                        sessions += r.sessions_removed;
                        entries += r.entries_removed;
                    }
                    Err(e) => eprintln!("irsync: {}: {e}", root.display()),
                }
            }
            if !log.quiet {
                println!(
                    "{parts} part file(s), {sessions} session(s), {entries} index entr{} {}",
                    if entries == 1 { "y" } else { "ies" },
                    verb(dry_run)
                );
            }
            Ok(())
        })
    })();
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("irsync gc: {e:#}");
            1
        }
    }
}

fn verb(dry_run: bool) -> &'static str {
    if dry_run {
        "would be removed"
    } else {
        "removed"
    }
}

/// The daemon's EndpointId: an alias from the hosts table, else the host
/// string itself must be an EndpointId.
fn resolve_target(hosts: &Hosts, host: &str) -> anyhow::Result<EndpointId> {
    if let Some(entry) = hosts.get(host) {
        return Ok(entry.endpoint_id);
    }
    parse_endpoint_id(host).map_err(|_| {
        let missing = if hosts.path.exists() {
            ""
        } else {
            "; no such file"
        };
        anyhow::anyhow!(
            "{host:?} is not an alias in {} and not an EndpointId{missing}",
            hosts.path.display()
        )
    })
}

/// What all sessions of one invocation did together.
struct Totals {
    outcome: Outcome,
    sessions: usize,
    /// Sessions that ended in an error (their sources did not sync).
    failed: usize,
}

/// The sources, the destination and which way the data flows.
fn locations(args: &Args) -> anyhow::Result<(Vec<Location>, Location, Direction)> {
    let srcs = args
        .sync
        .src()
        .iter()
        .map(|s| Location::parse(s))
        .collect::<Result<Vec<_>, _>>()?;
    let dest = Location::parse(args.sync.dest().context("missing destination")?)?;
    let direction = validate(&srcs, &dest)?;
    Ok((srcs, dest, direction))
}

/// Which way the data flows, from the source and destination arguments.
fn direction_of(args: &Args) -> anyhow::Result<Direction> {
    Ok(locations(args)?.2)
}

/// A client endpoint ready to dial `host`, plus what the session setup
/// still needs from the configuration.
struct Connected {
    endpoint: iroh::Endpoint,
    target: EndpointAddr,
    settings: Settings,
    host_entry: Option<irsync::config::hosts::HostEntry>,
    /// `opts` with the per-host and settings defaults filled in.
    opts: SyncOptions,
}

/// Resolves `host`, the direct addresses, the client identity and the
/// endpoint, the same way for a sync and for a listing.
async fn connect_for(host: &str, opts: &SyncOptions) -> anyhow::Result<Connected> {
    let paths = Paths::resolve(opts.config.as_deref());
    let Config {
        settings, hosts, ..
    } = Config::load(&paths)?;
    let host_entry = hosts.get(host).cloned();
    let target = resolve_target(&hosts, host)?;
    // Precedence: command line, then the host entry, then the client
    // settings, then the built-in default.
    let mut opts = opts.clone();
    opts.state_dir = Some(settings.state_dir.clone());
    if opts.parallel.is_none() {
        opts.parallel = host_entry
            .as_ref()
            .and_then(|h| h.parallel)
            .or(settings.client.parallel);
    }
    // Direct addresses: the command line's, else the host's. Any address
    // means isolated mode: dial those and nothing else. Resolved before the
    // identity is loaded so a bad address never prompts for a passphrase.
    let addresses = if opts.addresses.is_empty() {
        host_entry
            .as_ref()
            .map(|h| h.address.clone())
            .unwrap_or_default()
    } else {
        opts.addresses.clone()
    };
    let mut resolved = Vec::new();
    for entry in &addresses {
        let addrs = entry
            .resolve()
            .await
            .with_context(|| format!("resolving address {entry}"))?;
        if addrs.is_empty() {
            bail!("address {entry} resolved to no addresses");
        }
        resolved.extend(addrs);
    }
    let pass = PassphraseSource::from_env(opts.passphrase_file.clone());
    let (secret, source) = client_identity(opts.identity.as_deref(), &settings, &pass)?;
    if let KeySource::Generated(path) = &source {
        eprintln!("generated identity {}", path.display());
        eprintln!("{}", irsync::identity::public_key_line(&secret, "irsync"));
        eprintln!("{}", secret.public());
    }
    if !addresses.is_empty() {
        tracing::info!(
            addresses = ?resolved,
            "isolated mode: dialling directly, relay and lookups off"
        );
    }
    let endpoint_opts = client_endpoint_opts(
        secret,
        opts.relay.as_deref(),
        opts.no_relay,
        opts.no_dht,
        &settings.client,
        host_entry.as_ref(),
        &addresses,
    )?;
    let endpoint = build_endpoint(endpoint_opts).await?;
    let target = EndpointAddr::from_parts(target, dial_addrs(resolved)?);
    Ok(Connected {
        endpoint,
        target,
        settings,
        host_entry,
        opts,
    })
}

/// The transport addresses to dial: the fault layer's custom ones when it is on.
fn dial_addrs(resolved: Vec<std::net::SocketAddr>) -> anyhow::Result<Vec<TransportAddr>> {
    #[cfg(feature = "netsim")]
    if irsync::net::netsim::active()?.is_some() {
        return resolved
            .into_iter()
            .map(|a| match a {
                std::net::SocketAddr::V4(v4) => {
                    Ok(TransportAddr::Custom(irsync::net::netsim::custom_addr(v4)))
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
fn client_state_dir(opts: &SyncOptions) -> PathBuf {
    CLIENT_STATE
        .get()
        .or(opts.state_dir.as_ref())
        .cloned()
        .unwrap_or_else(state_dir)
}

/// `irsync HOST:` lists the exports this client may reach; `irsync
/// HOST:PATH` lists a directory: a dry-run pull into a temporary directory
/// that also holds the run's index, so nothing is written or kept.
fn run_list(mut args: Args, opts: &SyncOptions) -> i32 {
    let operand = args.sync.paths[0].clone();
    let dir_listing = match Location::parse(&operand) {
        Ok(Location::Remote { path, .. }) => !path.is_empty(),
        _ => false,
    };
    if dir_listing {
        let tmp = match tempfile::tempdir() {
            Ok(t) => t,
            Err(e) => {
                eprintln!("irsync: creating a temporary directory: {e}");
                return 1;
            }
        };
        let _ = CLIENT_STATE.set(tmp.path().join("state"));
        let dest = tmp.path().join("list").to_string_lossy().into_owned();
        args.sync.paths = vec![operand, dest];
        let mut opts = opts.clone();
        opts.dry_run = true;
        opts.verbose = opts.verbose.max(1);
        // A listing shows the tree, so it recurses unless told not to (a
        // sync needs -r or -a for that).
        opts.preserve.recursive = !args.sync.no_recursive;
        // A listing is a question: with the daemon unreachable it fails
        // now rather than waiting out a sync's reconnect window.
        opts.reconnect = Some(std::time::Duration::ZERO);
        return run_sync(&args, &opts);
    }
    let log = LogWriter::new();
    init_logging(opts.verbose, opts.quiet, log.clone());
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("irsync: {e}");
            return 1;
        }
    };
    match rt.block_on(list_exports_of(&operand, opts)) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("irsync: {e:#}");
            1
        }
    }
}

async fn list_exports_of(operand: &str, opts: &SyncOptions) -> anyhow::Result<()> {
    let Location::Remote { host, .. } = Location::parse(operand)? else {
        bail!("internal: a listing takes a remote operand");
    };
    let c = connect_for(&host, opts).await?;
    let hello = Hello {
        protocol_version: PROTOCOL_VERSION,
        direction: hello::Direction::ListExports as i32,
        chunk_min: CHUNK_PARAMS.min,
        chunk_avg: CHUNK_PARAMS.avg,
        chunk_max: CHUNK_PARAMS.max,
        build: Some(BuildInfo::current().to_wire()),
        ..Hello::default()
    };
    let client = c.endpoint.id();
    let conn = c
        .endpoint
        .connect(c.target.clone(), irsync_proto::ALPN)
        .await
        .map_err(|e| anyhow::anyhow!(describe_dial(&c.target.id, &e)))?;
    let entries = list_exports(&conn, hello)
        .await
        .map_err(|e| explain(e, Some(&conn), operand, &client))?;
    for e in entries {
        let access = if e.access == export_entry::Access::Rw as i32 {
            "rw"
        } else {
            "ro"
        };
        println!("{} {} {access}", e.name, escape_path(&e.path));
    }
    conn.close(0u32.into(), b"done");
    c.endpoint.close().await;
    Ok(())
}

/// `log`: where a failed source is reported (above the live block).
async fn sync(
    args: &Args,
    opts: &SyncOptions,
    bus: &EventBus,
    log: &LogWriter,
) -> anyhow::Result<Totals> {
    let (srcs, dest, direction) = locations(args)?;
    let remote = match direction {
        Direction::ClientSends => &dest,
        Direction::DaemonSends => &srcs[0],
    };
    let Location::Remote { host, .. } = remote else {
        bail!("internal: no remote location");
    };
    // The operand as typed, for the no-access hint.
    let remote_text: String = match direction {
        Direction::ClientSends => args.sync.dest().unwrap_or_default().to_string(),
        Direction::DaemonSends => args.sync.src()[0].clone(),
    };

    let Connected {
        endpoint,
        target,
        settings,
        host_entry,
        opts,
    } = connect_for(host, opts).await?;
    let (opts, host_entry) = (&opts, host_entry.as_ref());
    let filters = Arc::new(FilterSet::from_rules(&opts.filters).context("reading filter rules")?);

    // One session per source; several sources only when pushing.
    let pairs: Vec<(&Location, &Location)> = srcs.iter().map(|s| (s, &dest)).collect();
    let mut total = Outcome::default();
    let (mut sessions, mut failed) = (0, 0);
    let of = pairs.len();
    for (index, (src, dst)) in pairs.into_iter().enumerate() {
        bus.emit(Event::SessionStart { index, of });
        let (local, remote) = match direction {
            Direction::ClientSends => (src, dst),
            Direction::DaemonSends => (dst, src),
        };
        let (
            Location::Local {
                path,
                contents_only: local_co,
            },
            Location::Remote {
                path: rpath,
                contents_only: remote_co,
                ..
            },
        ) = (local, remote)
        else {
            bail!("internal: unexpected location kinds");
        };
        let (root, name) = resolve_roots(direction, path, *local_co, rpath, *remote_co);
        let hello = Hello {
            protocol_version: PROTOCOL_VERSION,
            session_id: vec![],
            path: rpath.clone(),
            name,
            direction: match direction {
                Direction::ClientSends => hello::Direction::ClientSends,
                Direction::DaemonSends => hello::Direction::DaemonSends,
            } as i32,
            chunk_min: CHUNK_PARAMS.min,
            chunk_avg: CHUNK_PARAMS.avg,
            chunk_max: CHUNK_PARAMS.max,
            contents_only: *remote_co,
            build: Some(BuildInfo::current().to_wire()),
            // The receiver-side options are filled in from `opts` by the session.
            ..Hello::default()
        };
        let ropts = ReconnectOpts {
            max: opts
                .reconnect
                .or(host_entry.and_then(|h| h.reconnect))
                .or(settings.client.reconnect)
                .unwrap_or(ReconnectOpts::default().max),
            dial_retries: opts
                .dial_retries
                .or(host_entry.and_then(|h| h.dial_retries))
                .or(settings.client.dial_retries)
                .unwrap_or(ReconnectOpts::default().dial_retries),
            source_index: index,
            ..ReconnectOpts::default()
        };
        sessions += 1;
        let started = Instant::now();
        let operand_from = args.sync.src()[index].clone();
        let operand_to = args.sync.dest().unwrap_or_default().to_string();
        let result = session(
            &endpoint,
            target.clone(),
            &remote_text,
            direction,
            root,
            *local_co,
            hello,
            &filters,
            opts,
            ropts,
            bus,
        )
        .await;
        let mut summary = SessionSummary {
            index,
            of,
            verb: match direction {
                Direction::ClientSends => "push",
                Direction::DaemonSends => "pull",
            },
            from: operand_from,
            to: operand_to,
            reconnects: 0,
            conn: None,
            usage: Usage::default(),
            files_transferred: 0,
            files_hashed: 0,
            bytes_sent: 0,
            bytes_received: 0,
        };
        let out = match result {
            Ok(out) => {
                summary.reconnects = out.reconnects;
                summary.conn = out.conn;
                summary.usage = out.usage.clone();
                summary.files_transferred = out.files_transferred;
                summary.files_hashed = out.files_hashed;
                summary.bytes_sent = out.bytes_sent;
                summary.bytes_received = out.bytes_received;
                bus.emit(Event::SessionEnd(Arc::new(summary)));
                out
            }
            Err(e) => {
                summary.usage.wall = started.elapsed();
                bus.emit(Event::SessionEnd(Arc::new(summary)));
                // Keep what earlier sources achieved; report this one and go on.
                log.eprintln(&format!("irsync: {}: {e:#}", path.display()));
                failed += 1;
                continue;
            }
        };
        total.absorb(out);
    }
    endpoint.close().await;
    Ok(Totals {
        outcome: total,
        sessions,
        failed,
    })
}

/// One source: open the local index, then run the session, reconnecting and
/// resuming as needed.
#[allow(clippy::too_many_arguments)]
async fn session(
    endpoint: &iroh::Endpoint,
    target: EndpointAddr,
    remote: &str,
    direction: Direction,
    root: PathBuf,
    local_contents_only: bool,
    hello: Hello,
    filters: &Arc<FilterSet>,
    opts: &SyncOptions,
    ropts: ReconnectOpts,
    bus: &EventBus,
) -> anyhow::Result<Outcome> {
    let dry_run = opts.dry_run;
    let state = client_state_dir(opts);
    let index = tokio::task::spawn_blocking({
        let root = root.clone();
        move || -> anyhow::Result<_> {
            // A dry run creates nothing (the receiver lists a missing root as empty).
            if direction == Direction::DaemonSends && !dry_run {
                std::fs::create_dir_all(&root)
                    .with_context(|| format!("creating {}", root.display()))?;
            }
            Ok(Index::open(&state, &root)
                .with_context(|| format!("opening index for {}", root.display()))?
                .into_handle())
        }
    })
    .await??;
    if !dry_run {
        // Housekeeping for this root, at most daily unless partial records
        // have gone stale (an hour untouched) since; never fatal.
        match irsync::daemon::gc_if_due(index.clone(), &root, CLIENT_GC_AGE, CLIENT_GC_STALE).await
        {
            Ok(Some(r)) => tracing::debug!(
                parts_removed = r.parts_removed.len(),
                sessions_removed = r.sessions_removed,
                entries_removed = r.entries_removed,
                "gc finished"
            ),
            Ok(None) => tracing::debug!("gc not due"),
            Err(e) => tracing::debug!("gc failed: {e}"),
        }
    }
    let mut side = LocalSide::new(root, local_contents_only, index, filters.clone());
    side.hooks.bus = bus.clone();
    let hello_for = |id: Option<[u8; 16]>| Hello {
        session_id: id.map(|i| i.to_vec()).unwrap_or_default(),
        ..hello.clone()
    };
    let target_id = target.id;
    let on_state = |s: ConnState| match s {
        ConnState::Connecting => tracing::info!(target = %target_id, "connecting"),
        ConnState::Direct => tracing::info!("connected directly"),
        ConnState::Relay => tracing::info!("connected through a relay"),
        ConnState::Reconnecting { attempt } => {
            tracing::info!(attempt, "reconnecting")
        }
    };
    run_with_reconnect(
        endpoint, target, remote, hello_for, side, opts, ropts, on_state,
    )
    .await
}

/// `irsync index`: hashes every regular file under each target into its
/// index (see `irsync::warm`). Exit code 23 when something could not be
/// read or listed, 1 when a target could not be resolved at all.
fn run_index(a: &IndexArgs) -> i32 {
    init_logging(a.log.verbose, a.log.quiet, LogWriter::new());
    let state = match Config::load(&file_paths(&a.files)) {
        Ok(c) => c.settings.state_dir,
        Err(e) => {
            eprintln!("irsync index: {e:#}");
            return 1;
        }
    };
    let targets = a
        .targets
        .iter()
        .map(|t| (state.clone(), t.clone()))
        .collect();
    match index_roots(targets, a.dry_run, a.log.quiet) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("irsync index: {e:#}");
            1
        }
    }
}

/// `export update`: the named exports (`all`: every export) under the
/// daemon's state directory.
fn export_update(paths: &Paths, names: &[String], dry_run: bool, log: LogArgs) -> i32 {
    init_logging(log.verbose, log.quiet, LogWriter::new());
    let result = (|| -> anyhow::Result<i32> {
        let config = Config::load(paths)?;
        let (settings, exports) = (config.settings, config.exports);
        let names: Vec<&str> = if names.iter().any(|n| n == "all") {
            anyhow::ensure!(names.len() == 1, "all names every export; give it alone");
            exports.exports.keys().map(String::as_str).collect()
        } else {
            names.iter().map(String::as_str).collect()
        };
        let targets = names
            .into_iter()
            .map(|n| {
                let m = exports
                    .exports
                    .get(n)
                    .with_context(|| format!("no export {n:?} in {}", exports.path.display()))?;
                Ok((settings.state_dir.clone(), m.path.clone()))
            })
            .collect::<anyhow::Result<_>>()?;
        index_roots(targets, dry_run, log.quiet)
    })();
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("irsync export update: {e:#}");
            1
        }
    }
}

/// Hashes each `(state directory, root)` into the root's index and prints
/// a summary line per root. Exit code 23 when a root had errors or was
/// left incomplete.
fn index_roots(
    targets: Vec<(PathBuf, PathBuf)>,
    dry_run: bool,
    quiet: bool,
) -> anyhow::Result<i32> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut code = 0;
        for (state, root) in targets {
            let index = Index::open(&state, &root)
                .with_context(|| format!("open index for {}", root.display()))?
                .into_handle();
            let r = irsync::warm::index_root(index, &root, HASH_WORKERS, dry_run)
                .await
                .with_context(|| format!("indexing {}", root.display()))?;
            for (p, msg) in &r.errors {
                eprintln!("irsync: {}: {msg}", root.join(p.to_os()).display());
            }
            let what = if dry_run { "to hash" } else { "hashed" };
            if !quiet {
                println!(
                    "{}: {} file(s), {} {what} ({} bytes)",
                    root.display(),
                    r.examined,
                    r.hashed,
                    r.bytes
                );
            }
            if !r.errors.is_empty() || r.incomplete {
                code = 23;
            }
        }
        Ok(code)
    })
}

fn run_daemon(d: &DaemonArgs) -> i32 {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("irsync daemon: {e:#}");
            return 1;
        }
    };
    let settings = early_settings(d.config.as_deref());
    let debug_addr = settings.daemon.debug_addr;
    let mut telemetry = {
        let _rt = rt.enter();
        init_telemetry(
            TelemetryConfig {
                side: Side::Daemon,
                log_default: daemon_log_default(d.log.verbose, d.log.quiet).to_string(),
                summary: false,
                debug_addr,
                otlp: OtlpConfig::resolve(&settings.telemetry),
                trace_files: trace_files_from_env(),
            },
            std::io::stderr,
        )
    };

    let result = {
        let paths = Paths::resolve(d.config.as_deref());
        let pass = PassphraseSource::from_env(d.passphrase_file.clone());
        rt.block_on(async {
            let debug = match telemetry.start().await {
                Ok(bound) => bound,
                Err(e) => {
                    if let Some(a) = debug_addr {
                        tracing::warn!("debug port {a}: {e}");
                    }
                    None
                }
            };
            let shutdown = tokio_util::sync::CancellationToken::new();
            let token = shutdown.clone();
            tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                token.cancel();
            });
            // SIGHUP: re-read irsync.toml and apply the [daemon] settings.
            let reload = std::sync::Arc::new(tokio::sync::Notify::new());
            let hup = reload.clone();
            tokio::spawn(async move {
                use tokio::signal::unix::{signal, SignalKind};
                let Ok(mut hangup) = signal(SignalKind::hangup()) else {
                    return;
                };
                while hangup.recv().await.is_some() {
                    hup.notify_one();
                }
            });
            let res = irsync::daemon::run_from_settings(
                &paths,
                &pass,
                d.listen.clone(),
                shutdown,
                reload,
                debug,
            )
            .await;
            telemetry.shutdown().await;
            res
        })
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("irsync daemon: {e:#}");
            1
        }
    }
}

/// The document a key lives in: a split file when one holds the table.
fn doc_for(
    paths: &Paths,
    config: &Config,
    key: &[String],
) -> anyhow::Result<toml_edit::DocumentMut> {
    let owner = match key.first().map(String::as_str) {
        Some("hosts") => config.split_hosts.as_ref(),
        Some("exports") | Some("all") => config.split_exports.as_ref(),
        _ => None,
    };
    Ok(manage::load_doc(owner.unwrap_or(&paths.config))?)
}

/// A `hosts.*`, `exports.*` or `all.*` key is refused when a split file
/// owns the table, like the nouns.
fn config_editable(paths: &Paths, key: &[String]) -> anyhow::Result<()> {
    let table = match key.first().map(String::as_str) {
        Some("hosts") => manage::Table::Hosts,
        Some("exports") | Some("all") => manage::Table::Exports,
        _ => return Ok(()),
    };
    Ok(manage::ensure_editable(paths, table)?)
}

fn run_config(c: &ConfigArgs) -> i32 {
    use irsync::manage::config::{self as cfg, Got};
    let paths = file_paths(&c.files);
    match &c.cmd {
        ConfigCmd::Get { key } => {
            let r = (|| -> anyhow::Result<bool> {
                let path = cfg::parse_key(key)?;
                if cfg::kind_of(&path).is_none() {
                    bail!("unknown key {key:?}");
                }
                let config = Config::load(&paths)?;
                let doc = doc_for(&paths, &config, &path)?;
                let lines = match cfg::get(&doc, &path)? {
                    Got::Lines(lines) => lines,
                    Got::Unset => match cfg::default_of(&path) {
                        Some(lines) => lines,
                        None => return Ok(false),
                    },
                };
                for line in lines {
                    println!("{line}");
                }
                Ok(true)
            })();
            match r {
                Ok(true) => 0,
                Ok(false) => 1,
                Err(e) => {
                    eprintln!("irsync config get: {e:#}");
                    1
                }
            }
        }
        ConfigCmd::List => finish(
            "config list",
            (|| {
                let config = Config::load(&paths)?;
                let mut docs = vec![manage::load_doc(&paths.config)?];
                for split in [&config.split_hosts, &config.split_exports]
                    .into_iter()
                    .flatten()
                {
                    docs.push(manage::load_doc(split)?);
                }
                for doc in &docs {
                    for line in cfg::list(doc) {
                        println!("{line}");
                    }
                }
                Ok(())
            })(),
        ),
        ConfigCmd::Set { key, values } => finish(
            "config set",
            (|| {
                let path = cfg::parse_key(key)?;
                let kind =
                    cfg::kind_of(&path).ok_or_else(|| anyhow::anyhow!("unknown key {key:?}"))?;
                config_editable(&paths, &path)?;
                let value = cfg::parse_value(kind, values).with_context(|| key.to_string())?;
                let lock = manage::lock_config(&paths.config)?;
                let mut doc = lock.load()?;
                cfg::set(&mut doc, &path, value).with_context(|| key.to_string())?;
                lock.save(&doc).with_context(|| key.to_string())?;
                Ok(())
            })(),
        ),
        ConfigCmd::Unset { key } => finish(
            "config unset",
            (|| {
                let path = cfg::parse_key(key)?;
                if cfg::kind_of(&path).is_none() {
                    bail!("unknown key {key:?}");
                }
                config_editable(&paths, &path)?;
                let lock = manage::lock_config(&paths.config)?;
                let mut doc = lock.load()?;
                cfg::unset(&mut doc, &path).with_context(|| key.to_string())?;
                lock.save(&doc).with_context(|| key.to_string())?;
                Ok(())
            })(),
        ),
    }
}

/// Report a management command's result: `irsync <cmd>: error` and exit 1.
fn finish(cmd: &str, r: anyhow::Result<()>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("irsync {cmd}: {e:#}");
            1
        }
    }
}

fn file_paths(f: &FilesArgs) -> Paths {
    Paths::resolve(f.config.as_deref())
}

fn run_export(e: &ExportArgs) -> i32 {
    let paths = file_paths(&e.files);
    match &e.cmd {
        ExportCmd::Add(a) => finish(
            "export add",
            (|| {
                let (name, path) =
                    manage::exports::parse_add_target(&a.target, &std::env::current_dir()?)?;
                manage::ensure_editable(&paths, manage::Table::Exports)?;
                let lock = manage::lock_config(&paths.config)?;
                let mut doc = lock.load()?;
                manage::exports::add_export(&mut doc, &name, &path, a.ro, a.warm_index)?;
                // The daemon identity comes first: when it cannot be had, the
                // export is not added either.
                let settings = Config::load(&paths)?.settings;
                let pass = PassphraseSource::from_env(None);
                let secret = identity_of(&settings.daemon.identity, "irsync-daemon", &pass)?;
                lock.save(&doc)?;
                eprintln!(
                    "added export {name} ({}) to {}",
                    path.display(),
                    paths.config.display()
                );
                println!("{}", secret.public());
                Ok(())
            })(),
        ),
        ExportCmd::Remove { name } => finish(
            "export remove",
            (|| {
                manage::ensure_editable(&paths, manage::Table::Exports)?;
                let lock = manage::lock_config(&paths.config)?;
                let mut doc = lock.load()?;
                manage::exports::remove_export(&mut doc, name)
                    .with_context(|| format!("in {}", paths.config.display()))?;
                lock.save(&doc)?;
                eprintln!("removed export {name} from {}", paths.config.display());
                Ok(())
            })(),
        ),
        ExportCmd::Update {
            dry_run,
            log,
            names,
        } => export_update(&paths, names, *dry_run, *log),
        ExportCmd::List => finish(
            "export list",
            (|| {
                let Config { hosts, exports, .. } = Config::load(&paths)?;
                print!("{}", manage::show::export_blocks(&exports, &hosts));
                Ok(())
            })(),
        ),
    }
}

fn run_grant(g: &GrantArgs) -> i32 {
    let paths = file_paths(&g.files);
    match &g.cmd {
        GrantCmd::Add { export, who, ro } => finish(
            "grant add",
            (|| {
                if export == "immediate" && *ro {
                    bail!("immediate grants are read-write only: drop --ro");
                }
                let hosts = Config::load(&paths)?.hosts;
                let spec = PeerSpec::parse(who, &hosts)?;
                let resolve = |s: &str| hosts.get(s).map(|h| h.endpoint_id);
                manage::ensure_editable(&paths, manage::Table::Exports)?;
                let lock = manage::lock_config(&paths.config)?;
                let mut doc = lock.load()?;
                if export == "immediate" {
                    let added = manage::grants::grant_immediate(&mut doc, &spec, &resolve)
                        .with_context(|| format!("in {}", paths.config.display()))?;
                    lock.save(&doc)?;
                    if added {
                        eprintln!("{}: immediate access", spec.entry());
                    } else {
                        eprintln!("{}: already has immediate access", spec.entry());
                    }
                    return Ok(());
                }
                let target = (export != "all").then_some(export.as_str());
                manage::grants::grant_add(&mut doc, target, &spec, *ro, &resolve)
                    .with_context(|| format!("in {}", paths.config.display()))?;
                lock.save(&doc)?;
                let mode = if *ro { "ro" } else { "rw" };
                match target {
                    Some(e) => eprintln!("{}: {mode} on {e}", spec.entry()),
                    None => eprintln!("{}: {mode} on all exports", spec.entry()),
                }
                Ok(())
            })(),
        ),
        GrantCmd::Remove { export, who } => finish(
            "grant remove",
            (|| {
                let hosts = Config::load(&paths)?.hosts;
                let resolve = |s: &str| hosts.get(s).map(|h| h.endpoint_id);
                if who == "*" {
                    bail!(
                        "\"*\" names no peer: irsync grant remove {export} all removes every grant {}",
                        if export == "all" { "in the file".to_string() } else { format!("on {export}") }
                    );
                }
                let is_immediate = export == "immediate";
                let export = (export != "all").then_some(export.as_str());
                let who = (who != "all").then_some(who.as_str());
                // Text that names no known peer still matches entries literally
                // (an alias no longer in the hosts table).
                let spec = match who.map(|w| (w, PeerSpec::parse(w, &hosts))) {
                    None => None,
                    Some((_, Ok(s))) => Some(s),
                    Some((
                        w,
                        Err(manage::ManageError::Config(irsync::config::ConfigError::BadPeer(_))),
                    )) => Some(PeerSpec::Alias(w.to_string())),
                    Some((_, Err(e))) => return Err(e.into()),
                };
                manage::ensure_editable(&paths, manage::Table::Exports)?;
                let lock = manage::lock_config(&paths.config)?;
                let mut doc = lock.load()?;
                let n = if is_immediate {
                    manage::grants::grant_remove_immediate(&mut doc, spec.as_ref(), &resolve)
                } else {
                    manage::grants::grant_remove(&mut doc, export, spec.as_ref(), &resolve)
                }
                .with_context(|| format!("in {}", paths.config.display()))?;
                lock.save(&doc)?;
                eprintln!("removed {n} {}", if n == 1 { "entry" } else { "entries" });
                Ok(())
            })(),
        ),
        GrantCmd::List { target } => finish(
            "grant list",
            (|| {
                let Config { hosts, exports, .. } = Config::load(&paths)?;
                let text = match target.as_deref() {
                    None => manage::grants::list_all(&exports, &hosts),
                    Some(t) => match manage::grants::list_export(&exports, &hosts, t) {
                        Some(block) => block,
                        None => {
                            let Ok(id) = hosts.resolve_peer(t) else {
                                bail!(
                                    "{t:?} is not an export, an alias in {} or an EndpointId",
                                    paths.config.display()
                                );
                            };
                            let label = if hosts.get(t).is_some() {
                                format!("{t} ({id})")
                            } else {
                                id.to_string()
                            };
                            manage::grants::list_peer(&exports, &hosts, &label, &id)
                        }
                    },
                };
                print!("{text}");
                Ok(())
            })(),
        ),
    }
}

fn run_host(h: &HostArgs) -> i32 {
    let paths = file_paths(&h.files);
    match &h.cmd {
        HostCmd::Add(a) => finish("host add", host_add(a, &paths)),
        HostCmd::Remove { alias } => finish(
            "host remove",
            (|| {
                manage::ensure_editable(&paths, manage::Table::Hosts)?;
                let lock = manage::lock_config(&paths.config)?;
                let mut doc = lock.load()?;
                manage::hosts::host_remove(&mut doc, alias)
                    .with_context(|| format!("in {}", paths.config.display()))?;
                lock.save(&doc)?;
                eprintln!("removed host {alias} from {}", paths.config.display());
                Ok(())
            })(),
        ),
        HostCmd::List => finish(
            "host list",
            (|| {
                // Load first: an old-layout file is an error, not an empty list.
                let config = Config::load(&paths)?;
                let doc = manage::load_doc(&config.hosts.path)?;
                for line in manage::hosts::host_list(&doc) {
                    println!("{line}");
                }
                Ok(())
            })(),
        ),
    }
}

/// The client identity: `explicit` (`--identity`) or the config's `identity`
/// when set, else the default key, else `~/.ssh/id_ed25519`, else a key
/// generated at the default path. Says where it came from.
fn client_identity(
    explicit: Option<&std::path::Path>,
    settings: &irsync::config::settings::Settings,
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
    let key = irsync::identity::client_key_path(path, ssh);
    let (secret, source) = load_client_identity(path, ssh, pass, "irsync")
        .with_context(|| format!("loading identity {}", key.display()))?;
    if let KeySource::Ssh(p) = &source {
        tracing::info!(path = %p.display(), "using the ssh key as the client identity");
    }
    Ok((secret, source))
}

/// Loads (generating if missing, with a stderr notice) the identity at `path`.
fn identity_of(
    path: &std::path::Path,
    comment: &str,
    pass: &PassphraseSource,
) -> anyhow::Result<iroh::SecretKey> {
    let (secret, generated) = load_or_generate(path, pass, comment)
        .with_context(|| format!("identity {}", path.display()))?;
    if generated {
        eprintln!("generated identity {}", path.display());
    }
    Ok(secret)
}

fn run_id(i: &IdArgs) -> i32 {
    match &i.cmd {
        IdCmd::Show {
            daemon,
            all,
            passphrase_file,
        } => id_show(&i.files, *daemon, *all, passphrase_file.clone()),
        IdCmd::Generate { daemon, out, force } => {
            id_generate(&i.files, *daemon, out.as_deref(), *force)
        }
    }
}

/// `irsync id generate`: writes the key pair, prints the public key line
/// and the EndpointId. Replaces an existing file only with `force`.
fn id_generate(f: &FilesArgs, daemon: bool, out: Option<&std::path::Path>, force: bool) -> i32 {
    finish(
        "id generate",
        (|| {
            let (path, comment) = match out {
                Some(p) => (
                    p.to_path_buf(),
                    if daemon { "irsync-daemon" } else { "irsync" },
                ),
                None => {
                    let settings = Config::load(&file_paths(f))?.settings;
                    if daemon {
                        (settings.daemon.identity, "irsync-daemon")
                    } else {
                        (settings.identity, "irsync")
                    }
                }
            };
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            let mut pub_path = path.as_os_str().to_owned();
            pub_path.push(".pub");
            let pub_path = PathBuf::from(pub_path);
            if force {
                for p in [&path, &pub_path] {
                    if p.symlink_metadata().is_ok() {
                        std::fs::remove_file(p)
                            .with_context(|| format!("removing {}", p.display()))?;
                    }
                }
            }
            let secret = irsync::identity::generate_openssh(&path, comment)?;
            let line = std::fs::read_to_string(&pub_path)
                .with_context(|| format!("reading {}", pub_path.display()))?;
            eprintln!("wrote {} and {}", path.display(), pub_path.display());
            println!("{}", line.trim());
            println!("{}", secret.public());
            Ok(())
        })(),
    )
}

fn id_show(f: &FilesArgs, daemon: bool, all: bool, passphrase_file: Option<PathBuf>) -> i32 {
    finish(
        "id show",
        (|| {
            let settings = Config::load(&file_paths(f))?.settings;
            let pass = PassphraseSource::from_env(passphrase_file);
            let print = |secret: &iroh::SecretKey, comment: &str| {
                println!("{}", irsync::identity::public_key_line(secret, comment));
                println!("{}", secret.public());
            };
            let client = || -> anyhow::Result<()> {
                let (secret, source) = client_identity(None, &settings, &pass)?;
                if let KeySource::Generated(p) = &source {
                    eprintln!("generated identity {}", p.display());
                }
                print(&secret, "irsync");
                Ok(())
            };
            let show_daemon = || -> anyhow::Result<()> {
                // Showing is read-only: never create the daemon identity.
                let path = &settings.daemon.identity;
                if path.symlink_metadata().is_err() {
                    anyhow::bail!(
                        "no daemon identity at {}; run the daemon once or `irsync id generate --daemon`",
                        path.display()
                    );
                }
                print(
                    &identity_of(&settings.daemon.identity, "irsync-daemon", &pass)?,
                    "irsync-daemon",
                );
                Ok(())
            };
            if all {
                println!("client:");
                client()?;
                println!("daemon:");
                show_daemon()?;
            } else if daemon {
                show_daemon()?;
            } else {
                client()?;
            }
            Ok(())
        })(),
    )
}

fn run_show(f: &FilesArgs, passphrase_file: Option<PathBuf>) -> i32 {
    finish(
        "show",
        (|| {
            let paths = file_paths(f);
            let config = Config::load(&paths)?;
            let settings = &config.settings;
            // The hosts section is `host list`, line for line, from the
            // file that holds the table.
            let host_lines = manage::hosts::host_list(&manage::load_doc(&config.hosts.path)?);
            let pass = PassphraseSource::from_env(passphrase_file);
            let (client, source) = client_identity(None, settings, &pass)?;
            if let KeySource::Generated(p) = &source {
                eprintln!("generated identity {}", p.display());
            }
            let client_line = irsync::identity::public_key_line(&client, "irsync");
            let daemon = identity_of(&settings.daemon.identity, "irsync-daemon", &pass)?;
            print!(
                "{}",
                manage::show::report(
                    &paths,
                    &config,
                    &host_lines,
                    (&client.public(), &client_line),
                    &daemon.public(),
                )
            );
            Ok(())
        })(),
    )
}

fn host_add(a: &HostAddArgs, paths: &Paths) -> anyhow::Result<()> {
    let id = parse_endpoint_id(&a.id).with_context(|| format!("ID {:?}", a.id))?;
    let flags = manage::hosts::HostFlags {
        relay: a
            .relay
            .as_deref()
            .map(|r| r.parse().with_context(|| format!("--relay {r:?}")))
            .transpose()?,
        relay_mode: a.relay_mode,
        address: (!a.address.is_empty()).then(|| a.address.clone()),
        parallel: a.parallel,
        reconnect: a.reconnect.clone(),
        dial_retries: a.dial_retries,
        dht: a.dht,
        mdns: a.mdns,
        ssh: None,
    };
    manage::ensure_editable(paths, manage::Table::Hosts)?;
    let lock = manage::lock_config(&paths.config)?;
    let mut doc = lock.load()?;
    let old = manage::hosts::host_add(&mut doc, &a.alias, id, &flags)?;
    lock.save(&doc)?;
    match old {
        Some(old) => println!("{}: {old} -> {id}", a.alias),
        None => println!("{}: {id}", a.alias),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(max_delete: bool, errors: usize) -> Outcome {
        Outcome {
            max_delete_reached: max_delete,
            errors: (0..errors)
                .map(|_| (irsync::fs::paths::RelPath::root(), "e".to_string()))
                .collect(),
            ..Outcome::default()
        }
    }

    #[test]
    fn exit_codes_rank_25_over_23_over_1_over_0() {
        assert_eq!(combine_exit_codes(&[out(false, 0)], 0, 1), 0);
        assert_eq!(combine_exit_codes(&[out(false, 2)], 0, 1), 23);
        assert_eq!(combine_exit_codes(&[out(true, 2)], 0, 1), 25);
        // Several sources: one failed, the rest synced.
        assert_eq!(combine_exit_codes(&[out(false, 0)], 1, 3), 23);
        assert_eq!(combine_exit_codes(&[out(true, 0)], 1, 3), 25);
        assert_eq!(combine_exit_codes(&[out(true, 1), out(false, 3)], 0, 2), 25);
        // Nothing got through.
        assert_eq!(combine_exit_codes(&[out(false, 0)], 2, 2), 1);
    }
}
