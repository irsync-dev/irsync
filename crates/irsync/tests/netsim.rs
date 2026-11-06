//! The fault layer end to end: a real daemon and a real client with
//! `IRSYNC_NETSIM` set, syncing through the `test-blackout` profile's dead zone.
#![cfg(feature = "netsim")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::{irsync_bin, spawn_daemon_binary_env, BinaryDaemon};

const DAEMON_NETSIM: &str = "profile=test-blackout,side=daemon,seed=1,stream=0";
const CLIENT_NETSIM: &str = "profile=test-blackout,side=client,seed=1,stream=1";

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
        .nth(1)
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
    let deadline = Instant::now() + Duration::from_secs(60);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let out = child.wait_with_output().unwrap();
            panic!(
                "irsync {args:?} did not finish within 60 s: {}",
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

/// Syncs a 4 MiB file through the blackout; returns the client's output and
/// log, the source and destination directories, and the rig that holds them.
fn sync_through_blackout(direction: &str) -> (Output, String, PathBuf, PathBuf, Rig) {
    let rig = rig(&[("IRSYNC_NETSIM", DAEMON_NETSIM), ("RUST_LOG", "info")]);
    let (src, dst) = match direction {
        "push" => (rig.local.clone(), rig.export.clone()),
        _ => (rig.export.clone(), rig.local.clone()),
    };
    std::fs::write(src.join("big"), noise(4 << 20)).unwrap();
    let remote = format!("{}:m/", rig.daemon.id);
    let local = format!("{}/", rig.local.display());
    let (from, to) = match direction {
        "push" => (local.as_str(), remote.as_str()),
        _ => (remote.as_str(), local.as_str()),
    };
    let address = rig.daemon.listen.to_string();
    let (out, log) = client(
        &rig,
        &[("IRSYNC_NETSIM", CLIENT_NETSIM), ("RUST_LOG", "info")],
        &[
            "-a",
            "--address",
            &address,
            "--timeout",
            "2",
            "--reconnect",
            "1m",
            from,
            to,
        ],
    );
    (out, log, src, dst, rig)
}

/// The event's own fields in the first `session started` line; the span
/// fields before the message carry their own `resumed`.
fn session_started(log: &str) -> Option<&str> {
    let line = log.lines().find(|l| l.contains("session started"))?;
    Some(&line[line.find("session started").unwrap()..])
}

/// The id from the first `session started` line.
fn session_of(log: &str) -> String {
    let event = session_started(log).unwrap_or_else(|| panic!("no session started: {log}"));
    let rest = &event[event.find(" session=").expect("a session field") + " session=".len()..];
    rest.split_whitespace().next().unwrap().to_string()
}

#[test]
fn a_dead_zone_forces_a_reconnect_and_the_session_resumes() {
    for direction in ["push", "pull"] {
        let (out, log, src, dst, _rig) = sync_through_blackout(direction);
        assert!(out.status.success(), "{direction}: {log}");
        assert!(log.contains("netsim active"), "{log}");
        let open = log
            .find("netsim: dead zone open")
            .expect("dead zone opened");
        assert!(
            log[open..].contains("connection lost; reconnecting"),
            "{log}"
        );
        let first = session_of(&log[..open]);
        let resumed = session_started(&log[open..]).expect("redialled");
        assert!(
            resumed.contains("resumed=true") && resumed.contains(&format!("session={first}")),
            "{resumed}"
        );
        assert_eq!(
            std::fs::read(src.join("big")).unwrap(),
            std::fs::read(dst.join("big")).unwrap()
        );
    }
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
