//! The daemon holds `daemon.lock` in its state dir while it runs.

mod common;

use common::{irsync_bin, spawn_daemon_binary};
use irsync::daemon_lock::probe;
use std::time::{Duration, Instant};

#[test]
fn acquire_writes_info_and_probe_sees_it() {
    let home = tempfile::tempdir().unwrap();
    let d = spawn_daemon_binary(home.path());
    let info = probe(&home.path().join("xdg/irsync")).expect("lock held");
    assert_eq!(info.pid, d.pid());
    assert_eq!(
        info.exe,
        std::path::PathBuf::from(env!("CARGO_BIN_EXE_irsync"))
    );
    assert_eq!(info.version, *irsync::build::LONG_VERSION);
}

#[test]
fn second_daemon_refuses() {
    let home = tempfile::tempdir().unwrap();
    let _d = spawn_daemon_binary(home.path());
    let out = irsync_bin(home.path(), &["daemon", "--listen", "127.0.0.1:0"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    let want = format!(
        "another irsync daemon (pid {}, {}) is already running for {}",
        _d.pid(),
        env!("CARGO_BIN_EXE_irsync"),
        home.path().join("xdg/irsync").display()
    );
    assert!(err.contains(&want), "stderr: {err}\nwanted: {want}");
}

#[test]
fn probe_none_after_sigterm() {
    let home = tempfile::tempdir().unwrap();
    let mut d = spawn_daemon_binary(home.path());
    let st = std::process::Command::new("kill")
        .args(["-TERM", &d.pid().to_string()])
        .status()
        .unwrap();
    assert!(st.success());
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(s) = d.guard.0.try_wait().unwrap() {
            break s;
        }
        assert!(Instant::now() < deadline, "daemon did not exit on SIGTERM");
        std::thread::sleep(Duration::from_millis(20));
    };
    // A clean exit (not killed by the signal) shows the shutdown path ran.
    assert!(status.success(), "daemon exit status: {status:?}");
    assert!(probe(&home.path().join("xdg/irsync")).is_none());
}
