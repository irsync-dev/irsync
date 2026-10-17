use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context};
use iroh::{EndpointAddr, EndpointId};
use irsync::cli::location::Location;
use irsync::cli::{validate, Args, Command, DaemonArgs, Direction, SyncOptions};
use irsync::config::client::{ClientConfig, HostConfig};
use irsync::config::daemon::DaemonConfig;
use irsync::filter::FilterSet;
use irsync::identity::{load_secret_key, parse_endpoint_id};
use irsync::index::Index;
use irsync::net::{build_endpoint, EndpointOpts, RelayChoice};
use irsync::session::reconnect::{run_with_reconnect, ConnState, ReconnectOpts};
use irsync::session::{resolve_roots, LocalSide, Outcome};
use irsync_proto::chunk::CHUNK_PARAMS;
use irsync_proto::v1::{hello, Hello};
use irsync_proto::PROTOCOL_VERSION;

fn main() {
    let args = Args::parse();
    match &args.cmd {
        Some(Command::Daemon(d)) => std::process::exit(run_daemon(d, args.sync.verbose)),
        Some(Command::Keygen { .. }) => eprintln!("keygen: not implemented yet"),
        Some(Command::Gc { .. }) => eprintln!("gc: not implemented yet"),
        None => {
            let opts = SyncOptions::from_args(&args);
            if opts.dry_run {
                println!("{opts:#?}");
                return;
            }
            std::process::exit(run_sync(&args, &opts));
        }
    }
    std::process::exit(1);
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

/// Exit codes as rsync: 0 ok, 1 usage/connection errors, 23 partial transfer.
fn run_sync(args: &Args, opts: &SyncOptions) -> i32 {
    init_logging(opts.verbose, opts.quiet);
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("irsync: {e}");
            return 1;
        }
    };
    match rt.block_on(sync(args, opts)) {
        Ok(Totals {
            outcome,
            sessions,
            failed,
        }) => {
            for (path, msg) in &outcome.errors {
                eprintln!("irsync: {}: {msg}", path.to_os().to_string_lossy());
            }
            if !opts.quiet {
                let mut line = format!(
                    "{} files transferred, {} bytes",
                    outcome.files_transferred, outcome.bytes_transferred
                );
                // Only the receiving side knows what was already up to date.
                if outcome.files_skipped > 0 {
                    line += &format!(", {} up to date", outcome.files_skipped);
                }
                if !outcome.errors.is_empty() {
                    line += &format!(", {} errors", outcome.errors.len());
                }
                println!("{line}");
            }
            match failed {
                0 => outcome.exit_code(),
                // Nothing got through: like a connection or usage error.
                f if f == sessions => 1,
                // Some sources synced, others did not: a partial transfer.
                _ => 23,
            }
        }
        Err(e) => {
            eprintln!("irsync: {e:#}");
            1
        }
    }
}

fn state_dir() -> PathBuf {
    dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("irsync")
}

/// The daemon's EndpointId: from the client config's `Host` block, else the
/// host string itself must be an EndpointId.
fn resolve_target(hc: &HostConfig, host: &str) -> anyhow::Result<EndpointId> {
    match hc.endpoint_id {
        Some(id) => Ok(id),
        None => parse_endpoint_id(host)
            .with_context(|| format!("{host:?} is neither a configured host nor an EndpointId")),
    }
}

/// What all sessions of one invocation did together.
struct Totals {
    outcome: Outcome,
    sessions: usize,
    /// Sessions that ended in an error (their sources did not sync).
    failed: usize,
}

async fn sync(args: &Args, opts: &SyncOptions) -> anyhow::Result<Totals> {
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

    let cfg = ClientConfig::load(opts.config.as_deref())?;
    let hc = cfg.resolve(host);
    let target = resolve_target(&hc, host)?;
    let identity = opts
        .identity
        .clone()
        .or_else(|| hc.identity_file.clone())
        .or_else(|| dirs::home_dir().map(|h| h.join(".ssh/id_ed25519")))
        .context("no identity file")?;
    let secret = load_secret_key(&identity, None)
        .with_context(|| format!("loading identity {}", identity.display()))?;
    let relay = match (&opts.relay, &hc.relay) {
        (Some(r), _) => RelayChoice::Custom(r.parse().context("bad --relay URL")?),
        (None, Some(u)) => RelayChoice::Custom(u.clone()),
        (None, None) => RelayChoice::N0Default,
    };
    let endpoint = build_endpoint(EndpointOpts {
        secret,
        relay,
        dht: !opts.no_dht && hc.dht.unwrap_or(true),
        dns: true,
        mdns: hc.mdns.unwrap_or(true),
        publish: false,
        bind_addr: None,
    })
    .await?;
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
                module,
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
            module: module.clone(),
            path: rpath.as_bytes().to_vec(),
            direction: match direction {
                Direction::ClientSends => hello::Direction::ClientSends,
                Direction::DaemonSends => hello::Direction::DaemonSends,
            } as i32,
            chunk_min: CHUNK_PARAMS.min,
            chunk_avg: CHUNK_PARAMS.avg,
            chunk_max: CHUNK_PARAMS.max,
            contents_only: *remote_co,
        };
        let ropts = ReconnectOpts {
            max: opts
                .reconnect
                .or(hc.reconnect)
                .unwrap_or(ReconnectOpts::default().max),
            ..ReconnectOpts::default()
        };
        sessions += 1;
        let out = match session(
            &endpoint, target, direction, root, *local_co, hello, &filters, opts, ropts,
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
        total.files_transferred += out.files_transferred;
        total.bytes_transferred += out.bytes_transferred;
        total.files_skipped += out.files_skipped;
        total.files_hashed += out.files_hashed;
        total.errors.extend(out.errors);
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
    target: EndpointId,
    direction: Direction,
    root: PathBuf,
    local_contents_only: bool,
    hello: Hello,
    filters: &Arc<FilterSet>,
    opts: &SyncOptions,
    ropts: ReconnectOpts,
) -> anyhow::Result<Outcome> {
    let index = tokio::task::spawn_blocking({
        let root = root.clone();
        move || -> anyhow::Result<_> {
            if direction == Direction::DaemonSends {
                std::fs::create_dir_all(&root)
                    .with_context(|| format!("creating {}", root.display()))?;
            }
            Ok(Index::open(&state_dir(), &root)
                .with_context(|| format!("opening index for {}", root.display()))?
                .into_handle())
        }
    })
    .await??;
    let side = LocalSide::new(root, local_contents_only, index, filters.clone());
    let hello_for = |id: Option<[u8; 16]>| Hello {
        session_id: id.map(|i| i.to_vec()).unwrap_or_default(),
        ..hello.clone()
    };
    let on_state = |s: ConnState| match s {
        ConnState::Connecting => tracing::info!(%target, "connecting"),
        ConnState::Direct => tracing::info!("connected directly"),
        ConnState::Relay => tracing::info!("connected through a relay"),
        ConnState::Reconnecting { attempt } => {
            tracing::info!(attempt, "reconnecting")
        }
    };
    run_with_reconnect(
        endpoint,
        EndpointAddr::new(target),
        hello_for,
        side,
        opts,
        ropts,
        on_state,
    )
    .await
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
        let cfg = DaemonConfig::load(d.config.as_deref())?;
        if d.show_id {
            println!("{}", irsync::daemon::show_id(&cfg)?);
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
            irsync::daemon::run_from_config(cfg, shutdown).await
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
