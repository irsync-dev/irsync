mod common;

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
    let o = irsync(&home, &["remove", "photos"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success() && err.contains("no such entry") && err.contains(exports.to_str().unwrap()), "{err}");
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

#[test]
fn id_generates_and_prints_client_then_daemon_then_both() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let o = irsync(&home, &["id"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stderr).contains("generated identity"));
    let out = String::from_utf8(o.stdout).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("ssh-ed25519 "));
    let client = irsync::identity::parse_endpoint_id(lines[1]).unwrap();
    assert!(home.join(".config/irsync/id_ed25519").exists());
    let o = irsync(&home, &["id", "--daemon"]);
    let daemon = irsync::identity::parse_endpoint_id(
        String::from_utf8(o.stdout).unwrap().lines().nth(1).unwrap(),
    )
    .unwrap();
    assert_ne!(client, daemon);
    let o = irsync(&home, &["id", "--all"]);
    let out = String::from_utf8(o.stdout).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!((lines[0], lines[3]), ("client:", "daemon:"));
    assert_eq!(irsync::identity::parse_endpoint_id(lines[2]).unwrap(), client);
    assert_eq!(irsync::identity::parse_endpoint_id(lines[5]).unwrap(), daemon);
    assert!(!String::from_utf8_lossy(&irsync(&home, &["id"]).stderr).contains("generated"));
}

#[test]
fn id_derives_the_key_line_from_the_key_not_a_pub_file() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    assert!(irsync(&home, &["id"]).status.success());
    std::fs::remove_file(home.join(".config/irsync/id_ed25519.pub")).unwrap();
    let o = irsync(&home, &["id"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8(o.stdout).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    let pk = ssh_key::PublicKey::from_openssh(lines[0]).unwrap();
    let from_line = irsync::identity::endpoint_id_of_public_key(&pk).unwrap();
    assert_eq!(from_line, irsync::identity::parse_endpoint_id(lines[1]).unwrap());
    assert!(irsync(&home, &["show"]).status.success());
}

#[test]
fn show_lists_files_identities_exports_and_flags_unknown_aliases() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("p")).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    assert!(irsync(&home, &["host", "add", "laptop", &id]).status.success());
    assert!(irsync(&home, &["add", &format!("p:{}", home.join("p").display())]).status.success());
    assert!(irsync(&home, &["allow", "p", "laptop"]).status.success());
    std::fs::write(
        home.join(".config/irsync/exports.toml"),
        format!("[p]\npath = {:?}\nrw = [\"laptop\", \"ghost\"]\n", home.join("p")),
    )
    .unwrap();
    let o = irsync(&home, &["show"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8(o.stdout).unwrap();
    assert!(out.contains("irsync.toml") && out.contains("hosts.toml") && out.contains("exports.toml"), "{out}");
    assert!(out.contains(&format!("laptop ({id})")) && out.contains("ghost (unknown alias)"), "{out}");
    let line = format!("p: {}", home.join("p").display());
    assert!(out.lines().any(|l| l == line), "{out}");
    assert!(out.contains("client identity: ") && out.contains("daemon identity: "), "{out}");
    assert!(out.contains("irsync.toml (missing)"), "{out}");
}

#[test]
fn sync_generates_the_client_identity_on_first_run() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let src = d.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    // A loopback endpoint that refuses every connection: the client fails
    // at once instead of waiting out the QUIC idle timeout.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let ep = rt.block_on(common::loopback_endpoint());
    let id = ep.id().to_string();
    let port = ep.addr().ip_addrs().next().unwrap().port();
    rt.spawn(async move {
        while let Some(incoming) = ep.accept().await {
            incoming.refuse();
        }
    });
    let o = irsync(
        &home,
        &[
            "-a",
            "--address",
            &format!("127.0.0.1:{port}"),
            "--reconnect",
            "0",
            &format!("{}/", src.display()),
            &format!("{id}:x/"),
        ],
    );
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    let key = home.join(".config/irsync/id_ed25519");
    assert!(err.contains("generated identity") && err.contains(key.to_str().unwrap()), "{err}");
    assert!(key.exists() && home.join(".config/irsync/id_ed25519.pub").exists());
}

#[test]
fn an_unreadable_daemon_key_is_named_in_the_error() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    assert!(irsync(&home, &["id", "--daemon"]).status.success());
    let key = home.join(".config/irsync/daemon_ed25519");
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&key).is_ok() {
        eprintln!("skipping: running as root, mode 0o000 does not stop reads");
        return;
    }
    for args in [&["id", "--daemon"][..], &["daemon", "--show-id"][..]] {
        let o = irsync(&home, args);
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(!o.status.success() && err.contains(key.to_str().unwrap()), "{args:?}: {err}");
    }
}

#[test]
fn allow_refuses_a_pasted_key_line_with_a_multi_line_comment() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let exports = home.join(".config/irsync/exports.toml");
    let line = irsync::identity::public_key_line(&iroh::SecretKey::from_bytes(&[7; 32]), "x");
    let key = line.rsplit_once(' ').unwrap().0;
    let other = iroh::SecretKey::from_bytes(&[8; 32]).public();
    let evil = format!("{key} laptop\n]\n[etc]\npath = \"/etc\"\nrw = [\"{other}\"");
    // No exports.toml yet: nothing is created.
    let o = irsync(&home, &["allow", "all", &evil]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success() && err.contains("control characters"), "{err}");
    assert!(!exports.exists());
    std::fs::create_dir_all(home.join("p")).unwrap();
    assert!(irsync(&home, &["add", &format!("p:{}", home.join("p").display())]).status.success());
    let before = std::fs::read_to_string(&exports).unwrap();
    let o = irsync(&home, &["allow", "p", &evil]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success() && err.contains("control characters"), "{err}");
    assert_eq!(std::fs::read_to_string(&exports).unwrap(), before);
}
