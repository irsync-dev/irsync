use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context};
use iroh::{EndpointAddr, EndpointId, TransportAddr};
use irsync::cli::location::Location;
use irsync::cli::progress::Reporter;
use irsync::cli::{
    validate, AddArgs, Args, Command, DaemonArgs, Direction, FilesArgs, HostAddArgs, HostCmd,
    IndexArgs, SyncOptions,
};
use irsync::config::exports::Exports;
use irsync::config::hosts::Hosts;
use irsync::config::paths::{state_dir, Paths};
use irsync::config::settings::Settings;
use irsync::filter::FilterSet;
use irsync::identity::{load_or_generate, parse_endpoint_id, PassphraseSource};
use irsync::index::Index;
use irsync::manage::{self, PeerSpec};
use irsync::net::{build_endpoint, client_endpoint_opts};
use irsync::session::reconnect::{run_with_reconnect, ConnState, ReconnectOpts};
use irsync::session::{resolve_roots, EventBus, LocalSide, Outcome, HASH_WORKERS};
use irsync_proto::chunk::CHUNK_PARAMS;
use irsync_proto::v1::{hello, Hello};
use irsync_proto::PROTOCOL_VERSION;

fn main() {
    let args = Args::parse();
    match &args.cmd {
        Some(Command::Daemon(d)) => std::process::exit(run_daemon(d, args.sync.verbose)),
        Some(Command::Keygen { out }) => std::process::exit(run_keygen(out.as_deref())),
        Some(Command::Gc { dry_run }) => std::process::exit(run_gc(*dry_run, args.sync.verbose)),
        Some(Command::Index(ix)) => std::process::exit(run_index(ix, args.sync.verbose)),
        Some(Command::Add(a)) => std::process::exit(run_add(a)),
        Some(Command::Remove { name, files }) => std::process::exit(run_remove(name, files)),
        Some(Command::Allow {
            export,
            who,
            ro,
            files,
        }) => std::process::exit(run_allow(export, who, *ro, files)),
        Some(Command::Revoke { args, files }) => std::process::exit(run_revoke(args, files)),
        Some(Command::Host(h)) => std::process::exit(run_host(h)),
        Some(Command::Id { daemon, all, files }) => {
            std::process::exit(run_id(*daemon, *all, files))
        }
        Some(Command::Show { files }) => std::process::exit(run_show(files)),
        None => {
            let opts = SyncOptions::from_args(&args);
            std::process::exit(run_sync(&args, &opts));
        }
    }
}

fn init_logging(verbose: u8, quiet: bool) {
    let default = match (quiet, verbose) {
        (true, _) => "error",
        (false, 0) => "warn",
        (false, 1) => "warn,irsync=info",
        (false, 2) => "info,irsync=debug",
        _ => "debug,irsync=trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

/// Exit codes as rsync: 0 ok, 1 usage/connection errors, 23 partial
/// transfer, 25 deletions stopped at `--max-delete`.
fn run_sync(args: &Args, opts: &SyncOptions) -> i32 {
    init_logging(opts.verbose, opts.quiet);
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("irsync: {e}");
            return 1;
        }
    };
    let (result, reporter, elapsed) = rt.block_on(async {
        let bus = EventBus::default();
        // `-q` silences everything but errors. The live progress line is
        // drawn only on a terminal and only when asked for.
        let progress = opts.progress
            || opts.info.as_deref().is_some_and(|i| {
                i.split(',')
                    .any(|f| matches!(f.trim(), "progress1" | "progress2"))
            });
        let tty = progress && std::io::stdout().is_terminal();
        let task = (!opts.quiet).then(|| {
            let r = Reporter::new(
                Box::new(std::io::stdout()),
                tty,
                opts.verbose,
                opts.stats,
                opts.itemize,
                opts.dry_run,
            );
            tokio::spawn(r.run(bus.subscribe(), None))
        });
        let start = Instant::now();
        let result = sync(args, opts, &bus).await;
        // Every session has dropped its sender clone: the reporter drains and stops.
        drop(bus);
        let reporter = match task {
            Some(t) => t.await.ok(),
            None => None,
        };
        (result, reporter, start.elapsed())
    });
    match result {
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

/// `irsync keygen`: writes the key pair, prints the public key line and the
/// EndpointId (both on stdout, one per line).
fn run_keygen(out: Option<&std::path::Path>) -> i32 {
    let result = (|| -> anyhow::Result<()> {
        let path = match out {
            Some(p) => p.to_path_buf(),
            None => dirs::home_dir()
                .context("no home directory; use --out")?
                .join(".config/irsync/id_ed25519"),
        };
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let secret = irsync::identity::generate_openssh(&path, "irsync")?;
        let mut pub_path = path.as_os_str().to_owned();
        pub_path.push(".pub");
        let line = std::fs::read_to_string(&pub_path)
            .with_context(|| format!("reading {}", std::path::Path::new(&pub_path).display()))?;
        eprintln!(
            "wrote {} and {}",
            path.display(),
            std::path::Path::new(&pub_path).display()
        );
        println!("{}", line.trim());
        println!("{}", secret.public());
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("irsync keygen: {e:#}");
            1
        }
    }
}

/// `irsync gc`: collects stale resume state for every root with an index
/// under the state directory.
fn run_gc(dry_run: bool, verbose: u8) -> i32 {
    init_logging(verbose, false);
    let result = (|| -> anyhow::Result<()> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async {
            let dir = state_dir();
            let (mut parts, mut sessions) = (0usize, 0u32);
            let Ok(rd) = std::fs::read_dir(dir.join("index")) else {
                println!("0 part file(s), 0 session(s) {}", verb(dry_run));
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
                            println!("{}", root.join(p.to_os()).display());
                        }
                        for (p, msg) in &r.errors {
                            eprintln!("irsync: {}: {msg}", root.join(p.to_os()).display());
                        }
                        parts += r.parts_removed.len();
                        sessions += r.sessions_removed;
                    }
                    Err(e) => eprintln!("irsync: {}: {e}", root.display()),
                }
            }
            println!(
                "{parts} part file(s), {sessions} session(s) {}",
                verb(dry_run)
            );
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

/// The daemon's EndpointId: an alias from `hosts.toml`, else the host
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

async fn sync(args: &Args, opts: &SyncOptions, bus: &EventBus) -> anyhow::Result<Totals> {
    let srcs = args
        .sync
        .src()
        .iter()
        .map(|s| Location::parse(s))
        .collect::<Result<Vec<_>, _>>()?;
    let dest = Location::parse(args.sync.dest().context("missing destination")?)?;
    let direction = validate(&srcs, &dest)?;
    let remote = match direction {
        Direction::ClientSends => &dest,
        Direction::DaemonSends => &srcs[0],
    };
    let Location::Remote { host, .. } = remote else {
        bail!("internal: no remote location");
    };

    let paths = Paths::resolve(opts.config.as_deref(), opts.hosts.as_deref(), None);
    let settings = Settings::load(&paths.config)?;
    let hosts = Hosts::load(&paths.hosts)?;
    let host_entry = hosts.get(host);
    let target = resolve_target(&hosts, host)?;
    // Precedence: command line, then the host entry, then the client
    // settings, then the built-in default.
    let mut opts = opts.clone();
    if opts.parallel.is_none() {
        opts.parallel = host_entry
            .and_then(|h| h.parallel)
            .or(settings.client.parallel);
    }
    let opts = &opts;
    // Direct addresses: the command line's, else the host's. Any address
    // means isolated mode: dial those and nothing else. Resolved before the
    // identity is loaded so a bad address never prompts for a passphrase.
    let addresses = if opts.addresses.is_empty() {
        host_entry.map(|h| h.address.clone()).unwrap_or_default()
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
    let identity = opts
        .identity
        .clone()
        .unwrap_or_else(|| settings.identity.clone());
    let pass = PassphraseSource::from_env(opts.passphrase_file.clone());
    let (secret, generated) = load_or_generate(&identity, &pass, "irsync")
        .with_context(|| format!("loading identity {}", identity.display()))?;
    if generated {
        let mut pub_path = identity.as_os_str().to_owned();
        pub_path.push(".pub");
        let line = std::fs::read_to_string(&pub_path).unwrap_or_default();
        eprintln!("generated identity {}", identity.display());
        eprintln!("{}", line.trim());
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
        opts.no_dht,
        &settings.client,
        host_entry,
        &addresses,
    )?;
    let endpoint = build_endpoint(endpoint_opts).await?;
    let target = EndpointAddr::from_parts(target, resolved.into_iter().map(TransportAddr::Ip));
    let filters = Arc::new(FilterSet::from_rules(&opts.filters).context("reading filter rules")?);

    // One session per source; several sources only when pushing.
    let pairs: Vec<(&Location, &Location)> = srcs.iter().map(|s| (s, &dest)).collect();
    let mut total = Outcome::default();
    let (mut sessions, mut failed) = (0, 0);
    for (src, dst) in pairs {
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
                export,
                path: rpath,
                contents_only: remote_co,
                ..
            },
        ) = (local, remote)
        else {
            bail!("internal: unexpected location kinds");
        };
        let (root, rpath) = resolve_roots(direction, path, *local_co, rpath, *remote_co);
        let hello = Hello {
            protocol_version: PROTOCOL_VERSION,
            session_id: vec![],
            export: export.clone(),
            path: rpath.as_bytes().to_vec(),
            direction: match direction {
                Direction::ClientSends => hello::Direction::ClientSends,
                Direction::DaemonSends => hello::Direction::DaemonSends,
            } as i32,
            chunk_min: CHUNK_PARAMS.min,
            chunk_avg: CHUNK_PARAMS.avg,
            chunk_max: CHUNK_PARAMS.max,
            contents_only: *remote_co,
            // The receiver-side options are filled in from `opts` by the session.
            ..Hello::default()
        };
        let ropts = ReconnectOpts {
            max: opts
                .reconnect
                .or(host_entry.and_then(|h| h.reconnect))
                .or(settings.client.reconnect)
                .unwrap_or(ReconnectOpts::default().max),
            ..ReconnectOpts::default()
        };
        sessions += 1;
        let out = match session(
            &endpoint,
            target.clone(),
            direction,
            root,
            *local_co,
            hello,
            &filters,
            opts,
            ropts,
            bus,
        )
        .await
        {
            Ok(out) => out,
            Err(e) => {
                // Keep what earlier sources achieved; report this one and go on.
                eprintln!("irsync: {}: {e:#}", path.display());
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
    let index = tokio::task::spawn_blocking({
        let root = root.clone();
        move || -> anyhow::Result<_> {
            // A dry run creates nothing (the receiver lists a missing root as empty).
            if direction == Direction::DaemonSends && !dry_run {
                std::fs::create_dir_all(&root)
                    .with_context(|| format!("creating {}", root.display()))?;
            }
            Ok(Index::open(&state_dir(), &root)
                .with_context(|| format!("opening index for {}", root.display()))?
                .into_handle())
        }
    })
    .await??;
    if !dry_run {
        // Housekeeping for this root, at most daily unless partial records
        // have gone stale (an hour untouched); never fatal.
        match irsync::daemon::gc_if_due(index.clone(), &root, CLIENT_GC_AGE, CLIENT_GC_STALE).await
        {
            Ok(Some(r)) => tracing::debug!(
                parts_removed = r.parts_removed.len(),
                sessions_removed = r.sessions_removed,
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
    run_with_reconnect(endpoint, target, hello_for, side, opts, ropts, on_state).await
}

/// `irsync index`: hashes every regular file under each target into its
/// index (see `irsync::warm`). Exit code 23 when something could not be
/// read or listed, 1 when a target could not be resolved at all.
fn run_index(a: &IndexArgs, verbose: u8) -> i32 {
    init_logging(verbose, false);
    let result = (|| -> anyhow::Result<i32> {
        // (state directory, root) per target.
        let targets: Vec<(PathBuf, PathBuf)> = if a.daemon {
            let paths = Paths::resolve(a.config.as_deref(), None, a.exports.as_deref());
            let settings = Settings::load(&paths.config)?;
            let exports = Exports::load(&paths.exports)?;
            let names: Vec<String> = if a.targets.is_empty() {
                exports.exports.keys().cloned().collect()
            } else {
                a.targets
                    .iter()
                    .map(|t| t.to_string_lossy().into_owned())
                    .collect()
            };
            names
                .into_iter()
                .map(|n| {
                    let m = exports.exports.get(&n).with_context(|| {
                        format!("no export {n:?} in {}", exports.path.display())
                    })?;
                    Ok((settings.state_dir.clone(), m.path.clone()))
                })
                .collect::<anyhow::Result<_>>()?
        } else {
            a.targets.iter().map(|t| (state_dir(), t.clone())).collect()
        };
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async {
            let mut code = 0;
            for (state, root) in targets {
                let index = Index::open(&state, &root)
                    .with_context(|| format!("open index for {}", root.display()))?
                    .into_handle();
                let r = irsync::warm::index_root(index, &root, HASH_WORKERS, a.dry_run)
                    .await
                    .with_context(|| format!("indexing {}", root.display()))?;
                for (p, msg) in &r.errors {
                    eprintln!("irsync: {}: {msg}", root.join(p.to_os()).display());
                }
                let what = if a.dry_run { "to hash" } else { "hashed" };
                println!(
                    "{}: {} file(s), {} {what} ({} bytes)",
                    root.display(),
                    r.examined,
                    r.hashed,
                    r.bytes
                );
                if !r.errors.is_empty() || r.incomplete {
                    code = 23;
                }
            }
            Ok(code)
        })
    })();
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("irsync index: {e:#}");
            1
        }
    }
}

fn run_daemon(d: &DaemonArgs, verbose: u8) -> i32 {
    let default = match verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    let result = (|| -> anyhow::Result<()> {
        let paths = Paths::resolve(
            d.config.as_deref(),
            d.hosts.as_deref(),
            d.exports.as_deref(),
        );
        let mut settings = Settings::load(&paths.config)?;
        if !d.listen.is_empty() {
            settings.daemon.listen = d.listen.clone();
        }
        let pass = PassphraseSource::from_env(d.passphrase_file.clone());
        if d.show_id {
            println!("{}", irsync::daemon::show_id(&settings, &pass)?);
            return Ok(());
        }
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async {
            let shutdown = tokio_util::sync::CancellationToken::new();
            let token = shutdown.clone();
            tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                token.cancel();
            });
            irsync::daemon::run_from_settings(settings, &paths, &pass, shutdown).await
        })
    })();
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("irsync daemon: {e:#}");
            1
        }
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
    Paths::resolve(f.config.as_deref(), f.hosts.as_deref(), f.exports.as_deref())
}

fn run_add(a: &AddArgs) -> i32 {
    finish(
        "add",
        (|| {
            let (name, path) =
                manage::exports::parse_add_target(&a.target, &std::env::current_dir()?)?;
            let paths = file_paths(&a.files);
            let mut doc = manage::load_doc(&paths.exports)?;
            manage::exports::add_export(&mut doc, &name, &path, a.ro, a.warm_index)?;
            manage::save_doc(&paths.exports, &doc)?;
            eprintln!(
                "added export {name} ({}) to {}",
                path.display(),
                paths.exports.display()
            );
            let settings = Settings::load(&paths.config)?;
            let pass = PassphraseSource::from_env(None);
            let (secret, generated) =
                load_or_generate(&settings.daemon.identity, &pass, "irsync-daemon")
                    .with_context(|| format!("identity {}", settings.daemon.identity.display()))?;
            if generated {
                eprintln!("generated identity {}", settings.daemon.identity.display());
            }
            println!("{}", secret.public());
            Ok(())
        })(),
    )
}

fn run_remove(name: &str, f: &FilesArgs) -> i32 {
    finish(
        "remove",
        (|| {
            let paths = file_paths(f);
            let mut doc = manage::load_doc(&paths.exports)?;
            manage::exports::remove_export(&mut doc, name)
                .with_context(|| format!("in {}", paths.exports.display()))?;
            manage::save_doc(&paths.exports, &doc)?;
            eprintln!("removed export {name} from {}", paths.exports.display());
            Ok(())
        })(),
    )
}

fn run_allow(export: &str, who: &str, ro: bool, f: &FilesArgs) -> i32 {
    finish(
        "allow",
        (|| {
            let paths = file_paths(f);
            let hosts = Hosts::load(&paths.hosts)?;
            let spec = PeerSpec::parse(who, &hosts)?;
            let resolve = |s: &str| hosts.get(s).map(|h| h.endpoint_id);
            let mut doc = manage::load_doc(&paths.exports)?;
            let target = (export != "all").then_some(export);
            manage::exports::allow(&mut doc, target, &spec, ro, &resolve)
                .with_context(|| format!("in {}", paths.exports.display()))?;
            manage::save_doc(&paths.exports, &doc)?;
            let mode = if ro { "ro" } else { "rw" };
            match target {
                Some(e) => eprintln!("{}: {mode} on {e}", spec.entry()),
                None => eprintln!("{}: {mode} on all exports", spec.entry()),
            }
            Ok(())
        })(),
    )
}

fn run_revoke(args: &[String], f: &FilesArgs) -> i32 {
    finish(
        "revoke",
        (|| {
            let paths = file_paths(f);
            let hosts = Hosts::load(&paths.hosts)?;
            let resolve = |s: &str| hosts.get(s).map(|h| h.endpoint_id);
            let (export, who) = match args {
                [w] => (None, Some(w.as_str())),
                [e, w] => (
                    (e != "all").then_some(e.as_str()),
                    (w != "all").then_some(w.as_str()),
                ),
                _ => bail!("expected WHO or EXPORT WHO"),
            };
            // Text that names no known peer still matches entries literally
            // (an alias no longer in hosts.toml).
            let spec = match who.map(|w| (w, PeerSpec::parse(w, &hosts))) {
                None => None,
                Some((_, Ok(s))) => Some(s),
                Some((w, Err(manage::ManageError::Config(irsync::config::ConfigError::BadPeer(_))))) => {
                    Some(PeerSpec::Alias(w.to_string()))
                }
                Some((_, Err(e))) => return Err(e.into()),
            };
            let mut doc = manage::load_doc(&paths.exports)?;
            let n = manage::exports::revoke(&mut doc, export, spec.as_ref(), &resolve)
                .with_context(|| format!("in {}", paths.exports.display()))?;
            manage::save_doc(&paths.exports, &doc)?;
            eprintln!("removed {n} {}", if n == 1 { "entry" } else { "entries" });
            Ok(())
        })(),
    )
}

fn run_host(h: &HostCmd) -> i32 {
    match h {
        HostCmd::Add(a) => finish("host add", host_add(a)),
        HostCmd::Remove { alias, files } => finish(
            "host remove",
            (|| {
                let paths = file_paths(files);
                let mut doc = manage::load_doc(&paths.hosts)?;
                manage::hosts::host_remove(&mut doc, alias)
                    .with_context(|| format!("in {}", paths.hosts.display()))?;
                manage::save_doc(&paths.hosts, &doc)?;
                eprintln!("removed host {alias} from {}", paths.hosts.display());
                Ok(())
            })(),
        ),
        HostCmd::List { files } => finish(
            "host list",
            (|| {
                let doc = manage::load_doc(&file_paths(files).hosts)?;
                for line in manage::hosts::host_list(&doc) {
                    println!("{line}");
                }
                Ok(())
            })(),
        ),
    }
}

/// Loads (generating if missing, with a stderr notice) the identity at `path`.
fn identity_of(path: &std::path::Path, comment: &str) -> anyhow::Result<iroh::SecretKey> {
    let pass = PassphraseSource::from_env(None);
    let (secret, generated) = load_or_generate(path, &pass, comment)
        .with_context(|| format!("identity {}", path.display()))?;
    if generated {
        eprintln!("generated identity {}", path.display());
    }
    Ok(secret)
}

/// The OpenSSH public key line, from the identity's `.pub` file.
fn public_line(path: &std::path::Path) -> anyhow::Result<String> {
    let mut pub_path = path.as_os_str().to_owned();
    pub_path.push(".pub");
    let pub_path = PathBuf::from(pub_path);
    let line = std::fs::read_to_string(&pub_path)
        .with_context(|| format!("reading {}", pub_path.display()))?;
    Ok(line.trim().to_string())
}

fn run_id(daemon: bool, all: bool, f: &FilesArgs) -> i32 {
    finish(
        "id",
        (|| {
            let settings = Settings::load(&file_paths(f).config)?;
            let show = |path: &std::path::Path, comment: &str| -> anyhow::Result<()> {
                let secret = identity_of(path, comment)?;
                println!("{}", public_line(path)?);
                println!("{}", secret.public());
                Ok(())
            };
            if all {
                println!("client:");
                show(&settings.identity, "irsync")?;
                println!("daemon:");
                show(&settings.daemon.identity, "irsync-daemon")?;
            } else if daemon {
                show(&settings.daemon.identity, "irsync-daemon")?;
            } else {
                show(&settings.identity, "irsync")?;
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
            let settings = Settings::load(&paths.config)?;
            let hosts = Hosts::load(&paths.hosts)?;
            let exports = Exports::load(&paths.exports)?;
            let client = identity_of(&settings.identity, "irsync")?;
            let client_line = public_line(&settings.identity)?;
            let daemon = identity_of(&settings.daemon.identity, "irsync-daemon")?;
            print!(
                "{}",
                manage::show::report(
                    &paths,
                    &hosts,
                    &exports,
                    (&client.public(), &client_line),
                    &daemon.public(),
                )
            );
            Ok(())
        })(),
    )
}

fn host_add(a: &HostAddArgs) -> anyhow::Result<()> {
    let id = parse_endpoint_id(&a.id).with_context(|| format!("ID {:?}", a.id))?;
    let flags = manage::hosts::HostFlags {
        relay: a
            .relay
            .as_deref()
            .map(|r| r.parse().with_context(|| format!("--relay {r:?}")))
            .transpose()?,
        address: (!a.address.is_empty()).then(|| a.address.clone()),
        parallel: a.parallel,
        reconnect: a.reconnect.clone(),
        dht: a.dht,
        mdns: a.mdns,
    };
    let paths = file_paths(&a.files);
    let mut doc = manage::load_doc(&paths.hosts)?;
    let old = manage::hosts::host_add(&mut doc, &a.alias, id, &flags)?;
    manage::save_doc(&paths.hosts, &doc)?;
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
