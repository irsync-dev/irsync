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
fn grant_remove_all_removes_an_alias_that_left_hosts_toml() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("srv/p")).unwrap();
    let cfg = home.join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public();
    std::fs::write(
        cfg.join("hosts.toml"),
        format!("[hosts.laptop]\nendpoint_id = \"{id}\"\n"),
    )
    .unwrap();
    let target = format!("p:{}", home.join("srv/p").display());
    assert!(irsync(&home, &["export", "add", &target]).status.success());
    assert!(irsync(&home, &["grant", "add", "p", "laptop"])
        .status
        .success());
    std::fs::write(cfg.join("hosts.toml"), "").unwrap();
    let o = irsync(&home, &["grant", "remove", "all", "laptop"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!std::fs::read_to_string(cfg.join("irsync.toml"))
        .unwrap()
        .contains("laptop"));
}

#[test]
fn export_add_grant_add_grant_remove_export_remove_round_trip_with_default_paths() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("srv/photos")).unwrap();
    let target = format!("photos:{}", home.join("srv/photos").display());
    let o = irsync(&home, &["export", "add", &target]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8(o.stdout).unwrap();
    irsync::identity::parse_endpoint_id(out.trim()).expect("stdout is the daemon id");
    assert!(home.join(".config/irsync/daemon_ed25519").exists());
    let exports = home.join(".config/irsync/irsync.toml");
    assert_eq!(
        std::fs::read_to_string(&exports).unwrap(),
        format!("[exports.photos]\npath = {:?}\n", home.join("srv/photos"))
    );
    let id = iroh::SecretKey::from_bytes(&[9; 32]).public().to_string();
    assert!(irsync(&home, &["grant", "add", "photos", &id])
        .status
        .success());
    assert!(std::fs::read_to_string(&exports)
        .unwrap()
        .contains(&format!("rw = [\"{id}\"]")));
    let o = irsync(&home, &["grant", "add", "photos", "all"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("no open access"));
    let o = irsync(
        &home,
        &[
            "export",
            "add",
            &format!("all:{}", home.join("srv/photos").display()),
        ],
    );
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("reserved"));
    assert!(irsync(&home, &["grant", "remove", "all", &id])
        .status
        .success());
    assert!(!std::fs::read_to_string(&exports).unwrap().contains(&id));
    assert!(irsync(&home, &["export", "remove", "photos"])
        .status
        .success());
    assert_eq!(std::fs::read_to_string(&exports).unwrap().trim(), "");
    let o = irsync(&home, &["export", "remove", "photos"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !o.status.success()
            && err.contains("no such entry")
            && err.contains(exports.to_str().unwrap()),
        "{err}"
    );
}

#[test]
fn host_add_then_grant_add_by_alias_then_list() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("p")).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    assert!(
        irsync(&home, &["host", "add", "laptop", &id, "--parallel", "4"])
            .status
            .success()
    );
    assert_eq!(
        std::fs::read_to_string(home.join(".config/irsync/irsync.toml")).unwrap(),
        format!("[hosts.laptop]\nendpoint_id = \"{id}\"\nparallel = 4\n")
    );
    assert!(irsync(
        &home,
        &["export", "add", &format!("p:{}", home.join("p").display())]
    )
    .status
    .success());
    assert!(irsync(&home, &["grant", "add", "p", "laptop"])
        .status
        .success());
    assert!(
        std::fs::read_to_string(home.join(".config/irsync/irsync.toml"))
            .unwrap()
            .contains("rw = [\"laptop\"]")
    );
    let o = irsync(&home, &["host", "list"]);
    let out = String::from_utf8(o.stdout).unwrap();
    assert!(
        out.starts_with(&format!("laptop: {id}\n")) && out.contains("parallel = 4"),
        "{out}"
    );
    let o = irsync(&home, &["host", "add", &id, &id]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("alias"));
    assert!(irsync(&home, &["host", "remove", "laptop"])
        .status
        .success());
    // The alias is gone; the export and its grant entry stay (host remove
    // leaves grants alone).
    let text = std::fs::read_to_string(home.join(".config/irsync/irsync.toml")).unwrap();
    assert!(
        !text.contains("[hosts.laptop]") && text.contains("[exports.p]"),
        "{text}"
    );
}

#[test]
fn id_generates_and_prints_client_then_daemon_then_both() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let o = irsync(&home, &["id", "show"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stderr).contains("generated identity"));
    let out = String::from_utf8(o.stdout).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("ssh-ed25519 "));
    let client = irsync::identity::parse_endpoint_id(lines[1]).unwrap();
    assert!(home.join(".config/irsync/id_ed25519").exists());
    let o = irsync(&home, &["id", "show", "--daemon"]);
    let daemon = irsync::identity::parse_endpoint_id(
        String::from_utf8(o.stdout).unwrap().lines().nth(1).unwrap(),
    )
    .unwrap();
    assert_ne!(client, daemon);
    let o = irsync(&home, &["id", "show", "--all"]);
    let out = String::from_utf8(o.stdout).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!((lines[0], lines[3]), ("client:", "daemon:"));
    assert_eq!(
        irsync::identity::parse_endpoint_id(lines[2]).unwrap(),
        client
    );
    assert_eq!(
        irsync::identity::parse_endpoint_id(lines[5]).unwrap(),
        daemon
    );
    assert!(!String::from_utf8_lossy(&irsync(&home, &["id", "show"]).stderr).contains("generated"));
}

#[test]
fn id_derives_the_key_line_from_the_key_not_a_pub_file() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    assert!(irsync(&home, &["id", "show"]).status.success());
    std::fs::remove_file(home.join(".config/irsync/id_ed25519.pub")).unwrap();
    let o = irsync(&home, &["id", "show"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8(o.stdout).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    let pk = ssh_key::PublicKey::from_openssh(lines[0]).unwrap();
    let from_line = irsync::identity::endpoint_id_of_public_key(&pk).unwrap();
    assert_eq!(
        from_line,
        irsync::identity::parse_endpoint_id(lines[1]).unwrap()
    );
    assert!(irsync(&home, &["show"]).status.success());
}

#[test]
fn show_lists_files_identities_exports_and_flags_unknown_aliases() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("p")).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    assert!(irsync(&home, &["host", "add", "laptop", &id])
        .status
        .success());
    assert!(irsync(
        &home,
        &["export", "add", &format!("p:{}", home.join("p").display())]
    )
    .status
    .success());
    assert!(irsync(&home, &["grant", "add", "p", "laptop"])
        .status
        .success());
    // A hand edit of the one file: an alias nobody defines.
    let config = home.join(".config/irsync/irsync.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(text.contains("rw = [\"laptop\"]"), "{text}");
    std::fs::write(
        &config,
        text.replace("rw = [\"laptop\"]", "rw = [\"laptop\", \"ghost\"]"),
    )
    .unwrap();
    let o = irsync(&home, &["show"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8(o.stdout).unwrap();
    assert!(
        out.lines()
            .any(|l| l == format!("hosts: {}", config.display()))
            && out
                .lines()
                .any(|l| l == format!("exports: {}", config.display())),
        "{out}"
    );
    assert!(
        out.contains(&format!("laptop ({id})")) && out.contains("ghost (unknown alias)"),
        "{out}"
    );
    let line = format!("p: {}", home.join("p").display());
    assert!(out.lines().any(|l| l == line), "{out}");
    assert!(
        out.contains("client identity: ") && out.contains("daemon identity: "),
        "{out}"
    );
    assert!(
        out.lines()
            .any(|l| l == format!("config: {}", config.display())),
        "{out}"
    );
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
    assert!(
        err.contains("generated identity") && err.contains(key.to_str().unwrap()),
        "{err}"
    );
    assert!(key.exists() && home.join(".config/irsync/id_ed25519.pub").exists());
}

#[test]
fn an_unreadable_daemon_key_is_named_in_the_error() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    assert!(irsync(&home, &["id", "show", "--daemon"]).status.success());
    let key = home.join(".config/irsync/daemon_ed25519");
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&key).is_ok() {
        eprintln!("skipping: running as root, mode 0o000 does not stop reads");
        return;
    }
    let o = irsync(&home, &["id", "show", "--daemon"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !o.status.success() && err.contains(key.to_str().unwrap()),
        "{err}"
    );
    // `export add` needs the daemon identity, and adds nothing without it.
    std::fs::create_dir_all(home.join("p")).unwrap();
    let o = irsync(
        &home,
        &["export", "add", &format!("p:{}", home.join("p").display())],
    );
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !o.status.success() && err.contains(key.to_str().unwrap()),
        "{err}"
    );
    assert!(!home.join(".config/irsync/irsync.toml").exists());
}

#[test]
fn grant_add_refuses_a_pasted_key_line_with_a_multi_line_comment() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let exports = home.join(".config/irsync/irsync.toml");
    let line = irsync::identity::public_key_line(&iroh::SecretKey::from_bytes(&[7; 32]), "x");
    let key = line.rsplit_once(' ').unwrap().0;
    let other = iroh::SecretKey::from_bytes(&[8; 32]).public();
    let evil = format!("{key} laptop\n]\n[etc]\npath = \"/etc\"\nrw = [\"{other}\"");
    // No config file yet: nothing is created.
    let o = irsync(&home, &["grant", "add", "all", &evil]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !o.status.success() && err.contains("control characters"),
        "{err}"
    );
    assert!(!exports.exists());
    std::fs::create_dir_all(home.join("p")).unwrap();
    assert!(irsync(
        &home,
        &["export", "add", &format!("p:{}", home.join("p").display())]
    )
    .status
    .success());
    let before = std::fs::read_to_string(&exports).unwrap();
    let o = irsync(&home, &["grant", "add", "p", &evil]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !o.status.success() && err.contains("control characters"),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&exports).unwrap(), before);
}

#[test]
fn export_add_of_a_bare_path_named_all_is_refused_and_writes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("srv/all")).unwrap();
    let o = irsync(
        &home,
        &["export", "add", home.join("srv/all").to_str().unwrap()],
    );
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !o.status.success() && err.contains("\"all\" is reserved"),
        "{err}"
    );
    assert!(!home.join(".config/irsync/irsync.toml").exists());
}

#[test]
fn config_flag_after_the_noun_names_the_file() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("dir")).unwrap();
    let c = d.path().join("elsewhere/irsync.toml");
    let c_str = c.to_str().unwrap();
    let dir = home.join("dir");
    let o = irsync(
        &home,
        &[
            "export",
            "--config",
            c_str,
            "add",
            &format!("p:{}", dir.display()),
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = irsync(
        &home,
        &[
            "export",
            "add",
            &format!("p2:{}", dir.display()),
            "--config",
            c_str,
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // The table lands in the named file or, until the nouns move, in the
    // exports.toml beside it; never under the default directory.
    let read = |p: &Path| std::fs::read_to_string(p).unwrap_or_default();
    let text = read(&c) + &read(&c.with_file_name("exports.toml"));
    assert!(
        text.contains("[exports.p]\n") && text.contains("[exports.p2]\n"),
        "{text}"
    );
    assert!(!home.join(".config/irsync/exports.toml").exists());
    assert!(!home.join(".config/irsync/irsync.toml").exists());
}

#[test]
fn export_list_prints_the_export_blocks_of_show() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("p")).unwrap();
    std::fs::create_dir_all(home.join("q")).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    for args in [
        vec!["host", "add", "laptop", &id],
        vec![
            "export",
            "add",
            &format!("p:{}", home.join("p").display()),
            "--ro",
        ],
        vec!["export", "add", &format!("q:{}", home.join("q").display())],
        vec!["grant", "add", "p", "laptop"],
        vec!["grant", "add", "all", "laptop", "--ro"],
    ] {
        let o = irsync(&home, &args);
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
    let show_out = String::from_utf8(irsync(&home, &["show"]).stdout).unwrap();
    let o = irsync(&home, &["export", "list"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let list_out = String::from_utf8(o.stdout).unwrap();
    let blocks: Vec<&str> = show_out
        .lines()
        .skip_while(|l| !l.starts_with("daemon identity:"))
        .skip(1)
        .take_while(|l| *l != "all:")
        .collect();
    assert_eq!(list_out, blocks.join("\n") + "\n", "show:\n{show_out}");
    assert!(
        list_out.starts_with("p: ")
            && list_out.contains("    read_only\n")
            && list_out.contains("    rw: laptop ("),
        "{list_out}"
    );
}

#[test]
fn grant_list_by_export_by_peer_and_for_an_unknown_name() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("p")).unwrap();
    std::fs::create_dir_all(home.join("q")).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    for args in [
        vec!["host", "add", "laptop", &id],
        vec!["export", "add", &format!("p:{}", home.join("p").display())],
        vec![
            "export",
            "add",
            &format!("q:{}", home.join("q").display()),
            "--ro",
        ],
        vec!["grant", "add", "p", "laptop"],
        vec!["grant", "add", "q", "laptop"],
    ] {
        let o = irsync(&home, &args);
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
    let run = |args: &[&str]| {
        let o = irsync(&home, args);
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8(o.stdout).unwrap()
    };
    let all = run(&["grant", "list"]);
    assert!(
        all.starts_with("all:\n")
            && all.contains("p:\n    rw: laptop (")
            && all.contains("q:\n    rw: laptop ("),
        "{all}"
    );
    assert_eq!(
        run(&["grant", "list", "p"]),
        format!("p:\n    rw: laptop ({id})\n")
    );
    assert_eq!(
        run(&["grant", "list", "laptop"]),
        format!("laptop ({id}):\n    p: rw\n    q: ro (read_only export)\n")
    );
    assert!(run(&["grant", "list", &id]).starts_with(&format!("{id}:\n")));
    let o = irsync(&home, &["grant", "list", "ghost"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !o.status.success()
            && err.contains("\"ghost\" is not an export, an alias in")
            && err.contains("irsync.toml")
            && err.contains("EndpointId"),
        "{err}"
    );
}

#[test]
fn host_list_refuses_an_old_layout_file() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let cfg = home.join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public();
    std::fs::write(
        cfg.join("hosts.toml"),
        format!("[home]\nendpoint_id = \"{id}\"\n"),
    )
    .unwrap();
    let o = irsync(&home, &["host", "list"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !o.status.success() && err.contains("hosts live under [hosts.home]"),
        "{err}"
    );
}

#[test]
fn grant_remove_all_all_locks_everyone_out() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("p")).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    for args in [
        vec!["host", "add", "laptop", &id],
        vec!["export", "add", &format!("p:{}", home.join("p").display())],
        vec!["grant", "add", "p", "laptop"],
        vec!["grant", "add", "all", "laptop", "--ro"],
    ] {
        let o = irsync(&home, &args);
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
    let o = irsync(&home, &["grant", "remove", "all", "all"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stderr).contains("removed 2 entries"));
    let o = irsync(&home, &["grant", "list", "laptop"]);
    assert_eq!(
        String::from_utf8(o.stdout).unwrap(),
        format!("laptop ({id}):\n")
    );
}

#[test]
fn export_update_hashes_the_named_export_into_the_daemon_state() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("srv/photos")).unwrap();
    std::fs::write(home.join("srv/photos/a.jpg"), vec![1u8; 4096]).unwrap();
    std::fs::write(home.join("srv/photos/b.jpg"), vec![2u8; 8192]).unwrap();
    let target = format!("photos:{}", home.join("srv/photos").display());
    assert!(irsync(&home, &["export", "add", &target]).status.success());
    let state = home.join("xdg/irsync");
    let config = home.join(".config/irsync/irsync.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, format!("state_dir = {:?}\n{text}", state)).unwrap();
    // A dry run reads nothing and creates no index.
    let o = irsync(&home, &["export", "update", "-n", "all"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8(o.stdout).unwrap();
    assert!(
        out.contains("2 file(s), 2 to hash (12288 bytes)"),
        "{out:?}"
    );
    let o = irsync(&home, &["export", "update", "photos"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8(o.stdout).unwrap();
    assert!(out.contains("2 file(s), 2 hashed (12288 bytes)"), "{out:?}");
    assert!(std::fs::read_dir(state.join("index"))
        .unwrap()
        .next()
        .is_some());
    // A second run finds every row fresh.
    let o = irsync(&home, &["export", "update", "all"]);
    let out = String::from_utf8(o.stdout).unwrap();
    assert!(out.contains("2 file(s), 0 hashed (0 bytes)"), "{out:?}");
    // `all` stands alone.
    let o = irsync(&home, &["export", "update", "all", "photos"]);
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("irsync export update: all names every export; give it alone"),
        "{err}"
    );
    // An unknown name fails and names the file.
    let o = irsync(&home, &["export", "update", "music"]);
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("irsync export update: no export \"music\" in"),
        "{err}"
    );
    assert!(err.contains("irsync.toml"), "{err}");
}

#[test]
fn id_generate_writes_the_configured_identity_and_refuses_a_second_time() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let cfg = home.join(".config/irsync");
    // The client's identity by default.
    let o = irsync(&home, &["id", "generate"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let key = cfg.join("id_ed25519");
    assert!(String::from_utf8_lossy(&o.stderr).contains(&format!("wrote {}", key.display())));
    assert!(key.exists() && cfg.join("id_ed25519.pub").exists());
    let out = String::from_utf8(o.stdout).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(
        lines[0].starts_with("ssh-ed25519 ") && lines[0].ends_with(" irsync"),
        "{out}"
    );
    let client = irsync::identity::parse_endpoint_id(lines[1]).unwrap();
    // `id show` finds it rather than generating another.
    let o = irsync(&home, &["id", "show"]);
    assert!(!String::from_utf8_lossy(&o.stderr).contains("generated"));
    let shown = String::from_utf8(o.stdout).unwrap();
    assert_eq!(shown.lines().nth(1).unwrap(), client.to_string());
    // The daemon's with --daemon.
    let o = irsync(&home, &["id", "generate", "--daemon"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(cfg.join("daemon_ed25519").exists());
    let out = String::from_utf8(o.stdout).unwrap();
    assert!(
        out.lines().next().unwrap().ends_with(" irsync-daemon"),
        "{out}"
    );
    // An existing key is never replaced.
    let before = std::fs::read(&key).unwrap();
    let o = irsync(&home, &["id", "generate"]);
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.starts_with("irsync id generate: ") && err.contains("refusing to overwrite"),
        "{err}"
    );
    assert_eq!(std::fs::read(&key).unwrap(), before);
    // The paths come from the settings file.
    let other = home.join("elsewhere/client_key");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(cfg.join("irsync.toml"), format!("identity = {:?}\n", other)).unwrap();
    let o = irsync(&home, &["id", "generate"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(other.exists() && home.join("elsewhere/client_key.pub").exists());
}

#[test]
fn id_generate_force_replaces_an_existing_identity() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let cfg = home.join(".config/irsync");
    let key = cfg.join("id_ed25519");
    let pub_key = cfg.join("id_ed25519.pub");
    let o = irsync(&home, &["id", "generate"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let old_id = String::from_utf8(o.stdout)
        .unwrap()
        .lines()
        .nth(1)
        .unwrap()
        .to_string();
    let old_key = std::fs::read(&key).unwrap();
    let old_pub = std::fs::read(&pub_key).unwrap();
    for args in [
        &["id", "generate", "--force"][..],
        &["id", "generate", "-f"][..],
    ] {
        let o = irsync(&home, args);
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(err.contains(&format!("wrote {}", key.display())), "{err}");
        let out = String::from_utf8(o.stdout).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "{out}");
        let new_id = irsync::identity::parse_endpoint_id(lines[1]).unwrap();
        assert_ne!(new_id.to_string(), old_id);
        assert_ne!(std::fs::read(&key).unwrap(), old_key);
        assert_ne!(std::fs::read(&pub_key).unwrap(), old_pub);
        assert!(lines[0].starts_with("ssh-ed25519 "));
        assert_eq!(
            std::fs::read_to_string(&pub_key).unwrap().trim(),
            lines[0],
            "the .pub file matches the printed line"
        );
        // `id show` now reports the new key.
        let shown = String::from_utf8(irsync(&home, &["id", "show"]).stdout).unwrap();
        assert_eq!(shown.lines().nth(1).unwrap(), new_id.to_string());
    }
    // Without the flag the refusal still stands.
    let o = irsync(&home, &["id", "generate"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("refusing to overwrite"));
}

fn ok(home: &Path, args: &[&str]) -> String {
    let o = irsync(home, args);
    assert!(
        o.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn host_list_prints_the_hosts_section_of_show() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    ok(
        &home,
        &[
            "host",
            "add",
            "laptop",
            &id,
            "--relay",
            "https://r.example/",
            "--parallel",
            "4",
        ],
    );
    ok(&home, &["host", "add", "desk", &id]);
    let list = ok(&home, &["host", "list"]);
    assert_eq!(
        list,
        format!("laptop: {id}\n    relay = \"https://r.example/\"\n    parallel = 4\ndesk: {id}\n")
    );
    let show = ok(&home, &["show"]);
    let section: Vec<&str> = show
        .lines()
        .skip_while(|l| !l.starts_with("hosts: "))
        .skip(1)
        .take_while(|l| !l.starts_with("exports: "))
        .collect();
    assert_eq!(list, section.join("\n") + "\n", "show:\n{show}");
}

#[test]
fn grant_list_prefers_an_export_over_an_alias_of_the_same_name() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("p")).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    ok(&home, &["host", "add", "p", &id]);
    ok(
        &home,
        &["export", "add", &format!("p:{}", home.join("p").display())],
    );
    ok(&home, &["grant", "add", "p", "p"]);
    // The export's block, not the peer's reach.
    assert_eq!(
        ok(&home, &["grant", "list", "p"]),
        format!("p:\n    rw: p ({id})\n")
    );
    // The peer is still reachable by its id.
    assert_eq!(
        ok(&home, &["grant", "list", &id]),
        format!("{id}:\n    p: rw\n")
    );
}

#[test]
fn grant_list_lines_appear_verbatim_in_show() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("p")).unwrap();
    std::fs::create_dir_all(home.join("q")).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    let other = iroh::SecretKey::from_bytes(&[6; 32]).public().to_string();
    ok(&home, &["host", "add", "laptop", &id]);
    ok(
        &home,
        &["export", "add", &format!("p:{}", home.join("p").display())],
    );
    ok(
        &home,
        &["export", "add", &format!("q:{}", home.join("q").display())],
    );
    ok(&home, &["grant", "add", "p", "laptop"]);
    ok(&home, &["grant", "add", "p", &other, "--ro"]);
    ok(&home, &["grant", "add", "all", "laptop", "--ro"]);
    let list = ok(&home, &["grant", "list"]);
    let show = ok(&home, &["show"]);
    let show_lines: Vec<&str> = show.lines().collect();
    let mut grant_lines = 0;
    for line in list.lines() {
        if line.starts_with("    ") || line == "all:" {
            assert!(show_lines.contains(&line), "{line:?} not in show:\n{show}");
            grant_lines += 1;
        }
    }
    assert_eq!(grant_lines, 4, "{list}");
    // The `all:` block is the tail of show, line for line.
    let tail: Vec<&str> = show_lines
        .iter()
        .copied()
        .skip_while(|l| *l != "all:")
        .collect();
    let all_block: Vec<&str> = list.lines().take_while(|l| !l.starts_with("p:")).collect();
    assert_eq!(tail, all_block, "{show}");
}

#[test]
fn grant_remove_with_a_star_points_at_all() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("p")).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    ok(
        &home,
        &["export", "add", &format!("p:{}", home.join("p").display())],
    );
    ok(&home, &["grant", "add", "p", &id]);
    for (args, hint) in [
        (
            &["grant", "remove", "p", "*"][..],
            "irsync grant remove p all",
        ),
        (
            &["grant", "remove", "all", "*"][..],
            "irsync grant remove all all",
        ),
    ] {
        let o = irsync(&home, args);
        assert_eq!(o.status.code(), Some(1), "{args:?}");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(
            err.contains("\"*\" names no peer") && err.contains(hint),
            "{args:?}: {err}"
        );
    }
    // Nothing was removed.
    assert!(ok(&home, &["grant", "list", "p"]).contains(&id));
    // `grant add` keeps its own wording: there is no open access to grant.
    let o = irsync(&home, &["grant", "add", "p", "*"]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("no open access"));
}

#[test]
fn show_names_split_files_it_read() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let cfg = home.join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public();
    let out = ok(&home, &["show"]);
    assert!(
        out.lines()
            .any(|l| l == format!("hosts: {}", cfg.join("irsync.toml").display())),
        "{out}"
    );
    std::fs::write(
        cfg.join("hosts.toml"),
        format!("[hosts.laptop]\nendpoint_id = \"{id}\"\n"),
    )
    .unwrap();
    let out = ok(&home, &["show"]);
    assert!(
        out.lines()
            .any(|l| l == format!("hosts: {} (also read)", cfg.join("hosts.toml").display())),
        "{out}"
    );
    assert!(out.contains(&format!("laptop: {id}")), "{out}");
    assert!(
        out.lines()
            .any(|l| l == format!("exports: {}", cfg.join("irsync.toml").display())),
        "{out}"
    );
}

#[test]
fn nouns_refuse_a_table_kept_in_a_split_file() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let cfg = home.join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::create_dir_all(home.join("p")).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    std::fs::write(cfg.join("hosts.toml"), "").unwrap();
    let o = irsync(&home, &["host", "add", "laptop", &id]);
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("hosts are kept in") && err.contains("hosts.toml"),
        "{err}"
    );
    assert!(!cfg.join("irsync.toml").exists());
    // Exports are not split: the export lands in irsync.toml.
    ok(
        &home,
        &["export", "add", &format!("p:{}", home.join("p").display())],
    );
    assert!(std::fs::read_to_string(cfg.join("irsync.toml"))
        .unwrap()
        .contains("[exports.p]"));
    // Now exports are split too: every grant and export edit is refused.
    std::fs::remove_file(cfg.join("hosts.toml")).unwrap();
    let text = std::fs::read_to_string(cfg.join("irsync.toml")).unwrap();
    std::fs::write(cfg.join("exports.toml"), &text).unwrap();
    std::fs::write(cfg.join("irsync.toml"), "").unwrap();
    for args in [
        &["grant", "add", "p", &id][..],
        &["export", "remove", "p"],
        &["export", "add", &format!("q:{}", home.join("p").display())],
    ] {
        let o = irsync(&home, args);
        assert_eq!(o.status.code(), Some(1), "{args:?}");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(
            err.contains("exports are kept in") && err.contains("exports.toml"),
            "{args:?}: {err}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(cfg.join("exports.toml")).unwrap(),
        text
    );
    // Reading still works across the split.
    assert!(ok(&home, &["export", "list"]).starts_with("p: "));
}

#[test]
fn get_prints_defaults_and_exits_1_for_unset_optional_keys() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    assert_eq!(
        ok(&home, &["config", "get", "daemon.idle_timeout"]),
        "30m\n"
    );
    let o = irsync(&home, &["config", "get", "daemon.listen"]);
    assert!(o.status.success() && o.stdout.is_empty(), "{o:?}");
    let o = irsync(&home, &["config", "get", "client.parallel"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(o.stdout.is_empty() && o.stderr.is_empty(), "{o:?}");
    let o = irsync(&home, &["config", "get", "daemon.nope"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("unknown key \"daemon.nope\""),
        "{o:?}"
    );
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    ok(&home, &["host", "add", "laptop", &id]);
    assert_eq!(
        ok(&home, &["config", "get", "hosts.laptop.endpoint_id"]),
        format!("{id}\n")
    );
    assert_eq!(
        ok(&home, &["config", "get", "hosts.laptop"]),
        format!("endpoint_id = \"{id}\"\n")
    );
    let o = irsync(&home, &["config", "get", "hosts.desk"]);
    assert_eq!(o.status.code(), Some(1));
}

#[test]
fn list_covers_the_config_and_split_files() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let cfg = home.join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    std::fs::write(cfg.join("irsync.toml"), "[client]\nparallel = 3\n").unwrap();
    std::fs::write(
        cfg.join("hosts.toml"),
        format!("[hosts.laptop]\nendpoint_id = \"{id}\"\n"),
    )
    .unwrap();
    assert_eq!(
        ok(&home, &["config", "list"]),
        format!("client.parallel = 3\nhosts.laptop.endpoint_id = \"{id}\"\n")
    );
    // get reads across the split too.
    assert_eq!(
        ok(&home, &["config", "get", "hosts.laptop.endpoint_id"]),
        format!("{id}\n")
    );
}

#[test]
fn set_with_a_bad_value_leaves_the_file_untouched() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let cfg = home.join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    let config = cfg.join("irsync.toml");
    std::fs::write(&config, "[client]\nparallel = 4\n").unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    for (args, needle) in [
        (
            &["config", "set", "hosts.home.endpoint_id", "nope"][..],
            "hosts.home.endpoint_id",
        ),
        (
            &["config", "set", "exports.p.path", "./x"],
            "exports.p.path",
        ),
        (
            &["config", "set", "client.parallel", "x"],
            "client.parallel",
        ),
        (&["config", "set", "daemon.nope", "1"], "daemon.nope"),
        (
            &["config", "set", "hosts.all.endpoint_id", &id],
            "hosts.all.endpoint_id",
        ),
        (&["config", "set", "hosts.home", "1"], "hosts.home"),
        (&["config", "set", "client.parallel"], "client.parallel"),
        (&["config", "unset", "client.reconnect"], "client.reconnect"),
    ] {
        let o = irsync(&home, args);
        assert_eq!(o.status.code(), Some(1), "{args:?}");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(err.contains(needle), "{args:?}: {err}");
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "[client]\nparallel = 4\n",
            "{args:?}"
        );
    }
}

#[test]
fn set_unset_and_the_nouns_share_one_file() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let cfg = home.join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::create_dir_all(home.join("p")).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    let p = home.join("p").to_str().unwrap().to_string();
    ok(&home, &["config", "set", "exports.p.path", &p]);
    ok(&home, &["config", "set", "exports.p.rw", &id]);
    assert_eq!(
        ok(&home, &["grant", "list", "p"]),
        format!("p:\n    rw: {id}\n")
    );
    ok(&home, &["config", "set", "all.rw"]);
    assert!(ok(&home, &["config", "get", "all.rw"]).is_empty());
    assert!(std::fs::read_to_string(cfg.join("irsync.toml"))
        .unwrap()
        .contains("rw = []"));
    ok(
        &home,
        &["config", "set", "daemon.listen", "127.0.0.1:1", "[::1]:1"],
    );
    assert_eq!(
        ok(&home, &["config", "get", "daemon.listen"]),
        "127.0.0.1:1\n[::1]:1\n"
    );
    ok(&home, &["config", "set", "client.reconnect", "2h"]);
    assert_eq!(ok(&home, &["config", "get", "client.reconnect"]), "2h\n");
    ok(&home, &["config", "unset", "exports.p"]);
    assert_eq!(ok(&home, &["export", "list"]), "");
    // A hosts table kept in a split file is refused for set and unset too.
    std::fs::write(cfg.join("hosts.toml"), "").unwrap();
    for args in [
        &["config", "set", "hosts.laptop.endpoint_id", &id][..],
        &["config", "unset", "hosts.laptop"],
    ] {
        let o = irsync(&home, args);
        assert_eq!(o.status.code(), Some(1), "{args:?}");
        assert!(
            String::from_utf8_lossy(&o.stderr).contains("hosts are kept in"),
            "{args:?}"
        );
    }
}

#[test]
fn dotted_names_round_trip_quoted() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("w")).unwrap();
    let w = home.join("w").to_str().unwrap().to_string();
    ok(
        &home,
        &["config", "set", "exports.\"www.example.com\".path", &w],
    );
    let list = ok(&home, &["config", "list"]);
    assert!(
        list.contains(&format!("exports.\"www.example.com\".path = {w:?}")),
        "{list}"
    );
    assert_eq!(
        ok(
            &home,
            &["config", "get", "exports.\"www.example.com\".path"]
        ),
        format!("{w}\n")
    );
    assert!(ok(&home, &["export", "list"]).starts_with("www.example.com: "));
}
