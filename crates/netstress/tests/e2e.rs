//! End-to-end: the built rig against fault builds of `irsync`, `filegen`
//! and `fileset`. The dead zones are real time, so these take about a minute.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_irsync-netstress"))
}

/// The workspace root: two levels above this crate.
fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Builds the fault binaries once per test binary and returns their
/// directory, `target/netsim/debug`.
fn netsim_bin_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let ws = workspace();
        let target = ws.join("target/netsim");
        let status = Command::new(env!("CARGO"))
            .args(["build", "--workspace", "--features", "netsim"])
            .arg("--target-dir")
            .arg(&target)
            .current_dir(&ws)
            .stdin(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "fault build failed: {status}");
        target.join("debug")
    })
}

/// A running rig. If a test panics before it has exited, dropping it sends
/// SIGINT (so the rig stops its daemon), waits up to 10 s, then kills it.
struct Rig(Child);

impl Drop for Rig {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(None)) {
            return;
        }
        let _ = kill(Pid::from_raw(self.0.id() as i32), Signal::SIGINT);
        let deadline = Instant::now() + Duration::from_secs(10);
        while matches!(self.0.try_wait(), Ok(None)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Like `Command::output`, but panics (stopping the rig) if it has not
/// exited within `timeout`.
fn output_within(mut cmd: Command, timeout: Duration) -> Output {
    let mut h = Rig(cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap());
    let read_all = |mut r: Box<dyn Read + Send>| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = r.read_to_end(&mut buf);
            buf
        })
    };
    let stdout = read_all(Box::new(h.0.stdout.take().unwrap()));
    let stderr = read_all(Box::new(h.0.stderr.take().unwrap()));
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = h.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "rig still running after {timeout:?}"
        );
        thread::sleep(Duration::from_millis(50));
    };
    Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    }
}

fn show(o: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// Runs the rig with the given tools, `--seed 7`, `args`, then `root`.
fn run_with_tools(
    irsync: &Path,
    filegen: &Path,
    fileset: &Path,
    root: &Path,
    args: &[&str],
) -> Output {
    let mut cmd = Command::new(bin());
    cmd.arg("--irsync")
        .arg(irsync)
        .arg("--filegen")
        .arg(filegen)
        .arg("--fileset")
        .arg(fileset)
        .args(["--seed", "7"])
        .args(args)
        .arg(root)
        .env_remove("IRSYNC_PASSPHRASE");
    output_within(cmd, Duration::from_secs(300))
}

/// Runs the rig against the fault build; the root is `<tempdir>/run`, and
/// the temp dir is returned with it.
fn run_rig(args: &[&str]) -> (Output, RunRoot) {
    let dir = netsim_bin_dir();
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("run");
    let o = run_with_tools(
        &dir.join("irsync"),
        &dir.join("filegen"),
        &dir.join("fileset"),
        &root,
        args,
    );
    (o, RunRoot { _t: t, root })
}

/// The run root and the temp dir holding it.
struct RunRoot {
    _t: tempfile::TempDir,
    root: PathBuf,
}

impl RunRoot {
    fn path(&self) -> &Path {
        &self.root
    }
}

fn exec_script(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Runs the rig with `irsync` as the irsync binary and stub filegen and
/// fileset; the root is `<tempdir>/run`, which must not get created.
fn run_rig_with_irsync(irsync: &Path, args: &[&str]) -> Output {
    let t = tempfile::tempdir().unwrap();
    let (filegen, fileset) = (t.path().join("filegen"), t.path().join("fileset"));
    exec_script(&filegen, "#!/bin/sh\necho 'filegen 0.1.0'\n");
    exec_script(&fileset, "#!/bin/sh\necho 'fileset 0.1.0'\n");
    let root = t.path().join("run");
    let o = run_with_tools(irsync, &filegen, &fileset, &root, args);
    assert!(!root.exists(), "the root was created");
    o
}

/// A stub that answers `--version` like an irsync built without netsim.
fn normal_irsync(dir: &Path) -> PathBuf {
    let p = dir.join("irsync");
    exec_script(&p, "#!/bin/sh\necho 'irsync 0.1.0 (test)'\n");
    p
}

#[test]
fn test_blackout_push_survives_its_dead_zones_over_three_iterations() {
    let (o, root) = run_rig(&[
        "--profile",
        "test-blackout",
        "--direction",
        "push",
        "--iterations",
        "3",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", show(&o));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("network:   test-blackout"), "{out}");
    let iters: Vec<&str> = out.lines().filter(|l| l.starts_with("iter ")).collect();
    assert_eq!(iters.len(), 3, "{out}");
    // The fill moves megabytes, so the first sync always crosses the 256 KiB
    // threshold; a later one may move too little and is then noted, not failed.
    assert!(iters[0].contains("outages 1"), "{out}");
    assert!(out.contains("outages="), "{out}");
    let run = std::fs::read_to_string(root.path().join("run.toml")).unwrap();
    assert!(
        run.contains("[network]") && run.contains("profile = \"test-blackout\""),
        "{run}"
    );
}

#[test]
fn test_blackout_pull_passes() {
    let (o, _root) = run_rig(&[
        "--profile",
        "test-blackout",
        "--direction",
        "pull",
        "--iterations",
        "1",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", show(&o));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("outages 1"), "{out}");
}

#[test]
fn too_little_traffic_means_faults_did_not_bite() {
    let (o, _root) = run_rig(&[
        "--profile",
        "test-blackout",
        "--direction",
        "push",
        "--iterations",
        "1",
        "--max-total-size",
        "64K",
        "--max-file-size",
        "16K",
        "--steps",
        "5",
    ]);
    assert_eq!(o.status.code(), Some(3), "{}", show(&o));
}

#[test]
fn dsl_video_pull_passes_without_reconnects() {
    let (o, _root) = run_rig(&[
        "--profile",
        "dsl-video",
        "--direction",
        "pull",
        "--iterations",
        "1",
        "--max-total-size",
        "2M",
        "--max-file-size",
        "512K",
        "--steps",
        "20",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", show(&o));
}

#[test]
fn a_normal_irsync_is_refused() {
    let t = tempfile::tempdir().unwrap();
    let o = run_rig_with_irsync(
        &normal_irsync(t.path()),
        &["--profile", "dsl-video", "--direction", "push"],
    );
    assert_eq!(o.status.code(), Some(2), "{}", show(&o));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("--features netsim"),
        "{}",
        show(&o)
    );
}
