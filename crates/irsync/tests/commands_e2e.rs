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
