//! A paced run's sleep ends on a signal. A test binary of its own: raising
//! a signal in the library's test process would interrupt its concurrent
//! runs.

use std::time::{Duration, Instant};

use filegen::cli::RunArgs;
use filegen::pace::Pace;
use filegen::run::{run, Outcome, RunOpts, Start};
use nix::sys::signal::{raise, Signal};

#[test]
fn pace_sleep_ends_on_signal() {
    let t = tempfile::TempDir::new().unwrap();
    let root = t.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let flags = RunArgs {
        path: root.clone(),
        steps: 100,
        seed: Some(1),
        resume: None,
        config: None,
        out_dir: Some(t.path().join("out")),
        prefix: None,
        set: Vec::new(),
        dry_run: false,
        force: false,
        quiet: true,
        verbose: false,
        max_total_size: None,
        max_file_size: None,
        max_depth: None,
        max_path_len: None,
        remove_tree_max_entries: None,
        fsync: None,
        checkpoint_steps: None,
        pace: Pace::Steady { per_sec: 1 },
    };
    let opts = RunOpts {
        root,
        steps: 100,
        start: Start::Seed(1),
        config_file: None,
        sets: vec![
            ("max_file_size".into(), "64K".into()),
            ("max_total_size".into(), "4M".into()),
        ],
        flags,
        dry_run: false,
        force: false,
        quiet: true,
        verbose: false,
        pace: Pace::Steady { per_sec: 1 },
    };
    // `run` installs its handlers on entry; signal well after that.
    let sender = std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(200));
        raise(Signal::SIGTERM).unwrap();
    });
    let started = Instant::now();
    let outcome = run(opts).unwrap();
    let took = started.elapsed();
    sender.join().unwrap();
    assert_eq!(outcome, Outcome::Interrupted(libc::SIGTERM));
    assert!(took < Duration::from_millis(500), "{took:?}");
}
