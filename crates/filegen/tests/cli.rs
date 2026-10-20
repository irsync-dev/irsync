use std::process::Command;

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
