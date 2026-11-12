//! Wrappers for the `filegen` binary's `run` verb.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;

use crate::{process, signals};

pub struct FilegenOpts<'a> {
    pub steps: u64,
    pub max_total_size: Option<&'a str>,
    pub max_file_size: Option<&'a str>,
    pub sets: &'a [String],
}

fn common_flags(args: &mut Vec<OsString>, o: &FilegenOpts) {
    args.push("-q".into());
    if let Some(s) = o.max_total_size {
        args.extend(["--max-total-size".into(), s.into()]);
    }
    if let Some(s) = o.max_file_size {
        args.extend(["--max-file-size".into(), s.into()]);
    }
    for kv in o.sets {
        args.extend(["--set".into(), kv.into()]);
    }
}

/// `run --seed <seed> --steps <n> --out-dir <out_dir> -q [--max-total-size S]
/// [--max-file-size S] [--set KV]... <tree>`.
pub fn fill_args(seed: u64, out_dir: &Path, tree: &Path, o: &FilegenOpts) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "run".into(),
        "--seed".into(),
        seed.to_string().into(),
        "--steps".into(),
        o.steps.to_string().into(),
        "--out-dir".into(),
        out_dir.into(),
    ];
    common_flags(&mut args, o);
    args.push(tree.into());
    args
}

/// `run --resume <state> --steps <n> -q [same size flags and sets] <tree>`.
pub fn mutate_args(state: &Path, tree: &Path, o: &FilegenOpts) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "run".into(),
        "--resume".into(),
        state.into(),
        "--steps".into(),
        o.steps.to_string().into(),
    ];
    common_flags(&mut args, o);
    args.push(tree.into());
    args
}

/// Runs `<filegen> <args>` with stdin null, stdout discarded and stderr
/// appended to `stderr_log`; a non-zero exit is an error naming the exit
/// status and `stderr_log`.
pub fn run(filegen: &Path, args: &[OsString], stderr_log: &Path) -> Result<()> {
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(stderr_log)
        .with_context(|| format!("opening {}", stderr_log.display()))?;
    let status = process::status(
        Command::new(filegen)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log)),
    )
    .with_context(|| format!("running {}", filegen.display()))?;
    if !status.success() {
        bail!(
            "filegen run failed ({status}); see {}",
            stderr_log.display()
        );
    }
    Ok(())
}

/// How long a dropped `RunningFilegen` gets after SIGTERM before SIGKILL.
const DROP_GRACE: Duration = Duration::from_secs(5);
/// How long `stop` waits for filegen to exit.
const STOP_WAIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(10);

/// A filegen running in the background. Its pid is in the signal
/// handler's background slot while it runs. Dropping it (on every path,
/// `Err` included) sends SIGTERM, waits up to 5 s, then SIGKILLs.
pub struct RunningFilegen {
    child: Option<Child>,
    pid: u32,
    stderr_log: PathBuf,
}

impl RunningFilegen {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Clears the background slot if it still holds this child's pid.
    fn clear_slot(&self) {
        let _ = signals::background_slot().compare_exchange(
            self.pid as i32,
            0,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    /// Waits up to `limit` (`None`: for as long as it takes) for the
    /// child to exit; `None` if it has not.
    fn wait_for(&mut self, limit: Option<Duration>) -> Result<Option<ExitStatus>> {
        let child = self.child.as_mut().expect("a running filegen");
        let deadline = limit.map(|l| Instant::now() + l);
        loop {
            if let Some(status) = child.try_wait().context("waiting for filegen")? {
                self.child = None;
                self.clear_slot();
                return Ok(Some(status));
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Ok(None);
            }
            thread::sleep(POLL);
        }
    }

    fn terminate(&self) {
        if let Some(c) = &self.child {
            let _ = kill(Pid::from_raw(c.id() as i32), Signal::SIGTERM);
        }
    }
}

impl Drop for RunningFilegen {
    fn drop(&mut self) {
        if self.child.is_none() {
            return;
        }
        self.terminate();
        if !matches!(self.wait_for(Some(DROP_GRACE)), Ok(Some(_))) {
            if let Some(mut c) = self.child.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
            self.clear_slot();
        }
    }
}

/// Starts `<filegen> <args>` in the background, with stdin null, stdout
/// discarded and stderr appended to `stderr_log`, and publishes its pid to
/// the signal handler.
pub fn spawn(filegen: &Path, args: &[OsString], stderr_log: &Path) -> Result<RunningFilegen> {
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(stderr_log)
        .with_context(|| format!("opening {}", stderr_log.display()))?;
    // Held from before the spawn until the pid is in the slot, so the
    // handler never misses a filegen that exists.
    let blocked = signals::block();
    let child = process::spawn(
        Command::new(filegen)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log)),
    )
    .with_context(|| format!("running {}", filegen.display()))?;
    let pid = child.id();
    signals::background_slot().store(pid as i32, Ordering::SeqCst);
    let running = RunningFilegen {
        child: Some(child),
        pid,
        stderr_log: stderr_log.to_path_buf(),
    };
    drop(blocked);
    Ok(running)
}

/// Stops a background filegen: SIGTERM (filegen finishes the current step,
/// saves its state and exits 143) and a wait of at most 60 s; or, with
/// `wait_only`, a wait with no limit for the run to end by itself (a
/// replayed round may be long; the signal handler still stops filegen and
/// the harness on Ctrl-C). Exit 0 or 143 is fine, and so is death by our SIGTERM (filegen
/// had not yet set up its handler, so it had taken no step); anything
/// else, or no exit in time, is an error naming the stderr log.
pub fn stop(mut f: RunningFilegen, wait_only: bool) -> Result<()> {
    if !wait_only {
        f.terminate();
    }
    let log = f.stderr_log.clone();
    match f.wait_for((!wait_only).then_some(STOP_WAIT))? {
        Some(status) if matches!(status.code(), Some(0) | Some(143)) => Ok(()),
        Some(status) if !wait_only && status.signal() == Some(libc::SIGTERM) => Ok(()),
        Some(status) => Err(anyhow!(
            "background filegen failed ({status}); see {}",
            log.display()
        )),
        // `f` drops here: SIGTERM, then SIGKILL.
        None => Err(anyhow!(
            "background filegen did not exit within {STOP_WAIT:?}; see {}",
            log.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use super::*;

    fn strs(v: Vec<OsString>) -> Vec<String> {
        v.into_iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    fn fake(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn fill_and_mutate_args() {
        let o = FilegenOpts {
            steps: 100,
            max_total_size: Some("8M"),
            max_file_size: Some("1M"),
            sets: &["names.max_len=200".into()],
        };
        let a = strs(fill_args(
            42,
            Path::new("/r/filegen"),
            Path::new("/r/SRC/data"),
            &o,
        ));
        assert_eq!(
            a,
            [
                "run",
                "--seed",
                "42",
                "--steps",
                "100",
                "--out-dir",
                "/r/filegen",
                "-q",
                "--max-total-size",
                "8M",
                "--max-file-size",
                "1M",
                "--set",
                "names.max_len=200",
                "/r/SRC/data"
            ]
        );
        let o2 = FilegenOpts {
            steps: 5,
            max_total_size: None,
            max_file_size: None,
            sets: &[],
        };
        let m = strs(mutate_args(
            Path::new("/r/filegen/filegen-42.state.yaml"),
            Path::new("/r/SRC/data"),
            &o2,
        ));
        assert_eq!(
            m,
            [
                "run",
                "--resume",
                "/r/filegen/filegen-42.state.yaml",
                "--steps",
                "5",
                "-q",
                "/r/SRC/data"
            ]
        );
    }

    /// Waits up to 10 s for `p` to exist.
    fn wait_for(p: &Path) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !p.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "{} never appeared",
                p.display()
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn stop_waits_for_the_step_to_finish() {
        let t = tempfile::tempdir().unwrap();
        let log = t.path().join("filegen.stderr.log");
        let ready = t.path().join("ready");
        // The trap finishes the "step" (a 1 s sleep) and exits 143, as
        // filegen does on SIGTERM; `ready` says the trap is set.
        let f = fake(
            t.path(),
            "slow.sh",
            "trap 'sleep 1; exit 143' TERM; : > \"$1\"; while :; do sleep 0.05; done",
        );
        let running = spawn(&f, &[ready.clone().into()], &log).unwrap();
        wait_for(&ready);
        let started = std::time::Instant::now();
        stop(running, false).unwrap();
        assert!(started.elapsed() >= std::time::Duration::from_secs(1));
    }

    #[test]
    fn stop_wait_only_lets_the_run_end_and_a_bad_exit_names_the_log() {
        let t = tempfile::tempdir().unwrap();
        let log = t.path().join("filegen.stderr.log");
        let done = fake(t.path(), "done.sh", "sleep 0.2; exit 0");
        stop(spawn(&done, &[], &log).unwrap(), true).unwrap();
        let bad = fake(t.path(), "bad.sh", "echo broke >&2; exit 3");
        let e = stop(spawn(&bad, &[], &log).unwrap(), true)
            .unwrap_err()
            .to_string();
        assert!(e.contains(&log.display().to_string()), "{e}");
        assert!(std::fs::read_to_string(&log).unwrap().contains("broke"));
    }

    #[test]
    fn dropping_a_running_filegen_terminates_it() {
        let t = tempfile::tempdir().unwrap();
        let log = t.path().join("filegen.stderr.log");
        let ready = t.path().join("ready");
        let f = fake(
            t.path(),
            "forever.sh",
            ": > \"$1\"; while :; do sleep 0.05; done",
        );
        let running = spawn(&f, &[ready.clone().into()], &log).unwrap();
        wait_for(&ready);
        let pid = nix::unistd::Pid::from_raw(running.pid() as i32);
        drop(running);
        // Reaped by the drop, so the pid no longer exists.
        assert!(nix::sys::signal::kill(pid, None).is_err());
    }

    #[test]
    fn non_zero_exit_is_an_error_naming_the_code_and_the_stderr_log() {
        let t = tempfile::tempdir().unwrap();
        let log = t.path().join("filegen.stderr.log");
        let ok = fake(t.path(), "ok.sh", "echo 'first run' >&2; exit 0");
        run(&ok, &["run".into()], &log).unwrap();
        let bad = fake(t.path(), "bad.sh", "echo 'no space left' >&2; exit 2");
        let e = run(&bad, &["run".into()], &log).unwrap_err().to_string();
        assert!(e.contains('2'), "{e}");
        assert!(e.contains(&log.display().to_string()), "{e}");
        // appended, not truncated
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "first run\nno space left\n"
        );
    }
}
