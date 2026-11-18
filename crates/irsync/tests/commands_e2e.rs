mod common;

use std::path::Path;
use std::process::{Command, Stdio};

fn irsync(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_irsync"))
        .args(args)
        .env("HOME", home)
        .env("XDG_STATE_HOME", home.join("xdg"))
        .env_remove("IRSYNC_PASSPHRASE")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn grant_remove_all_removes_an_alias_that_left_the_hosts_table() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("srv/p")).unwrap();
    let cfg = home.join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public();
    let host = format!("[hosts.laptop]\nendpoint_id = \"{id}\"\n");
    std::fs::write(cfg.join("irsync.toml"), &host).unwrap();
    let target = format!("p:{}", home.join("srv/p").display());
    assert!(irsync(&home, &["export", "add", &target]).status.success());
    assert!(irsync(&home, &["grant", "add", "p", "laptop"])
        .status
        .success());
    // The host entry goes by hand; the grant still names it.
    let text = std::fs::read_to_string(cfg.join("irsync.toml")).unwrap();
    assert!(text.contains(&host), "{text}");
    std::fs::write(cfg.join("irsync.toml"), text.replace(&host, "")).unwrap();
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
    // A grant names the host: it stays until the grant is gone.
    let o = irsync(&home, &["host", "remove", "laptop"]);
    assert_eq!(o.status.code(), Some(1));
    ok(&home, &["grant", "remove", "p", "laptop"]);
    let o = irsync(&home, &["host", "remove", "laptop"]);
    assert!(o.status.success());
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        format!(
            "removed host laptop from {}\n",
            home.join(".config/irsync/irsync.toml").display()
        )
    );
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
    // No daemon identity yet: no pointer to one.
    assert!(!String::from_utf8_lossy(&o.stderr).contains("also has a daemon identity"));
    let out = String::from_utf8(o.stdout).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], "client:");
    assert!(lines[1].starts_with("ssh-ed25519 "));
    let client = irsync::identity::parse_endpoint_id(lines[2]).unwrap();
    assert!(home.join(".config/irsync/id_ed25519").exists());
    // `id show --daemon` never creates the identity.
    let o = irsync(&home, &["id", "show", "--daemon"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("no daemon identity"));
    assert!(!home.join(".config/irsync/daemon_ed25519").exists());
    assert!(irsync(&home, &["id", "generate", "--daemon"])
        .status
        .success());
    let o = irsync(&home, &["id", "show", "--daemon"]);
    let out = String::from_utf8(o.stdout).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], "daemon:");
    let daemon = irsync::identity::parse_endpoint_id(lines[2]).unwrap();
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

/// On a machine that also runs a daemon, plain `id show` (the client's
/// identity) says where the daemon's is: clients need that one.
#[test]
fn id_show_on_a_server_points_at_the_daemon_id() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    assert!(irsync(&home, &["id", "generate", "--daemon"])
        .status
        .success());
    let o = irsync(&home, &["id", "show"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("irsync: this machine also has a daemon identity"),
        "{err}"
    );
    assert!(err.contains("irsync id show --daemon"), "{err}");
    for flag in ["--daemon", "--all"] {
        let o = irsync(&home, &["id", "show", flag]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(!err.contains("also has a daemon identity"), "{flag}: {err}");
    }
}

#[test]
fn id_show_uses_the_ssh_key_until_an_irsync_key_exists() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    let ssh =
        irsync::identity::generate_openssh(&home.join(".ssh/id_ed25519"), "me@laptop").unwrap();
    let endpoint_of = |o: &std::process::Output| {
        irsync::identity::parse_endpoint_id(
            String::from_utf8_lossy(&o.stdout).lines().last().unwrap(),
        )
        .unwrap()
    };
    // The user's ssh key is the identity; nothing is generated or written.
    let o = irsync(&home, &["id", "show"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(endpoint_of(&o), ssh.public());
    assert!(!String::from_utf8_lossy(&o.stderr).contains("generated"));
    assert!(!home.join(".config/irsync/id_ed25519").exists());
    assert!(irsync(&home, &["show"]).status.success());
    // An irsync key, once made, takes precedence.
    assert!(irsync(&home, &["id", "generate"]).status.success());
    let o = irsync(&home, &["id", "show"]);
    let own = endpoint_of(&o);
    assert_ne!(own, ssh.public());
    // A named identity is the whole answer: generated beside the ssh key.
    let named = home.join("elsewhere/key");
    std::fs::write(
        home.join(".config/irsync/irsync.toml"),
        format!("identity = {:?}\n", named.display().to_string()),
    )
    .unwrap();
    let o = irsync(&home, &["id", "show"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stderr).contains("generated identity"));
    let from_named = endpoint_of(&o);
    assert_ne!(from_named, ssh.public());
    assert_ne!(from_named, own);
    assert!(named.exists());
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
    let pk = ssh_key::PublicKey::from_openssh(lines[1]).unwrap();
    let from_line = irsync::identity::endpoint_id_of_public_key(&pk).unwrap();
    assert_eq!(
        from_line,
        irsync::identity::parse_endpoint_id(lines[2]).unwrap()
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
        out.lines().any(|l| l == "hosts:") && out.lines().any(|l| l == "exports:"),
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
    assert!(irsync(&home, &["id", "generate", "--daemon"])
        .status
        .success());
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

/// `id list` prints what the machine has: the client's identity (made
/// when missing), and the daemon's only when there is one.
#[test]
fn id_list_prints_the_client_and_the_daemon_when_there_is_one() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    // Fresh machine: the client identity is created, the daemon's is not.
    let o = irsync(&home, &["id", "list"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stderr).contains("generated identity"));
    let out = String::from_utf8(o.stdout).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "{out}");
    assert_eq!(lines[0], "client:");
    assert!(home.join(".config/irsync/id_ed25519").exists());
    assert!(!home.join(".config/irsync/daemon_ed25519").exists());
    // With a daemon identity: both blocks, exactly what `show -a` prints,
    // and no pointer on stderr.
    ok(&home, &["id", "generate", "--daemon"]);
    let o = irsync(&home, &["id", "list"]);
    assert!(o.status.success());
    assert!(
        o.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let out = String::from_utf8(o.stdout).unwrap();
    assert_eq!(out, ok(&home, &["id", "show", "--all"]));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!((lines.len(), lines[0], lines[3]), (6, "client:", "daemon:"));
}

/// A daemon key that exists but cannot be read is an error that names the
/// file, not a daemon quietly left out.
#[test]
fn id_list_names_a_daemon_key_it_cannot_read() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    ok(&home, &["id", "generate", "--daemon"]);
    let key = home.join(".config/irsync/daemon_ed25519");
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&key).is_ok() {
        eprintln!("skipping: running as root, mode 0o000 does not stop reads");
        return;
    }
    let o = irsync(&home, &["id", "list"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !o.status.success() && err.contains(key.to_str().unwrap()),
        "{err}"
    );
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
    // The table lands in the named file, never under the default directory.
    let text = std::fs::read_to_string(&c).unwrap();
    assert!(
        text.contains("[exports.p]\n") && text.contains("[exports.p2]\n"),
        "{text}"
    );
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
        cfg.join("irsync.toml"),
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
    assert_eq!(shown.lines().last().unwrap(), client.to_string());
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
        assert_eq!(shown.lines().last().unwrap(), new_id.to_string());
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
        .skip_while(|l| *l != "hosts:")
        .skip(1)
        .take_while(|l| *l != "exports:")
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
    // `grant list` puts the `immediate:` block right after it.
    let all_block: Vec<&str> = list.lines().take_while(|l| *l != "immediate:").collect();
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
fn show_names_the_config_file_once() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let cfg = home.join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    let out = ok(&home, &["show"]);
    let file = cfg.join("irsync.toml").display().to_string();
    assert_eq!(out.matches(&file).count(), 1, "{out}");
    assert!(out.starts_with(&format!("config: {file}")), "{out}");
    // The hosts and exports sections are headed by their names alone.
    assert!(
        out.lines().any(|l| l == "hosts:") && out.lines().any(|l| l == "exports:"),
        "{out}"
    );
}

#[test]
fn split_file_warns_on_stderr_and_is_not_read() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let cfg = home.join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public();
    std::fs::write(
        cfg.join("hosts.toml"),
        format!("[hosts.box]\nendpoint_id = \"{id}\"\n"),
    )
    .unwrap();
    let o = irsync(&home, &["host", "list"]);
    let (out, err) = (
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr),
    );
    assert!(o.status.success(), "{err}");
    assert!(!out.contains("box"), "{out}");
    let path = cfg.join("hosts.toml").display().to_string();
    assert!(
        err.contains(&format!("ignoring {path}")) && err.contains("no longer read"),
        "{err}"
    );
}

#[test]
fn host_add_works_beside_a_split_hosts_file() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let cfg = home.join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    std::fs::write(cfg.join("hosts.toml"), "").unwrap();
    // The editing commands warn too: the new table lands in irsync.toml
    // while whatever hosts.toml held stays ignored.
    let warning = format!("ignoring {}", cfg.join("hosts.toml").display());
    for args in [
        &["host", "add", "box", &id][..],
        &["config", "set", "client.parallel", "2"],
        &["config", "unset", "client.parallel"],
        &["host", "remove", "box"],
    ] {
        let o = irsync(&home, args);
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success(), "{args:?}: {err}");
        assert_eq!(err.matches(&warning).count(), 1, "{args:?}: {err}");
    }
    ok(&home, &["host", "add", "box", &id]);
    assert!(std::fs::read_to_string(cfg.join("irsync.toml"))
        .unwrap()
        .contains("[hosts.box]"));
    assert_eq!(std::fs::read_to_string(cfg.join("hosts.toml")).unwrap(), "");
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
    assert_eq!(ok(&home, &["config", "get", "client.parallel"]), "128\n");
    let o = irsync(&home, &["config", "get", "client.relay"]);
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

#[test]
fn a_lock_file_blocks_edits_and_names_itself() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let cfg = home.join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(cfg.join("irsync.toml.lock"), "").unwrap();
    let o = irsync(&home, &["config", "set", "client.parallel", "4"]);
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("irsync.toml.lock"), "{err}");
    assert!(!cfg.join("irsync.toml").exists());
    std::fs::remove_file(cfg.join("irsync.toml.lock")).unwrap();
    ok(&home, &["config", "set", "client.parallel", "4"]);
    assert!(!cfg.join("irsync.toml.lock").exists());
    assert_eq!(ok(&home, &["config", "get", "client.parallel"]), "4\n");
}

#[test]
fn unset_of_the_last_key_leaves_no_empty_table() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    ok(&home, &["config", "set", "hosts.laptop.endpoint_id", &id]);
    ok(&home, &["config", "unset", "hosts.laptop.endpoint_id"]);
    let text = std::fs::read_to_string(home.join(".config/irsync/irsync.toml")).unwrap();
    assert!(!text.contains("hosts"), "{text:?}");
}

#[test]
fn set_refuses_an_export_name_with_a_slash() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let o = irsync(&home, &["config", "set", "exports.\"a/b\".path", "/x"]);
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("is not a valid export name"), "{err}");
    assert!(!home.join(".config/irsync/irsync.toml").exists());
}

#[test]
fn index_and_gc_use_the_configured_state_dir() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let root = d.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(root.join("f"), b"data").unwrap();
    let state = d.path().join("state");
    let config = d.path().join("irsync.toml");
    std::fs::write(&config, format!("state_dir = {state:?}\n")).unwrap();
    let c = config.to_str().unwrap();
    let o = irsync(&home, &["index", "--config", c, root.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let files = |dir: &Path| -> Vec<_> {
        std::fs::read_dir(dir.join("index"))
            .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default()
    };
    assert_eq!(files(&state).len(), 1, "{:?}", files(&state));
    assert!(files(&home.join("xdg/irsync")).is_empty());
    // gc reads the same file for its state dir.
    let o = irsync(&home, &["gc", "-n", "--config", c]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = irsync(&home, &["gc", "-n", c]);
    assert!(!o.status.success(), "gc takes no operands");
}

#[test]
fn gc_removes_an_older_builds_index_file_unless_it_is_open() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let root = d.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let state = d.path().join("state");
    let config = d.path().join("irsync.toml");
    std::fs::write(&config, format!("state_dir = {state:?}\n")).unwrap();
    let c = config.to_str().unwrap();
    let o = irsync(&home, &["index", "--config", c, root.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // A version 1 build's file for another root, in WAL mode.
    let old = state.join("index/00000000000000000000000000000000.sqlite");
    let held = rusqlite::Connection::open(&old).unwrap();
    held.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE meta (key TEXT PRIMARY KEY, value);")
        .unwrap();
    let stdout = |o: &std::process::Output| String::from_utf8_lossy(&o.stdout).into_owned();

    let o = irsync(&home, &["gc", "-n", "--config", c]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(
        stdout(&o).contains(&old.display().to_string()),
        "{}",
        stdout(&o)
    );
    assert!(
        stdout(&o).contains("1 old index file(s) would be removed"),
        "{}",
        stdout(&o)
    );
    assert!(old.exists());

    // Open (as an old daemon would hold it): kept.
    let o = irsync(&home, &["gc", "--config", c]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stderr).contains("in use, kept"));
    assert!(!stdout(&o).contains("old index file"), "{}", stdout(&o));
    assert!(old.exists());

    drop(held);
    let o = irsync(&home, &["gc", "--config", c]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(
        stdout(&o).contains("1 old index file(s) removed"),
        "{}",
        stdout(&o)
    );
    let left: Vec<_> = std::fs::read_dir(state.join("index"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(left[0].ends_with(".v2.sqlite"), "{left:?}");
}

#[test]
fn config_sets_and_checks_the_telemetry_keys() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    ok(
        &home,
        &["config", "set", "telemetry.otlp_endpoint", "http://c:4318"],
    );
    ok(&home, &["config", "set", "daemon.debug_addr", "9464"]);
    assert_eq!(
        ok(&home, &["config", "get", "telemetry.otlp_endpoint"]),
        "http://c:4318\n"
    );
    assert_eq!(ok(&home, &["config", "get", "daemon.debug_addr"]), "9464\n");
    let file = home.join(".config/irsync/irsync.toml");
    let saved = std::fs::read_to_string(&file).unwrap();
    // Values that would not load are refused, and the file is left as it was.
    for (key, bad) in [
        ("telemetry.otlp_endpoint", "notaurl"),
        ("daemon.debug_addr", "nope"),
    ] {
        let o = irsync(&home, &["config", "set", key, bad]);
        assert_eq!(o.status.code(), Some(1), "{key}: {o:?}");
        assert!(String::from_utf8_lossy(&o.stderr).contains(key), "{o:?}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), saved, "{key}");
    }
    // A refusal says where and why once each.
    let o = irsync(&home, &["config", "set", "daemon.debug_addr", "nope"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(err.matches("bad debug address").count(), 1, "{err}");
    assert_eq!(err.matches("irsync.toml").count(), 1, "{err}");
}

/// The getting-started text in `--help` must point at the line of
/// `irsync id show` that holds the EndpointId, which is the last one.
#[test]
fn help_points_at_the_last_line_of_id_show() {
    let d = tempfile::tempdir().unwrap();
    let o = irsync(&d.path().join("home"), &["--help"]);
    assert!(o.status.success());
    let help = String::from_utf8_lossy(&o.stdout);
    assert!(help.contains("print its id (the last line)"), "{help}");
    assert!(!help.contains("second line"), "{help}");
}

/// A noun typed alone runs its `list`: same output, same exit code.
#[test]
fn a_bare_noun_runs_its_list() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("p")).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    ok(&home, &["host", "add", "laptop", &id]);
    ok(
        &home,
        &["export", "add", &format!("p:{}", home.join("p").display())],
    );
    ok(&home, &["grant", "add", "p", "laptop"]);
    // The client identity exists before the comparison, so neither run
    // prints "generated identity".
    ok(&home, &["id", "generate"]);
    for noun in ["id", "host", "export", "grant", "config"] {
        let bare = irsync(&home, &[noun]);
        let list = irsync(&home, &[noun, "list"]);
        assert!(bare.status.success(), "{noun}");
        assert!(!list.stdout.is_empty(), "{noun}");
        assert_eq!(bare.stdout, list.stdout, "{noun}");
        assert_eq!(bare.stderr, list.stderr, "{noun}");
    }
    // The flag after a bare noun names the file.
    let other = d.path().join("other.toml");
    std::fs::write(&other, format!("[hosts.nas]\nendpoint_id = \"{id}\"\n")).unwrap();
    let c = other.to_str().unwrap();
    assert_eq!(
        ok(&home, &["host", "--config", c]),
        ok(&home, &["host", "list", "--config", c])
    );
    assert!(ok(&home, &["host", "--config", c]).contains("nas"));
}

/// A bare noun only lists: it writes nothing, `--help` is still help, and
/// the arguments of `list` need `list` typed out.
#[test]
fn a_bare_noun_changes_nothing_and_takes_no_list_arguments() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    for noun in ["host", "export", "grant", "config"] {
        assert!(irsync(&home, &[noun]).status.success(), "{noun}");
    }
    assert!(!home.join(".config/irsync/irsync.toml").exists());
    assert!(!home.join(".config/irsync/daemon_ed25519").exists());
    let o = irsync(&home, &["id", "--help"]);
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("Usage: irsync id"));
    assert!(!home.join(".config/irsync/id_ed25519").exists());
    for args in [&["grant", "photos"][..], &["id", "--daemon"]] {
        let o = irsync(&home, args);
        assert_eq!(o.status.code(), Some(2), "{args:?}");
    }
}

/// A daemon key whose directory cannot be entered is not "no daemon
/// identity": `id list` fails and names the path instead of leaving the
/// daemon out.
#[test]
fn id_list_names_a_daemon_key_in_a_directory_it_cannot_enter() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let locked = d.path().join("locked");
    let key = locked.join("key");
    std::fs::create_dir_all(home.join(".config/irsync")).unwrap();
    std::fs::write(
        home.join(".config/irsync/irsync.toml"),
        format!("[daemon]\nidentity = \"{}\"\n", key.display()),
    )
    .unwrap();
    ok(&home, &["id", "generate", "--daemon"]);
    assert!(key.exists());
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let skip = key.symlink_metadata().is_ok();
    let o = irsync(&home, &["id", "list"]);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    if skip {
        eprintln!("skipping: running as root, mode 0o000 does not stop lookups");
        return;
    }
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !o.status.success() && err.contains(key.to_str().unwrap()),
        "{err}"
    );
}

/// A home whose config is `text`; the home and the config file.
fn home_with(d: &tempfile::TempDir, text: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let home = d.path().join("home");
    let cfg = home.join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    let file = cfg.join("irsync.toml");
    if !text.is_empty() {
        std::fs::write(&file, text).unwrap();
    }
    (home, file)
}

fn peer_id(n: u8) -> String {
    iroh::SecretKey::from_bytes(&[n; 32]).public().to_string()
}

#[test]
fn grant_add_by_id_writes_and_prints_the_alias() {
    let d = tempfile::tempdir().unwrap();
    let (home, file) = home_with(&d, "");
    let id = peer_id(5);
    ok(&home, &["host", "add", "box", &id]);
    let o = irsync(&home, &["grant", "add", "all", &id]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert!(err.starts_with("box: rw on all exports"), "{err}");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("rw = [\"box\"]"), "{text}");
    let o = irsync(&home, &["grant", "add", "immediate", &id]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.starts_with("box: immediate access"), "{err}");
    let text = std::fs::read_to_string(&file).unwrap();
    assert_eq!(text.matches(&id).count(), 1, "only the host entry: {text}");
}

#[test]
fn host_add_renames_existing_grants() {
    let d = tempfile::tempdir().unwrap();
    let id = peer_id(5);
    let (home, file) = home_with(&d, &format!("[all]\nrw = [\"{id}\"]\n"));
    ok(&home, &["host", "add", "box", &id]);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("[all]\nrw = [\"box\"]\n"), "{text}");
}

#[test]
fn unrelated_config_set_cleans_the_file() {
    let d = tempfile::tempdir().unwrap();
    let id = peer_id(5);
    let (home, file) = home_with(
        &d,
        &format!("[hosts.box]\nendpoint_id = \"{id}\"\n[all]\nrw = [\"{id}\"]\n"),
    );
    ok(&home, &["config", "set", "client.parallel", "4"]);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("[all]\nrw = [\"box\"]\n"), "{text}");
}

#[test]
fn alias_set_through_config_renames_grants() {
    let d = tempfile::tempdir().unwrap();
    let id = peer_id(5);
    let (home, file) = home_with(&d, &format!("[all]\nrw = [\"{id}\"]\n"));
    ok(&home, &["config", "set", "hosts.box.endpoint_id", &id]);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("[all]\nrw = [\"box\"]\n"), "{text}");
}

#[test]
fn key_comment_stays_beside_the_alias() {
    let d = tempfile::tempdir().unwrap();
    let (home, file) = home_with(&d, "");
    let secret = iroh::SecretKey::from_bytes(&[5; 32]);
    let id = secret.public().to_string();
    ok(&home, &["host", "add", "box", &id]);
    let line = irsync::identity::public_key_line(&secret, "mike@laptop");
    ok(&home, &["grant", "add", "all", &line]);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("\"box\", # mike@laptop"), "{text}");
    assert_eq!(text.matches(&id).count(), 1, "{text}");
}

#[test]
fn host_remove_counts_a_hand_written_id_as_a_grant() {
    let d = tempfile::tempdir().unwrap();
    let id = peer_id(5);
    let before = format!(
        "[hosts.box]\nendpoint_id = \"{id}\"\n[all]\nrw = [\"{id}\"]\n[immediate]\nrw = [\"box\"]\n"
    );
    let (home, file) = home_with(&d, &before);
    let o = irsync(&home, &["host", "remove", "box"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(
        err.contains("named by grants in all, immediate") && err.contains("--force"),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
    let o = irsync(&home, &["host", "remove", "box", "-f"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert_eq!(
        err,
        format!("removed host box and 2 grants from {}\n", file.display())
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "[all]\nrw = []\n[immediate]\nrw = []\n"
    );
    // One grant: singular.
    let (home, file) = home_with(
        &d,
        &format!("[hosts.box]\nendpoint_id = \"{id}\"\n[all]\nro = [\"box\"]\n"),
    );
    let o = irsync(&home, &["host", "remove", "--force", "box"]);
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        format!("removed host box and 1 grant from {}\n", file.display())
    );
}

#[test]
fn config_unset_does_not_remove_a_host_that_grants_name() {
    let d = tempfile::tempdir().unwrap();
    let id = peer_id(5);
    let before = format!(
        "[hosts.box]\nendpoint_id = \"{id}\"\n[hosts.free]\nendpoint_id = \"{}\"\n[all]\nrw = [\"{id}\"]\n",
        peer_id(6)
    );
    let (home, file) = home_with(&d, &before);
    for key in ["hosts.box.endpoint_id", "hosts.box", "hosts"] {
        let o = irsync(&home, &["config", "unset", key]);
        let err = String::from_utf8_lossy(&o.stderr);
        assert_eq!(o.status.code(), Some(1), "{key}: {err}");
        assert!(
            err.contains("host \"box\" is named by grants in all")
                && err.contains("irsync host remove"),
            "{key}: {err}"
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before, "{key}");
    }
    // A host no grant names goes as before.
    ok(&home, &["config", "unset", "hosts.free"]);
    assert!(!std::fs::read_to_string(&file).unwrap().contains("free"));
}

#[test]
fn host_remove_on_a_file_that_does_not_load_gives_the_plain_error() {
    let d = tempfile::tempdir().unwrap();
    let (home, file) = home_with(&d, "[hosts.box]\nendpoint_id = 5\n");
    let plain = String::from_utf8_lossy(&irsync(&home, &["host", "list"]).stderr)
        .replace("irsync host list: ", "");
    assert!(plain.contains(&file.display().to_string()), "{plain}");
    for (args, cmd) in [
        (&["host", "remove", "box"][..], "host remove"),
        (&["config", "unset", "hosts.box"], "config unset"),
    ] {
        let o = irsync(&home, args);
        assert_eq!(o.status.code(), Some(1));
        let err = String::from_utf8_lossy(&o.stderr);
        assert_eq!(err, format!("irsync {cmd}: {plain}"), "{args:?}");
    }
}

#[test]
fn host_remove_leaves_an_id_grant_to_another_alias_of_the_same_id() {
    let d = tempfile::tempdir().unwrap();
    let id = peer_id(5);
    // `amy` sorts first, so a plain save would write the id as `amy`.
    let hosts = |names: &[&str]| -> String {
        names
            .iter()
            .map(|n| format!("[hosts.{n}]\nendpoint_id = \"{id}\"\n"))
            .collect()
    };
    let (home, file) = home_with(
        &d,
        &format!("{}[all]\nrw = [\"{id}\"]\n", hosts(&["amy", "zed"])),
    );
    // The id is not amy's alone: zed still names the peer.
    ok(&home, &["host", "remove", "amy"]);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        format!("{}[all]\nrw = [\"zed\"]\n", hosts(&["zed"]))
    );
    // A grant that names amy itself still stops the removal.
    std::fs::write(
        &file,
        format!("{}[all]\nrw = [\"amy\"]\n", hosts(&["amy", "zed"])),
    )
    .unwrap();
    let o = irsync(&home, &["host", "remove", "amy"]);
    assert_eq!(o.status.code(), Some(1));
    // The same through config unset.
    std::fs::write(
        &file,
        format!("{}[all]\nrw = [\"{id}\"]\n", hosts(&["amy", "zed"])),
    )
    .unwrap();
    ok(&home, &["config", "unset", "hosts.amy"]);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        format!("{}[all]\nrw = [\"zed\"]\n", hosts(&["zed"]))
    );
}

#[test]
fn host_add_says_when_grants_move_to_a_new_id() {
    let d = tempfile::tempdir().unwrap();
    let (old, new) = (peer_id(5), peer_id(6));
    let (home, _) = home_with(
        &d,
        &format!(
            "[hosts.box]\nendpoint_id = \"{old}\"\n[all]\nrw = [\"box\"]\n\
             [exports.p]\npath = \"/p\"\nro = [\"{old}\"]\n"
        ),
    );
    // The same id again, or another key of the host: nothing moves.
    let o = irsync(&home, &["host", "add", "box", &old, "--parallel", "2"]);
    assert!(o.status.success());
    assert_eq!(String::from_utf8_lossy(&o.stderr), "");
    let o = irsync(&home, &["host", "add", "box", &new]);
    assert!(o.status.success());
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        format!("box: {old} -> {new}\n")
    );
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        "irsync host add: note: the grants that name box (in all, exports.p) now apply to the new EndpointId\n"
    );
    // An alias no grant names moves without a note.
    ok(&home, &["host", "add", "free", &old]);
    let o = irsync(&home, &["host", "add", "free", &new]);
    assert_eq!(String::from_utf8_lossy(&o.stderr), "");
}

fn fresh_home() -> (tempfile::TempDir, std::path::PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    (d, home)
}

#[test]
fn verbose_before_a_management_command_runs_the_command() {
    let (_d, home) = fresh_home();
    // The first `id show` generates the client identity and says so.
    assert!(irsync(&home, &["id", "show"]).status.success());
    let plain = irsync(&home, &["id", "show"]);
    for args in [
        &["-v", "id", "show"][..],
        &["id", "show", "-v"],
        &["-q", "id", "show"],
    ] {
        let o = irsync(&home, args);
        assert!(o.status.success(), "{args:?}");
        assert_eq!(o.stdout, plain.stdout, "{args:?}");
        assert!(o.stderr.is_empty(), "{args:?}");
    }
    assert!(plain.stderr.is_empty());
    let o = irsync(&home, &["host", "list", "-v"]);
    assert!(o.status.success() && o.stderr.is_empty());
}

#[test]
fn config_before_a_command_names_the_file_the_command_uses() {
    let (_d, home) = fresh_home();
    let other = home.join("other.toml");
    let other_s = other.to_str().unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    assert!(
        irsync(&home, &["--config", other_s, "host", "add", "laptop", &id])
            .status
            .success()
    );
    assert!(std::fs::read_to_string(&other)
        .unwrap()
        .contains("[hosts.laptop]"));
    assert!(!home.join(".config/irsync/irsync.toml").exists());
    let before = irsync(&home, &["--config", other_s, "host", "list"]);
    let after = irsync(&home, &["host", "list", "--config", other_s]);
    assert!(before.status.success() && before.stdout == after.stdout);
    assert!(String::from_utf8_lossy(&before.stdout).contains("laptop"));
    assert!(!String::from_utf8_lossy(&irsync(&home, &["host", "list"]).stdout).contains("laptop"));
}

#[test]
fn vv_turns_logging_on_for_every_command() {
    let (_d, home) = fresh_home();
    for args in [
        &["gc", "-vv"][..],
        &["-vv", "id", "show"],
        &["host", "list", "-vv"],
        &["show", "-vv"],
    ] {
        let o = irsync(&home, args);
        assert!(o.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&o.stderr).contains("starting irsync"),
            "{args:?}"
        );
    }
    // stdout is the same with and without logging.
    assert_eq!(
        irsync(&home, &["-vv", "id", "show"]).stdout,
        irsync(&home, &["id", "show"]).stdout
    );
}

#[test]
fn one_v_on_gc_logs_nothing_and_quiet_drops_the_summary() {
    let (_d, home) = fresh_home();
    let o = irsync(&home, &["gc", "-v"]);
    assert!(o.status.success() && o.stderr.is_empty());
    assert!(String::from_utf8_lossy(&o.stdout).contains("0 part file(s)"));
    let o = irsync(&home, &["-q", "gc"]);
    assert!(o.status.success() && o.stdout.is_empty() && o.stderr.is_empty());
    let o = irsync(&home, &["gc", "-vv", "-q"]);
    assert!(o.stdout.is_empty() && o.stderr.is_empty());
}

#[test]
fn a_sync_option_before_a_command_exits_2() {
    let (_d, home) = fresh_home();
    let o = irsync(&home, &["-a", "gc"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr)
        .contains("-a is a sync option and cannot be used with 'gc'"));
}

/// A temp home with a passphrase file `pf` in it.
fn passphrase_home(d: &tempfile::TempDir, text: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let home = d.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let pf = home.join("pf");
    std::fs::write(&pf, text).unwrap();
    (home, pf)
}

#[test]
fn id_generate_encrypts_the_key_with_the_passphrase_file() {
    let d = tempfile::tempdir().unwrap();
    let (home, pf) = passphrase_home(&d, "hunter2\nnot part of it\n");
    let pf_s = pf.to_str().unwrap();
    let key = home.join(".config/irsync/id_ed25519");
    let gen = irsync(&home, &["id", "generate", "--passphrase-file", pf_s]);
    assert!(
        gen.status.success(),
        "{}",
        String::from_utf8_lossy(&gen.stderr)
    );
    assert!(ssh_key::PrivateKey::read_openssh_file(&key)
        .unwrap()
        .is_encrypted());
    assert!(String::from_utf8_lossy(&gen.stderr).contains("wrote "));
    // stdout: the public key line, then the id, as without the flag.
    let out = String::from_utf8_lossy(&gen.stdout).to_string();
    let id = out.lines().last().unwrap().to_string();
    assert_eq!(out.lines().count(), 2);
    assert!(out.starts_with("ssh-ed25519 "));

    // The same flag and file unlock it, before or after the command.
    for args in [
        &["id", "show", "--passphrase-file", pf_s][..],
        &["--passphrase-file", pf_s, "id", "show"],
    ] {
        let o = irsync(&home, args);
        assert!(o.status.success(), "{args:?}");
        assert!(String::from_utf8_lossy(&o.stdout).contains(&id), "{args:?}");
    }
    // Without a passphrase it does not load.
    let o = irsync(&home, &["id", "show"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("is encrypted"));
    // A wrong passphrase is refused.
    let wrong = home.join("wrong");
    std::fs::write(&wrong, "hunter3\n").unwrap();
    assert!(!irsync(
        &home,
        &["id", "show", "--passphrase-file", wrong.to_str().unwrap()]
    )
    .status
    .success());
    // A bare `id` with the flag and an existing key lists; it is not a usage error.
    let o = irsync(&home, &["id", "--passphrase-file", pf_s]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stdout).contains(&id));
}

#[test]
fn id_generate_encrypts_with_daemon_out_and_force() {
    let d = tempfile::tempdir().unwrap();
    let (home, pf) = passphrase_home(&d, "s3cret\r\n");
    let pf_s = pf.to_str().unwrap();
    let o = irsync(
        &home,
        &["id", "generate", "--daemon", "--passphrase-file", pf_s],
    );
    assert!(o.status.success());
    let daemon_key = home.join(".config/irsync/daemon_ed25519");
    assert!(ssh_key::PrivateKey::read_openssh_file(&daemon_key)
        .unwrap()
        .is_encrypted());
    assert!(irsync(
        &home,
        &["id", "show", "--daemon", "--passphrase-file", pf_s]
    )
    .status
    .success());

    let out = home.join("elsewhere/key");
    let out_s = out.to_str().unwrap();
    assert!(irsync(
        &home,
        &["--passphrase-file", pf_s, "id", "generate", "--out", out_s]
    )
    .status
    .success());
    let first = std::fs::read(&out).unwrap();
    assert!(ssh_key::PrivateKey::read_openssh_file(&out)
        .unwrap()
        .is_encrypted());
    // --force replaces it, encrypted again.
    assert!(irsync(
        &home,
        &[
            "id",
            "generate",
            "--out",
            out_s,
            "--force",
            "--passphrase-file",
            pf_s
        ]
    )
    .status
    .success());
    assert_ne!(std::fs::read(&out).unwrap(), first);
    assert!(ssh_key::PrivateKey::read_openssh_file(&out)
        .unwrap()
        .is_encrypted());
}

#[test]
fn id_generate_with_a_bad_passphrase_file_writes_and_removes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let (home, _pf) = passphrase_home(&d, "x\n");
    let key = home.join(".config/irsync/id_ed25519");
    let pub_key = home.join(".config/irsync/id_ed25519.pub");
    let empty = home.join("empty");
    std::fs::write(&empty, "").unwrap();
    let blank_first = home.join("blank");
    std::fs::write(&blank_first, "\nhunter2\n").unwrap();
    let missing = home.join("missing");
    for (f, msg) in [
        (&empty, "is empty"),
        (&blank_first, "is empty"),
        (&missing, "missing"),
    ] {
        let o = irsync(
            &home,
            &["id", "generate", "--passphrase-file", f.to_str().unwrap()],
        );
        assert_eq!(o.status.code(), Some(1), "{f:?}");
        assert!(String::from_utf8_lossy(&o.stderr).contains(msg), "{f:?}");
        assert!(!key.exists() && !pub_key.exists(), "{f:?}");
        assert!(o.stdout.is_empty());
    }
    // The error names the missing file once.
    let o = irsync(
        &home,
        &[
            "id",
            "generate",
            "--passphrase-file",
            missing.to_str().unwrap(),
        ],
    );
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(
        err.contains(&format!(
            "reading passphrase file: {}: No such file",
            missing.display()
        )),
        "{err}"
    );
    assert_eq!(err.matches(missing.to_str().unwrap()).count(), 1, "{err}");
    assert_eq!(err.matches("No such file").count(), 1, "{err}");
    // With --force the existing key survives a bad passphrase file.
    assert!(irsync(&home, &["id", "generate"]).status.success());
    let before = std::fs::read(&key).unwrap();
    let o = irsync(
        &home,
        &[
            "id",
            "generate",
            "--force",
            "--passphrase-file",
            empty.to_str().unwrap(),
        ],
    );
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(std::fs::read(&key).unwrap(), before);
    assert!(pub_key.exists());
}

#[test]
fn export_add_unlocks_an_encrypted_daemon_key_with_the_passphrase_file() {
    let d = tempfile::tempdir().unwrap();
    let (home, pf) = passphrase_home(&d, "hunter2\n");
    let pf_s = pf.to_str().unwrap();
    std::fs::create_dir_all(home.join("srv/p")).unwrap();
    let gen = irsync(
        &home,
        &["id", "generate", "--daemon", "--passphrase-file", pf_s],
    );
    assert!(
        gen.status.success(),
        "{}",
        String::from_utf8_lossy(&gen.stderr)
    );
    let show = irsync(
        &home,
        &["id", "show", "--daemon", "--passphrase-file", pf_s],
    );
    assert!(show.status.success());
    let show = String::from_utf8_lossy(&show.stdout).to_string();
    let id = show.lines().last().unwrap().trim().to_string();
    let target = format!("p:{}", home.join("srv/p").display());
    let o = irsync(
        &home,
        &["export", "add", &target, "--passphrase-file", pf_s],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), id);
}

#[test]
fn rust_log_overrides_the_level_the_flags_give() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let run = |rust_log: &str, args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_irsync"))
            .args(args)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", home.join("xdg"))
            .env("RUST_LOG", rust_log)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stderr).to_string()
    };
    assert!(run("info", &["gc"]).contains("starting irsync"));
    assert!(!run("error", &["gc", "-vv"]).contains("starting irsync"));
}

#[test]
fn only_id_generate_with_the_file_encrypts() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let key = home.join(".config/irsync/id_ed25519");
    // IRSYNC_PASSPHRASE alone never encrypts a new key (the helper removes
    // the variable, so build this Command by hand).
    let o = Command::new(env!("CARGO_BIN_EXE_irsync"))
        .args(["id", "generate"])
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join("xdg"))
        .env("IRSYNC_PASSPHRASE", "hunter2")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!ssh_key::PrivateKey::read_openssh_file(&key)
        .unwrap()
        .is_encrypted());
}

#[test]
fn commands_refuse_to_generate_a_key_on_the_side_with_a_passphrase_file() {
    let d = tempfile::tempdir().unwrap();
    // A fresh home with no key of any kind (no ~/.ssh/id_ed25519 either).
    let (home, pf) = passphrase_home(&d, "hunter2\r\n");
    let pf_s = pf.to_str().unwrap();
    let key = home.join(".config/irsync/id_ed25519");
    let pub_key = home.join(".config/irsync/id_ed25519.pub");
    let daemon_key = home.join(".config/irsync/daemon_ed25519");
    // Any valid endpoint id: the sync and the listing must fail on the key
    // before they dial.
    let other = d.path().join("other");
    let id = irsync::identity::generate_openssh(&other, "x")
        .unwrap()
        .public()
        .to_string();
    let src = format!("{}/", home.join("src").display());
    std::fs::create_dir_all(home.join("src")).unwrap();
    let dest = format!("{id}:photos/");
    let listing = format!("{id}:");
    let export = format!("photos:{}", home.join("src").display());
    let config = home.join(".config/irsync/irsync.toml");
    for args in [
        &["id", "show", "--passphrase-file", pf_s][..],
        &["--passphrase-file", pf_s, "id", "show"],
        &["id", "list", "--passphrase-file", pf_s],
        &["show", "--passphrase-file", pf_s],
        &["daemon", "--passphrase-file", pf_s],
        &["export", "add", &export, "--passphrase-file", pf_s],
        &[
            "ssh-setup",
            "--client",
            &id,
            "--client-name-hex",
            "6d79636c69656e74",
            "--passphrase-file",
            pf_s,
        ],
        &["--passphrase-file", pf_s, "-a", &src, &dest],
        &["--passphrase-file", pf_s, &listing],
    ] {
        let o = irsync(&home, args);
        assert_eq!(o.status.code(), Some(1), "{args:?}");
        let err = String::from_utf8_lossy(&o.stderr).to_string();
        assert!(
            err.contains("id generate --passphrase-file"),
            "{args:?}: {err}"
        );
        assert!(!String::from_utf8_lossy(&o.stdout).contains("endpoint id:"));
        assert!(
            !key.exists() && !pub_key.exists() && !daemon_key.exists(),
            "{args:?}"
        );
        // The daemon takes no lock and the export is not added: the
        // refusing commands write nothing at all.
        assert!(!home.join("xdg/irsync").exists(), "{args:?}");
        assert!(!home.join(".config").exists(), "{args:?}");
        assert!(!config.exists(), "{args:?}");
    }
    // The full form makes the protected key, and then the same commands work.
    assert!(
        irsync(&home, &["id", "generate", "--passphrase-file", pf_s])
            .status
            .success()
    );
    assert!(irsync(&home, &["id", "show", "--passphrase-file", pf_s])
        .status
        .success());

    // Without the flag a missing key is still generated, unencrypted
    // (second fresh home).
    let home2 = d.path().join("home2");
    let key2 = home2.join(".config/irsync/id_ed25519");
    let o = irsync(&home2, &["id", "show"]);
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("generated identity"));
    assert!(!ssh_key::PrivateKey::read_openssh_file(&key2)
        .unwrap()
        .is_encrypted());
}

#[test]
fn host_rename_renames_the_host_and_its_grants_in_the_file() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let cfg = home.join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public();
    // `[all]` names the host by its id and `[immediate]` by its alias;
    // the export also names `work`, which is no host yet.
    let text = format!(
        "[hosts.laptop]\nendpoint_id = \"{id}\"\n\
         [all]\nro = [\"{id}\"]\n\
         [immediate]\nrw = [\"laptop\"]\n\
         [exports.p]\npath = \"/p\"\nrw = [\"laptop\", \"work\"]\n"
    );
    std::fs::write(cfg.join("irsync.toml"), &text).unwrap();
    let o = irsync(&home, &["host", "rename", "laptop", "work"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        format!(
            "renamed host laptop to work; updated 3 grants in {}\n",
            cfg.join("irsync.toml").display()
        )
    );
    assert_eq!(
        std::fs::read_to_string(cfg.join("irsync.toml")).unwrap(),
        format!(
            "[hosts.work]\nendpoint_id = \"{id}\"\n\
             [all]\nro = [\"work\"]\n\
             [immediate]\nrw = [\"work\"]\n\
             [exports.p]\npath = \"/p\"\nrw = [\"work\"]\n"
        )
    );
    // A second rename of the old name fails and changes nothing.
    let before = std::fs::read_to_string(cfg.join("irsync.toml")).unwrap();
    let o = irsync(&home, &["host", "rename", "laptop", "other"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("no such entry \"laptop\""));
    assert_eq!(
        std::fs::read_to_string(cfg.join("irsync.toml")).unwrap(),
        before
    );
}

/// `irsync --help` names `sync` and `list` and keeps the transfer options;
/// each has its own help, `list` with its one operand.
#[test]
fn help_names_sync_and_list_and_each_has_its_own() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let top = irsync(&home, &["--help"]);
    assert!(top.status.success());
    let top = String::from_utf8_lossy(&top.stdout);
    assert!(
        top.contains("\n  sync ") && top.contains("\n  list "),
        "{top}"
    );
    assert!(
        top.contains("--archive"),
        "transfer options stay at the top: {top}"
    );
    for (cmd, want) in [("sync", "SRC... DEST"), ("list", "HOST[:PATH]")] {
        let o = irsync(&home, &[cmd, "--help"]);
        assert!(o.status.success(), "{cmd}");
        let h = String::from_utf8_lossy(&o.stdout);
        assert!(h.contains(want) && h.contains("--archive"), "{cmd}: {h}");
    }
    let o = irsync(&home, &["list", "--help"]);
    let h = String::from_utf8_lossy(&o.stdout);
    assert!(h.contains("irsync list [OPTIONS] <HOST[:PATH]>\n"), "{h}");
    assert!(!h.contains("several sources"), "{h}");
}

/// A `list` usage error shows the usage of `list`.
#[test]
fn list_usage_error_shows_list_usage() {
    let d = tempfile::tempdir().unwrap();
    let o = irsync(&d.path().join("home"), &["list", "./photos"]);
    assert_eq!(o.status.code(), Some(2));
    let e = String::from_utf8_lossy(&o.stderr);
    assert!(e.contains("list takes one operand"), "{e}");
    assert!(e.contains("irsync list [OPTIONS] <HOST[:PATH]>"), "{e}");
}

/// `host add` changes the daemon id. For a machine that also has a client
/// id, grants stay with that one, and the command says so instead of
/// claiming they moved.
#[test]
fn host_add_says_when_grants_stay_with_the_client_id() {
    let d = tempfile::tempdir().unwrap();
    let (daemon, client, new) = (peer_id(5), peer_id(6), peer_id(7));
    let (home, config) = home_with(
        &d,
        &format!(
            "[hosts.box]\nendpoint_id = \"{daemon}\"\nclient_id = \"{client}\"\n\
             [hosts.phone]\nclient_id = \"{client}\"\n[immediate]\nrw = [\"box\"]\n"
        ),
    );
    let o = irsync(&home, &["host", "add", "box", &new]);
    assert!(o.status.success());
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        format!("box: {daemon} -> {new}\n")
    );
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        format!(
            "irsync host add: note: the grants that name box (in immediate) stay with \
             its client_id {client}; `irsync config set hosts.box.client_id ID` changes that\n"
        )
    );
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(
        text.contains(&format!("client_id = \"{client}\"")),
        "{text}"
    );
    // A client-only host that no grant names gets its daemon id quietly.
    let o = irsync(&home, &["host", "add", "phone", &new]);
    assert_eq!(String::from_utf8_lossy(&o.stderr), "");
}

/// `config set` and `config unset` on a host's ids can change which id
/// its grants are for. That is never silent.
#[test]
fn config_says_when_a_hosts_grants_move_to_another_id() {
    let d = tempfile::tempdir().unwrap();
    let (daemon, client, new) = (peer_id(5), peer_id(6), peer_id(7));
    let (home, _) = home_with(
        &d,
        &format!(
            "[hosts.box]\nendpoint_id = \"{daemon}\"\nclient_id = \"{client}\"\n\
             [hosts.free]\nendpoint_id = \"{daemon}\"\nclient_id = \"{client}\"\n\
             [immediate]\nrw = [\"box\"]\n"
        ),
    );
    let note = |cmd: &str, from: &str, to: &str| {
        format!(
            "irsync config {cmd}: note: the grants that name box (in immediate) now apply \
             to {to} (was {from})\n"
        )
    };
    // The daemon id is not what grants go by here: no note.
    let other = peer_id(8);
    let o = irsync(&home, &["config", "set", "hosts.box.endpoint_id", &other]);
    assert!(o.status.success());
    assert_eq!(String::from_utf8_lossy(&o.stderr), "");
    let o = irsync(&home, &["config", "set", "hosts.box.client_id", &new]);
    assert!(o.status.success());
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        note("set", &client, &new)
    );
    // Without its client id, the grants fall to the daemon id.
    let o = irsync(&home, &["config", "unset", "hosts.box.client_id"]);
    assert!(o.status.success());
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        note("unset", &new, &other)
    );
    let o = irsync(&home, &["config", "set", "hosts.box.endpoint_id", &daemon]);
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        note("set", &other, &daemon)
    );
    // A host no grant names changes without a note.
    let o = irsync(&home, &["config", "unset", "hosts.free.client_id"]);
    assert!(o.status.success());
    assert_eq!(String::from_utf8_lossy(&o.stderr), "");
}

/// A host with a daemon id and a client id, and one that is a client
/// only, through `host list`, `show`, `host rename` and `host remove`.
#[test]
fn hosts_with_a_client_id_list_show_rename_and_remove() {
    let d = tempfile::tempdir().unwrap();
    let (daemon, client, phone) = (peer_id(5), peer_id(6), peer_id(7));
    let (home, config) = home_with(
        &d,
        &format!(
            "[hosts.desk]\nendpoint_id = \"{daemon}\"\nclient_id = \"{client}\"\n\
             [hosts.phone]\nclient_id = \"{phone}\"\n[immediate]\nrw = [\"desk\", \"phone\"]\n"
        ),
    );
    let list = ok(&home, &["host", "list"]);
    assert_eq!(
        list,
        format!(
            "desk: {daemon}\n    client_id = \"{client}\"\n\
             phone: (client only)\n    client_id = \"{phone}\"\n"
        )
    );
    // `show` has the same lines, and a grant is for the client id.
    let show = ok(&home, &["show"]);
    assert!(show.contains(&list), "{show}");
    let grants = ok(&home, &["grant", "list"]);
    assert!(
        grants.contains(&format!("rw: desk ({client}), phone ({phone})")),
        "{grants}"
    );

    // A rename takes both ids and the grants along.
    ok(&home, &["host", "rename", "desk", "box"]);
    let t: toml::Table = std::fs::read_to_string(&config).unwrap().parse().unwrap();
    assert!(t["hosts"].get("desk").is_none(), "{t}");
    assert_eq!(t["hosts"]["box"]["endpoint_id"].as_str(), Some(&*daemon));
    assert_eq!(t["hosts"]["box"]["client_id"].as_str(), Some(&*client));
    assert_eq!(t["immediate"]["rw"][0].as_str(), Some("box"), "{t}");

    // A client-only host is removed like any other: not while grants
    // name it, unless forced.
    let o = irsync(&home, &["host", "remove", "phone"]);
    assert!(!o.status.success());
    ok(&home, &["host", "remove", "phone", "-f"]);
    ok(&home, &["host", "remove", "box", "-f"]);
    let t: toml::Table = std::fs::read_to_string(&config).unwrap().parse().unwrap();
    assert!(
        t.get("hosts")
            .is_none_or(|h| h.as_table().unwrap().is_empty()),
        "{t}"
    );
    assert!(
        t.get("immediate")
            .and_then(|i| i.get("rw"))
            .is_none_or(|rw| rw.as_array().unwrap().is_empty()),
        "{t}"
    );
}
