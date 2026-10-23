use std::path::Path;
use std::process::{Command, Stdio};

fn irsync(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_irsync"))
        .args(args)
        .env("HOME", home)
        .env("XDG_STATE_HOME", home.join("xdg"))
        .env_remove("IRSYNC_PASSPHRASE")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn revoke_removes_an_alias_that_left_hosts_toml() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("srv/p")).unwrap();
    let cfg = home.join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public();
    std::fs::write(cfg.join("hosts.toml"), format!("[laptop]\nendpoint_id = \"{id}\"\n")).unwrap();
    let target = format!("p:{}", home.join("srv/p").display());
    assert!(irsync(&home, &["add", &target]).status.success());
    assert!(irsync(&home, &["allow", "p", "laptop"]).status.success());
    std::fs::write(cfg.join("hosts.toml"), "").unwrap();
    let o = irsync(&home, &["revoke", "laptop"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!std::fs::read_to_string(cfg.join("exports.toml")).unwrap().contains("laptop"));
}

#[test]
fn add_allow_revoke_remove_round_trip_with_default_paths() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("srv/photos")).unwrap();
    let target = format!("photos:{}", home.join("srv/photos").display());
    let o = irsync(&home, &["add", &target]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8(o.stdout).unwrap();
    irsync::identity::parse_endpoint_id(out.trim()).expect("stdout is the daemon id");
    assert!(home.join(".config/irsync/daemon_ed25519").exists());
    let exports = home.join(".config/irsync/exports.toml");
    assert_eq!(std::fs::read_to_string(&exports).unwrap(), format!("[photos]\npath = {:?}\n", home.join("srv/photos")));
    let id = iroh::SecretKey::from_bytes(&[9; 32]).public().to_string();
    assert!(irsync(&home, &["allow", "photos", &id]).status.success());
    assert!(std::fs::read_to_string(&exports).unwrap().contains(&format!("rw = [\"{id}\"]")));
    let o = irsync(&home, &["allow", "photos", "all"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("no open access"));
    let o = irsync(&home, &["add", &format!("rw:{}", home.join("srv/photos").display())]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("reserved"));
    assert!(irsync(&home, &["revoke", &id]).status.success());
    assert!(!std::fs::read_to_string(&exports).unwrap().contains(&id));
    assert!(irsync(&home, &["remove", "photos"]).status.success());
    assert_eq!(std::fs::read_to_string(&exports).unwrap().trim(), "");
}

#[test]
fn host_add_then_allow_by_alias_then_list() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("p")).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    assert!(irsync(&home, &["host", "add", "laptop", &id, "--parallel", "4"]).status.success());
    assert_eq!(std::fs::read_to_string(home.join(".config/irsync/hosts.toml")).unwrap(), format!("[laptop]\nendpoint_id = \"{id}\"\nparallel = 4\n"));
    assert!(irsync(&home, &["add", &format!("p:{}", home.join("p").display())]).status.success());
    assert!(irsync(&home, &["allow", "p", "laptop"]).status.success());
    assert!(std::fs::read_to_string(home.join(".config/irsync/exports.toml")).unwrap().contains("rw = [\"laptop\"]"));
    let o = irsync(&home, &["host", "list"]);
    let out = String::from_utf8(o.stdout).unwrap();
    assert!(out.starts_with(&format!("laptop: {id}\n")) && out.contains("parallel = 4"), "{out}");
    let o = irsync(&home, &["host", "add", &id, &id]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("alias"));
    assert!(irsync(&home, &["host", "remove", "laptop"]).status.success());
    assert_eq!(std::fs::read_to_string(home.join(".config/irsync/hosts.toml")).unwrap().trim(), "");
}
