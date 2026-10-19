use std::process::Command;
fn bin() -> Command { Command::new(env!("CARGO_BIN_EXE_fileset")) }

#[test]
fn help_lists_both_verbs() {
    let out = bin().arg("--help").output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("update") && s.contains("check"));
}

#[test]
fn check_rejects_relaxed_with_paranoid() {
    let out = bin().args(["check", "--relaxed", "--paranoid", "."]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}
