// Unsafe code belongs in the library's platform module.
#![deny(unsafe_code)]

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context};
use irsync::build_info::BuildInfo;
use irsync::cli::progress::render::Geometry;
use irsync::cli::progress::screen::{LogWriter, Screen, WriterTerm};
use irsync::cli::progress::Reporter;
use irsync::cli::{
    Args, ClientArgs, ClientCmd, Command, ConfigArgs, ConfigCmd, DaemonArgs, ExportArgs, ExportCmd,
    FilesArgs, GrantArgs, GrantCmd, IdArgs, IdCmd, IndexArgs, LogArgs, PairArgs, RemoteArgs,
    ServerAddArgs, ServerArgs, ServerCmd,
};
use irsync::config::clients::Clients;
use irsync::config::dial::{Dial, DialFlags};
use irsync::config::file::Config;
use irsync::config::paths::Paths;
use irsync::config::settings::Settings;
use irsync::fs::paths::{escape_os_path, escape_path, escape_text};
use irsync::identity::{load_or_generate, parse_endpoint_id, KeySource, PassphraseSource};
use irsync::index::Index;
use irsync::location::Location;
use irsync::manage;
use irsync::options::{Direction, SyncOptions};
use irsync::session::{EventBus, Outcome, Party, HASH_WORKERS};
use irsync::side::Side;
use irsync::ssh::onboard::SetupFlags;
use irsync::telemetry::init::trace_files_from_env;
use irsync::telemetry::{self, OtlpConfig, Telemetry, TelemetryConfig};
use irsync_proto::v1::export_entry;

mod remote;

/// A line on stderr that says what a command did; nothing with `-q`.
/// Errors do not go through here: they print anyway.
macro_rules! say {
    ($log:expr, $($arg:tt)*) => {
        if !$log.quiet {
            eprintln!($($arg)*);
        }
    };
}

/// A confirmation on stdout, such as `server add`'s `NAME: ID`; nothing
/// with `-q`. What a command is run for (a list, an id) is not one.
macro_rules! say_out {
    ($log:expr, $($arg:tt)*) => {
        if !$log.quiet {
            println!($($arg)*);
        }
    };
}

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
        Some(Command::Server(s)) => std::process::exit(run_server(s, log, files)),
        Some(Command::Client(c)) => std::process::exit(run_client(c, log, files)),
        Some(Command::Export(e)) => std::process::exit(run_export(e, log, files)),
        Some(Command::Grant(g)) => std::process::exit(run_grant(g, log, files)),
        Some(Command::Id(i)) => std::process::exit(run_id(i, log, files)),
        Some(Command::Config(c)) => std::process::exit(run_config(c, log, files)),
        Some(Command::Pair(a)) => {
            let opts = SyncOptions::for_pair(&args, &a.conn);
            std::process::exit(run_pair(a, &opts, log, files))
        }
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

/// Exit codes: 0 ok, 1 configuration or connection errors, 23 partial
/// transfer, 24 some source files vanished (`--paranoid`), 25 deletions
/// stopped at `--max-delete` (23, 24 and 25 as in rsync; a bad command
/// line, operands included, has already exited 2).
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
            let source = match args.sync_args().operands().direction {
                Direction::DaemonSends => Party::Peer,
                Direction::ClientSends => Party::Local,
            };
            let r =
                Reporter::with_screen(screen, opts.verbose, opts.stats, opts.itemize, opts.dry_run)
                    .with_quiet(opts.quiet)
                    .with_delete_mode(opts.delete)
                    .with_source(source);
            tokio::spawn(r.run(bus.subscribe(), None))
        };
        let start = Instant::now();
        let operands = args.sync_args().operands();
        let result = irsync::client::sync(&operands, opts, &bus, &|line| log.eprintln(line)).await;
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
        Ok(irsync::client::Totals {
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

fn run_gc(dry_run: bool, log: LogArgs, files: &FilesArgs) -> i32 {
    let result = (|| -> anyhow::Result<usize> {
        let dir = Config::load(&file_paths(files))?.settings.state_dir;
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async {
            let (mut parts, mut sessions, mut entries) = (0usize, 0u32, 0usize);
            // What could not be read or removed: the exit code is 23 then.
            let mut errors = 0usize;
            let Ok(rd) = std::fs::read_dir(dir.join("index")) else {
                if !log.quiet {
                    println!(
                        "0 part file(s), 0 session(s), 0 index entries {}",
                        verb(dry_run)
                    );
                }
                return Ok(0);
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
                        Err(e) => {
                            errors += 1;
                            eprintln!("irsync: {}: {e}", file.display());
                        }
                    }
                    continue;
                }
                match Index::root_of_file(&file) {
                    Ok(Some(r)) => roots.push(r),
                    Ok(None) => {}
                    Err(e) => {
                        errors += 1;
                        eprintln!("irsync: {}: {e}", file.display());
                    }
                }
            }
            for root in &roots {
                // A root inside this one has its own index: leave it to that.
                let skip = irsync::gc::nested_roots(root, roots.iter().map(PathBuf::as_path));
                let root = root.clone();
                let index = match Index::open(&dir, &root) {
                    Ok(i) => i.into_handle(),
                    Err(e) => {
                        errors += 1;
                        eprintln!("irsync: {}: {e}", root.display());
                        continue;
                    }
                };
                match irsync::gc::gc_skipping(
                    index,
                    &root,
                    irsync::client::CLIENT_GC_AGE,
                    dry_run,
                    &skip,
                )
                .await
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
                        errors += r.errors.len();
                        parts += r.parts_removed.len();
                        sessions += r.sessions_removed;
                        entries += r.entries_removed;
                    }
                    Err(e) => {
                        errors += 1;
                        eprintln!("irsync: {}: {e}", root.display());
                    }
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
            Ok(errors)
        })
    })();
    match result {
        Ok(0) => 0,
        // Some of it could not be done, as `index` says for its files.
        Ok(_) => 23,
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
fn run_pair(a: &PairArgs, opts: &SyncOptions, quiet: LogArgs, files: &FilesArgs) -> i32 {
    let log = LogWriter::new();
    // What pair did, line by line; nothing with `-q`.
    let say = |line: &str| {
        if !quiet.quiet {
            log.eprintln(line);
        }
    };
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
                say(&format!("irsync: {typed} is saved as {}", t.host));
            }
            // No ssh call yet: these errors need no "over ssh failed" context.
            match &t.name {
                Some(name) => irsync::ssh::onboard::check(name, &t.dest)?,
                None => irsync::ssh::onboard::check_dest(&t.dest)?,
            }
            let secret =
                irsync::client::client_key(files.identity.as_deref(), &config.settings, &pass)?;
            let rsh = irsync::ssh::client::Rsh::from_opts(a.rsh.as_deref());
            say(&format!("irsync: pairing with {} over ssh", t.host));
            let flags = SetupFlags {
                recheck: t.kind == irsync::ssh::pair::PairKind::Saved,
                no_dht: a.server_no_dht,
                no_install: a.no_ssh_install
                    || !Dial::resolve(&DialFlags::default(), None, &config.settings.defaults)
                        .map_err(anyhow::Error::msg)?
                        .ssh_install,
                no_service: a.no_service,
            };
            let host = &t.host;
            let o = irsync::ssh::onboard::onboard(
                &paths,
                &rsh,
                t.name.as_deref(),
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
                irsync::ssh::pair::PairKind::First => {
                    format!("pairing with {host} over ssh failed")
                }
                irsync::ssh::pair::PairKind::Saved => {
                    format!("checking {host} over ssh failed")
                }
            })?;
            if !quiet.quiet {
                report_onboarded(&log, &o);
            }
            if o.report.grant == irsync::ssh::setup::GrantOutcome::Missing {
                bail!(irsync::session::reconnect::no_access_hint(
                    &format!("{}:", o.name),
                    &secret.public()
                ));
            }
            // Last, the daemon is dialled as a sync will dial it (#312).
            let name = &o.name;
            say(&format!("irsync: checking that {name} answers over iroh"));
            let limit = irsync::client::pair_check_limit();
            let rt = tokio::runtime::Runtime::new()?;
            let checked = rt.block_on(irsync::client::check_paired(name, opts, limit, &|line| {
                log.eprintln(line)
            }));
            match checked {
                Ok(()) => say(&format!("irsync: {name} answers over iroh")),
                Err(e)
                    if e.downcast_ref::<irsync::session::reconnect::NeverReached>()
                        .is_some() =>
                {
                    tracing::info!(error = %format!("{e:#}"), "pair check gave up");
                    bail!(irsync::ssh::onboard::not_found(&o, limit));
                }
                Err(e) => {
                    return Err(e.context(format!(
                        "{name} is set up, but its daemon refused this client"
                    )))
                }
            }
            say(&format!(
                "irsync: {}",
                irsync::ssh::pair::paired_line(&o.name)
            ));
            Ok(())
        })(),
    )
}

/// `irsync HOST:` lists the exports this client may reach; `irsync
/// HOST:PATH` lists a directory: a dry-run pull into a temporary directory
/// that also holds the run's index, so nothing is written or kept.
fn run_list(mut args: Args, opts: &SyncOptions) -> i32 {
    let Some(Command::List(list)) = &args.cmd else {
        unreachable!("run_list takes a list command")
    };
    let operand = list.operand.clone();
    let dir_listing = match Location::parse_listed(&operand) {
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
        irsync::client::use_state_dir(tmp.path().join("state"));
        // With the slash a listed file keeps its own name.
        let dest = format!("{}/", tmp.path().join("list").display());
        // The listing is a dry-run pull into the temporary directory.
        let Some(Command::List(list)) = args.cmd.take() else {
            unreachable!("run_list takes a list command")
        };
        args.cmd = Some(Command::Sync(list.as_sync(vec![operand, dest])));
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
    let say = |line: &str| log.eprintln(line);
    match rt.block_on(irsync::client::list_exports(&operand, opts, &say)) {
        Ok(entries) => {
            // An empty list would print nothing at all, which looks like
            // nothing happened (#353).
            if entries.is_empty() && !opts.quiet {
                let host = match Location::parse(&operand) {
                    Ok(Location::Remote { host, .. }) => host,
                    _ => operand.trim_end_matches(':').to_string(),
                };
                say(&format!(
                    "irsync: connected to {}; it shares no exports with this client",
                    escape_text(&host)
                ));
            }
            for e in entries {
                let access = if e.access == export_entry::Access::Rw as i32 {
                    "rw"
                } else {
                    "ro"
                };
                println!(
                    "{} {} {access}",
                    escape_text(&e.name),
                    escape_os_path(&e.path)
                );
            }
            0
        }
        Err(e) => {
            eprintln!("irsync: {}", escape_text(&format!("{e:#}")));
            1
        }
    }
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
        let c = irsync::client::connect(&r.server, opts, &|line| log.eprintln(line)).await?;
        let work = remote::run_remote(&c, plan.request);
        // Ctrl-C ends the wait for an indexing run, not the run.
        let done = if matches!(plan.shape, remote::Shape::Indexed) {
            tokio::select! {
                d = work => d,
                _ = tokio::signal::ctrl_c() => Err(remote::Interrupted.into()),
            }
        } else {
            work.await
        };
        // Closed on every way out (#256).
        c.endpoint.close().await;
        done
    });
    match done.and_then(|outcome| remote::print(&plan.shape, &r.server, outcome, opts.quiet)) {
        Ok(code) => code,
        Err(e) if e.is::<remote::Interrupted>() => {
            let server = escape_text(&r.server);
            eprintln!("irsync: indexing goes on on {server}; see irsync remote {server} index");
            130
        }
        Err(e) => {
            eprintln!("irsync remote: {}", escape_text(&format!("{e:#}")));
            1
        }
    }
}

/// `irsync index`: hashes every regular file under each target into its
/// index (see `irsync::warm`). Exit code 23 when something could not be
/// read or listed, 1 when a target could not be resolved at all.
fn run_index(a: &IndexArgs, log: LogArgs, files: &FilesArgs) -> i32 {
    let result = (|| -> anyhow::Result<i32> {
        let config = Config::load(&file_paths(files))?;
        let state = config.settings.state_dir;
        let mut targets: Vec<(PathBuf, PathBuf)> = a
            .targets
            .iter()
            .map(|t| (state.clone(), t.clone()))
            .collect();
        for root in export_roots(&config.exports, &a.exports)? {
            targets.push((state.clone(), root));
        }
        index_roots(targets, a.dry_run, log.quiet)
    })();
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("irsync index: {e:#}");
            1
        }
    }
}

/// The paths of the exports `names` gives (`all`: every export), each
/// once, in the order given.
fn export_roots(
    exports: &irsync::config::exports::Exports,
    names: &[String],
) -> anyhow::Result<Vec<PathBuf>> {
    let names: Vec<&str> = if names.iter().any(|n| n == "all") {
        anyhow::ensure!(
            names.len() == 1,
            "--export all names every export; give it alone"
        );
        exports.exports.keys().map(String::as_str).collect()
    } else {
        names.iter().map(String::as_str).collect()
    };
    let mut roots = Vec::new();
    for n in names {
        let e = exports
            .exports
            .get(n)
            .with_context(|| format!("no export {n:?} in {}", exports.path.display()))?;
        if !roots.contains(&e.path) {
            roots.push(e.path.clone());
        }
    }
    Ok(roots)
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

fn run_config(c: &ConfigArgs, log: LogArgs, files: &FilesArgs) -> i32 {
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
        ConfigCmd::Set { key, values } => {
            saying("config set", log, cfg::set_in_file(&paths, key, values))
        }
        ConfigCmd::Unset { key } => saying("config unset", log, cfg::unset_in_file(&paths, key)),
    }
}

/// [`finish`] for a command that says what it did: each line `r` gives
/// goes to stderr (nothing with `-q`).
fn saying(cmd: &str, log: LogArgs, r: anyhow::Result<impl Said>) -> i32 {
    finish(
        cmd,
        r.map(|said| {
            for line in said.lines() {
                say!(log, "{line}");
            }
        }),
    )
}

/// What a management command gives to say: a line, maybe one, or several.
trait Said {
    fn lines(self) -> Vec<String>;
}

impl Said for String {
    fn lines(self) -> Vec<String> {
        vec![self]
    }
}

impl Said for Option<String> {
    fn lines(self) -> Vec<String> {
        self.into_iter().collect()
    }
}

impl Said for Vec<String> {
    fn lines(self) -> Vec<String> {
        self
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
    use manage::exports;
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
                let flags = manage::exports::ExportFlags {
                    read_only: a.read_only,
                    index_at_start: a.index_at_start,
                    fsync: a.fsync,
                };
                let ((old, secret), _) = manage::edit_as_is(&paths.config, |msg| {
                    let old = manage::exports::add_export(msg, &name, &path, flags)?;
                    // The daemon identity comes first: when it cannot be had,
                    // the export is not added either.
                    let secret = identity_of(daemon_key, "irsync-daemon", &pass)?;
                    Ok((old, secret))
                })?;
                say!(
                    log,
                    "{}",
                    manage::exports::added_line(&name, &path, old.as_deref(), &paths.config)
                );
                println!("{}", secret.public());
                Ok(())
            })(),
        ),
        ExportCmd::Remove { name } => saying("export remove", log, exports::remove(&paths, name)),
        ExportCmd::Set { name, settings } => {
            saying("export set", log, exports::set(&paths, name, settings))
        }
        ExportCmd::Rename { old, new } => {
            saying("export rename", log, exports::rename(&paths, old, new))
        }
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

fn run_grant(g: &GrantArgs, log: LogArgs, files: &FilesArgs) -> i32 {
    let paths = file_paths(files);
    // A noun alone lists.
    let list = GrantCmd::List { target: None };
    match g.cmd.as_ref().unwrap_or(&list) {
        GrantCmd::Add { export, who, ro } => saying(
            "grant add",
            log,
            manage::grants::add(&paths, export, who, *ro),
        ),
        GrantCmd::Remove { export, who } => {
            let r = match manage::grants::remove(&paths, export, who) {
                Ok(lines) => Ok(lines),
                Err(f) => {
                    for line in &f.held {
                        eprintln!("{line}");
                    }
                    Err(f.error)
                }
            };
            saying("grant remove", log, r)
        }
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
    let Ok(id) = clients.resolve_client(t) else {
        bail!("{t:?} is not an export, a client in {place} or an EndpointId");
    };
    let label = if clients.get(t).is_some() {
        format!("{t} ({id})")
    } else {
        id.to_string()
    };
    Ok(manage::grants::list_client(
        exports,
        clients,
        allow_all_paths,
        &label,
        &id,
    ))
}

fn run_server(s: &ServerArgs, log: LogArgs, files: &FilesArgs) -> i32 {
    use manage::servers;
    let paths = file_paths(files);
    // A noun alone lists.
    let list = ServerCmd::List;
    match s.cmd.as_ref().unwrap_or(&list) {
        ServerCmd::Add(a) => finish("server add", server_add(a, log, &paths)),
        ServerCmd::Remove { name } => saying("server remove", log, servers::remove(&paths, name)),
        ServerCmd::Rename { old, new } => {
            saying("server rename", log, servers::rename(&paths, old, new))
        }
        ServerCmd::List => finish(
            "server list",
            (|| {
                // Load first: a file that does not load is an error, not an
                // empty list.
                manage::load_first(&paths)?;
                for line in manage::servers::server_list(&manage::load_message(&paths.config)?) {
                    println!("{line}");
                }
                Ok(())
            })(),
        ),
    }
}

fn run_client(c: &ClientArgs, log: LogArgs, files: &FilesArgs) -> i32 {
    use manage::clients;
    let paths = file_paths(files);
    // A noun alone lists.
    let list = ClientCmd::List;
    match c.cmd.as_ref().unwrap_or(&list) {
        ClientCmd::Add { name, id } => finish(
            "client add",
            clients::add(&paths, name, id).map(|(line, notes)| {
                say_out!(log, "{line}");
                // The id the grants are for may have changed: say so.
                for note in notes {
                    say!(log, "{note}");
                }
            }),
        ),
        ClientCmd::Remove { name, force } => {
            saying("client remove", log, clients::remove(&paths, name, *force))
        }
        ClientCmd::Rename { old, new } => {
            saying("client rename", log, clients::rename(&paths, old, new))
        }
        ClientCmd::Set { name, settings } => {
            saying("client set", log, clients::set(&paths, name, settings))
        }
        ClientCmd::List => finish(
            "client list",
            (|| {
                manage::load_first(&paths)?;
                for line in manage::clients::client_list(&manage::load_message(&paths.config)?) {
                    println!("{line}");
                }
                Ok(())
            })(),
        ),
    }
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
        eprintln!("irsync: generated identity {}", path.display());
    }
    Ok(secret)
}

fn run_id(i: &IdArgs, log: LogArgs, files: &FilesArgs) -> i32 {
    // A noun alone lists.
    match i.cmd.as_ref().unwrap_or(&IdCmd::List) {
        IdCmd::List => id_list(files),
        IdCmd::Show { daemon, all } => id_show(files, log, *daemon, *all),
        IdCmd::Generate { daemon, out, force } => {
            id_generate(files, log, *daemon, out.as_deref(), *force)
        }
    }
}

/// `irsync id generate`: writes the key pair, prints the public key line
/// and the EndpointId. Replaces an existing file only with `force`.
fn id_generate(
    f: &FilesArgs,
    log: LogArgs,
    daemon: bool,
    out: Option<&std::path::Path>,
    force: bool,
) -> i32 {
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
                        // `--identity` names the client's key for every command.
                        (f.identity.clone().unwrap_or(settings.identity), "irsync")
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
            say!(log, "wrote {} and {}", path.display(), pub_path.display());
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

/// The client's identity (`--identity` when given), generated when no key
/// serves.
fn print_client_identity(
    explicit: Option<&Path>,
    settings: &Settings,
    pass: &PassphraseSource,
) -> anyhow::Result<()> {
    let (secret, source) = irsync::client::client_identity(explicit, settings, pass)?;
    if let KeySource::Generated(p) = &source {
        eprintln!("irsync: generated identity {}", p.display());
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

fn id_show(f: &FilesArgs, log: LogArgs, daemon: bool, all: bool) -> i32 {
    finish(
        "id show",
        (|| {
            let settings = Config::load(&file_paths(f))?.settings;
            let pass = PassphraseSource::from_env(
                f.passphrase_file.clone(),
                settings.unsafe_config_allowed,
            );
            if all {
                print_client_identity(f.identity.as_deref(), &settings, &pass)?;
                print_daemon_identity(&settings, &pass)?;
            } else if daemon {
                print_daemon_identity(&settings, &pass)?;
            } else {
                print_client_identity(f.identity.as_deref(), &settings, &pass)?;
                if has_daemon_identity(&settings) {
                    say!(
                        log,
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
            print_client_identity(f.identity.as_deref(), &settings, &pass)?;
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
            let (client, source) =
                irsync::client::client_identity(f.identity.as_deref(), settings, &pass)?;
            if let KeySource::Generated(p) = &source {
                eprintln!("irsync: generated identity {}", p.display());
            }
            let client_line = irsync::identity::public_key_line(&client, "irsync");
            // Read-only, like `id show`: the daemon's key is never made here.
            let daemon = if has_daemon_identity(settings) {
                let comment = IdRole::Daemon.comment();
                Some(identity_of(&settings.daemon.identity, comment, &pass)?.public())
            } else {
                None
            };
            print!(
                "{}",
                manage::show::report(
                    &paths,
                    &config,
                    &server_lines,
                    &client_lines,
                    (&client.public(), &client_line),
                    daemon.as_ref(),
                )
            );
            Ok(())
        })(),
    )
}

/// `server add`: the server `NAME`, set to dial `ID`.
fn server_add(a: &ServerAddArgs, log: LogArgs, paths: &Paths) -> anyhow::Result<()> {
    let id = parse_endpoint_id(&a.id).with_context(|| format!("ID {:?}", a.id))?;
    let flags = manage::servers::ServerFlags {
        relay: a.relay.clone(),
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
        dns: a.dns,
        mdns: a.mdns,
        pq_key_exchange: a.pq_key_exchange,
        ssh: None,
    };
    let line = manage::servers::add(paths, &a.name, id, &flags)?;
    say_out!(log, "{line}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
