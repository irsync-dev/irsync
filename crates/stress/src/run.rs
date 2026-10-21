//! The whole stress run: setup, daemon, sentinel round, fill, iterations.

use std::ffi::OsString;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

use crate::cli::{Cli, Direction};
use crate::client::{self, ClientSetup, SyncExit};
use crate::daemon::{self, Daemon, DaemonConfig, DaemonLog};
use crate::filegen::{self, FilegenOpts};
use crate::fileset;
use crate::layout::Layout;
use crate::report::{self, Iteration};
use crate::run_file::{DaemonInfo, RunFile};
use crate::signals;
use crate::tools::Tools;
use crate::verify;

/// How long the daemon may take to print its startup lines.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the daemon may take to exit after SIGINT before SIGKILL.
const STOP_GRACE: Duration = Duration::from_secs(10);

/// The running daemon and the signal handler's pid slot. Dropping it (on
/// every path, `Err` included) stops the daemon and only then clears the
/// slot, so a signal during the stop's grace period still reaches it.
struct RunningDaemon {
    daemon: Daemon,
    pid_slot: Arc<AtomicI32>,
}

impl Deref for RunningDaemon {
    type Target = Daemon;
    fn deref(&self) -> &Daemon {
        &self.daemon
    }
}

impl DerefMut for RunningDaemon {
    fn deref_mut(&mut self) -> &mut Daemon {
        &mut self.daemon
    }
}

impl Drop for RunningDaemon {
    fn drop(&mut self) {
        self.daemon.stop(STOP_GRACE);
        self.pid_slot.store(0, Ordering::SeqCst);
    }
}

/// What every sync of the run shares.
struct Syncer<'a> {
    cli: &'a Cli,
    layout: &'a Layout,
    tools: &'a Tools,
    setup: ClientSetup,
    args: Vec<OsString>,
}

impl Syncer<'_> {
    fn run(&self, log: &std::path::Path) -> Result<client::SyncResult> {
        client::run_sync(
            &self.tools.irsync.path,
            &self.args,
            &self.setup,
            &self.cli.log_level,
            log,
        )
    }
}

/// The whole sequence of the spec's "Sequence" section. Returns the process
/// exit code (0 or 1); `Err` means exit 2. The daemon is stopped on every
/// path (explicitly, or by its `Drop`); the root is never deleted.
pub fn run(cli: &Cli) -> Result<i32> {
    let start = Instant::now();
    // Tools first: a missing tool must leave no directories behind.
    let tools = Tools::locate(
        cli.irsync.as_deref(),
        cli.filegen.as_deref(),
        cli.fileset.as_deref(),
    )?;
    let layout = Layout::create(&cli.root)?;
    let seed = cli.seed.unwrap_or_else(rand::random);
    let started = jiff::Timestamp::now().to_string();
    let mut run_file = new_run_file(cli, &tools, seed, &started);
    run_file.write(&layout.run_file)?;

    let pid_slot = signals::install();
    let (mut daemon, sync) = start_daemon(cli, &layout, &tools, pid_slot, &mut run_file)?;
    println!(
        "{}",
        report::header(
            &layout.root,
            seed,
            cli.direction,
            &daemon.startup.endpoint_id,
            &daemon.startup.address,
        )
    );

    ping(&daemon, &sync, seed, &started)?;

    let opts = FilegenOpts {
        steps: cli.steps,
        max_total_size: cli.max_total_size.as_deref(),
        max_file_size: cli.max_file_size.as_deref(),
        sets: &cli.filegen_set,
    };
    run_filegen(
        &tools,
        &layout,
        seed,
        &filegen::fill_args(seed, &layout.filegen_dir, &layout.src_data, &opts),
    )?;
    fileset::init(&tools.fileset.path, &layout.src_data)?;

    let mut results = Vec::new();
    for i in 1..=cli.iterations {
        let it = iteration(&mut daemon, &sync, i)?;
        println!("{}", it.line());
        let passed = it.passed();
        results.push(it);
        if !passed {
            println!("{}", layout.check_report(i).display());
            if !cli.keep_going {
                break;
            }
        }
        if i < cli.iterations {
            run_filegen(
                &tools,
                &layout,
                seed,
                &filegen::mutate_args(&layout.filegen_state(seed), &layout.src_data, &opts),
            )?;
            fileset::update(&tools.fileset.path, &layout.src_data)?;
        }
    }

    drop(daemon); // stops it, then clears the signal handler's pid slot
    println!("{}", report::footer(&results, start.elapsed()));
    Ok(report::exit_code(&results))
}

/// One filegen run, its stderr appended to `logs/filegen.stderr.log`; a
/// failure also names filegen's own log.
fn run_filegen(tools: &Tools, layout: &Layout, seed: u64, args: &[OsString]) -> Result<()> {
    filegen::run(&tools.filegen.path, args, &layout.filegen_stderr_log()).with_context(|| {
        format!(
            "filegen failed; its own log is {}",
            layout.filegen_log(seed).display()
        )
    })
}

fn direction_name(d: Direction) -> &'static str {
    match d {
        Direction::Push => "push",
        Direction::Pull => "pull",
    }
}

fn new_run_file(cli: &Cli, tools: &Tools, seed: u64, started: &str) -> RunFile {
    RunFile {
        started: started.to_string(),
        seed,
        direction: direction_name(cli.direction).to_string(),
        iterations: cli.iterations,
        steps: cli.steps,
        max_total_size: cli.max_total_size.clone(),
        max_file_size: cli.max_file_size.clone(),
        filegen_set: cli.filegen_set.clone(),
        log_level: cli.log_level.clone(),
        keep_going: cli.keep_going,
        client_args: cli
            .client_args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect(),
        tools: tools.versions(),
        daemon: None,
    }
}

/// Client key, daemon config, daemon start; publishes the pid to the
/// signal handler and to `run.toml`.
fn start_daemon<'a>(
    cli: &'a Cli,
    layout: &'a Layout,
    tools: &'a Tools,
    pid_slot: Arc<AtomicI32>,
    run_file: &mut RunFile,
) -> Result<(RunningDaemon, Syncer<'a>)> {
    let (client_config, _) = layout.client_side(cli.direction);
    let (daemon_config, daemon_data) = layout.daemon_side(cli.direction);
    let (key, pub_line) = daemon::client_keygen(&tools.irsync.path, client_config)?;
    let toml = daemon::write_config(&DaemonConfig {
        config_dir: daemon_config,
        module_path: daemon_data,
        authorized_pub_line: &pub_line,
    })?;
    let log = DaemonLog::create(&layout.daemon_log())?;
    // From the spawn until the pid is in the slot the handler could not
    // reach the daemon: hold SIGINT and SIGTERM pending meanwhile. If the
    // start fails, `Daemon::start` has already killed the child.
    let blocked = signals::block();
    let daemon = Daemon::start(
        &tools.irsync.path,
        &toml,
        log,
        &cli.log_level,
        STARTUP_TIMEOUT,
    )?;
    let pid = daemon.pid();
    pid_slot.store(pid as i32, Ordering::SeqCst);
    let daemon = RunningDaemon { daemon, pid_slot };
    drop(blocked); // a pending signal is delivered here, with the pid set
    run_file.daemon = Some(DaemonInfo {
        endpoint_id: daemon.startup.endpoint_id.clone(),
        address: daemon.startup.address.to_string(),
        pid,
    });
    run_file.write(&layout.run_file)?;

    let setup = client::setup(client_config, key)?;
    let args = client::sync_args(
        cli.direction,
        layout,
        &daemon.startup.endpoint_id,
        daemon.startup.address.port(),
        &setup,
        &cli.client_args,
    );
    let sync = Syncer {
        cli,
        layout,
        tools,
        setup,
        args,
    };
    Ok((daemon, sync))
}

/// The sentinel round: one sync of a single small file, which must arrive
/// byte for byte. Any failure is a setup failure.
fn ping(daemon: &Daemon, sync: &Syncer, seed: u64, started: &str) -> Result<()> {
    let src = sync.layout.ping_file();
    let content = format!("seed={seed} started={started}\n");
    std::fs::write(&src, &content).with_context(|| format!("writing {}", src.display()))?;
    daemon.marker("ping")?;
    let log = sync.layout.ping_client_log();
    let r = sync.run(&log)?;
    if r.exit != SyncExit::Ok {
        bail!(
            "sentinel sync failed: irsync client exited {}; see {}",
            r.exit.code(),
            log.display()
        );
    }
    let name = src.file_name().expect("the ping file has a name");
    let dst = sync.layout.dst_data.join(name);
    let got = std::fs::read(&dst)
        .with_context(|| format!("sentinel sync did not create {}", dst.display()))?;
    if got != content.as_bytes() {
        bail!("sentinel {} differs from the source", dst.display());
    }
    Ok(())
}

/// One iteration: sync, manifest comparison, fileset check.
fn iteration(daemon: &mut Daemon, sync: &Syncer, i: u64) -> Result<Iteration> {
    let layout = sync.layout;
    daemon.check_alive()?;
    daemon.marker(&format!("iteration {i}"))?;
    let log = layout.client_log(i);
    let r = sync.run(&log)?;
    if let SyncExit::Other(c) = r.exit {
        let daemon_log = daemon.log_path().to_path_buf();
        if let Err(e) = daemon.check_alive() {
            return Err(e.context(format!(
                "daemon died during iteration {i}; see {}",
                daemon_log.display()
            )));
        }
        return Err(anyhow!(
            "irsync client exited {c} in iteration {i}; see {} and {}",
            log.display(),
            daemon_log.display()
        ));
    }
    let hashes = verify::compare_manifests(
        &layout.src_data,
        &layout.dst_data,
        &layout.manifest_hashes(i),
    )?;
    let check = fileset::check(
        &sync.tools.fileset.path,
        &layout.dst_data,
        &layout.check_report(i),
    )?;
    Ok(Iteration {
        number: i,
        total: sync.cli.iterations,
        sync: r,
        manifest_ok: hashes.matches(),
        check,
    })
}
