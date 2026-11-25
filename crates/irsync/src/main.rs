// Unsafe code belongs in the library's platform module.
#![deny(unsafe_code)]

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
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
    validate, Args, ClientArgs, ClientCmd, Command, ConfigArgs, ConfigCmd, DaemonArgs, DeleteMode,
    Direction, ExportArgs, ExportCmd, FilesArgs, GrantArgs, GrantCmd, IdArgs, IdCmd, IndexArgs,
    LogArgs, PairArgs, RemoteArgs, ServerAddArgs, ServerArgs, ServerCmd, SyncOptions,
};
use irsync::config::clients::Clients;
use irsync::config::file::Config;
use irsync::config::paths::{state_dir, Paths};
use irsync::config::servers::{ServerEntry, Servers};
use irsync::config::settings::{PqKeyExchange, Settings};
use irsync::filter::FilterSet;
use irsync::fs::paths::{escape_path, escape_text};
use irsync::identity::{
    load_client_identity, load_or_generate, parse_endpoint_id, KeySource, PassphraseSource,
};
use irsync::index::Index;
use irsync::manage::{self, PeerSpec};
use irsync::net::{build_endpoint, client_endpoint_opts};
use irsync::session::engine::list_exports;
use irsync::session::reconnect::{
    describe_dial, explain, never_reached, probe_path, run_with_reconnect, ConnState, NeverReached,
    ReconnectOpts, PAIRED_FIRST_DIAL,
};
use irsync::session::single::{
    is_one_component, place_file, source_is_single, split_last, PlaceError,
};
use irsync::session::{resolve_roots, Event, EventBus, LocalSide, Outcome, Party, HASH_WORKERS};
use irsync::ssh::onboard::SetupFlags;
use irsync::telemetry::init::trace_files_from_env;
use irsync::telemetry::meter::Usage;
use irsync::telemetry::{self, OtlpConfig, Side, Telemetry, TelemetryConfig};
use irsync_proto::chunk::CHUNK_PARAMS;
use irsync_proto::v1::{export_entry, hello, path_info, Hello};
use irsync_proto::PROTOCOL_VERSION;

mod remote;

fn main() {
    let args = Args::parse();
    // The receiver holds a descriptor per file in flight; see
    // `platform::raise_handle_limit`.
    let _ = irsync::platform::raise_handle_limit();
    // Read the umask now, while this is the only thread.
    let _ = irsync::fs::meta::default_file_mode();
    let (log, files) = (args.log, &args.files);
    if !matches!(
        args.cmd,
        Some(Command::Sync(_) | Command::List(_) | Command::Daemon(_))
    ) {
        init_command_logging(log);
    }
    match &args.cmd {
        Some(Command::Sync(_)) => {
            let opts = SyncOptions::from_args(&args);
            exit_after_capture(run_sync(&args, &opts));
        }
        Some(Command::List(_)) => {
            let opts = SyncOptions::from_args(&args);
            exit_after_capture(run_list(args, &opts));
        }
        Some(Command::Daemon(d)) => exit_after_capture(run_daemon(d, log, files)),
        Some(Command::Gc { dry_run }) => std::process::exit(run_gc(*dry_run, log, files)),
        Some(Command::Index(ix)) => std::process::exit(run_index(ix, log, files)),
        Some(Command::Server(s)) => std::process::exit(run_server(s, files)),
        Some(Command::Client(c)) => std::process::exit(run_client(c, files)),
        Some(Command::Export(e)) => std::process::exit(run_export(e, log, files)),
        Some(Command::Grant(g)) => std::process::exit(run_grant(g, files)),
        Some(Command::Id(i)) => std::process::exit(run_id(i, files)),
        Some(Command::Config(c)) => std::process::exit(run_config(c, files)),
        Some(Command::Pair(a)) => std::process::exit(run_pair(a, files)),
        Some(Command::Show) => std::process::exit(run_show(files)),
        Some(Command::Remote(r)) => {
            let opts = SyncOptions::for_command(&args);
            exit_after_capture(run_remote(r, &opts));
        }
        Some(Command::SshSetup {
            client,
            client_name_hex,
            recheck,
            no_dht,
            no_service,
        }) => std::process::exit(run_ssh_setup(
            client,
            client_name_hex,
            *recheck,
            *no_dht,
            *no_service,
            files,
        )),
        Some(Command::VerifySignature {
            signature,
            public_key,
            trusted_comment,
            file,
        }) => std::process::exit(run_verify_signature(
            file,
            signature.as_deref(),
            public_key,
            trusted_comment.as_deref(),
        )),
        None => unreachable!("try_parse_from always sets a command"),
    }
}

/// Exits with `code` once the protocol capture, if one is open, is on disk.
/// The daemon gets here after SIGTERM or SIGINT too.
fn exit_after_capture(code: i32) -> ! {
    irsync::net::tap::drain_capture(std::time::Duration::from_secs(2));
    std::process::exit(code)
}

/// Logging for every command but a sync and the daemon, which set up
/// their own: `-v` alone is still warnings, as for a sync.
fn init_command_logging(log: LogArgs) {
    init_logging(log.verbose.saturating_sub(1), log.quiet, LogWriter::new());
}

/// The log filter for `-q` and `-v` on both sides, unless `RUST_LOG` is set.
/// Callers pass the `-v` count minus one: `-v` alone is still warnings.
fn log_default(verbose: u8, quiet: bool) -> &'static str {
    match (quiet, verbose) {
        (true, _) => "error",
        (false, 0) => "warn",
        (false, 1) => "warn,irsync=info",
        (false, 2) => "info,irsync=debug",
        (false, 3) => "debug,irsync=trace,irsync::wire=off",
        // `-vvvvv`: every protocol message too.
        _ => "debug,irsync=trace",
    }
}

/// Logging alone, for the commands without telemetry.
fn init_logging(verbose: u8, quiet: bool, writer: LogWriter) {
    let cfg = TelemetryConfig::logs_only(Side::Client, log_default(verbose, quiet));
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
/// gives the defaults here and fails later where it is used. A config file
/// that is open to other users is the one error returned: nothing may
/// happen before it is refused.
fn early_settings(
    config: Option<&std::path::Path>,
) -> Result<Settings, irsync::config::ConfigError> {
    match Config::load(&Paths::resolve(config)) {
        Ok(c) => Ok(c.settings),
        Err(e @ irsync::config::ConfigError::TooOpen { .. }) => Err(e),
        Err(_) => Ok(Settings::default()),
    }
}

/// Exit codes: 0 ok, 1 configuration, operand or connection errors, 23
/// partial transfer, 24 some source files vanished (`--paranoid`), 25
/// deletions stopped at `--max-delete` (23, 24 and 25 as in rsync; a command
/// line clap cannot parse has already exited 2).
fn run_sync(args: &Args, opts: &SyncOptions) -> i32 {
    let settings = match early_settings(opts.config.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("irsync: {e}");
            return 1;
        }
    };
    let log = LogWriter::new();
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("irsync: {e}");
            return 1;
        }
    };
    let mut telemetry = {
        let _rt = rt.enter();
        init_telemetry(
            TelemetryConfig {
                side: Side::Client,
                // `-v` lists files; logging starts at `-vv`.
                log_default: log_default(opts.verbose.saturating_sub(1), opts.quiet).to_string(),
                summary: opts.verbose >= 2 && !opts.quiet,
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
        // The live block only with `--progress`, on a terminal (`TERM=dumb`
        // is not one), and never with `-q`. Without it the reporter still
        // runs: it prints failures on stderr as they happen.
        let tty = opts.progress
            && !opts.quiet
            && std::io::stdout().is_terminal()
            && std::env::var("TERM").map_or(true, |t| t != "dumb");
        let task = {
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
            // With a block, log lines print above it.
            log.attach(screen.multi());
            // A bad command line fails in `sync` below with the same error.
            let source = match direction_of(args) {
                Ok(Direction::DaemonSends) => Party::Peer,
                Ok(Direction::ClientSends) | Err(_) => Party::Local,
            };
            let r =
                Reporter::with_screen(screen, opts.verbose, opts.stats, opts.itemize, opts.dry_run)
                    .with_quiet(opts.quiet)
                    .with_delete_mode(opts.delete)
                    .with_source(source);
            tokio::spawn(r.run(bus.subscribe(), None))
        };
        let start = Instant::now();
        let result = sync(args, opts, &bus, &log).await;
        // Every session has dropped its sender clone: the reporter drains and stops.
        drop(bus);
        let reporter = task.await.ok();
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
            irsync::file_report::record_outcome(&outcome);
            for path in &outcome.vanished {
                eprintln!(
                    "irsync: {}: vanished during the sync",
                    escape_path(path.as_bytes())
                );
            }
            if outcome.vanished_omitted > 0 {
                eprintln!(
                    "irsync: and {} more files vanished during the sync",
                    outcome.vanished_omitted
                );
            }
            // Those the reporter printed as they happened are not repeated.
            for (path, msg) in &outcome.errors {
                if !reporter.as_mut().is_some_and(|r| r.take_shown(path, msg)) {
                    eprintln!(
                        "irsync: {}: {}",
                        escape_path(path.as_bytes()),
                        escape_text(msg)
                    );
                }
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
            // A refusal or a close reason is the daemon's own text.
            eprintln!("irsync: {}", escape_text(&format!("{e:#}")));
            1
        }
    };
    rt.block_on(async {
        telemetry.flush();
        if let (Some(addr), false) = (debug_port, opts.debug_no_wait) {
            // Listen before saying so: a signal sent on seeing the line ends the wait.
            let signals = irsync::platform::exit_signals();
            eprintln!("irsync: debug port open at {addr}; Ctrl-C to exit");
            irsync::platform::wait_for_exit_signal(signals).await;
        }
        telemetry.shutdown().await;
    });
    code
}

/// The process exit code for `total` sessions of which `failed` errored:
/// all failed = 1 (nothing got through); otherwise 25 (deletions stopped at
/// `--max-delete`) outranks 23 (partial transfer, per-file errors or some
/// sources failed) outranks 24 (source files vanished, `--paranoid`)
/// outranks 0.
fn combine_exit_codes(outcomes: &[Outcome], failed: usize, total: usize) -> i32 {
    if failed > 0 && failed >= total {
        1
    } else if outcomes.iter().any(|o| o.max_delete_reached) {
        25
    } else if failed > 0 || outcomes.iter().any(|o| !o.errors.is_empty()) {
        23
    } else if outcomes.iter().any(|o| o.vanished_count() > 0) {
        24
    } else {
        0
    }
}

/// Partial files and sessions untouched for this long are collected.
const CLIENT_GC_AGE: std::time::Duration = std::time::Duration::from_secs(7 * 86400);
/// A partial record untouched this long makes the next run's GC walk due.
const CLIENT_GC_STALE: std::time::Duration = std::time::Duration::from_secs(3600);

fn run_gc(dry_run: bool, log: LogArgs, files: &FilesArgs) -> i32 {
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
            let mut roots = Vec::new();
            let mut old = 0usize;
            for file in files {
                // An older build's index: this one never reads it. Removed
                // unless a process (that build's daemon) still has it open.
                if !Index::is_current_file(&file) {
                    let gone = if dry_run {
                        Ok(true)
                    } else {
                        Index::remove_unused(&file)
                    };
                    match gone {
                        Ok(true) => {
                            old += 1;
                            if !log.quiet {
                                println!("{}", file.display());
                            }
                        }
                        Ok(false) => eprintln!("irsync: {}: in use, kept", file.display()),
                        Err(e) => eprintln!("irsync: {}: {e}", file.display()),
                    }
                    continue;
                }
                match Index::root_of_file(&file) {
                    Ok(Some(r)) => roots.push(r),
                    Ok(None) => {}
                    Err(e) => eprintln!("irsync: {}: {e}", file.display()),
                }
            }
            for root in &roots {
                // A root inside this one has its own index: leave it to that.
                let skip = irsync::daemon::nested_roots(root, roots.iter().map(PathBuf::as_path));
                let root = root.clone();
                let index = match Index::open(&dir, &root) {
                    Ok(i) => i.into_handle(),
                    Err(e) => {
                        eprintln!("irsync: {}: {e}", root.display());
                        continue;
                    }
                };
                match irsync::daemon::gc_skipping(index, &root, CLIENT_GC_AGE, dry_run, &skip).await
                {
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
                let old = if old > 0 {
                    format!(", {old} old index file(s)")
                } else {
                    String::new()
                };
                println!(
                    "{parts} part file(s), {sessions} session(s), {entries} index entr{}{old} {}",
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

/// The daemon's EndpointId: a server's name, else the host string itself
/// must be an EndpointId. `clients` is only for the message about a name
/// that is a client's and no server's.
fn resolve_target(servers: &Servers, clients: &Clients, host: &str) -> anyhow::Result<EndpointId> {
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
        .sync_args()
        .src()
        .iter()
        .map(|s| Location::parse(s))
        .collect::<Result<Vec<_>, _>>()?;
    let dest = Location::parse(args.sync_args().dest().context("missing destination")?)?;
    let direction = validate(&srcs, &dest)?;
    Ok((srcs, dest, direction))
}

/// Which way the data flows, from the source and destination arguments.
fn direction_of(args: &Args) -> anyhow::Result<Direction> {
    Ok(locations(args)?.2)
}

/// Whether a connection to this host must have the post-quantum hybrid key
/// exchange: the flag, else the host entry, else the client settings.
fn pq_required(opts: &SyncOptions, host_entry: Option<&ServerEntry>, settings: &Settings) -> bool {
    pq_required_of(
        opts.pq_key_exchange,
        host_entry.and_then(|h| h.pq_key_exchange),
        settings.client.pq_key_exchange,
    )
}

/// [`pq_required`] from the three places that can say: the first that is
/// set decides.
fn pq_required_of(
    flag: Option<PqKeyExchange>,
    host: Option<PqKeyExchange>,
    client: Option<PqKeyExchange>,
) -> bool {
    flag.or(host).or(client) == Some(PqKeyExchange::Require)
}

/// A client endpoint ready to dial `host`, plus what the session setup
/// still needs from the configuration.
struct Connected {
    endpoint: iroh::Endpoint,
    target: EndpointAddr,
    settings: Settings,
    host_entry: Option<ServerEntry>,
    /// The saved alias `host_entry` is under: the host as typed, or the
    /// alias a first run saved it as.
    alias: Option<String>,
    /// `opts` with the per-host and settings defaults filled in.
    opts: SyncOptions,
}

impl Connected {
    /// The name of a server `irsync pair` saved (one with an `ssh` value).
    fn paired(&self) -> Option<&str> {
        self.host_entry
            .as_ref()
            .filter(|h| h.ssh.is_some())
            .and(self.alias.as_deref())
    }

    /// The bound on each dial before the daemon has answered: see
    /// [`PAIRED_FIRST_DIAL`] for a paired server, else iroh's own.
    fn first_dial(&self) -> Option<std::time::Duration> {
        first_dial_limit(self.paired().map(|_| PAIRED_FIRST_DIAL))
    }
}

/// Resolves `host`, the direct addresses, the client identity and the
/// endpoint, the same way for a sync and for a listing.
async fn connect_for(host: &str, opts: &SyncOptions, log: &LogWriter) -> anyhow::Result<Connected> {
    let paths = Paths::resolve(opts.config.as_deref());
    let config = Config::load(&paths)?;
    let pass = PassphraseSource::from_env(
        opts.passphrase_file.clone(),
        config.settings.unsafe_config_allowed,
    );
    // An address that is the `ssh` value of a saved server finds that
    // server: no ssh is needed for that.
    let by_ssh = irsync::ssh::pair::saved_by_ssh(&config.servers, &config.clients, host)?;
    if let Some(alias) = &by_ssh {
        log.eprintln(&format!("irsync: {host} is saved as {alias}"));
    }
    let host = by_ssh.as_deref().unwrap_or(host);
    // A sync never sets a server up: it says how to.
    if config.servers.get(host).is_none() && parse_endpoint_id(host).is_err() {
        if config.clients.get(host).is_some() {
            bail!(irsync::session::reconnect::client_only(host));
        }
        bail!(irsync::session::reconnect::not_paired(host, &paths.config));
    }
    let Config {
        settings,
        servers,
        clients,
        ..
    } = config;
    let host_entry = servers.get(host).cloned();
    let target = resolve_target(&servers, &clients, host)?;
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
    let secret = load_client_key(&opts, &settings, &pass)?;
    if !addresses.is_empty() {
        tracing::info!(
            addresses = ?resolved,
            "isolated mode: dialling directly, relay and lookups off"
        );
    }
    let mut endpoint_opts = client_endpoint_opts(
        secret,
        opts.relay.as_deref(),
        opts.no_relay,
        opts.no_dht,
        &settings.client,
        host_entry.as_ref(),
        &addresses,
    )?;
    if !addresses.is_empty() {
        // Bound to every interface, the client would offer the daemon all
        // of them for extra paths; bound to the source of the dial, only
        // that one.
        endpoint_opts.bind_addrs = irsync::net::addr::source_binds(&resolved);
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
        settings,
        alias: host_entry.is_some().then(|| host.to_string()),
        host_entry,
        opts,
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

/// What setting a host up over ssh did, then the service how-to.
fn report_onboarded(log: &LogWriter, o: &irsync::ssh::onboard::Onboarded) {
    for line in irsync::ssh::onboard::summary(o) {
        log.eprintln(&format!("irsync: {line}"));
    }
    // Raw, so the unit file and commands can be pasted.
    for line in irsync::ssh::onboard::howto(o) {
        log.eprintln(&line);
    }
}

/// `irsync pair HOST`: sets the server up over ssh and saves it.
fn run_pair(a: &PairArgs, files: &FilesArgs) -> i32 {
    let log = LogWriter::new();
    finish(
        "pair",
        (|| {
            let paths = file_paths(files);
            let config = Config::load(&paths)?;
            let pass = PassphraseSource::from_env(
                files.passphrase_file.clone(),
                config.settings.unsafe_config_allowed,
            );
            let t = irsync::ssh::pair::target(&config.servers, &config.clients, &a.host)?;
            if let Some(typed) = &t.found_as {
                log.eprintln(&format!("irsync: {typed} is saved as {}", t.host));
            }
            // No ssh call yet: these errors need no "over ssh failed" context.
            match &t.alias {
                Some(alias) => irsync::ssh::onboard::check(alias, &t.dest)?,
                None => irsync::ssh::onboard::check_dest(&t.dest)?,
            }
            let secret = client_key(a.identity.as_deref(), &config.settings, &pass)?;
            let rsh = irsync::ssh::client::Rsh::from_opts(a.rsh.as_deref());
            log.eprintln(&format!("irsync: pairing with {} over ssh", t.host));
            let flags = SetupFlags {
                recheck: t.kind == irsync::ssh::pair::PairKind::Saved,
                no_dht: a.no_dht,
                no_install: a.no_install || config.settings.client.ssh_install == Some(false),
                no_service: a.no_service,
            };
            let host = &t.host;
            let o = irsync::ssh::onboard::onboard(
                &paths,
                &rsh,
                t.alias.as_deref(),
                &t.dest,
                secret.public(),
                flags,
            )
            .with_context(|| match t.kind {
                irsync::ssh::pair::PairKind::ClientOnly => format!(
                    "{host} is saved in {} as a client only; setting its daemon \
                     up over ssh failed",
                    paths.config.display()
                ),
                irsync::ssh::pair::PairKind::First => format!(
                    "{host:?} is not a server in {} and not an EndpointId; \
                     setting it up over ssh failed",
                    paths.config.display()
                ),
                irsync::ssh::pair::PairKind::Saved => {
                    format!("checking {host} over ssh failed")
                }
            })?;
            report_onboarded(&log, &o);
            if o.report.grant == irsync::ssh::setup::GrantOutcome::Missing {
                bail!(irsync::session::reconnect::no_access_hint(
                    &format!("{}:", o.alias),
                    &secret.public()
                ));
            }
            log.eprintln(&format!(
                "irsync: {}",
                irsync::ssh::pair::paired_line(&o.alias)
            ));
            Ok(())
        })(),
    )
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
fn client_key(
    explicit: Option<&Path>,
    settings: &Settings,
    pass: &PassphraseSource,
) -> anyhow::Result<iroh::SecretKey> {
    let (secret, source) = client_identity(explicit, settings, pass)?;
    if let KeySource::Generated(path) = &source {
        eprintln!("generated identity {}", path.display());
        eprintln!("{}", irsync::identity::public_key_line(&secret, "irsync"));
        eprintln!("{}", secret.public());
    }
    Ok(secret)
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
    let operand = args.sync_args().paths[0].clone();
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
        // With the slash a listed file keeps its own name.
        let dest = format!("{}/", tmp.path().join("list").display());
        // The listing is a dry-run pull into the temporary directory.
        let Some(Command::List(mut s)) = args.cmd.take() else {
            unreachable!("run_list takes a list command")
        };
        s.paths = vec![operand, dest];
        args.cmd = Some(Command::Sync(s));
        let mut opts = opts.clone();
        opts.dry_run = true;
        opts.verbose = opts.verbose.max(1);
        // A listing shows the tree, so it recurses unless told not to (a
        // sync needs -r or -a for that).
        opts.preserve.recursive = !args.sync_args().no_recursive;
        // A listing is a question: with the daemon unreachable it fails
        // now rather than waiting out a sync's reconnect window.
        opts.reconnect = Some(std::time::Duration::ZERO);
        return run_sync(&args, &opts);
    }
    let log = LogWriter::new();
    // Counts -v as a sync does: logging starts at -vv.
    init_logging(opts.verbose.saturating_sub(1), opts.quiet, log.clone());
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("irsync: {e}");
            return 1;
        }
    };
    match rt.block_on(list_exports_of(&operand, opts, &log)) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("irsync: {}", escape_text(&format!("{e:#}")));
            1
        }
    }
}

async fn list_exports_of(operand: &str, opts: &SyncOptions, log: &LogWriter) -> anyhow::Result<()> {
    let Location::Remote { host, .. } = Location::parse(operand)? else {
        bail!("internal: a listing takes a remote operand");
    };
    let c = connect_for(&host, opts, log).await?;
    // Closed on every way out: an endpoint dropped open makes iroh log an
    // error line before irsync's own message (#256).
    let listed = list_exports_over(&c, operand).await;
    c.endpoint.close().await;
    listed
}

async fn list_exports_over(c: &Connected, operand: &str) -> anyhow::Result<()> {
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
    let conn = dial(c).await?;
    let entries = list_exports(&conn, hello)
        .await
        .map_err(|e| explain(e, Some(&conn), operand, &client))?;
    for e in entries {
        let access = if e.access == export_entry::Access::Rw as i32 {
            "rw"
        } else {
            "ro"
        };
        println!("{} {} {access}", escape_text(&e.name), escape_path(&e.path));
    }
    conn.close(0u32.into(), b"done");
    Ok(())
}

/// Dials the daemon of `c` once, bounded as a first dial is, and checks the
/// post-quantum key exchange when it is required. For the one-shot exchanges
/// (a listing, `remote`); the caller closes the connection.
async fn dial(c: &Connected) -> anyhow::Result<iroh::endpoint::Connection> {
    let dial = c.endpoint.connect(c.target.clone(), irsync_proto::ALPN);
    let dialled = match c.first_dial() {
        Some(limit) => match tokio::time::timeout(limit, dial).await {
            Ok(r) => r.map_err(|e| describe_dial(&c.target.id, &e)),
            Err(_) => Err(format!(
                "connecting to {}: no answer within {} s",
                c.target.id,
                limit.as_secs()
            )),
        },
        None => dial.await.map_err(|e| describe_dial(&c.target.id, &e)),
    };
    let conn = dialled.map_err(|msg| never_reached(anyhow::anyhow!(msg), c.paired()))?;
    irsync::net::tap::ConnTap::of(&conn).set_local_id(c.endpoint.id());
    irsync::session::reconnect::check_pq_key_exchange(
        &conn,
        pq_required(&c.opts, c.host_entry.as_ref(), &c.settings),
        &c.target.id,
    )?;
    Ok(conn)
}

/// `irsync remote SERVER COMMAND`: one change or one read of the
/// server's config, printed as the same command prints it locally.
fn run_remote(r: &RemoteArgs, opts: &SyncOptions) -> i32 {
    let plan = match remote::plan(&r.server, &r.cmd) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("irsync remote: {}", escape_text(&format!("{e:#}")));
            return 1;
        }
    };
    let log = LogWriter::new();
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("irsync remote: {e}");
            return 1;
        }
    };
    let done = rt.block_on(async {
        let c = connect_for(&r.server, opts, &log).await?;
        // Closed on every way out (#256).
        let done = remote::run_remote(&c, plan.request).await;
        c.endpoint.close().await;
        done
    });
    match done.and_then(|outcome| remote::print(&plan.shape, &r.server, outcome)) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("irsync remote: {}", escape_text(&format!("{e:#}")));
            1
        }
    }
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
        Direction::ClientSends => args.sync_args().dest().unwrap_or_default().to_string(),
        Direction::DaemonSends => args.sync_args().src()[0].clone(),
    };

    let conn = connect_for(host, opts, log).await?;
    let filters = match FilterSet::from_rules(&conn.opts.filters).context("reading filter rules") {
        Ok(f) => Arc::new(f),
        Err(e) => {
            conn.endpoint.close().await;
            return Err(e);
        }
    };

    // One session per source; several sources only when pushing.
    let pairs: Vec<(&Location, &Location)> = srcs.iter().map(|s| (s, &dest)).collect();
    let mut total = Outcome::default();
    let (mut sessions, mut failed) = (0, 0);
    let of = pairs.len();
    for (index, (src, dst)) in pairs.into_iter().enumerate() {
        let operand_from = args.sync_args().src()[index].clone();
        let operand_to = args.sync_args().dest().unwrap_or_default().to_string();
        bus.emit(Event::SessionStart {
            index,
            of,
            from: operand_from.clone(),
            to: operand_to.clone(),
        });
        let (opts, settings, host_entry) = (&conn.opts, &conn.settings, conn.host_entry.as_ref());
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
        let dir_plan = Plan {
            root,
            local_contents_only: *local_co,
            hello: Hello {
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
            },
            single: None,
        };
        let several = of > 1;
        let ropts = ReconnectOpts {
            max: opts
                .reconnect
                .or(host_entry.and_then(|h| h.reconnect))
                .or(settings.client.reconnect)
                .unwrap_or(ReconnectOpts::default().max),
            // A paired server is dialled once (see `PAIRED_FIRST_DIAL`), unless
            // the command line asks for retries.
            dial_retries: match conn.paired() {
                Some(_) => opts.dial_retries.unwrap_or(0),
                None => opts
                    .dial_retries
                    .or(host_entry.and_then(|h| h.dial_retries))
                    .or(settings.client.dial_retries)
                    .unwrap_or(ReconnectOpts::default().dial_retries),
            },
            source_index: index,
            first_dial_timeout: conn.first_dial(),
            require_pq: pq_required(opts, host_entry, settings),
            ..ReconnectOpts::default()
        };
        sessions += 1;
        let started = Instant::now();
        let result = {
            let res = async {
                let plan = match direction {
                    Direction::ClientSends => {
                        push_plan(dir_plan.clone(), path, *local_co, several)?
                    }
                    // With a trailing slash the remote path must be a directory.
                    Direction::DaemonSends if !*remote_co => {
                        let (endpoint, target) = (&conn.endpoint, conn.target.clone());
                        let timeout = conn.opts.timeout;
                        let kind =
                            probe_path(endpoint, target, rpath, &remote_text, ropts, timeout)
                                .await?;
                        pull_plan(dir_plan.clone(), kind, path, *local_co, rpath)?
                    }
                    Direction::DaemonSends => dir_plan.clone(),
                };
                session(
                    &conn.endpoint,
                    conn.target.clone(),
                    &remote_text,
                    direction,
                    plan,
                    &filters,
                    &conn.opts,
                    ropts,
                    bus,
                )
                .await
            }
            .await;
            // A server that was never reached (a refusal is final).
            match res {
                Err(e) if e.downcast_ref::<NeverReached>().is_some() => {
                    Err(never_reached(e, conn.paired()))
                }
                res => res,
            }
        };
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
            key_exchange: None,
            usage: Usage::default(),
            files_transferred: 0,
            files_hashed: 0,
            bytes_sent: 0,
            bytes_received: 0,
            failed: false,
        };
        let out = match result {
            Ok(out) => {
                summary.reconnects = out.reconnects;
                summary.conn = out.conn;
                summary.key_exchange = out.key_exchange.clone();
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
                summary.failed = true;
                bus.emit(Event::SessionEnd(Arc::new(summary)));
                // Keep what earlier sources achieved; report this one and go on.
                // The error may carry the daemon's own words.
                let e = escape_text(&format!("{e:#}"));
                log.eprintln(&format!("irsync: {}: {e}", path.display()));
                failed += 1;
                continue;
            }
        };
        total.absorb(out);
    }
    conn.endpoint.close().await;
    Ok(Totals {
        outcome: total,
        sessions,
        failed,
    })
}

/// One source's session: the local root, and the `Hello` to send.
#[derive(Clone)]
struct Plan {
    root: PathBuf,
    local_contents_only: bool,
    hello: Hello,
    /// The local name of the one file a single-file session copies.
    single: Option<Vec<u8>>,
}

/// A push of `path`: a file is pushed on its own from the directory that
/// holds it (a trailing slash on it is an error); anything else as `dir`.
/// Several sources always go into the destination as a directory.
fn push_plan(dir: Plan, path: &Path, contents_only: bool, several: bool) -> anyhow::Result<Plan> {
    // The caller's message already starts with `path`.
    let single = source_is_single(path, contents_only).map_err(|e| match e {
        PlaceError::Io(_, e) => anyhow::Error::new(e),
        PlaceError::NotADirectory(_) => anyhow::anyhow!("not a directory"),
        e => e.into(),
    })?;
    if !single {
        return Ok(dir);
    }
    let (root, name) = split_last(path);
    let hello = Hello {
        single_file: true,
        name: name.clone(),
        contents_only: dir.hello.contents_only || several,
        ..dir.hello
    };
    Ok(Plan {
        root,
        local_contents_only: false,
        hello,
        single: Some(name),
    })
}

/// A pull of `rpath`, which the daemon says is `kind`, into `local`: a file
/// is placed by rsync's rules and pulled on its own; anything else as `dir`.
fn pull_plan(
    dir: Plan,
    kind: path_info::Kind,
    local: &Path,
    contents_only: bool,
    rpath: &[u8],
) -> anyhow::Result<Plan> {
    if kind != path_info::Kind::File {
        return Ok(dir);
    }
    let basename = rpath.rsplit(|&b| b == b'/').next().unwrap_or_default();
    if !is_one_component(basename) || basename == b"~" {
        bail!("cannot tell the file's name from {}", escape_path(rpath));
    }
    let placed = place_file(local, contents_only, basename, true)?;
    let hello = Hello {
        single_file: true,
        name: placed.name.clone(),
        contents_only: false,
        ..dir.hello
    };
    Ok(Plan {
        root: placed.root,
        local_contents_only: false,
        hello,
        single: Some(placed.name),
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
    plan: Plan,
    filters: &Arc<FilterSet>,
    opts: &SyncOptions,
    ropts: ReconnectOpts,
    bus: &EventBus,
) -> anyhow::Result<Outcome> {
    let Plan {
        root,
        local_contents_only,
        hello,
        single,
    } = plan;
    // A single-file copy never deletes anything.
    let single_opts;
    let opts = match single {
        Some(_) => {
            single_opts = SyncOptions {
                delete: DeleteMode::Off,
                delete_excluded: false,
                max_delete: None,
                ..opts.clone()
            };
            &single_opts
        }
        None => opts,
    };
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
    // A single file's root is a directory that holds other things: no
    // housekeeping there.
    if !dry_run && single.is_none() {
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
    if let Some(name) = single {
        side = side.with_single(name);
    }
    side.hooks.bus = bus.clone();
    #[cfg(feature = "test-hooks")]
    {
        side.hooks.no_change_checks = irsync::session::no_change_checks_from_env();
    }
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
fn run_index(a: &IndexArgs, log: LogArgs, files: &FilesArgs) -> i32 {
    let state = match Config::load(&file_paths(files)) {
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
    match index_roots(targets, a.dry_run, log.quiet) {
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

/// What `irsync daemon` says when it will not run as root.
const ROOT_REFUSAL: &str = "refusing to run as root: the daemon has not had a security audit,
  and as root any client with write access can create setuid files and device nodes.
  Run it as an unprivileged account. To run as root anyway, pass
  --run-as-root-i-know-what-i-am-doing or set `allow_root = true` under [daemon].";

/// Whether this process runs as root (effective user id 0). A test-hooks
/// build also says yes when `IRSYNC_TEST_EUID_ROOT` is `1`.
fn running_as_root() -> bool {
    #[cfg(feature = "test-hooks")]
    if std::env::var("IRSYNC_TEST_EUID_ROOT").as_deref() == Ok("1") {
        return true;
    }
    irsync::platform::is_root()
}

fn run_daemon(d: &DaemonArgs, log: LogArgs, files: &FilesArgs) -> i32 {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("irsync daemon: {e:#}");
            return 1;
        }
    };
    // Before any key, socket, lock or state file is touched: not as root,
    // unless the server's owner said so.
    let as_root = running_as_root();
    if as_root && !d.run_as_root {
        match Config::load(&Paths::resolve(files.config.as_deref())) {
            // The key may be in a file that does not load: say that.
            Err(e) => {
                eprintln!("irsync daemon: {e:#}");
                return 1;
            }
            Ok(c) if !c.settings.daemon.allow_root => {
                eprintln!("irsync daemon: {ROOT_REFUSAL}");
                return 1;
            }
            Ok(_) => {}
        }
    }
    let settings = match early_settings(files.config.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("irsync daemon: {e}");
            return 1;
        }
    };
    let debug_addr = settings.daemon.debug_addr;
    let mut telemetry = {
        let _rt = rt.enter();
        init_telemetry(
            TelemetryConfig {
                side: Side::Daemon,
                log_default: log_default(log.verbose.saturating_sub(1), log.quiet).to_string(),
                summary: false,
                debug_addr,
                otlp: OtlpConfig::resolve(&settings.telemetry),
                trace_files: trace_files_from_env(),
            },
            std::io::stderr,
        )
    };

    // Printed, not logged: -q and RUST_LOG must not hide it.
    if as_root {
        eprintln!(
            "irsync daemon: warning: running as root: any client with write access can \
             create setuid files and device nodes"
        );
    }

    let result = {
        let paths = Paths::resolve(files.config.as_deref());
        // The daemon sets `allow_open` from the config each time it reads it.
        let pass = PassphraseSource::from_env(files.passphrase_file.clone(), false);
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
                irsync::platform::stop_signal().await;
                token.cancel();
            });
            // SIGHUP: re-read irsync.toml and apply the [daemon] settings.
            let reload = std::sync::Arc::new(tokio::sync::Notify::new());
            let hup = reload.clone();
            tokio::spawn(async move {
                irsync::platform::on_each_reload_signal(|| hup.notify_one()).await;
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

fn run_config(c: &ConfigArgs, files: &FilesArgs) -> i32 {
    use irsync::manage::config as cfg;
    let paths = file_paths(files);
    // A noun alone lists.
    let list = ConfigCmd::List;
    match c.cmd.as_ref().unwrap_or(&list) {
        ConfigCmd::Get { key } => {
            let r = (|| -> anyhow::Result<bool> {
                let path = cfg::key_path(key)?;
                // Load first: a file that would not load is an error.
                Config::load(&paths)?;
                let msg = manage::load_message(&paths.config)?;
                let Some(lines) = cfg::get_or_default(&msg, &path)? else {
                    return Ok(false);
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
                Config::load(&paths)?;
                for line in cfg::list(&manage::load_message(&paths.config)?) {
                    println!("{line}");
                }
                Ok(())
            })(),
        ),
        ConfigCmd::Set { key, values } => finish(
            "config set",
            (|| {
                let path = cfg::parse_key(key)?;
                let kind = cfg::kind_of(&path).ok_or_else(|| cfg::unknown_key(key))?;
                cfg::writable(&path)?;
                let value = cfg::parse_value(kind, values).with_context(|| key.to_string())?;
                let mut lock = manage::lock_config(&paths.config)?;
                let mut msg = lock.load()?;
                let before = peer_ids(&msg, &paths.config);
                cfg::set(&mut msg, &path, value).with_context(|| key.to_string())?;
                lock.save(&mut msg).with_context(|| key.to_string())?;
                note_moved_grants("config set", &before, &msg, &paths.config);
                Ok(())
            })(),
        ),
        ConfigCmd::Unset { key } => finish(
            "config unset",
            (|| {
                let path = cfg::parse_key(key)?;
                if cfg::kind_of(&path).is_none() {
                    return Err(cfg::unknown_key(key).into());
                }
                cfg::writable(&path)?;
                let mut lock = manage::lock_config(&paths.config)?;
                let mut msg = lock.load()?;
                let before = peer_ids(&msg, &paths.config);
                // Unsetting a client's table or its id removes the client:
                // like `client remove`, not while grants name it. A server
                // goes whatever the grants say: they name clients only.
                let clients = if path[0] == "clients" {
                    // A file that does not load is that error, not a
                    // refused save.
                    Config::load(&paths)?;
                    match path.get(1) {
                        Some(name) => manage::canonicalize_without(&mut msg, &paths.config, name)?,
                        None => manage::canonicalize_doc(&mut msg, &paths.config)?,
                    };
                    manage::clients::client_names(&msg)
                } else {
                    Vec::new()
                };
                cfg::unset(&mut msg, &path).with_context(|| key.to_string())?;
                let left = manage::clients::client_names(&msg);
                for name in clients.iter().filter(|a| !left.contains(a)) {
                    let lists = manage::grants::lists_naming(&msg, name);
                    if !lists.is_empty() {
                        bail!(
                            "client {name:?} is named by grants in {}: remove those grants \
                             first, or use `irsync client remove {name} --force`",
                            lists.join(", ")
                        );
                    }
                }
                lock.save(&mut msg).with_context(|| key.to_string())?;
                note_moved_grants("config unset", &before, &msg, &paths.config);
                Ok(())
            })(),
        ),
    }
}

/// The id of each client of `msg`; see [`manage::clients::client_ids`].
fn peer_ids(
    msg: &irsync_proto::config::Config,
    path: &Path,
) -> std::collections::BTreeMap<String, iroh::EndpointId> {
    manage::clients::client_ids(msg, path)
}

/// Says on stderr which grants now apply to another id; see
/// [`manage::clients::moved_grant_notes`].
fn note_moved_grants(
    cmd: &str,
    before: &std::collections::BTreeMap<String, iroh::EndpointId>,
    msg: &irsync_proto::config::Config,
    path: &Path,
) {
    for note in manage::clients::moved_grant_notes(cmd, before, msg, path) {
        eprintln!("{note}");
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

/// `irsync verify-signature`: 0 good, 1 bad, missing or untrusted, 2 usage.
fn run_verify_signature(
    file: &std::path::Path,
    signature: Option<&std::path::Path>,
    public_keys: &[String],
    trusted_comment: Option<&str>,
) -> i32 {
    if public_keys.iter().any(|k| k.trim().is_empty()) {
        eprintln!("irsync: --public-key: no key given");
        return 2;
    }
    let keys = if public_keys.is_empty() {
        irsync::signature::release_keys()
    } else {
        match public_keys
            .iter()
            .map(|k| irsync::signature::parse_keys(k))
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(v) => v.into_iter().flatten().collect(),
            Err(e) => {
                eprintln!("irsync: --public-key: {e}");
                return 2;
            }
        }
    };
    let sig = signature.map(std::path::PathBuf::from).unwrap_or_else(|| {
        let mut name = file.as_os_str().to_owned();
        name.push(".minisig");
        name.into()
    });
    match irsync::signature::verify_file(file, &sig, &keys, trusted_comment) {
        Ok(v) => {
            println!(
                "good signature: {}, key {}, trusted comment: {}",
                file.display(),
                v.key_id,
                v.trusted_comment
            );
            0
        }
        Err(irsync::signature::SignatureError::Io { path, source }) if path == file => {
            eprintln!("irsync: cannot read {}: {source}", file.display());
            1
        }
        Err(e) => {
            eprintln!("irsync: bad signature for {}: {e}", file.display());
            1
        }
    }
}

fn file_paths(f: &FilesArgs) -> Paths {
    Paths::resolve(f.config.as_deref())
}

/// `irsync ssh-setup`: the report between its marker lines on stdout.
fn run_ssh_setup(
    client: &str,
    client_name_hex: &str,
    recheck: bool,
    no_dht: bool,
    no_service: bool,
    files: &FilesArgs,
) -> i32 {
    // The client stops when its probe sees root; this holds when it did not.
    if running_as_root() {
        eprintln!(
            "irsync: this account is root, and {}",
            irsync::ssh::onboard::ROOT_ACCOUNT
        );
        return 1;
    }
    let result = parse_endpoint_id(client)
        .with_context(|| format!("--client {client:?}"))
        .and_then(|id| {
            // The client never passes --passphrase-file, and setup never
            // asks for a passphrase. Given by hand with no daemon key, refuse
            // rather than generate an unencrypted one.
            if files.passphrase_file.is_some() {
                let settings = Config::load(&file_paths(files))?.settings;
                let key = settings.daemon.identity;
                let pass = PassphraseSource::from_env(
                    files.passphrase_file.clone(),
                    settings.unsafe_config_allowed,
                );
                irsync::identity::refuse_to_generate_with_passphrase_file(&key, &pass)
                    .with_context(|| format!("identity {}", key.display()))?;
            }
            let name =
                irsync::ssh::client::decode_name(client_name_hex).context("--client-name-hex")?;
            irsync::ssh::setup::run(&file_paths(files), id, &name, recheck, no_dht, no_service)
        });
    match result {
        Ok(report) => {
            print!("{}", report.to_lines());
            0
        }
        Err(e) => {
            eprintln!("irsync: {e:#}");
            1
        }
    }
}

fn run_export(e: &ExportArgs, log: LogArgs, files: &FilesArgs) -> i32 {
    let paths = file_paths(files);
    // A noun alone lists.
    let list = ExportCmd::List;
    match e.cmd.as_ref().unwrap_or(&list) {
        ExportCmd::Add(a) => finish(
            "export add",
            (|| {
                let (name, path) =
                    manage::exports::parse_add_target(&a.target, &std::env::current_dir()?)?;
                let settings = Config::load(&paths)?.settings;
                let pass = PassphraseSource::from_env(
                    files.passphrase_file.clone(),
                    settings.unsafe_config_allowed,
                );
                let daemon_key = &settings.daemon.identity;
                // With --passphrase-file and no daemon key, refuse before the
                // config lock creates anything.
                irsync::identity::refuse_to_generate_with_passphrase_file(daemon_key, &pass)
                    .with_context(|| format!("identity {}", daemon_key.display()))?;
                let mut lock = manage::lock_config(&paths.config)?;
                let mut msg = lock.load()?;
                manage::exports::add_export(&mut msg, &name, &path, a.ro, a.warm_index)?;
                // The daemon identity comes first: when it cannot be had, the
                // export is not added either.
                let secret = identity_of(daemon_key, "irsync-daemon", &pass)?;
                lock.save(&mut msg)?;
                eprintln!(
                    "{}",
                    manage::exports::added_line(&name, &path, &paths.config)
                );
                println!("{}", secret.public());
                Ok(())
            })(),
        ),
        ExportCmd::Remove { name } => finish(
            "export remove",
            (|| {
                let mut lock = manage::lock_config(&paths.config)?;
                let mut msg = lock.load()?;
                manage::exports::remove_export(&mut msg, name)
                    .with_context(|| format!("in {}", paths.config.display()))?;
                lock.save(&mut msg)?;
                eprintln!("{}", manage::exports::removed_line(name, &paths.config));
                Ok(())
            })(),
        ),
        ExportCmd::Update { dry_run, names } => export_update(&paths, names, *dry_run, log),
        ExportCmd::List => finish(
            "export list",
            (|| {
                let Config {
                    clients, exports, ..
                } = Config::load(&paths)?;
                print!("{}", manage::show::export_blocks(&exports, &clients));
                Ok(())
            })(),
        ),
    }
}

fn run_grant(g: &GrantArgs, files: &FilesArgs) -> i32 {
    let paths = file_paths(files);
    // A noun alone lists.
    let list = GrantCmd::List { target: None };
    match g.cmd.as_ref().unwrap_or(&list) {
        GrantCmd::Add { export, who, ro } => finish(
            "grant add",
            (|| {
                manage::grants::check_add_export(export, *ro)?;
                let Config {
                    servers, clients, ..
                } = Config::load(&paths)?;
                let spec = PeerSpec::parse(who, &clients, &servers)?;
                let resolve = |s: &str| clients.get(s).map(|c| c.client_id);
                let mut lock = manage::lock_config(&paths.config)?;
                let mut msg = lock.load()?;
                manage::grants::grant_add(&mut msg, export, &spec, *ro, &resolve)
                    .with_context(|| format!("in {}", paths.config.display()))?;
                lock.save(&mut msg)?;
                eprintln!(
                    "{}",
                    manage::grants::added_line(&spec, &clients, export, *ro)
                );
                Ok(())
            })(),
        ),
        GrantCmd::Remove { export, who } => finish(
            "grant remove",
            (|| {
                let Config {
                    servers, clients, ..
                } = Config::load(&paths)?;
                let resolve = |s: &str| clients.get(s).map(|c| c.client_id);
                manage::grants::check_remove(export, who)?;
                let export = (export != "all").then_some(export.as_str());
                let who = (who != "all").then_some(who.as_str());
                let spec = who
                    .map(|w| PeerSpec::or_literal(w, PeerSpec::parse(w, &clients, &servers)))
                    .transpose()?;
                let mut lock = manage::lock_config(&paths.config)?;
                let mut msg = lock.load()?;
                let removed =
                    manage::grants::grant_remove(&mut msg, export, spec.as_ref(), &resolve);
                let held =
                    manage::grants::rights_still_held(&clients, export, spec.as_ref(), &resolve);
                let n = match removed {
                    Ok(n) => n,
                    Err(e) => {
                        for line in &held {
                            eprintln!("{line}");
                        }
                        return Err(anyhow::Error::from(e)
                            .context(format!("in {}", paths.config.display())));
                    }
                };
                lock.save(&mut msg)?;
                eprintln!("{}", manage::grants::removed_line(n));
                for line in &held {
                    eprintln!("{line}");
                }
                Ok(())
            })(),
        ),
        GrantCmd::List { target } => finish(
            "grant list",
            (|| {
                let Config {
                    clients,
                    exports,
                    settings,
                    ..
                } = Config::load(&paths)?;
                print!(
                    "{}",
                    grant_list_text(
                        &exports,
                        &clients,
                        settings.daemon.allow_all_paths,
                        target.as_deref(),
                        &paths.config.display().to_string(),
                    )?
                );
                Ok(())
            })(),
        ),
    }
}

/// What `grant list [TARGET]` prints: every grant, one export's, or what
/// one peer may reach. `place` names where the clients are kept, for the
/// error of a target that is none of those.
fn grant_list_text(
    exports: &irsync::config::exports::Exports,
    clients: &Clients,
    allow_all_paths: bool,
    target: Option<&str>,
    place: &str,
) -> anyhow::Result<String> {
    let Some(t) = target else {
        return Ok(manage::grants::list_all(exports, clients));
    };
    if let Some(block) = manage::grants::list_export(exports, clients, t) {
        return Ok(block);
    }
    let Ok(id) = clients.resolve_peer(t) else {
        bail!("{t:?} is not an export, a client in {place} or an EndpointId");
    };
    let label = if clients.get(t).is_some() {
        format!("{t} ({id})")
    } else {
        id.to_string()
    };
    Ok(manage::grants::list_peer(
        exports,
        clients,
        allow_all_paths,
        &label,
        &id,
    ))
}

/// A file that does not load is that error, not a refused save.
fn load_first(paths: &Paths) -> anyhow::Result<()> {
    Config::load(paths)?;
    Ok(())
}

fn run_server(s: &ServerArgs, files: &FilesArgs) -> i32 {
    let paths = file_paths(files);
    // A noun alone lists.
    let list = ServerCmd::List;
    match s.cmd.as_ref().unwrap_or(&list) {
        ServerCmd::Add(a) => finish("server add", server_add(a, &paths)),
        ServerCmd::Remove { name } => finish(
            "server remove",
            (|| {
                load_first(&paths)?;
                let mut lock = manage::lock_config(&paths.config)?;
                let mut msg = lock.load()?;
                manage::servers::server_remove(&mut msg, name)
                    .with_context(|| format!("in {}", paths.config.display()))?;
                lock.save(&mut msg)?;
                eprintln!("removed server {name} from {}", paths.config.display());
                Ok(())
            })(),
        ),
        ServerCmd::Rename { old, new } => finish(
            "server rename",
            (|| {
                load_first(&paths)?;
                let mut lock = manage::lock_config(&paths.config)?;
                let mut msg = lock.load()?;
                manage::servers::server_rename(&mut msg, old, new)
                    .with_context(|| format!("in {}", paths.config.display()))?;
                lock.save(&mut msg)?;
                eprintln!(
                    "renamed server {old} to {new} in {}",
                    paths.config.display()
                );
                Ok(())
            })(),
        ),
        ServerCmd::List => finish(
            "server list",
            (|| {
                // Load first: a file that does not load is an error, not an
                // empty list.
                load_first(&paths)?;
                for line in manage::servers::server_list(&manage::load_message(&paths.config)?) {
                    println!("{line}");
                }
                Ok(())
            })(),
        ),
    }
}

fn run_client(c: &ClientArgs, files: &FilesArgs) -> i32 {
    let paths = file_paths(files);
    // A noun alone lists.
    let list = ClientCmd::List;
    match c.cmd.as_ref().unwrap_or(&list) {
        ClientCmd::Add { name, id } => finish(
            "client add",
            (|| {
                let id = parse_endpoint_id(id).with_context(|| format!("ID {id:?}"))?;
                let mut lock = manage::lock_config(&paths.config)?;
                let mut msg = lock.load()?;
                let before = peer_ids(&msg, &paths.config);
                let old = manage::clients::client_add(&mut msg, name, id)?;
                lock.save(&mut msg)?;
                println!("{}", manage::clients::added_line(name, old, id));
                // The id the grants are for may have changed: say so.
                note_moved_grants("client add", &before, &msg, &paths.config);
                Ok(())
            })(),
        ),
        ClientCmd::Remove { name, force } => finish(
            "client remove",
            (|| {
                load_first(&paths)?;
                let mut lock = manage::lock_config(&paths.config)?;
                let mut msg = lock.load()?;
                let grants = manage::clients::remove_client(&mut msg, &paths.config, name, *force)
                    .with_context(|| format!("in {}", paths.config.display()))?;
                lock.save(&mut msg)?;
                eprintln!(
                    "{}",
                    manage::clients::removed_line(name, grants, &paths.config)
                );
                Ok(())
            })(),
        ),
        ClientCmd::Rename { old, new } => finish(
            "client rename",
            (|| {
                load_first(&paths)?;
                let mut lock = manage::lock_config(&paths.config)?;
                let mut msg = lock.load()?;
                let grants = manage::clients::rename_client(&mut msg, &paths.config, old, new)
                    .with_context(|| format!("in {}", paths.config.display()))?;
                lock.save(&mut msg)?;
                eprintln!(
                    "{}",
                    manage::clients::renamed_line(old, new, grants, &paths.config)
                );
                Ok(())
            })(),
        ),
        ClientCmd::Set { name, settings } => finish(
            "client set",
            (|| {
                // Every pair is read before anything changes.
                let settings: Vec<_> = settings
                    .iter()
                    .map(|s| manage::clients::parse_setting(s))
                    .collect::<Result<_, _>>()?;
                load_first(&paths)?;
                let mut lock = manage::lock_config(&paths.config)?;
                let mut msg = lock.load()?;
                manage::clients::client_set(&mut msg, name, &settings)
                    .with_context(|| format!("in {}", paths.config.display()))?;
                lock.save(&mut msg)?;
                if let Some(line) = manage::clients::set_line(&msg, name) {
                    eprintln!("{line}");
                }
                Ok(())
            })(),
        ),
        ClientCmd::List => finish(
            "client list",
            (|| {
                load_first(&paths)?;
                for line in manage::clients::client_list(&manage::load_message(&paths.config)?) {
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

fn run_id(i: &IdArgs, files: &FilesArgs) -> i32 {
    // A noun alone lists.
    match i.cmd.as_ref().unwrap_or(&IdCmd::List) {
        IdCmd::List => id_list(files),
        IdCmd::Show { daemon, all } => id_show(files, *daemon, *all),
        IdCmd::Generate { daemon, out, force } => {
            id_generate(files, *daemon, out.as_deref(), *force)
        }
    }
}

/// `irsync id generate`: writes the key pair, prints the public key line
/// and the EndpointId. Replaces an existing file only with `force`.
fn id_generate(f: &FilesArgs, daemon: bool, out: Option<&std::path::Path>, force: bool) -> i32 {
    finish(
        "id generate",
        (|| {
            // The config is read first, also when --out names the key: it
            // says whether an open passphrase file may be used.
            let settings = Config::load(&file_paths(f))?.settings;
            let allow_open = settings.unsafe_config_allowed;
            let (path, comment) = match out {
                Some(p) => (
                    p.to_path_buf(),
                    if daemon { "irsync-daemon" } else { "irsync" },
                ),
                None => {
                    if daemon {
                        (settings.daemon.identity, "irsync-daemon")
                    } else {
                        (settings.identity, "irsync")
                    }
                }
            };
            // Read the passphrase before touching any file: a bad file must
            // leave nothing written and nothing removed.
            let passphrase = match &f.passphrase_file {
                None => None,
                Some(p) => {
                    // `File` already names the path; say it once.
                    let pass = irsync::identity::read_passphrase_file(p, allow_open).map_err(
                        |e| match e {
                            irsync::identity::IdentityError::File { path, source } => {
                                anyhow::anyhow!(
                                    "reading passphrase file: {}: {source}",
                                    path.display()
                                )
                            }
                            e => e.into(),
                        },
                    )?;
                    if pass.is_empty() {
                        return Err(irsync::identity::IdentityError::EmptyPassphraseFile(
                            p.clone(),
                        )
                        .into());
                    }
                    Some(pass)
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
            let secret =
                irsync::identity::generate_openssh_with(&path, comment, passphrase.as_deref())?;
            let line = std::fs::read_to_string(&pub_path)
                .with_context(|| format!("reading {}", pub_path.display()))?;
            eprintln!("wrote {} and {}", path.display(), pub_path.display());
            println!("{}", line.trim());
            println!("{}", secret.public());
            Ok(())
        })(),
    )
}

/// Which of a machine's two identities `irsync id show` prints.
#[derive(Clone, Copy)]
enum IdRole {
    Client,
    Daemon,
}

impl IdRole {
    /// The line printed above the identity.
    fn label(self) -> &'static str {
        match self {
            IdRole::Client => "client",
            IdRole::Daemon => "daemon",
        }
    }

    /// The comment on the public key line.
    fn comment(self) -> &'static str {
        match self {
            IdRole::Client => "irsync",
            IdRole::Daemon => "irsync-daemon",
        }
    }
}

/// One identity under its role's label, so a server's client ID is not
/// mistaken for its daemon's.
fn print_identity(secret: &iroh::SecretKey, role: IdRole) {
    println!("{}:", role.label());
    println!(
        "{}",
        irsync::identity::public_key_line(secret, role.comment())
    );
    println!("{}", secret.public());
}

/// The client's identity, generated when no key serves.
fn print_client_identity(settings: &Settings, pass: &PassphraseSource) -> anyhow::Result<()> {
    let (secret, source) = client_identity(None, settings, pass)?;
    if let KeySource::Generated(p) = &source {
        eprintln!("generated identity {}", p.display());
    }
    print_identity(&secret, IdRole::Client);
    Ok(())
}

/// Whether this machine has a daemon identity (the file, readable or not).
fn has_daemon_identity(settings: &Settings) -> bool {
    settings.daemon.identity.symlink_metadata().is_ok()
}

/// The daemon's identity. Showing is read-only: it is never created.
fn print_daemon_identity(settings: &Settings, pass: &PassphraseSource) -> anyhow::Result<()> {
    let path = &settings.daemon.identity;
    if !has_daemon_identity(settings) {
        anyhow::bail!(
            "no daemon identity at {}; run the daemon once or `irsync id generate --daemon`",
            path.display()
        );
    }
    let comment = IdRole::Daemon.comment();
    print_identity(&identity_of(path, comment, pass)?, IdRole::Daemon);
    Ok(())
}

fn id_show(f: &FilesArgs, daemon: bool, all: bool) -> i32 {
    finish(
        "id show",
        (|| {
            let settings = Config::load(&file_paths(f))?.settings;
            let pass = PassphraseSource::from_env(
                f.passphrase_file.clone(),
                settings.unsafe_config_allowed,
            );
            if all {
                print_client_identity(&settings, &pass)?;
                print_daemon_identity(&settings, &pass)?;
            } else if daemon {
                print_daemon_identity(&settings, &pass)?;
            } else {
                print_client_identity(&settings, &pass)?;
                if has_daemon_identity(&settings) {
                    eprintln!(
                        "irsync: this machine also has a daemon identity; clients connect \
                         to the daemon, whose id irsync id show --daemon prints"
                    );
                }
            }
            Ok(())
        })(),
    )
}

/// `irsync id list`: every identity this machine has. The client's is
/// generated when missing; the daemon's is printed only when it exists.
fn id_list(f: &FilesArgs) -> i32 {
    finish(
        "id list",
        (|| {
            let settings = Config::load(&file_paths(f))?.settings;
            let pass = PassphraseSource::from_env(
                f.passphrase_file.clone(),
                settings.unsafe_config_allowed,
            );
            print_client_identity(&settings, &pass)?;
            // Only a missing file means "no daemon here". A key that
            // cannot be looked at is an error, not a daemon left out.
            let path = &settings.daemon.identity;
            match path.symlink_metadata() {
                Ok(_) => print_daemon_identity(&settings, &pass)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(e).with_context(|| format!("daemon identity {}", path.display()))
                }
            }
            Ok(())
        })(),
    )
}

fn run_show(f: &FilesArgs) -> i32 {
    finish(
        "show",
        (|| {
            let paths = file_paths(f);
            let config = Config::load(&paths)?;
            let settings = &config.settings;
            // The servers and clients sections are `server list` and
            // `client list`, line for line.
            let msg = manage::load_message(&paths.config)?;
            let server_lines = manage::servers::server_list(&msg);
            let client_lines = manage::clients::client_list(&msg);
            let pass = PassphraseSource::from_env(
                f.passphrase_file.clone(),
                settings.unsafe_config_allowed,
            );
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
                    &server_lines,
                    &client_lines,
                    (&client.public(), &client_line),
                    &daemon.public(),
                )
            );
            Ok(())
        })(),
    )
}

/// `server add`: the server `NAME`, set to dial `ID`.
fn server_add(a: &ServerAddArgs, paths: &Paths) -> anyhow::Result<()> {
    let id = parse_endpoint_id(&a.id).with_context(|| format!("ID {:?}", a.id))?;
    let flags = manage::servers::ServerFlags {
        relay: a
            .relay
            .as_deref()
            .map(|r| r.parse().with_context(|| format!("--relay {r:?}")))
            .transpose()?,
        relay_mode: a.relay_mode,
        address: (!a.address.is_empty()).then(|| a.address.clone()),
        parallel: a.parallel,
        reconnect: a
            .reconnect
            .as_deref()
            .map(|r| {
                irsync::config::duration::parse(r).with_context(|| format!("--reconnect {r:?}"))
            })
            .transpose()?,
        dial_retries: a.dial_retries,
        dht: a.dht,
        mdns: a.mdns,
        pq_key_exchange: a.pq_key_exchange,
        ssh: None,
    };
    let mut lock = manage::lock_config(&paths.config)?;
    let mut msg = lock.load()?;
    let old = manage::servers::server_add(&mut msg, &a.name, id, &flags)?;
    lock.save(&mut msg)?;
    match old {
        Some(old) => println!("{}: {old} -> {id}", a.name),
        None => println!("{}: {id}", a.name),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_pq_key_exchange_that_is_set_decides() {
        use PqKeyExchange::{Prefer, Require};
        assert!(!pq_required_of(None, None, None));
        assert!(pq_required_of(None, None, Some(Require)));
        assert!(pq_required_of(None, Some(Require), Some(Prefer)));
        assert!(!pq_required_of(None, Some(Prefer), Some(Require)));
        assert!(pq_required_of(Some(Require), Some(Prefer), Some(Prefer)));
        assert!(!pq_required_of(Some(Prefer), Some(Require), Some(Require)));
    }

    /// Whether the filter `spec` lets `target` log at TRACE.
    fn filter_enables(spec: &str, target: &str) -> bool {
        use tracing_subscriber::layer::SubscriberExt;
        let sub = tracing_subscriber::registry().with(tracing_subscriber::EnvFilter::new(spec));
        tracing::subscriber::with_default(sub, || match target {
            "irsync::wire" => tracing::enabled!(target: "irsync::wire", tracing::Level::TRACE),
            _ => tracing::enabled!(target: "irsync::session", tracing::Level::TRACE),
        })
    }

    fn filter_enables_wire(spec: &str) -> bool {
        filter_enables(spec, "irsync::wire")
    }

    fn filter_enables_irsync_trace(spec: &str) -> bool {
        filter_enables(spec, "irsync::session")
    }

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

    #[test]
    fn wire_trace_only_at_five_v() {
        // log_default gets the sync's -v count minus one (run_sync).
        for v in 0..=3 {
            assert!(
                !filter_enables_wire(log_default(v, false)),
                "sync -v x{}",
                v + 1
            );
        }
        assert!(filter_enables_wire(log_default(4, false)));
        assert!(filter_enables_wire(log_default(9, false)));
        assert!(filter_enables_irsync_trace(log_default(3, false)));
    }

    #[test]
    fn daemon_counts_v_like_the_client() {
        let d = |v: u8| log_default(v.saturating_sub(1), false);
        assert_eq!(d(0), "warn");
        assert_eq!(d(2), "warn,irsync=info");
        assert!(!filter_enables_wire(d(4)));
        assert!(filter_enables_wire(d(5)));
    }

    fn out(max_delete: bool, errors: usize) -> Outcome {
        Outcome {
            max_delete_reached: max_delete,
            errors: (0..errors)
                .map(|_| (irsync::fs::paths::RelPath::root(), "e".to_string()))
                .collect(),
            ..Outcome::default()
        }
    }

    /// `out`, with a source file that vanished during the sync.
    fn vanished(max_delete: bool, errors: usize) -> Outcome {
        Outcome {
            vanished: vec![irsync::fs::paths::RelPath::new(b"v").unwrap()],
            ..out(max_delete, errors)
        }
    }

    #[test]
    fn exit_codes_rank_1_over_25_over_23_over_24_over_0() {
        assert_eq!(combine_exit_codes(&[out(false, 0)], 0, 1), 0);
        assert_eq!(combine_exit_codes(&[vanished(false, 0)], 0, 1), 24);
        assert_eq!(combine_exit_codes(&[out(false, 2)], 0, 1), 23);
        assert_eq!(combine_exit_codes(&[vanished(false, 2)], 0, 1), 23);
        assert_eq!(combine_exit_codes(&[out(true, 2)], 0, 1), 25);
        assert_eq!(combine_exit_codes(&[vanished(true, 0)], 0, 1), 25);
        // Several sources: one vanished a file, another synced cleanly.
        assert_eq!(
            combine_exit_codes(&[vanished(false, 0), out(false, 0)], 0, 2),
            24
        );
        assert_eq!(combine_exit_codes(&[vanished(false, 0)], 1, 3), 23);
        // Only counted: a daemon left the paths out of `vanished`.
        let omitted = Outcome {
            vanished_omitted: 3,
            ..out(false, 0)
        };
        assert_eq!(omitted.exit_code(), 24);
        assert_eq!(combine_exit_codes(&[omitted], 0, 1), 24);
        // Several sources: one failed, the rest synced.
        assert_eq!(combine_exit_codes(&[out(false, 0)], 1, 3), 23);
        assert_eq!(combine_exit_codes(&[out(true, 0)], 1, 3), 25);
        assert_eq!(combine_exit_codes(&[out(true, 1), out(false, 3)], 0, 2), 25);
        // Nothing got through.
        assert_eq!(combine_exit_codes(&[out(false, 0)], 2, 2), 1);
    }
}
