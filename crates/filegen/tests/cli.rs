mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant, SystemTime};

use common::snapshot;

#[test]
fn cli_help_and_seed_resume_exclusive() {
    let bin = env!("CARGO_BIN_EXE_filegen");
    assert!(Command::new(bin)
        .args(["run", "--help"])
        .output()
        .unwrap()
        .status
        .success());
    // neither --seed nor --resume
    let out = Command::new(bin)
        .args(["run", "--steps", "1", "/tmp"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let out = Command::new(bin)
        .args([
            "run", "--steps", "1", "--seed", "1", "--resume", "x", "/tmp",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn cli_resume_appends_where_the_state_file_is() {
    // The first run finds its out dir through $FILEGENTMP; the resume runs
    // with $FILEGENTMP pointing elsewhere and still appends to that history.
    let t = tempfile::TempDir::new().unwrap();
    let (root, first, second) = (
        t.path().join("root"),
        t.path().join("o1"),
        t.path().join("o2"),
    );
    for d in [&root, &first, &second] {
        std::fs::create_dir(d).unwrap();
    }
    let bin = env!("CARGO_BIN_EXE_filegen");
    let run = |env: &std::path::Path, args: &[&str]| {
        Command::new(bin)
            .env("FILEGENTMP", env)
            .args(["run", "-q", "--set", "max_file_size=16K"])
            .args(args)
            .arg(&root)
            .status()
            .unwrap()
            .code()
    };
    assert_eq!(run(&first, &["--seed", "3", "--steps", "20"]), Some(0));
    let state = first.join("filegen-3.state.yaml");
    let state_arg = state.to_str().unwrap();
    assert_eq!(
        run(&second, &["--resume", state_arg, "--steps", "10"]),
        Some(0)
    );
    let log = std::fs::read_to_string(first.join("filegen-3.log")).unwrap();
    assert_eq!(log.lines().filter(|l| l.starts_with("# run")).count(), 2);
    assert_eq!(log.lines().filter(|l| l.starts_with("step=")).count(), 30);
    assert_eq!(std::fs::read_dir(&second).unwrap().count(), 0);
}

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_filegen"))
}

/// A temp dir holding an empty `root` and an empty output dir `out`.
fn dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let t = tempfile::TempDir::new().unwrap();
    let (root, out) = (t.path().join("root"), t.path().join("out"));
    fs::create_dir(&root).unwrap();
    fs::create_dir(&out).unwrap();
    (t, root, out)
}

fn lines_starting(log: &Path, prefix: &str) -> Vec<String> {
    fs::read_to_string(log)
        .unwrap()
        .lines()
        .filter(|l| l.starts_with(prefix))
        .map(str::to_string)
        .collect()
}

fn steps_done(state: &Path) -> u64 {
    filegen::state::read(state).unwrap().steps_done
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The quoted value of `key="..."` in a log line, escapes kept.
fn quoted_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let start = line.find(&format!(" {key}=\""))? + key.len() + 3;
    let rest = &line[start..];
    let mut escaped = false;
    for (i, c) in rest.char_indices() {
        match c {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => return Some(&rest[..i]),
            _ => escaped = false,
        }
    }
    panic!("unterminated {key} value in {line:?}");
}

#[test]
fn cli_run_seed_then_resume_exit_codes_and_files() {
    let (_t, root, out) = dirs();
    let o = bin()
        .args(["run", "--seed", "5", "--steps", "100", "--out-dir"])
        .arg(&out)
        .args(["--set", "max_file_size=16K"])
        .arg(&root)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let (log, state) = (out.join("filegen-5.log"), out.join("filegen-5.state.yaml"));
    assert!(log.is_file() && state.is_file());
    let err = stderr(&o);
    let out_canon = out.canonicalize().unwrap();
    for p in [&log, &state] {
        let p = out_canon.join(p.file_name().unwrap());
        assert!(err.contains(p.to_str().unwrap()), "{err}");
    }
    assert_eq!(steps_done(&state), 100);

    let o = bin()
        .args(["run", "-q", "--steps", "50", "--resume"])
        .arg(&state)
        .arg(&root)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(steps_done(&state), 150);
    assert_eq!(lines_starting(&log, "# run ").len(), 2);
    assert_eq!(lines_starting(&log, "step=").len(), 150);

    let again = || {
        let mut c = bin();
        c.args(["run", "-q", "--seed", "5", "--steps", "1", "--out-dir"])
            .arg(&out)
            .arg(&root);
        c
    };
    let o = again().output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    let err = stderr(&o);
    for want in ["--resume", "--prefix", "--force"] {
        assert!(err.contains(want), "{err}");
    }
    // The refusal leaves the history alone.
    assert_eq!(lines_starting(&log, "# run ").len(), 2);

    let o = again().arg("--force").output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(lines_starting(&log, "# run ").len(), 1);
    assert_eq!(lines_starting(&log, "step=").len(), 1);
    assert_eq!(steps_done(&state), 1);
}

#[test]
fn cli_prefix_and_env_lookup() {
    let (t, root, _) = dirs();
    let (o2, o3) = (t.path().join("o2"), t.path().join("o3"));
    fs::create_dir(&o2).unwrap();
    fs::create_dir(&o3).unwrap();
    let o = bin()
        .env("FILEGENTMP", &o2)
        .env("TMPDIR", &o3)
        .args([
            "run", "-q", "--seed", "7", "--steps", "5", "--prefix", "mine",
        ])
        .args(["--set", "max_file_size=16K"])
        .arg(&root)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(o2.join("mine.log").is_file());
    assert!(o2.join("mine.state.yaml").is_file());
    assert_eq!(fs::read_dir(&o3).unwrap().count(), 0);

    let o = bin()
        .env_remove("FILEGENTMP")
        .env("TMPDIR", &o3)
        .args(["run", "-q", "--seed", "7", "--steps", "5"])
        .args(["--set", "max_file_size=16K"])
        .arg(&root)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(o3.join("filegen-7.log").is_file());
    assert!(o3.join("filegen-7.state.yaml").is_file());
    assert_eq!(fs::read_dir(&o2).unwrap().count(), 2);
}

#[test]
fn cli_bad_config_exits_2() {
    let (t, root, out) = dirs();
    let cfg = t.path().join("cfg.yaml");
    fs::write(&cfg, "max_total_sise: 4M\n").unwrap();
    let o = bin()
        .args(["run", "--seed", "1", "--steps", "1", "--config"])
        .arg(&cfg)
        .arg("--out-dir")
        .arg(&out)
        .arg(&root)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("max_total_sise"), "{}", stderr(&o));

    let o = bin()
        .args([
            "run",
            "--seed",
            "1",
            "--steps",
            "1",
            "--set",
            "names.bogus=1",
        ])
        .arg("--out-dir")
        .arg(&out)
        .arg(&root)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("bogus"), "{}", stderr(&o));
    // Neither refusal started a history or touched the tree.
    assert_eq!(fs::read_dir(&out).unwrap().count(), 0);
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
}

#[test]
fn cli_never_touches_planted_manifests() {
    let (_t, root, out) = dirs();
    fs::create_dir_all(root.join("d/.fileset.fjall")).unwrap();
    fs::create_dir_all(root.join("d/e")).unwrap();
    let planted = [
        (root.join(".fileset.sqlite"), &b"sqlite manifest"[..]),
        (root.join("d/.fileset.fjall/x"), &b"fjall segment"[..]),
        (
            root.join("d/e/.fileset.sqlite-wal"),
            &b"write-ahead log"[..],
        ),
    ];
    // An old mtime, so any rewrite would show.
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    for (p, bytes) in &planted {
        fs::write(p, bytes).unwrap();
        fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    let o = bin()
        .args(["run", "-q", "--seed", "6", "--steps", "2000", "--out-dir"])
        .arg(&out)
        .args(["--set", "max_file_size=16K", "--set", "max_total_size=4M"])
        .arg(&root)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    for (p, bytes) in &planted {
        assert_eq!(fs::read(p).unwrap(), *bytes, "{}", p.display());
        let md = fs::symlink_metadata(p).unwrap();
        assert_eq!(md.modified().unwrap(), old, "{}", p.display());
    }
    assert!(root.join("d/.fileset.fjall").is_dir());
    let steps = lines_starting(&out.join("filegen-6.log"), "step=");
    assert_eq!(steps.len(), 2000);
    for l in &steps {
        for key in ["path", "to"] {
            if let Some(v) = quoted_value(l, key) {
                assert!(!v.contains(".fileset"), "{l}");
            }
        }
    }
}

#[test]
fn cli_sigterm_resumable() {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;

    let (t, root, out) = dirs();
    let sets = ["--set", "max_file_size=4K"];
    let mut child = bin()
        .args(["run", "-q", "--seed", "8", "--steps", "200000", "--out-dir"])
        .arg(&out)
        .args(sets)
        .arg(&root)
        .spawn()
        .unwrap();
    let log = out.join("filegen-8.log");
    let state = out.join("filegen-8.state.yaml");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let n = fs::read_to_string(&log)
            .map(|s| s.lines().filter(|l| l.starts_with("step=")).count())
            .unwrap_or(0);
        if n >= 50 {
            break;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("no 50 step lines within 30 s (saw {n})");
        }
        if let Some(st) = child.try_wait().unwrap() {
            panic!("run ended before the signal: {st}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).unwrap();
    let st = child.wait().unwrap();
    assert_eq!(st.code(), Some(143));

    let done = steps_done(&state);
    assert!((50..200000).contains(&done), "{done}");
    assert_eq!(lines_starting(&log, "step=").len() as u64, done);
    let ends = lines_starting(&log, "# end ");
    assert_eq!(ends.len(), 1);
    assert!(
        ends[0].starts_with(&format!(
            "# end steps_done={done} outcome=\"interrupted: SIGTERM\" "
        )),
        "{}",
        ends[0]
    );

    let o = bin()
        .args(["run", "-q", "--steps", "100", "--resume"])
        .arg(&state)
        .arg(&root)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let total = done + 100;
    assert_eq!(steps_done(&state), total);

    // Absolute symlink targets carry the root's path, so the fresh run
    // happens at the same path once the resumed tree is snapshotted.
    let resumed = snapshot(&root);
    fs::rename(&root, t.path().join("resumed")).unwrap();
    fs::create_dir(&root).unwrap();
    let o = bin()
        .args(["run", "-q", "--seed", "8", "--steps", &total.to_string()])
        .arg("--out-dir")
        .arg(t.path().join("fresh-out"))
        .args(sets)
        .arg(&root)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(resumed, snapshot(&root));
}

#[test]
fn cli_dry_run_leaves_tree_untouched() {
    let (_t, root, out) = dirs();
    let real = bin()
        .args(["run", "-q", "--seed", "9", "--steps", "200", "--out-dir"])
        .arg(&out)
        .args(["--set", "max_file_size=16K"])
        .arg(&root)
        .output()
        .unwrap();
    assert_eq!(real.status.code(), Some(0), "{}", stderr(&real));
    let before = snapshot(&root);
    assert!(!before.is_empty());

    let o = bin()
        .args(["run", "-q", "--dry-run", "--seed", "10", "--steps", "300"])
        .arg("--out-dir")
        .arg(&out)
        .args(["--set", "max_file_size=16K"])
        .arg(&root)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(snapshot(&root), before);
    assert!(out.join("filegen-10.dry.state.yaml").is_file());
    assert!(!out.join("filegen-10.log").exists());
    let steps = lines_starting(&out.join("filegen-10.dry.log"), "step=");
    assert_eq!(steps.len(), 300);
    for l in &steps {
        assert!(l.split(' ').any(|w| w == "dry=1"), "{l}");
    }
}

#[test]
fn cli_verbose_echoes_steps_quiet_silences() {
    let (_t, root, out) = dirs();
    let o = bin()
        .args(["run", "-v", "--seed", "11", "--steps", "10", "--out-dir"])
        .arg(&out)
        .arg(&root)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let err = stderr(&o);
    assert!(
        err.lines()
            .filter(|l| l.starts_with("step=") && l.contains(" op="))
            .count()
            >= 10,
        "{err}"
    );

    let o = bin()
        .args(["run", "-q", "--seed", "12", "--steps", "10", "--out-dir"])
        .arg(&out)
        .arg(&root)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(o.stderr.is_empty(), "{}", stderr(&o));
    assert!(o.stdout.is_empty());
}
