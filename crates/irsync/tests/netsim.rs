//! The fault layer end to end. A dead zone and a slow uplink run client and
//! daemon in this process on tokio's paused clock, over an in-memory
//! switchboard: the minutes of network time they need pass in about a
//! second. A plain sync with the real binaries checks that without
//! `IRSYNC_NETSIM` nothing changes. The binaries' real UDP fault path is
//! exercised by the netstress tests.
#![cfg(feature = "netsim")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::{
    archive_opts, irsync_bin, spawn_daemon_binary_env, with_netsim, BinaryDaemon, Flow, Harness,
};
use irsync::net::netsim::ProfileName;
use irsync::session::reconnect::ReconnectOpts;

/// A daemon exporting `m` (read-write for the client) and a local directory.
struct Rig {
    _dir: tempfile::TempDir,
    daemon: BinaryDaemon,
    client: PathBuf,
    export: PathBuf,
    local: PathBuf,
}

fn ok(home: &Path, args: &[&str]) -> String {
    let o = irsync_bin(home, args);
    assert!(
        o.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

fn rig(daemon_env: &[(&str, &str)]) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path();
    let (server, client) = (tmp.join("server/home"), tmp.join("client/home"));
    let (export, local) = (tmp.join("srv/m"), tmp.join("local"));
    for d in [&server, &client, &export, &local] {
        std::fs::create_dir_all(d).unwrap();
    }
    let daemon = spawn_daemon_binary_env(&server, daemon_env);
    let client_id = ok(&client, &["id", "show"])
        .lines()
        .last()
        .unwrap()
        .to_string();
    ok(
        &server,
        &["export", "add", &format!("m:{}", export.display())],
    );
    ok(&server, &["grant", "add", "m", &client_id]);
    Rig {
        _dir: dir,
        daemon,
        client,
        export,
        local,
    }
}

/// Runs the client with `env`, bounded to 60 s; returns its output and its
/// stderr without colour codes.
fn client(rig: &Rig, env: &[(&str, &str)], args: &[&str]) -> (Output, String) {
    let limit = Duration::from_secs(60);
    let mut child = Command::new(env!("CARGO_BIN_EXE_irsync"))
        .args(args)
        .env("HOME", &rig.client)
        .env("XDG_STATE_HOME", rig.client.join("xdg"))
        .env_remove("IRSYNC_PASSPHRASE")
        .env_remove("IRSYNC_NETSIM")
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + limit;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let out = child.wait_with_output().unwrap();
            panic!(
                "irsync {args:?} did not finish within {limit:?}: {}",
                plain(&String::from_utf8_lossy(&out.stderr))
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().unwrap();
    let log = plain(&String::from_utf8_lossy(&out.stderr));
    (out, log)
}

/// Drops ANSI colour sequences, which tracing emits even on a pipe.
fn plain(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("\x1b[") {
        out.push_str(&rest[..i]);
        rest = &rest[i + 2..];
        rest = rest.find('m').map_or("", |j| &rest[j + 1..]);
    }
    out.push_str(rest);
    out
}

/// Bytes that do not compress.
fn noise(n: usize) -> Vec<u8> {
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// The same binaries without `IRSYNC_NETSIM`: a plain loopback sync.
fn plain_sync() -> (Output, String) {
    let rig = rig(&[("RUST_LOG", "info")]);
    std::fs::write(rig.local.join("small"), noise(64 << 10)).unwrap();
    let address = rig.daemon.listen.to_string();
    let (out, log) = client(
        &rig,
        &[("RUST_LOG", "info")],
        &[
            "-a",
            "--address",
            &address,
            &format!("{}/", rig.local.display()),
            &format!("{}:m/", rig.daemon.id),
        ],
    );
    let daemon_log = plain(&std::fs::read_to_string(&rig.daemon.log).unwrap());
    assert!(out.status.success(), "{log}");
    assert_eq!(
        std::fs::read(rig.local.join("small")).unwrap(),
        std::fs::read(rig.export.join("small")).unwrap()
    );
    (out, format!("{log}{daemon_log}"))
}

#[test]
fn without_the_variable_nothing_changes() {
    let (out, log) = plain_sync();
    assert!(out.status.success(), "{log}");
    assert!(!log.contains("netsim"), "{log}");
}

/// One sync through `profile`, on the [`Harness`] in `with_netsim`: its
/// result, how often it reconnected, how many sessions the daemon hosted,
/// and the network time it took.
async fn sync_through(
    h: &Harness,
    flow: Flow,
    opts: &irsync::cli::SyncOptions,
) -> (
    anyhow::Result<irsync::session::Outcome>,
    u32,
    usize,
    Duration,
) {
    let ropts = ReconnectOpts {
        max: Duration::from_secs(60),
        ..ReconnectOpts::default()
    };
    let start = tokio::time::Instant::now();
    let (out, reconnects) = h.run_reconnect(opts, flow, ropts, h.local_side()).await;
    (
        out,
        reconnects,
        h.daemon.session_ids().len(),
        start.elapsed(),
    )
}

/// `test-blackout`: the client's link goes dead for a few seconds once
/// 256 KiB have crossed it. With `--timeout 2` the client gives up on the
/// connection, redials, and resumes the same session, in both directions.
#[tokio::test(start_paused = true)]
async fn a_dead_zone_forces_a_reconnect_and_the_session_resumes() {
    for flow in [Flow::Push, Flow::Pull] {
        with_netsim(ProfileName::TestBlackout, 1, async {
            let h = Harness::new().await;
            std::fs::write(h.src(flow).join("big"), noise(4 << 20)).unwrap();
            let mut opts = archive_opts();
            opts.timeout = Some(Duration::from_secs(2));
            let (out, reconnects, sessions, _) = sync_through(&h, flow, &opts).await;
            out.unwrap_or_else(|e| panic!("{flow:?}: {e:#}"));
            assert!(
                reconnects >= 1,
                "{flow:?}: the dead zone forced no reconnect"
            );
            assert_eq!(sessions, 1, "{flow:?}: the redial started a new session");
            assert_eq!(
                std::fs::read(h.src(flow).join("big")).unwrap(),
                std::fs::read(h.dst(flow).join("big")).unwrap(),
                "{flow:?}"
            );
        })
        .await;
    }
}

/// A push over a slow but healthy uplink (`test-slow`, 256 kbit/s) that takes
/// well over a minute: the data streams share the link, so every range is
/// confirmed late. None may give up while the link is still carrying data
/// (the sender once allowed each range 60 s from the moment it was queued,
/// and the push failed).
#[tokio::test(start_paused = true)]
async fn a_slow_uplink_that_needs_more_than_a_minute_still_delivers_everything() {
    with_netsim(ProfileName::TestSlow, 1, async {
        let h = Harness::new().await;
        // 3 MiB: about 100 s at 32 KB/s.
        let names: Vec<String> = (0..12).map(|i| format!("f{i:02}")).collect();
        for (i, name) in names.iter().enumerate() {
            let mut bytes = noise(256 << 10);
            bytes[0] = i as u8;
            std::fs::write(h.local.path().join(name), bytes).unwrap();
        }
        let (out, reconnects, _, took) = sync_through(&h, Flow::Push, &archive_opts()).await;
        out.expect("push over the slow uplink");
        assert_eq!(reconnects, 0, "the healthy link was given up on");
        assert!(
            took > Duration::from_secs(60),
            "the sync took only {took:?}, so it no longer tests a wait over a minute"
        );
        for name in &names {
            assert_eq!(
                std::fs::read(h.local.path().join(name)).unwrap(),
                std::fs::read(h.export.path().join(name)).unwrap(),
                "{name}"
            );
        }
    })
    .await;
}
