//! The whole netstress run: irsync-stress's sequence with the fault layer
//! switched on in the daemon and every client, and each client log read
//! for what the network did.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use stress::client::{self, ClientSetup, SyncExit};
use stress::daemon::{self, Daemon, DaemonConfig, DaemonLog};
use stress::filegen::{self, FilegenOpts};
use stress::fileset;
use stress::layout::Layout;
use stress::report as base;
use stress::run::{new_run_file, ping, run_filegen, RunningDaemon};
use stress::run_file::DaemonInfo;
use stress::signals;
use stress::tools::Tools;
use stress::verify;

use crate::cli::{Acks, Cli, Congestion};
use crate::netlog;
use crate::profile::Profile;
use crate::report::{self, NetIteration};
use crate::run_file::{NetRunFile, Network};

/// How long the daemon may take to print its startup lines, and then to
/// log `netsim active`.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
/// The client's `--reconnect` window under every profile.
const RECONNECT: &str = "5m";
/// What a fault build's `irsync --version` ends with.
const NETSIM_SUFFIX: &str = " [netsim]";

/// What every sync of the run shares.
struct Syncer<'a> {
    cli: &'a stress::cli::Cli,
    profile: Profile,
    acks: Option<Acks>,
    seed: u64,
    layout: &'a Layout,
    tools: &'a Tools,
    setup: ClientSetup,
    args: Vec<OsString>,
}

impl Syncer<'_> {
    /// One client run with the fault layer on as stream `stream`.
    fn run(&self, log: &Path, stream: u64) -> Result<client::SyncResult> {
        client::run_sync_with_env(
            &self.tools.irsync.path,
            &self.args,
            &self.setup,
            &self.cli.log_level,
            log,
            &netsim_env(self.profile, self.acks, "client", self.seed, stream),
        )
    }
}

/// The client's `irsync.toml` with `[client] congestion` set.
fn client_settings(c: Congestion) -> String {
    format!("[client]\ncongestion = \"{}\"\n", c.name())
}

/// `IRSYNC_NETSIM=profile=<p>,side=<side>,seed=<seed>,stream=<stream>`,
/// with `,acks=<acks>` when given.
fn netsim_env(
    profile: Profile,
    acks: Option<Acks>,
    side: &str,
    seed: u64,
    stream: u64,
) -> Vec<(OsString, OsString)> {
    let mut v = format!(
        "profile={},side={side},seed={seed},stream={stream}",
        profile.name()
    );
    if let Some(a) = acks {
        v.push_str(&format!(",acks={}", a.name()));
    }
    vec![("IRSYNC_NETSIM".into(), v.into())]
}

/// The header's network line: the profile, then any setting changed from
/// irsync's own.
fn network_line(net: &Cli) -> String {
    let mut line = net.profile.name().to_string();
    if let Some(c) = net.congestion {
        line.push_str(&format!(", congestion {}", c.name()));
    }
    if let Some(a) = net.acks {
        line.push_str(&format!(", acks {}", a.name()));
    }
    line
}

/// irsync-stress's sequence under a network profile. Returns the process
/// exit code (0, 1 or 3); `Err` means exit 2. The daemon is stopped on
/// every path (explicitly, or by its `Drop`); the root is never deleted.
pub fn run(net: &Cli) -> Result<i32> {
    let start = Instant::now();
    let cli = net.to_stress();
    let defaults = net.profile.defaults();
    // Tools first: a missing tool or a normal irsync must leave no
    // directories behind.
    let tools = Tools::locate(
        cli.irsync.as_deref(),
        cli.filegen.as_deref(),
        cli.fileset.as_deref(),
    )?;
    if !tools.irsync.version.ends_with(NETSIM_SUFFIX) {
        bail!(
            "{} is not a netsim build; build with: cargo build --release --workspace --features netsim --target-dir target/netsim",
            tools.irsync.path.display()
        );
    }
    let layout = Layout::create(&cli.root)?;
    let seed = cli.seed.unwrap_or_else(rand::random);
    let started = jiff::Timestamp::now().to_string();
    let mut run_file = NetRunFile {
        base: new_run_file(&cli, &tools, seed, &started),
        network: Network {
            profile: net.profile.name().to_string(),
            client_timeout_secs: defaults.client_timeout_secs,
            reconnect: RECONNECT.to_string(),
        },
    };
    run_file.write(&layout.run_file)?;

    let pid_slot = signals::install();
    let (mut daemon, sync) =
        start_daemon(&cli, net, seed, &layout, &tools, pid_slot, &mut run_file)?;
    println!(
        "{}",
        report::header(
            &layout.root,
            seed,
            cli.direction,
            &daemon.startup.endpoint_id,
            &daemon.startup.address,
            &network_line(net),
        )
    );

    ping(&daemon, &layout, seed, &started, |log| sync.run(log, 0))?;

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
        let it = iteration(&mut daemon, &sync, i, defaults.has_outages)?;
        println!("{}", it.line());
        if let Some(reason) = &it.verdict {
            println!("  {reason}; see {}", layout.client_log(i).display());
        }
        let passed = it.passed();
        let base_passed = it.base.passed();
        results.push(it);
        if !passed {
            if !base_passed {
                println!("{}", layout.check_report(i).display());
            }
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
    let parts = leftover_parts(&[&layout.src_data, &layout.dst_data])?;
    for p in &parts {
        println!("partial file left behind: {}", p.display());
    }
    println!("{}", report::footer(&results, start.elapsed()));
    let code = report::exit_code(&results, &parts, defaults.has_outages);
    if code == 3 {
        println!("faults did not bite: no iteration had an outage");
    }
    Ok(code)
}

/// Client key, daemon config, daemon start with the fault layer on; checks
/// the daemon logged `netsim active`; publishes the pid to the signal
/// handler and to `run.toml`.
fn start_daemon<'a>(
    cli: &'a stress::cli::Cli,
    net: &Cli,
    seed: u64,
    layout: &'a Layout,
    tools: &'a Tools,
    pid_slot: Arc<AtomicI32>,
    run_file: &mut NetRunFile,
) -> Result<(RunningDaemon, Syncer<'a>)> {
    let profile = net.profile;
    let (client_config, _) = layout.client_side(cli.direction);
    let (daemon_config, daemon_data) = layout.daemon_side(cli.direction);
    let (key, _pub_line, client_id) =
        daemon::client_id_generate(&tools.irsync.path, client_config)?;
    let files = daemon::write_config(&DaemonConfig {
        config_dir: daemon_config,
        export_path: daemon_data,
        client_id: &client_id,
        congestion: net.congestion.map(Congestion::name),
    })?;
    let log = DaemonLog::create(&layout.daemon_log())?;
    // From the spawn until the pid is in the slot the handler could not
    // reach the daemon: hold SIGINT and SIGTERM pending meanwhile.
    let blocked = signals::block();
    let daemon = Daemon::start_with_env(
        &tools.irsync.path,
        &files,
        log,
        &cli.log_level,
        STARTUP_TIMEOUT,
        &netsim_env(profile, net.acks, "daemon", seed, 0),
    )?;
    let pid = daemon.pid();
    pid_slot.store(pid as i32, Ordering::SeqCst);
    let daemon = RunningDaemon::new(daemon, pid_slot);
    drop(blocked); // a pending signal is delivered here, with the pid set
    wait_for_netsim_active(&layout.daemon_log(), STARTUP_TIMEOUT)?;
    run_file.base.daemon = Some(DaemonInfo {
        endpoint_id: daemon.startup.endpoint_id.clone(),
        address: daemon.startup.address.to_string(),
        pid,
    });
    run_file.write(&layout.run_file)?;

    let setup = client::setup(client_config, key)?;
    if let Some(c) = net.congestion {
        // The client's config is empty: this is all of it.
        std::fs::write(&setup.config, client_settings(c))
            .with_context(|| format!("writing {}", setup.config.display()))?;
    }
    let mut extra: Vec<OsString> = vec![
        "--timeout".into(),
        profile.defaults().client_timeout_secs.to_string().into(),
    ];
    extra.extend(cli.client_args.iter().cloned());
    let args = client::sync_args_with(
        cli.direction,
        layout,
        &daemon.startup.endpoint_id,
        daemon.startup.address.port(),
        &setup,
        RECONNECT,
        &extra,
    );
    let sync = Syncer {
        cli,
        profile,
        acks: net.acks,
        seed,
        layout,
        tools,
        setup,
        args,
    };
    Ok((daemon, sync))
}

/// Waits up to `timeout` for the daemon log to show `netsim active`; the
/// daemon's stderr reaches the file through a copier thread, so it may lag
/// the startup lines a little.
fn wait_for_netsim_active(log: &Path, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let text = std::fs::read_to_string(log).unwrap_or_default();
        if netlog::read(&text).active {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "the daemon did not log `netsim active` within {timeout:?}; see {}",
                log.display()
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// One iteration: sync as stream `i`, daemon check, client log, manifest
/// comparison, fileset check.
fn iteration(
    daemon: &mut Daemon,
    sync: &Syncer,
    i: u64,
    has_outages: bool,
) -> Result<NetIteration> {
    let layout = sync.layout;
    daemon.check_alive()?;
    daemon.marker(&format!("iteration {i}"))?;
    let log = layout.client_log(i);
    let r = sync.run(&log, i)?;
    let daemon_log = daemon.log_path().to_path_buf();
    if let Err(e) = daemon.check_alive() {
        return Err(e.context(format!(
            "daemon died during iteration {i}; see {}",
            daemon_log.display()
        )));
    }
    let text =
        std::fs::read_to_string(&log).with_context(|| format!("reading {}", log.display()))?;
    let events = netlog::read(&text);
    if !events.active {
        bail!(
            "the irsync client did not log `netsim active` in iteration {i}; see {}",
            log.display()
        );
    }
    if let SyncExit::Other(c) = r.exit {
        return Err(anyhow!(
            "irsync client exited {c} in iteration {i}; see {} and {}",
            log.display(),
            daemon_log.display()
        ));
    }
    let verdict = events.verdict(has_outages);
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
    Ok(NetIteration {
        base: base::Iteration {
            number: i,
            total: sync.cli.iterations,
            sync: r,
            manifest_ok: hashes.matches(),
            check,
        },
        events,
        verdict,
    })
}

/// Every `.irsync.*.part` file under `trees`, sorted. Symlinks are not
/// followed.
fn leftover_parts(trees: &[&Path]) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut stack: Vec<PathBuf> = trees.iter().map(|t| t.to_path_buf()).collect();
    while let Some(dir) = stack.pop() {
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))?
        {
            let entry = entry.with_context(|| format!("reading {}", dir.display()))?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if entry.file_type()?.is_dir() {
                stack.push(entry.path());
            } else if name.starts_with(".irsync.") && name.ends_with(".part") {
                found.push(entry.path());
            }
        }
    }
    found.sort();
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leftover_parts_finds_only_partial_files_in_both_trees() {
        let t = tempfile::tempdir().unwrap();
        let (a, b) = (t.path().join("a"), t.path().join("b"));
        std::fs::create_dir_all(a.join("sub")).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        for f in [
            a.join("sub/.irsync.x.part"),
            b.join(".irsync.y.part"),
            a.join(".irsync.z.tmp"),
            a.join("plain.part"),
            b.join(".fileset.sqlite"),
        ] {
            std::fs::write(f, b"").unwrap();
        }
        let mut want = vec![a.join("sub/.irsync.x.part"), b.join(".irsync.y.part")];
        want.sort();
        assert_eq!(leftover_parts(&[&a, &b]).unwrap(), want);
    }

    #[test]
    fn client_settings_set_the_client_congestion() {
        let t: toml::Table = client_settings(Congestion::Cubic).parse().unwrap();
        assert_eq!(t["client"]["congestion"].as_str(), Some("cubic"));
    }

    #[test]
    fn netsim_env_has_all_four_keys() {
        assert_eq!(
            netsim_env(Profile::TestBlackout, None, "client", 7, 3),
            [(
                OsString::from("IRSYNC_NETSIM"),
                OsString::from("profile=test-blackout,side=client,seed=7,stream=3")
            )]
        );
    }

    #[test]
    fn netsim_env_adds_the_ack_choice_when_given() {
        assert_eq!(
            netsim_env(Profile::LongRtt, Some(Acks::Gentle), "daemon", 1, 0),
            [(
                OsString::from("IRSYNC_NETSIM"),
                OsString::from("profile=long-rtt,side=daemon,seed=1,stream=0,acks=gentle")
            )]
        );
    }

    #[test]
    fn the_network_line_names_what_was_changed() {
        use clap::Parser;
        let parse = |extra: &[&str]| {
            let mut a = vec![
                "irsync-netstress",
                "--profile",
                "long-rtt",
                "--direction",
                "pull",
            ];
            a.extend(extra);
            a.push("/r");
            Cli::try_parse_from(a).unwrap()
        };
        assert_eq!(network_line(&parse(&[])), "long-rtt");
        assert_eq!(
            network_line(&parse(&["--congestion", "cubic", "--acks", "peer-default"])),
            "long-rtt, congestion cubic, acks peer-default"
        );
    }
}
