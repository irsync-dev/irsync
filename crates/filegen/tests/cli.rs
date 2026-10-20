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
