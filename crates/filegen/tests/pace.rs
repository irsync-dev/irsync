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
    let watched = root.clone();
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
    // Signal only once the first step has written to the tree: by then `run` has installed its
    // handlers (a SIGTERM before that would kill this process).
    let sender = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        while std::fs::read_dir(&watched).map_or(true, |mut d| d.next().is_none()) {
            assert!(Instant::now() < deadline, "no file created within 30 s");
            std::thread::sleep(Duration::from_millis(5));
        }
        let sent = Instant::now();
        raise(Signal::SIGTERM).unwrap();
        sent
    });
    let outcome = run(opts).unwrap();
    let ended = Instant::now();
    let sent = sender.join().unwrap();
    assert_eq!(outcome, Outcome::Interrupted(libc::SIGTERM));
    // The remaining steps would take ~100 s at one a second.
    let took = ended.duration_since(sent);
    assert!(took < Duration::from_secs(2), "{took:?}");
}
