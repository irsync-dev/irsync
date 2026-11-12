//! End-to-end: the built harness against the real `irsync`, `filegen` and
//! `fileset` binaries.

use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use stress::run_file::RunFile;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_irsync-stress"))
}

/// The three tools must sit next to the harness binary: `cargo test --workspace` builds them; `cargo test -p stress` does not.
fn require_tools() {
    let dir = bin().parent().unwrap().to_path_buf();
    for t in ["irsync", "filegen", "fileset"] {
        assert!(
            dir.join(t).exists(),
            "{} missing: run `cargo build --workspace` first",
            dir.join(t).display()
        );
    }
}

fn run_harness(direction: &str, root: &Path, extra: &[&str]) -> Output {
    let mut cmd = Command::new(bin());
    cmd.args([
        "--direction",
        direction,
        "--iterations",
        "2",
        "--steps",
        "20",
        "--seed",
        "7",
        "--max-total-size",
        "8M",
        "--max-file-size",
        "1M",
    ])
    .args(extra)
    .arg(root)
    .env_remove("IRSYNC_PASSPHRASE");
    output_within(cmd, Duration::from_secs(300))
}

/// A running harness. If a test panics before it has exited, dropping it
/// sends SIGINT (so the harness stops its daemon), waits up to 10 s, then
/// kills it; nothing keeps running against a deleted temp dir.
struct Harness(Child);

impl Drop for Harness {
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

/// Like `Command::output`, but panics (stopping the harness) if it has not
/// exited within `timeout`.
fn output_within(mut cmd: Command, timeout: Duration) -> Output {
    let mut h = Harness(
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
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
            "harness still running after {timeout:?}"
        );
        thread::sleep(Duration::from_millis(50));
    };
    Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    }
}

/// Runs two iterations in `direction`, asserts they pass and leave every
/// artefact, and returns the temp dir holding the run root `run`.
fn assert_two_passing_iterations(direction: &str) -> tempfile::TempDir {
    require_tools();
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("run");
    let o = run_harness(direction, &root, &[]);
    assert!(
        o.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("iter 0001/0002")
            && out.contains("iter 0002/0002")
            && out.contains("check ok"),
        "stdout:\n{out}"
    );
    for f in [
        "run.toml",
        "logs/daemon.log",
        "logs/ping.client.log",
        "logs/filegen.stderr.log",
        "logs/iter-0001.client.log",
        "logs/iter-0002.client.log",
        "logs/iter-0001.check.txt",
        "logs/iter-0002.manifest.blake3",
        "DST/data/.fileset.sqlite",
        "SRC/data/.fileset.sqlite",
        "filegen/filegen-7.state.yaml",
        "filegen/filegen-7.log",
    ] {
        assert!(root.join(f).exists(), "{f} missing");
    }
    let daemon_log = std::fs::read_to_string(root.join("logs/daemon.log")).unwrap();
    assert!(daemon_log.contains("=== ping ===") && daemon_log.contains("=== iteration 2 ==="));
    let run = std::fs::read_to_string(root.join("run.toml")).unwrap();
    assert!(
        run.contains("seed = 7")
            && run.contains(&format!("direction = \"{direction}\""))
            && run.contains("endpoint_id"),
        "run.toml:\n{run}"
    );
    t
}

#[test]
fn push_two_iterations_pass_and_leave_the_artefacts() {
    assert_two_passing_iterations("push");
}

#[test]
fn pull_two_iterations_pass() {
    let t = assert_two_passing_iterations("pull");
    assert!(t.path().join("run/DST/data/.irsync-stress-ping").exists());
}

#[test]
fn non_empty_root_exits_2() {
    require_tools();
    let t = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("x"), b"").unwrap();
    let o = run_harness("push", t.path(), &[]);
    assert_eq!(o.status.code(), Some(2));
}

/// A short push run: `--iterations <iterations> --steps 5`, small sizes,
/// `extra` flags, then `root`.
fn run_short(iterations: &str, root: &Path, extra: &[&OsStr]) -> Output {
    let mut cmd = Command::new(bin());
    cmd.args([
        "--direction",
        "push",
        "--iterations",
        iterations,
        "--steps",
        "5",
        "--seed",
        "7",
        "--max-total-size",
        "8M",
        "--max-file-size",
        "1M",
    ])
    .args(extra)
    .arg(root)
    .env_remove("IRSYNC_PASSPHRASE");
    output_within(cmd, Duration::from_secs(300))
}

fn show(o: &Output) -> String {
    format!(
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        o.status,
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn missing_tool_leaves_the_root_untouched() {
    require_tools();
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("run");
    let o = run_short(
        "1",
        &root,
        &[OsStr::new("--irsync"), OsStr::new("/nonexistent/irsync")],
    );
    assert_eq!(o.status.code(), Some(2), "{}", show(&o));
    assert!(!root.exists(), "the harness created {}", root.display());
    // The same root is still usable.
    let o = run_short("1", &root, &[]);
    assert_eq!(o.status.code(), Some(0), "{}", show(&o));
}

/// A fake `fileset` whose `check` always reports one deleted file. `init`
/// creates an empty manifest (the sync carries it, so both sides hash the
/// same); `update` keeps it.
fn failing_fileset(dir: &Path) -> PathBuf {
    let p = dir.join("fileset");
    std::fs::write(
        &p,
        "#!/bin/sh\n\
         case \"$1\" in\n\
         --version) echo 'fileset fake' ;;\n\
         init) : > \"$3/.fileset.sqlite\" ;;\n\
         update) [ -e \"$3/.fileset.sqlite\" ] || : > \"$3/.fileset.sqlite\" ;;\n\
         check) echo 'added=0 deleted=1 changed=0 unchanged=1 warnings=0' >&2; exit 1 ;;\n\
         *) exit 2 ;;\n\
         esac\n",
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[test]
fn a_failed_check_stops_the_run_without_keep_going() {
    require_tools();
    let t = tempfile::tempdir().unwrap();
    let fileset = failing_fileset(t.path());
    let root = t.path().join("run");
    let o = run_short("3", &root, &[OsStr::new("--fileset"), fileset.as_os_str()]);
    assert_eq!(o.status.code(), Some(1), "{}", show(&o));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("iter 0001/0003") && out.contains("check FAILED"),
        "{}",
        show(&o)
    );
    assert!(!out.contains("iter 0002"), "{}", show(&o));
    assert!(!root.join("logs/iter-0002.client.log").exists());
    // The check report's path follows the iteration line.
    let lines: Vec<&str> = out.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.starts_with("iter 0001/0003"))
        .unwrap();
    let report = root
        .canonicalize()
        .unwrap()
        .join("logs/iter-0001.check.txt");
    assert_eq!(
        lines.get(at + 1).copied(),
        Some(report.to_str().unwrap()),
        "{}",
        show(&o)
    );
}

#[test]
fn a_failed_check_is_recorded_and_the_run_goes_on_with_keep_going() {
    require_tools();
    let t = tempfile::tempdir().unwrap();
    let fileset = failing_fileset(t.path());
    let root = t.path().join("run");
    let o = run_short(
        "3",
        &root,
        &[
            OsStr::new("--keep-going"),
            OsStr::new("--fileset"),
            fileset.as_os_str(),
        ],
    );
    assert_eq!(o.status.code(), Some(1), "{}", show(&o));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("iter 0003/0003") && out.contains("passed=0 failed=3"),
        "{}",
        show(&o)
    );
}

/// Polls `run.toml` every 100 ms until it names the daemon's pid.
fn wait_for_daemon_pid(run_toml: &Path, timeout: Duration) -> Pid {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(Some(d)) = RunFile::read(run_toml).map(|r| r.daemon) {
            return Pid::from_raw(d.pid as i32);
        }
        assert!(
            Instant::now() < deadline,
            "no daemon pid in {} after {timeout:?}",
            run_toml.display()
        );
        thread::sleep(Duration::from_millis(100));
    }
}

/// True once `pid` no longer exists, polling every 50 ms up to `timeout`.
fn wait_until_gone(pid: Pid, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if kill(pid, None).is_err() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    kill(pid, None).is_err()
}

#[test]
fn sigint_terminates_the_daemon() {
    require_tools();
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("run");
    let mut child = Harness(
        Command::new(bin())
            .args([
                "--direction",
                "push",
                "--iterations",
                "1000",
                "--steps",
                "200",
                "--seed",
                "7",
                "--max-total-size",
                "64M",
                "--max-file-size",
                "4M",
            ])
            .arg(&root)
            .env_remove("IRSYNC_PASSPHRASE")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    // wait until run.toml names the daemon pid (written once startup is parsed)
    let pid = wait_for_daemon_pid(&root.join("run.toml"), Duration::from_secs(30));
    kill(Pid::from_raw(child.0.id() as i32), Signal::SIGINT).unwrap();
    let status = child.0.wait().unwrap();
    assert_eq!(status.code(), Some(130));
    // the daemon is gone (poll up to 5 s: the handler signals it, the daemon exits on its own SIGINT path)
    assert!(wait_until_gone(pid, Duration::from_secs(5)));
    assert!(root.exists()); // never deleted
}

/// A `--concurrent` run of three rounds, seed 7, in `direction`, with
/// `extra` flags before the root.
///
/// The pace is a gentle steady:20, a few steps per sync: these tests check
/// the harness, not irsync. At full speed the judge finds real races
/// (see the campaigns in `bug-hunting.md`), which would make them flaky.
fn run_concurrent(direction: &str, root: &Path, extra: &[&OsStr]) -> Output {
    let mut cmd = Command::new(bin());
    cmd.args([
        "--direction",
        direction,
        "--concurrent",
        "--pace",
        "steady:20",
        "--iterations",
        "3",
        "--steps",
        "50",
        "--seed",
        "7",
        "--max-total-size",
        "8M",
        // filegen refuses a file cap (default 256M) above the total cap.
        "--max-file-size",
        "1M",
    ])
    .args(extra)
    .arg(root)
    .env_remove("IRSYNC_PASSPHRASE");
    // Generous: replaying a fast round on a loaded machine takes a while.
    output_within(cmd, Duration::from_secs(900))
}

/// Asserts a three-round concurrent run ended 0 or 4 and left its
/// per-round artefacts.
fn assert_concurrent_run(o: &Output, root: &Path) {
    assert!(matches!(o.status.code(), Some(0) | Some(4)), "{}", show(o));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("  clones:   "), "{}", show(o));
    for i in 1..=3 {
        assert!(out.contains(&format!("iter {i:04}/0003")), "{}", show(o));
        for f in [
            format!("logs/iter-{i:04}.judge.txt"),
            format!("logs/iter-{i:04}.client.log"),
            format!("logs/iter-{i:04}.quiet.client.log"),
        ] {
            assert!(root.join(&f).exists(), "{f} missing\n{}", show(o));
        }
    }
    let rounds = std::fs::read_to_string(root.join("logs/rounds.tsv")).unwrap();
    assert_eq!(rounds.lines().count(), 3, "{rounds}");
    if o.status.code() == Some(4) {
        assert!(
            out.contains("concurrent writes did not bite"),
            "{}",
            show(o)
        );
    }
    let run = std::fs::read_to_string(root.join("run.toml")).unwrap();
    assert!(run.contains("concurrent = true"), "run.toml:\n{run}");
}

#[test]
fn concurrent_push_and_pull_run() {
    require_tools();
    for direction in ["push", "pull"] {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("run");
        let o = run_concurrent(direction, &root, &[]);
        assert_concurrent_run(&o, &root);
    }
}

/// What a step did: op, path, to, bytes.
type Step = (String, Vec<u8>, Option<Vec<u8>>, u64);

/// Every step line in filegen's log under `root`.
fn step_lines(root: &Path) -> Vec<Step> {
    std::fs::read_to_string(root.join("filegen/filegen-7.log"))
        .unwrap()
        .lines()
        .filter_map(filegen::log::parse_step_line)
        .map(|s| (s.op.name().to_string(), s.path, s.to, s.bytes))
        .collect()
}

#[test]
fn replay_rounds_reproduces_steps() {
    require_tools();
    let t = tempfile::tempdir().unwrap();
    let first = t.path().join("first");
    let o = run_concurrent("push", &first, &[]);
    assert_concurrent_run(&o, &first);
    let rounds = first.join("logs/rounds.tsv");
    let again = t.path().join("again");
    let o = run_concurrent(
        "push",
        &again,
        &[OsStr::new("--replay-rounds"), rounds.as_os_str()],
    );
    assert_concurrent_run(&o, &again);
    assert_eq!(
        std::fs::read_to_string(&rounds).unwrap(),
        std::fs::read_to_string(again.join("logs/rounds.tsv")).unwrap()
    );
    assert_eq!(step_lines(&first), step_lines(&again));
}
