mod common;

use std::path::Path;
use std::process::Stdio;

use common::{irsync_command, StateHome};

fn irsync(home: &Path, args: &[&str]) -> std::process::Output {
    irsync_command(home)
        .args(args)
        .state_home(home.join("xdg"))
        .env_remove("IRSYNC_PASSPHRASE")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn grant_remove_all_removes_a_name_that_left_the_clients_table() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("srv/p")).unwrap();
    let cfg = home.join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public();
    let host = format!("[clients.laptop]\nclient_id = \"{id}\"\n");
    irsync::fs::private::write(cfg.join("irsync.toml"), &host).unwrap();
    let target = format!("p:{}", home.join("srv/p").display());
    assert!(irsync(&home, &["export", "add", &target]).status.success());
    assert!(irsync(&home, &["grant", "add", "p", "laptop"])
        .status
        .success());
    // The client entry goes by hand; the grant still names it.
    let text = std::fs::read_to_string(cfg.join("irsync.toml")).unwrap();
    assert!(text.contains(&host), "{text}");
    irsync::fs::private::write(cfg.join("irsync.toml"), text.replace(&host, "")).unwrap();
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
        format!(
            "config_version = 2\n\n[exports.photos]\npath = {:?}\n",
            home.join("srv/photos")
        )
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
    assert_eq!(
        std::fs::read_to_string(&exports).unwrap().trim(),
        "config_version = 2"
    );
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
        irsync(&home, &["server", "add", "laptop", &id, "--parallel", "4"])
            .status
            .success()
    );
    assert!(irsync(&home, &["client", "add", "laptop", &id])
        .status
        .success());
    assert_eq!(
        std::fs::read_to_string(home.join(".config/irsync/irsync.toml")).unwrap(),
        format!(
            "config_version = 2\n\n[servers.laptop]\nendpoint_id = \"{id}\"\nparallel = 4\n\n\
             [clients.laptop]\nclient_id = \"{id}\"\n"
        )
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
    let o = irsync(&home, &["server", "list"]);
    let out = String::from_utf8(o.stdout).unwrap();
    assert!(
        out.starts_with(&format!("laptop: {id}\n")) && out.contains("parallel = 4"),
        "{out}"
    );
    let o = irsync(&home, &["server", "add", &id, &id]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("not a valid name"));
    // A server is no grant's: it goes at once. A client stays until the
    // grant is gone.
    let o = irsync(&home, &["server", "remove", "laptop"]);
    assert!(o.status.success());
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        format!(
            "removed server laptop from {}\n",
            home.join(".config/irsync/irsync.toml").display()
        )
    );
    let o = irsync(&home, &["client", "remove", "laptop"]);
    assert_eq!(o.status.code(), Some(1));
    ok(&home, &["grant", "remove", "p", "laptop"]);
    let o = irsync(&home, &["client", "remove", "laptop"]);
    assert!(o.status.success());
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        format!(
            "removed client laptop from {}\n",
            home.join(".config/irsync/irsync.toml").display()
        )
    );
    let text = std::fs::read_to_string(home.join(".config/irsync/irsync.toml")).unwrap();
    assert!(
        !text.contains("laptop") && text.contains("[exports.p]"),
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

/// `show` is read-only for the daemon's key, like `id show`: without
/// one it says so and makes none.
#[test]
fn show_never_creates_the_daemon_identity() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let o = irsync(&home, &["show"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("daemon identity: none"), "{out}");
    assert!(!home.join(".config/irsync/daemon_ed25519").exists());
    assert!(irsync(&home, &["id", "generate", "--daemon"])
        .status
        .success());
    let o = irsync(&home, &["id", "show", "--daemon"]);
    let daemon = String::from_utf8(o.stdout).unwrap();
    let daemon = daemon.lines().last().unwrap();
    let out = String::from_utf8(irsync(&home, &["show"]).stdout).unwrap();
    assert!(out.contains(&format!("daemon identity: {daemon}")), "{out}");
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
    irsync::fs::private::write(
        home.join(".config/irsync/irsync.toml"),
        format!("[defaults]\nidentity = {:?}\n", named.display().to_string()),
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
    assert!(irsync(&home, &["client", "add", "laptop", &id])
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
    irsync::fs::private::write(
        &config,
        text.replace("rw = [\"laptop\"]", "rw = [\"laptop\", \"ghost\"]"),
    )
    .unwrap();
    let o = irsync(&home, &["show"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8(o.stdout).unwrap();
    assert!(
        out.lines().any(|l| l == "servers:")
            && out.lines().any(|l| l == "clients:")
            && out.lines().any(|l| l == "exports:"),
        "{out}"
    );
    assert!(
        out.contains(&format!("laptop ({id})")) && out.contains("ghost (unknown client)"),
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
#[cfg(unix)]
fn an_unreadable_daemon_key_is_named_in_the_error() {
    use irsync::platform::testutil::set_mode;
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    assert!(irsync(&home, &["id", "generate", "--daemon"])
        .status
        .success());
    let key = home.join(".config/irsync/daemon_ed25519");
    set_mode(&key, 0o000).unwrap();
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
#[cfg(unix)]
fn id_list_names_a_daemon_key_it_cannot_read() {
    use irsync::platform::testutil::set_mode;
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    ok(&home, &["id", "generate", "--daemon"]);
    let key = home.join(".config/irsync/daemon_ed25519");
    set_mode(&key, 0o000).unwrap();
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
    let o = irsync(&home, &["grant", "add", "p", &evil]);
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
        vec!["client", "add", "laptop", &id],
        vec![
            "export",
            "add",
            &format!("p:{}", home.join("p").display()),
            "--read-only",
        ],
        vec!["export", "add", &format!("q:{}", home.join("q").display())],
        vec!["grant", "add", "p", "laptop"],
        vec!["client", "set", "laptop", "all_exports=ro"],
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
        vec!["client", "add", "laptop", &id],
        vec!["export", "add", &format!("p:{}", home.join("p").display())],
        vec![
            "export",
            "add",
            &format!("q:{}", home.join("q").display()),
            "--read-only",
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
        all.starts_with("all_exports:\nall_paths:\nadmin:\n")
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
            && err.contains("\"ghost\" is not an export, a client in")
            && err.contains("irsync.toml")
            && err.contains("EndpointId"),
        "{err}"
    );
}

#[test]
fn server_list_refuses_an_unknown_table() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let cfg = home.join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public();
    irsync::fs::private::write(
        cfg.join("irsync.toml"),
        format!("[home]\nendpoint_id = \"{id}\"\n"),
    )
    .unwrap();
    let o = irsync(&home, &["server", "list"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !o.status.success() && err.contains("unknown key \"home\""),
        "{err}"
    );
}

#[test]
fn grant_remove_all_all_empties_the_exports_but_keeps_client_rights() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("p")).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    for args in [
        vec!["client", "add", "laptop", &id],
        vec!["export", "add", &format!("p:{}", home.join("p").display())],
        vec!["grant", "add", "p", "laptop"],
        vec!["client", "set", "laptop", "all_exports=ro"],
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
    assert!(String::from_utf8_lossy(&o.stderr).contains("removed 1 entry"));
    // The client's own right is not a grant on an export: it stays.
    let o = irsync(&home, &["grant", "list", "laptop"]);
    assert_eq!(
        String::from_utf8(o.stdout).unwrap(),
        format!("laptop ({id}):\n    p: ro (all_exports)\n")
    );
}

#[test]
fn index_export_hashes_the_named_export_into_the_daemon_state() {
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
    irsync::fs::private::write(&config, format!("state_dir = {:?}\n{text}", state)).unwrap();
    // A dry run reads nothing and creates no index.
    let o = irsync(&home, &["index", "-n", "--export", "all"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8(o.stdout).unwrap();
    assert!(
        out.contains("2 file(s), 2 to hash (12288 bytes)"),
        "{out:?}"
    );
    let o = irsync(&home, &["index", "--export", "photos"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8(o.stdout).unwrap();
    assert!(out.contains("2 file(s), 2 hashed (12288 bytes)"), "{out:?}");
    assert!(std::fs::read_dir(state.join("index"))
        .unwrap()
        .next()
        .is_some());
    // A second run finds every row fresh.
    let o = irsync(&home, &["index", "--export", "all"]);
    let out = String::from_utf8(o.stdout).unwrap();
    assert!(out.contains("2 file(s), 0 hashed (0 bytes)"), "{out:?}");
    // `all` stands alone.
    let o = irsync(&home, &["index", "--export", "all", "--export", "photos"]);
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("irsync index: --export all names every export; give it alone"),
        "{err}"
    );
    // An unknown name fails and names the file.
    let o = irsync(&home, &["index", "--export", "music"]);
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("irsync index: no export \"music\" in"),
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
    irsync::fs::private::write(
        cfg.join("irsync.toml"),
        format!("[defaults]\nidentity = {:?}\n", other),
    )
    .unwrap();
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
fn server_list_prints_the_servers_section_of_show() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    ok(
        &home,
        &[
            "server",
            "add",
            "laptop",
            &id,
            "--relay",
            "https://r.example/",
            "--parallel",
            "4",
        ],
    );
    ok(&home, &["server", "add", "desk", &id]);
    let list = ok(&home, &["server", "list"]);
    assert_eq!(
        list,
        format!("laptop: {id}\n    relay = \"https://r.example/\"\n    parallel = 4\ndesk: {id}\n")
    );
    let show = ok(&home, &["show"]);
    let section: Vec<&str> = show
        .lines()
        .skip_while(|l| *l != "servers:")
        .skip(1)
        .take_while(|l| *l != "clients:")
        .collect();
    assert_eq!(list, section.join("\n") + "\n", "show:\n{show}");
}

#[test]
fn grant_list_prefers_an_export_over_an_alias_of_the_same_name() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("p")).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    ok(&home, &["client", "add", "p", &id]);
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
    ok(&home, &["client", "add", "laptop", &id]);
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
    ok(&home, &["client", "set", "laptop", "all_exports=ro"]);
    let list = ok(&home, &["grant", "list"]);
    let show = ok(&home, &["show"]);
    let show_lines: Vec<&str> = show.lines().collect();
    // The export blocks of `grant list` are in show line for line; the
    // blocks of the clients' rights are in the clients section instead.
    let mut grant_lines = 0;
    let mut in_rights = false;
    for line in list.lines() {
        if !line.starts_with("    ") {
            in_rights = line.starts_with("all_");
        } else if !in_rights {
            assert!(show_lines.contains(&line), "{line:?} not in show:\n{show}");
            grant_lines += 1;
        }
    }
    assert_eq!(grant_lines, 2, "{list}");
    assert!(
        list.starts_with(&format!(
            "all_exports:\n    ro: laptop ({id})\nall_paths:\nadmin:\n"
        )),
        "{list}"
    );
    assert!(
        show.contains(&format!("laptop: {id} all_exports=ro\n")),
        "{show}"
    );
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
    // The sections are headed by their names alone.
    assert!(
        out.lines().any(|l| l == "servers:")
            && out.lines().any(|l| l == "clients:")
            && out.lines().any(|l| l == "exports:"),
        "{out}"
    );
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
    assert_eq!(ok(&home, &["config", "get", "defaults.parallel"]), "128\n");
    assert_eq!(ok(&home, &["config", "get", "defaults.relay"]), "default\n");
    let o = irsync(&home, &["config", "get", "daemon.debug_addr"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(o.stdout.is_empty() && o.stderr.is_empty(), "{o:?}");
    let o = irsync(&home, &["config", "get", "daemon.nope"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("unknown key \"daemon.nope\""),
        "{o:?}"
    );
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    ok(&home, &["server", "add", "laptop", &id]);
    assert_eq!(
        ok(&home, &["config", "get", "servers.laptop.endpoint_id"]),
        format!("{id}\n")
    );
    assert_eq!(
        ok(&home, &["config", "get", "servers.laptop"]),
        format!("endpoint_id = \"{id}\"\n")
    );
    assert_eq!(ok(&home, &["config", "get", "config_version"]), "2\n");
    let o = irsync(&home, &["config", "get", "servers.desk"]);
    assert_eq!(o.status.code(), Some(1));
    let o = irsync(&home, &["config", "get", "hosts.laptop"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        "irsync config get: unknown key \"hosts.laptop\"\n"
    );
}

#[test]
fn set_with_a_bad_value_leaves_the_file_untouched() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let cfg = home.join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    let config = cfg.join("irsync.toml");
    irsync::fs::private::write(&config, "[defaults]\nparallel = 4\n").unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    for (args, needle) in [
        (
            &["config", "set", "servers.home.endpoint_id", "nope"][..],
            "servers.home.endpoint_id",
        ),
        (
            &["config", "set", "hosts.home.endpoint_id", &id],
            "hosts.home.endpoint_id",
        ),
        (
            &["config", "set", "exports.p.path", "./x"],
            "exports.p.path",
        ),
        (
            &["config", "set", "defaults.parallel", "x"],
            "defaults.parallel",
        ),
        (&["config", "set", "daemon.nope", "1"], "daemon.nope"),
        (
            &["config", "set", "servers.all.endpoint_id", &id],
            "servers.all.endpoint_id",
        ),
        (
            &["config", "set", "clients.all.client_id", &id],
            "clients.all.client_id",
        ),
        (&["config", "set", "servers.home", "1"], "servers.home"),
        (&["config", "set", "defaults.parallel"], "defaults.parallel"),
        (
            &["config", "unset", "defaults.reconnect"],
            "defaults.reconnect",
        ),
    ] {
        let o = irsync(&home, args);
        assert_eq!(o.status.code(), Some(1), "{args:?}");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(err.contains(needle), "{args:?}: {err}");
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "[defaults]\nparallel = 4\n",
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
    ok(&home, &["config", "set", "exports.p.ro"]);
    assert!(ok(&home, &["config", "get", "exports.p.ro"]).is_empty());
    assert!(std::fs::read_to_string(cfg.join("irsync.toml"))
        .unwrap()
        .contains("ro = []"));
    // The old table of grants for every export is no key, with a pointer.
    let o = irsync(&home, &["config", "set", "all.rw", &id]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        "irsync config set: unknown key \"all.rw\"\n"
    );
    ok(
        &home,
        &["config", "set", "daemon.listen", "127.0.0.1:1", "[::1]:1"],
    );
    assert_eq!(
        ok(&home, &["config", "get", "daemon.listen"]),
        "127.0.0.1:1\n[::1]:1\n"
    );
    ok(&home, &["config", "set", "defaults.reconnect", "2h"]);
    assert_eq!(ok(&home, &["config", "get", "defaults.reconnect"]), "2h\n");
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
    let o = irsync(&home, &["config", "set", "defaults.parallel", "4"]);
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("irsync.toml.lock"), "{err}");
    assert!(!cfg.join("irsync.toml").exists());
    std::fs::remove_file(cfg.join("irsync.toml.lock")).unwrap();
    ok(&home, &["config", "set", "defaults.parallel", "4"]);
    assert!(!cfg.join("irsync.toml.lock").exists());
    assert_eq!(ok(&home, &["config", "get", "defaults.parallel"]), "4\n");
}

#[test]
fn unset_of_the_last_key_leaves_no_empty_table() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    ok(&home, &["config", "set", "servers.laptop.endpoint_id", &id]);
    ok(&home, &["config", "unset", "servers.laptop.endpoint_id"]);
    let text = std::fs::read_to_string(home.join(".config/irsync/irsync.toml")).unwrap();
    assert!(!text.contains("servers"), "{text:?}");
    let o = irsync(&home, &["config", "set", "config_version", "3"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        "irsync config set: config_version is set by irsync\n"
    );
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
    irsync::fs::private::write(&config, format!("state_dir = {state:?}\n")).unwrap();
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
    irsync::fs::private::write(&config, format!("state_dir = {state:?}\n")).unwrap();
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
    ok(&home, &["client", "add", "laptop", &id]);
    ok(&home, &["server", "add", "laptop", &id]);
    ok(
        &home,
        &["export", "add", &format!("p:{}", home.join("p").display())],
    );
    ok(&home, &["grant", "add", "p", "laptop"]);
    // The client identity exists before the comparison, so neither run
    // prints "generated identity".
    ok(&home, &["id", "generate"]);
    for noun in ["id", "server", "client", "export", "grant", "config"] {
        let bare = irsync(&home, &[noun]);
        let list = irsync(&home, &[noun, "list"]);
        assert!(bare.status.success(), "{noun}");
        assert!(!list.stdout.is_empty(), "{noun}");
        assert_eq!(bare.stdout, list.stdout, "{noun}");
        assert_eq!(bare.stderr, list.stderr, "{noun}");
    }
    // The flag after a bare noun names the file.
    let other = d.path().join("other.toml");
    irsync::fs::private::write(&other, format!("[servers.nas]\nendpoint_id = \"{id}\"\n")).unwrap();
    let c = other.to_str().unwrap();
    assert_eq!(
        ok(&home, &["server", "--config", c]),
        ok(&home, &["server", "list", "--config", c])
    );
    assert!(ok(&home, &["server", "--config", c]).contains("nas"));
}

/// A bare noun only lists: it writes nothing, `--help` is still help, and
/// the arguments of `list` need `list` typed out.
#[test]
fn a_bare_noun_changes_nothing_and_takes_no_list_arguments() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    for noun in ["server", "client", "export", "grant", "config"] {
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
#[cfg(unix)]
fn id_list_names_a_daemon_key_in_a_directory_it_cannot_enter() {
    use irsync::platform::testutil::set_mode;
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let locked = d.path().join("locked");
    let key = locked.join("key");
    std::fs::create_dir_all(home.join(".config/irsync")).unwrap();
    irsync::fs::private::write(
        home.join(".config/irsync/irsync.toml"),
        format!("[daemon]\nidentity = \"{}\"\n", key.display()),
    )
    .unwrap();
    ok(&home, &["id", "generate", "--daemon"]);
    assert!(key.exists());
    set_mode(&locked, 0o000).unwrap();
    let skip = key.symlink_metadata().is_ok();
    let o = irsync(&home, &["id", "list"]);
    set_mode(&locked, 0o700).unwrap();
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
        irsync::fs::private::write(&file, text).unwrap();
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
    ok(&home, &["client", "add", "box", &id]);
    std::fs::create_dir_all(home.join("p")).unwrap();
    ok(
        &home,
        &["export", "add", &format!("p:{}", home.join("p").display())],
    );
    let o = irsync(&home, &["grant", "add", "p", &id]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert!(err.starts_with("box: rw on p"), "{err}");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("rw = [\"box\"]"), "{text}");
    let text = std::fs::read_to_string(&file).unwrap();
    assert_eq!(text.matches(&id).count(), 1, "only the client: {text}");
}

#[test]
fn host_add_renames_existing_grants() {
    let d = tempfile::tempdir().unwrap();
    let id = peer_id(5);
    let (home, file) = home_with(
        &d,
        &format!("[exports.p]\npath = \"/p\"\nrw = [\"{id}\"]\n"),
    );
    // A server is no client: the grant keeps the id.
    ok(&home, &["server", "add", "box", &id]);
    let text = std::fs::read_to_string(&file).unwrap();
    assert_eq!(
        text,
        format!(
            "config_version = 2\n\n[servers.box]\nendpoint_id = \"{id}\"\n\n[exports.p]\npath = \"/p\"\nrw = [\"{id}\"]\n"
        )
    );
    ok(&home, &["client", "add", "box", &id]);
    let text = std::fs::read_to_string(&file).unwrap();
    assert_eq!(
        text,
        format!(
            "config_version = 2\n\n[servers.box]\nendpoint_id = \"{id}\"\n\n\
             [clients.box]\nclient_id = \"{id}\"\n\n[exports.p]\npath = \"/p\"\nrw = [\"box\"]\n"
        )
    );
}

#[test]
fn unrelated_config_set_cleans_the_file() {
    let d = tempfile::tempdir().unwrap();
    let id = peer_id(5);
    let (home, file) = home_with(
        &d,
        &format!(
            "[clients.box]\nclient_id = \"{id}\"\n[exports.p]\npath = \"/p\"\nrw = [\"{id}\"]\n"
        ),
    );
    ok(&home, &["config", "set", "defaults.parallel", "4"]);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("[exports.p]\npath = \"/p\"\nrw = [\"box\"]\n"),
        "{text}"
    );
}

#[test]
fn alias_set_through_config_renames_grants() {
    let d = tempfile::tempdir().unwrap();
    let id = peer_id(5);
    let (home, file) = home_with(
        &d,
        &format!("[exports.p]\npath = \"/p\"\nrw = [\"{id}\"]\n"),
    );
    ok(&home, &["config", "set", "clients.box.client_id", &id]);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("[exports.p]\npath = \"/p\"\nrw = [\"box\"]\n"),
        "{text}"
    );
}

#[test]
fn key_comment_stays_beside_the_alias() {
    let d = tempfile::tempdir().unwrap();
    let (home, file) = home_with(&d, "");
    let secret = iroh::SecretKey::from_bytes(&[5; 32]);
    let id = secret.public().to_string();
    ok(&home, &["config", "set", "clients.box.client_id", &id]);
    let line = irsync::identity::public_key_line(&secret, "mike@laptop");
    std::fs::create_dir_all(home.join("p")).unwrap();
    ok(
        &home,
        &["export", "add", &format!("p:{}", home.join("p").display())],
    );
    ok(&home, &["grant", "add", "p", &line]);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("\"box\", # mike@laptop"), "{text}");
    assert_eq!(text.matches(&id).count(), 1, "{text}");
}

#[test]
fn client_remove_counts_a_hand_written_id_as_a_grant() {
    let d = tempfile::tempdir().unwrap();
    let id = peer_id(5);
    let before = format!(
        "[servers.box]\nendpoint_id = \"{id}\"\n[clients.box]\nclient_id = \"{id}\"\n\
         [exports.a]\npath = \"/a\"\nrw = [\"{id}\"]\n[exports.b]\npath = \"/b\"\nrw = [\"box\"]\n"
    );
    let (home, file) = home_with(&d, &before);
    let o = irsync(&home, &["client", "remove", "box"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(
        err.contains("named by grants in exports.a, exports.b") && err.contains("--force"),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
    let o = irsync(&home, &["client", "remove", "box", "-f"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert_eq!(
        err,
        format!("removed client box and 2 grants from {}\n", file.display())
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        format!("config_version = 2\n\n[servers.box]\nendpoint_id = \"{id}\"\n\n[exports.a]\npath = \"/a\"\nrw = []\n\n[exports.b]\npath = \"/b\"\nrw = []\n")
    );
    // One grant: singular.
    let (home, file) = home_with(
        &d,
        &format!(
            "[clients.box]\nclient_id = \"{id}\"\n[exports.p]\npath = \"/p\"\nro = [\"box\"]\n"
        ),
    );
    let o = irsync(&home, &["client", "remove", "--force", "box"]);
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        format!("removed client box and 1 grant from {}\n", file.display())
    );
}

#[test]
fn config_unset_does_not_remove_a_client_that_grants_name() {
    let d = tempfile::tempdir().unwrap();
    let id = peer_id(5);
    let before = format!(
        "[servers.box]\nendpoint_id = \"{id}\"\n\
         [clients.box]\nclient_id = \"{id}\"\n[clients.free]\nclient_id = \"{}\"\n\
         [exports.p]\npath = \"/p\"\nrw = [\"{id}\"]\n",
        peer_id(6)
    );
    let (home, file) = home_with(&d, &before);
    for key in ["clients.box.client_id", "clients.box", "clients"] {
        let o = irsync(&home, &["config", "unset", key]);
        let err = String::from_utf8_lossy(&o.stderr);
        assert_eq!(o.status.code(), Some(1), "{key}: {err}");
        assert!(
            err.contains("client \"box\" is named by grants in exports.p")
                && err.contains("irsync client remove box --force"),
            "{key}: {err}"
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before, "{key}");
    }
    // A client no grant names goes as before, and a server always does:
    // grants name clients only.
    ok(&home, &["config", "unset", "clients.free"]);
    assert!(!std::fs::read_to_string(&file).unwrap().contains("free"));
    ok(&home, &["config", "unset", "servers.box"]);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        !text.contains("[servers.box]") && text.contains("[clients.box]"),
        "{text}"
    );
}

#[test]
fn server_and_client_remove_on_a_file_that_does_not_load_give_the_plain_error() {
    let d = tempfile::tempdir().unwrap();
    let (home, file) = home_with(&d, "[servers.box]\nendpoint_id = 5\n");
    let plain = String::from_utf8_lossy(&irsync(&home, &["server", "list"]).stderr)
        .replace("irsync server list: ", "");
    assert!(plain.contains(&file.display().to_string()), "{plain}");
    for (args, cmd) in [
        (&["server", "remove", "box"][..], "server remove"),
        (&["client", "remove", "box"], "client remove"),
        (&["config", "unset", "clients.box"], "config unset"),
    ] {
        let o = irsync(&home, args);
        assert_eq!(o.status.code(), Some(1));
        let err = String::from_utf8_lossy(&o.stderr);
        assert_eq!(err, format!("irsync {cmd}: {plain}"), "{args:?}");
    }
}

#[test]
fn client_remove_leaves_an_id_grant_to_another_name_of_the_same_id() {
    let d = tempfile::tempdir().unwrap();
    let id = peer_id(5);
    // `amy` sorts first, so a plain save would write the id as `amy`.
    let hosts = |names: &[&str]| -> String {
        names
            .iter()
            .map(|n| format!("[clients.{n}]\nclient_id = \"{id}\"\n"))
            .collect()
    };
    let (home, file) = home_with(
        &d,
        &format!(
            "{}[exports.p]\npath = \"/p\"\nrw = [\"{id}\"]\n",
            hosts(&["amy", "zed"])
        ),
    );
    // The id is not amy's alone: zed still names the peer.
    ok(&home, &["client", "remove", "amy"]);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        format!(
            "config_version = 2\n\n{}\n[exports.p]\npath = \"/p\"\nrw = [\"zed\"]\n",
            hosts(&["zed"])
        )
    );
    // A grant that names amy itself still stops the removal.
    std::fs::write(
        &file,
        format!(
            "{}[exports.p]\npath = \"/p\"\nrw = [\"amy\"]\n",
            hosts(&["amy", "zed"])
        ),
    )
    .unwrap();
    let o = irsync(&home, &["server", "remove", "amy"]);
    assert_eq!(o.status.code(), Some(1));
    // The same through config unset.
    std::fs::write(
        &file,
        format!(
            "{}[exports.p]\npath = \"/p\"\nrw = [\"{id}\"]\n",
            hosts(&["amy", "zed"])
        ),
    )
    .unwrap();
    ok(&home, &["config", "unset", "clients.amy"]);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        format!(
            "config_version = 2\n\n{}\n[exports.p]\npath = \"/p\"\nrw = [\"zed\"]\n",
            hosts(&["zed"])
        )
    );
}

/// A machine that is a server and a client: a new daemon id moves the
/// server only, and the grants stay with the client.
#[test]
fn host_add_moves_the_server_and_not_the_grants() {
    let d = tempfile::tempdir().unwrap();
    let (old, new) = (peer_id(5), peer_id(6));
    let (home, _) = home_with(
        &d,
        &format!(
            "[servers.box]\nendpoint_id = \"{old}\"\n\
             [clients.box]\nclient_id = \"{old}\"\nall_exports = \"rw\"\n\
             [exports.p]\npath = \"/p\"\nro = [\"{old}\"]\n"
        ),
    );
    let o = irsync(&home, &["server", "add", "box", &old, "--parallel", "2"]);
    assert!(o.status.success());
    assert_eq!(String::from_utf8_lossy(&o.stderr), "");
    let o = irsync(&home, &["server", "add", "box", &new]);
    assert!(o.status.success());
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        format!("box: {old} -> {new}\n")
    );
    assert_eq!(String::from_utf8_lossy(&o.stderr), "");
    assert_eq!(
        ok(&home, &["grant", "list", "box"]),
        format!("box ({old}):\n    p: ro\n")
    );
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
    let o = irsync(&home, &["server", "list", "-v"]);
    assert!(o.status.success() && o.stderr.is_empty());
}

#[test]
fn config_before_a_command_names_the_file_the_command_uses() {
    let (_d, home) = fresh_home();
    let other = home.join("other.toml");
    let other_s = other.to_str().unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    assert!(irsync(
        &home,
        &["--config", other_s, "server", "add", "laptop", &id]
    )
    .status
    .success());
    assert!(std::fs::read_to_string(&other)
        .unwrap()
        .contains("[servers.laptop]"));
    assert!(!home.join(".config/irsync/irsync.toml").exists());
    let before = irsync(&home, &["--config", other_s, "server", "list"]);
    let after = irsync(&home, &["server", "list", "--config", other_s]);
    assert!(before.status.success() && before.stdout == after.stdout);
    assert!(String::from_utf8_lossy(&before.stdout).contains("laptop"));
    assert!(
        !String::from_utf8_lossy(&irsync(&home, &["server", "list"]).stdout).contains("laptop")
    );
}

#[test]
fn vv_turns_logging_on_for_every_command() {
    let (_d, home) = fresh_home();
    for args in [
        &["gc", "-vv"][..],
        &["-vv", "id", "show"],
        &["server", "list", "-vv"],
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
    irsync::fs::private::write(&pf, text).unwrap();
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
    irsync::fs::private::write(&wrong, "hunter3\n").unwrap();
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
    // --identity names the client key that id show prints.
    let shown = irsync(
        &home,
        &["id", "show", "--identity", out_s, "--passphrase-file", pf_s],
    );
    assert!(
        shown.status.success(),
        "{}",
        String::from_utf8_lossy(&shown.stderr)
    );
    let pub_line = std::fs::read_to_string(home.join("elsewhere/key.pub")).unwrap();
    assert!(
        String::from_utf8_lossy(&shown.stdout).contains(pub_line.trim()),
        "{}",
        String::from_utf8_lossy(&shown.stdout)
    );
    // A daemon key at a path of your own: its comment says whose it is.
    let dkey = home.join("elsewhere/daemon_key");
    let o = irsync(
        &home,
        &[
            "id",
            "generate",
            "--daemon",
            "--out",
            dkey.to_str().unwrap(),
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let line = std::fs::read_to_string(home.join("elsewhere/daemon_key.pub")).unwrap();
    assert!(line.trim_end().ends_with(" irsync-daemon"), "{line}");
}

#[test]
fn id_generate_with_a_bad_passphrase_file_writes_and_removes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let (home, _pf) = passphrase_home(&d, "x\n");
    let key = home.join(".config/irsync/id_ed25519");
    let pub_key = home.join(".config/irsync/id_ed25519.pub");
    let empty = home.join("empty");
    irsync::fs::private::write(&empty, "").unwrap();
    let blank_first = home.join("blank");
    irsync::fs::private::write(&blank_first, "\nhunter2\n").unwrap();
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
        let o = irsync_command(&home)
            .args(args)
            .state_home(home.join("xdg"))
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
    let o = irsync_command(&home)
        .args(["id", "generate"])
        .state_home(home.join("xdg"))
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
fn server_and_client_rename_take_their_grants_in_the_file() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let cfg = home.join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public();
    // Export `a` names the host by its id and `b` by its alias; `p` also
    // names `work`, which is no host yet.
    let text = format!(
        "[servers.laptop]\nendpoint_id = \"{id}\"\n[clients.laptop]\nclient_id = \"{id}\"\n\
         [exports.a]\npath = \"/a\"\nro = [\"{id}\"]\n\
         [exports.b]\npath = \"/b\"\nrw = [\"laptop\"]\n\
         [exports.p]\npath = \"/p\"\nrw = [\"laptop\", \"work\"]\n"
    );
    irsync::fs::private::write(cfg.join("irsync.toml"), &text).unwrap();
    let o = irsync(&home, &["server", "rename", "laptop", "work"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        format!(
            "renamed server laptop to work in {}\n",
            cfg.join("irsync.toml").display()
        )
    );
    let o = irsync(&home, &["client", "rename", "laptop", "work"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        format!(
            "renamed client laptop to work; updated 3 grants in {}\n",
            cfg.join("irsync.toml").display()
        )
    );
    assert_eq!(
        std::fs::read_to_string(cfg.join("irsync.toml")).unwrap(),
        format!(
            "config_version = 2\n\n[servers.work]\nendpoint_id = \"{id}\"\n\n\
             [clients.work]\nclient_id = \"{id}\"\n\n\
             [exports.a]\npath = \"/a\"\nro = [\"work\"]\n\n\
             [exports.b]\npath = \"/b\"\nrw = [\"work\"]\n\n\
             [exports.p]\npath = \"/p\"\nrw = [\"work\"]\n"
        )
    );
    // A second rename of the old name fails and changes nothing.
    let before = std::fs::read_to_string(cfg.join("irsync.toml")).unwrap();
    let o = irsync(&home, &["client", "rename", "laptop", "other"]);
    assert!(!o.status.success());
    let o = irsync(&home, &["server", "rename", "laptop", "other"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("no such entry \"laptop\""));
    assert_eq!(
        std::fs::read_to_string(cfg.join("irsync.toml")).unwrap(),
        before
    );
}

/// `export add` on an existing name updates it, as `server add` and
/// `client add` do; `export set` and `export rename` change one export.
#[test]
fn export_add_updates_and_set_and_rename_change_an_export() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    for dir in ["p", "q"] {
        std::fs::create_dir_all(home.join(dir)).unwrap();
    }
    let ok = |args: &[&str]| {
        let o = irsync(&home, args);
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stderr).into_owned()
    };
    let at = |dir: &str| format!("photos:{}", home.join(dir).display());
    ok(&["export", "add", &at("p"), "--read-only"]);
    let err = ok(&["export", "add", &at("q"), "--fsync"]);
    assert!(err.contains("updated export photos ("), "{err}");
    let get = |key: &str| {
        let o = irsync(&home, &["config", "get", &format!("exports.photos.{key}")]);
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    assert_eq!(get("path"), home.join("q").display().to_string());
    assert_eq!(
        (get("read_only"), get("fsync")),
        ("true".into(), "true".into())
    );
    let err = ok(&[
        "export",
        "set",
        "photos",
        "read_only=false",
        "index_at_start=true",
    ]);
    assert!(
        err.contains("photos: read_only=false index_at_start=true fsync=true"),
        "{err}"
    );
    ok(&["export", "rename", "photos", "pics"]);
    let o = irsync(&home, &["export", "list"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.starts_with("pics: "), "{out}");
    let o = irsync(&home, &["export", "set", "photos", "fsync=false"]);
    assert_eq!(o.status.code(), Some(1));
}

/// `-q` leaves out the lines that say what a management command did; what
/// it is run for (a list, an id) and errors still print.
#[test]
fn quiet_management_commands_print_only_results_and_errors() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("p")).unwrap();
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    let p = format!("p:{}", home.join("p").display());
    for args in [
        vec!["-q", "server", "add", "s", &id],
        vec!["server", "rename", "s", "t", "-q"],
        vec!["-q", "client", "add", "c", &id],
        vec!["-q", "client", "set", "c", "all_exports=ro"],
        vec!["-q", "grant", "add", "p", "c"],
        vec!["-q", "grant", "remove", "p", "c"],
        vec!["-q", "config", "set", "defaults.parallel", "3"],
        vec!["-q", "server", "remove", "t"],
    ] {
        if args.contains(&"grant") && args.contains(&"add") {
            let o = irsync(&home, &["-q", "export", "add", &p]);
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
            // A key made on the way is still said: it is a new file.
            let err = String::from_utf8_lossy(&o.stderr);
            assert!(
                err.lines().all(|l| l.contains("generated identity")),
                "{err}"
            );
            // The daemon's id is what export add is run for.
            assert_eq!(String::from_utf8_lossy(&o.stdout).trim().len(), 64);
        }
        let o = irsync(&home, &args);
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        assert!(
            o.stdout.is_empty() && o.stderr.is_empty(),
            "{args:?}: {o:?}"
        );
    }
    let o = irsync(&home, &["-q", "client", "list"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("c: "));
    let o = irsync(&home, &["-q", "server", "remove", "nobody"]);
    assert!(!o.status.success() && !o.stderr.is_empty());
}

/// `irsync --help` names `sync` and `list` and keeps the transfer options;
/// each has its own help, `list` with its one operand and its own flags.
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
    let o = irsync(&home, &["sync", "--help"]);
    assert!(o.status.success());
    let h = String::from_utf8_lossy(&o.stdout);
    assert!(h.contains("SRC... DEST") && h.contains("--archive"), "{h}");
    let o = irsync(&home, &["list", "--help"]);
    assert!(o.status.success());
    let h = String::from_utf8_lossy(&o.stdout);
    assert!(h.contains("irsync list [OPTIONS] <HOST[:PATH]>\n"), "{h}");
    assert!(h.contains("--no-recursive") && h.contains("--relay"), "{h}");
    assert!(
        !h.contains("several sources") && !h.contains("--archive"),
        "{h}"
    );
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

/// `host add` changes the daemon id, which no grant goes by: grants name
/// the client, and stay with it, with nothing to say.
#[test]
fn host_add_leaves_the_grants_with_the_client() {
    let d = tempfile::tempdir().unwrap();
    let (daemon, client, new) = (peer_id(5), peer_id(6), peer_id(7));
    let (home, config) = home_with(
        &d,
        &format!(
            "[servers.box]\nendpoint_id = \"{daemon}\"\n[clients.box]\nclient_id = \"{client}\"\n\
             [clients.phone]\nclient_id = \"{client}\"\n[exports.p]\npath = \"/p\"\nrw = [\"box\"]\n"
        ),
    );
    let o = irsync(&home, &["server", "add", "box", &new]);
    assert!(o.status.success());
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        format!("box: {daemon} -> {new}\n")
    );
    assert_eq!(String::from_utf8_lossy(&o.stderr), "");
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(
        text.contains(&format!("client_id = \"{client}\"")),
        "{text}"
    );
    // A client that no grant names gets a server quietly.
    let o = irsync(&home, &["server", "add", "phone", &new]);
    assert_eq!(String::from_utf8_lossy(&o.stderr), "");
}

/// `config set` and `config unset` on a client's id can change which id
/// its grants are for. That is never silent; a server's id is no grant's.
#[test]
fn config_says_when_a_clients_grants_move_to_another_id() {
    let d = tempfile::tempdir().unwrap();
    let (daemon, client, new) = (peer_id(5), peer_id(6), peer_id(7));
    let (home, file) = home_with(
        &d,
        &format!(
            "[servers.box]\nendpoint_id = \"{daemon}\"\n[clients.box]\nclient_id = \"{client}\"\n\
             [clients.free]\nclient_id = \"{client}\"\n\
             [exports.p]\npath = \"/p\"\nrw = [\"box\"]\n"
        ),
    );
    let note = |cmd: &str, from: &str, to: &str| {
        format!(
            "irsync config {cmd}: note: the grants that name box (in exports.p) now apply \
             to {to} (was {from})\n"
        )
    };
    let other = peer_id(8);
    let o = irsync(&home, &["config", "set", "servers.box.endpoint_id", &other]);
    assert!(o.status.success());
    assert_eq!(String::from_utf8_lossy(&o.stderr), "");
    let o = irsync(&home, &["config", "set", "clients.box.client_id", &new]);
    assert!(o.status.success());
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        note("set", &client, &new)
    );
    // Without its id the client is gone: not while grants name it.
    let before = std::fs::read_to_string(&file).unwrap();
    let o = irsync(&home, &["config", "unset", "clients.box.client_id"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&o.stderr)
            .contains("client \"box\" is named by grants in exports.p"),
        "{o:?}"
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
    // A client no grant names changes without a note.
    let o = irsync(&home, &["config", "unset", "clients.free.client_id"]);
    assert!(o.status.success());
    assert_eq!(String::from_utf8_lossy(&o.stderr), "");
}

/// A machine that is a server and a client, and one that is a client
/// only, through the `server` and `client` commands and `show`.
#[test]
fn servers_and_clients_list_show_rename_and_remove() {
    let d = tempfile::tempdir().unwrap();
    let (daemon, client, phone) = (peer_id(5), peer_id(6), peer_id(7));
    let (home, config) = home_with(
        &d,
        &format!(
            "[servers.desk]\nendpoint_id = \"{daemon}\"\n[clients.desk]\nclient_id = \"{client}\"\nall_paths = \"rw\"\n\
             [clients.phone]\nclient_id = \"{phone}\"\nall_paths = \"rw\"\n"
        ),
    );
    let servers = ok(&home, &["server", "list"]);
    assert_eq!(servers, format!("desk: {daemon}\n"));
    let clients = ok(&home, &["client", "list"]);
    assert_eq!(
        clients,
        format!("desk: {client} all_paths=rw\nphone: {phone} all_paths=rw\n")
    );
    // `show` has the same lines, and a grant is for the client id.
    let show = ok(&home, &["show"]);
    assert!(
        show.contains(&format!("servers:\n{servers}clients:\n{clients}exports:")),
        "{show}"
    );
    let grants = ok(&home, &["grant", "list"]);
    assert!(
        grants.contains(&format!("rw: desk ({client}), phone ({phone})")),
        "{grants}"
    );

    // A rename takes the server, the client and the grants along.
    ok(&home, &["server", "rename", "desk", "box"]);
    ok(&home, &["client", "rename", "desk", "box"]);
    let t: toml::Table = std::fs::read_to_string(&config).unwrap().parse().unwrap();
    assert!(t["servers"].get("desk").is_none(), "{t}");
    assert!(t["clients"].get("desk").is_none(), "{t}");
    assert_eq!(t["servers"]["box"]["endpoint_id"].as_str(), Some(&*daemon));
    assert_eq!(t["clients"]["box"]["client_id"].as_str(), Some(&*client));
    assert_eq!(t["clients"]["box"]["all_paths"].as_str(), Some("rw"), "{t}");

    // A client's rights go with it; they never hold the removal back.
    ok(&home, &["client", "remove", "phone"]);
    ok(&home, &["client", "remove", "box"]);
    ok(&home, &["server", "remove", "box"]);
    let t: toml::Table = std::fs::read_to_string(&config).unwrap().parse().unwrap();
    for table in ["servers", "clients"] {
        assert!(
            t.get(table)
                .is_none_or(|h| h.as_table().unwrap().is_empty()),
            "{t}"
        );
    }
}

/// `client add` sets the id a machine has as a client, beside its
/// server's id, and says so when that moves the grants naming it.
#[test]
fn client_add_sets_the_id_and_says_when_grants_move() {
    let d = tempfile::tempdir().unwrap();
    let (daemon, client, other) = (peer_id(5), peer_id(6), peer_id(7));
    let (home, config) = home_with(&d, "");
    ok(&home, &["server", "add", "box", &daemon]);
    let o = irsync(&home, &["client", "add", "box", &client]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        format!("box: {client}\n")
    );
    assert_eq!(String::from_utf8_lossy(&o.stderr), "");
    let t: toml::Table = std::fs::read_to_string(&config).unwrap().parse().unwrap();
    assert_eq!(t["servers"]["box"]["endpoint_id"].as_str(), Some(&*daemon));
    assert_eq!(t["clients"]["box"]["client_id"].as_str(), Some(&*client));

    // Grants name it: a new client id moves them, and the command says so.
    std::fs::create_dir_all(home.join("p")).unwrap();
    ok(
        &home,
        &["export", "add", &format!("p:{}", home.join("p").display())],
    );
    ok(&home, &["grant", "add", "p", "box"]);
    let o = irsync(&home, &["client", "add", "box", &other]);
    assert!(o.status.success());
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        format!("box: {client} -> {other}\n")
    );
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        format!(
            "irsync client add: note: the grants that name box (in exports.p) now apply to \
             {other} (was {client})\n"
        )
    );
    // The same id again: nothing moved, nothing said.
    let o = irsync(&home, &["client", "add", "box", &other]);
    assert_eq!(String::from_utf8_lossy(&o.stderr), "");
    // Not an id.
    let o = irsync(&home, &["client", "add", "box", "nope"]);
    assert!(!o.status.success());
    let e = String::from_utf8_lossy(&o.stderr);
    assert!(e.contains("ID \"nope\""), "{e}");
}

/// Every command reads the config file first and stops at once when other
/// users can read or change it: nothing is made, changed or contacted.
#[test]
#[cfg(unix)]
fn a_config_open_to_other_users_stops_every_command_at_once() {
    use irsync::platform::testutil::set_mode;
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let cfg = home.join(".config/irsync");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::create_dir_all(home.join("src")).unwrap();
    let file = cfg.join("irsync.toml");
    let id = "6e7a1cdd29b0b78fd13af4c5598feff4ef2a97166e3ca6f2e4fbfccd80505bf1";
    let text = format!("[servers.nas]\nendpoint_id = \"{id}\"\n");
    std::fs::write(&file, &text).unwrap();
    set_mode(&file, 0o644).unwrap();
    let src = home.join("src");
    let src = src.to_str().unwrap();
    let commands: &[&[&str]] = &[
        &[src, "nas:x/"],
        &["nas:x/", src],
        &["list", "nas:"],
        &["daemon", "--listen", "127.0.0.1:0"],
        &["server", "list"],
        &["server", "add", "box", id],
        &["export", "add", src],
        &["client", "set", "nas", "all_exports=rw"],
        &["config", "get", "defaults.parallel"],
        &["config", "set", "defaults.parallel", "4"],
        &["config", "set", "unsafe_config_allowed", "true"],
        &["id", "show"],
        &["id", "list"],
        &["id", "generate"],
        &["show"],
        &["index", src],
        &["gc"],
    ];
    for args in commands {
        let o = irsync(&home, args);
        let err = String::from_utf8_lossy(&o.stderr);
        assert_eq!(o.status.code(), Some(1), "{args:?}: {err}");
        assert!(err.contains("has mode 0644"), "{args:?}: {err}");
        assert!(err.contains("chmod 600"), "{args:?}: {err}");
        assert!(o.stdout.is_empty(), "{args:?}");
        // Nothing was written: no key, no lock, no state, the file as it was.
        let mut left: Vec<_> = std::fs::read_dir(&cfg)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        left.sort();
        assert_eq!(left, ["irsync.toml"], "{args:?}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text, "{args:?}");
        assert!(!home.join("xdg").exists(), "{args:?}");
    }
    // The file's owner can say that this is meant.
    std::fs::write(&file, format!("unsafe_config_allowed = true\n{text}")).unwrap();
    assert!(ok(&home, &["server", "list"]).contains("nas"));
    ok(&home, &["config", "set", "defaults.parallel", "4"]);
    assert_eq!(
        ok(&home, &["config", "get", "unsafe_config_allowed"]),
        "true\n"
    );
    // Closing the file works as well, and the key is then not needed.
    std::fs::write(&file, &text).unwrap();
    set_mode(&file, 0o600).unwrap();
    assert!(ok(&home, &["server", "list"]).contains("nas"));
    assert_eq!(
        ok(&home, &["config", "get", "unsafe_config_allowed"]),
        "false\n"
    );
}

/// A private key or a passphrase file that other users can read stops the
/// command that needs it, unless the config allows it.
#[test]
#[cfg(unix)]
fn a_key_or_passphrase_file_open_to_other_users_stops_the_command() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let pf = home.join("pf");
    irsync::fs::private::write(&pf, "hunter2\n").unwrap();
    let pf_s = pf.to_str().unwrap();
    let chmod = |p: &Path, mode: u32| irsync::platform::testutil::set_mode(p, mode).unwrap();
    let cfg = home.join(".config/irsync");
    let key = cfg.join("id_ed25519");
    let config = cfg.join("irsync.toml");
    let fails = |args: &[&str], what: &str| {
        let o = irsync(&home, args);
        let err = String::from_utf8_lossy(&o.stderr);
        assert_eq!(o.status.code(), Some(1), "{args:?}: {err}");
        assert!(err.contains(what), "{args:?}: {err}");
        assert!(err.contains("chmod 600"), "{args:?}: {err}");
    };

    // An open passphrase file makes no key.
    chmod(&pf, 0o644);
    fails(
        &["id", "generate", "--passphrase-file", pf_s],
        "the passphrase file",
    );
    assert!(!key.exists());
    chmod(&pf, 0o600);
    ok(&home, &["id", "generate", "--passphrase-file", pf_s]);
    let shown = ok(&home, &["id", "show", "--passphrase-file", pf_s]);

    // Nor does it unlock one.
    chmod(&pf, 0o640);
    fails(
        &["id", "show", "--passphrase-file", pf_s],
        "the passphrase file",
    );
    chmod(&pf, 0o600);

    // An open key is not used.
    chmod(&key, 0o644);
    fails(&["id", "show", "--passphrase-file", pf_s], "a private key");
    fails(&["show", "--passphrase-file", pf_s], "a private key");

    // With the config's word, both are.
    irsync::fs::private::write(&config, "unsafe_config_allowed = true\n").unwrap();
    chmod(&pf, 0o644);
    assert_eq!(ok(&home, &["id", "show", "--passphrase-file", pf_s]), shown);
}

#[test]
fn server_commands_print_and_report() {
    let d = tempfile::tempdir().unwrap();
    let (home, file) = home_with(&d, "");
    let (a, b) = (peer_id(5), peer_id(6));
    assert_eq!(
        ok(&home, &["server", "add", "box", &a]),
        format!("box: {a}\n")
    );
    assert_eq!(
        ok(&home, &["server", "add", "box", &b]),
        format!("box: {a} -> {b}\n")
    );
    // A server and a client may have the same name.
    ok(&home, &["client", "add", "box", &a]);
    assert_eq!(ok(&home, &["server", "list"]), format!("box: {b}\n"));
    let o = irsync(&home, &["server", "rename", "box", "crate"]);
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        format!("renamed server box to crate in {}\n", file.display())
    );
    let o = irsync(&home, &["server", "remove", "crate"]);
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        format!("removed server crate from {}\n", file.display())
    );
    let o = irsync(&home, &["server", "remove", "crate"]);
    assert_eq!(o.status.code(), Some(1));
    // The client of that name is untouched.
    assert_eq!(ok(&home, &["client", "list"]), format!("box: {a}\n"));
}

#[test]
fn client_commands_print_and_report() {
    let d = tempfile::tempdir().unwrap();
    let (home, file) = home_with(&d, "");
    let (a, b) = (peer_id(5), peer_id(6));
    assert_eq!(
        ok(&home, &["client", "add", "amy", &a]),
        format!("amy: {a}\n")
    );
    assert_eq!(
        ok(&home, &["client", "add", "amy", &b]),
        format!("amy: {a} -> {b}\n")
    );
    std::fs::create_dir_all(home.join("p")).unwrap();
    ok(
        &home,
        &["export", "add", &format!("p:{}", home.join("p").display())],
    );
    ok(&home, &["grant", "add", "p", "amy"]);
    let o = irsync(&home, &["client", "rename", "amy", "bob"]);
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        format!(
            "renamed client amy to bob; updated 1 grant in {}\n",
            file.display()
        )
    );
    let o = irsync(&home, &["client", "remove", "bob", "-f"]);
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        format!("removed client bob and 1 grant from {}\n", file.display())
    );
    assert_eq!(ok(&home, &["client", "list"]), "");
}

#[test]
fn grant_add_names_a_server_only_name() {
    let d = tempfile::tempdir().unwrap();
    let (home, _) = home_with(&d, "");
    ok(&home, &["server", "add", "box", &peer_id(5)]);
    std::fs::create_dir_all(home.join("p")).unwrap();
    ok(
        &home,
        &["export", "add", &format!("p:{}", home.join("p").display())],
    );
    let o = irsync(&home, &["grant", "add", "p", "box"]);
    assert_eq!(o.status.code(), Some(1));
    let e = String::from_utf8_lossy(&o.stderr);
    assert!(e.contains("box") && e.contains("client"), "{e}");
}

/// `server add --pq-key-exchange` sets the server's key, and a later
/// `server add` without the flag keeps it.
#[test]
fn server_add_sets_pq_key_exchange() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let id = iroh::SecretKey::from_bytes(&[5; 32]).public().to_string();
    ok(
        &home,
        &["server", "add", "box", &id, "--pq-key-exchange", "require"],
    );
    let key = ["config", "get", "servers.box.pq_key_exchange"];
    assert_eq!(ok(&home, &key), "require\n");
    ok(&home, &["server", "add", "box", &id, "--parallel", "2"]);
    assert_eq!(ok(&home, &key), "require\n");
    ok(
        &home,
        &["server", "add", "box", &id, "--pq-key-exchange", "prefer"],
    );
    assert_eq!(ok(&home, &key), "prefer\n");
    let o = irsync(
        &home,
        &[
            "server",
            "add",
            "box",
            &id,
            "--pq-key-exchange",
            "sometimes",
        ],
    );
    assert!(
        !o.status.success()
            && String::from_utf8_lossy(&o.stderr).contains("expected prefer or require"),
        "{o:?}"
    );
}

#[test]
fn client_set_sets_reports_and_lists_the_rights() {
    let d = tempfile::tempdir().unwrap();
    let (home, file) = home_with(&d, "");
    ok(&home, &["client", "add", "laptop", &peer_id(5)]);
    let o = irsync(
        &home,
        &["client", "set", "laptop", "all_exports=rw", "all_paths=ro"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        "laptop: all_exports=rw all_paths=ro admin=none\n"
    );
    assert_eq!(
        ok(&home, &["client", "list"]),
        format!("laptop: {} all_exports=rw all_paths=ro\n", peer_id(5))
    );
    let o = irsync(&home, &["client", "set", "laptop", "all_paths=none"]);
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        "laptop: all_exports=rw all_paths=none admin=none\n"
    );
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(!text.contains("all_paths"), "{text}");
    let o = irsync(&home, &["client", "set", "ghost", "all_paths=rw"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("ghost"));
}

#[test]
fn client_set_with_a_bad_pair_changes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let (home, file) = home_with(&d, "");
    ok(&home, &["client", "add", "laptop", &peer_id(5)]);
    let before = std::fs::read(&file).unwrap();
    for pair in ["all_paths=yes", "admin=yes", "all_paths"] {
        let o = irsync(&home, &["client", "set", "laptop", "all_exports=rw", pair]);
        assert!(!o.status.success(), "{pair}");
        assert_eq!(std::fs::read(&file).unwrap(), before, "{pair}");
    }
}

#[test]
fn grant_remove_names_the_rights_the_client_still_holds() {
    let d = tempfile::tempdir().unwrap();
    let (home, file) = home_with(&d, "");
    ok(&home, &["client", "add", "laptop", &peer_id(5)]);
    ok(&home, &["client", "add", "phone", &peer_id(6)]);
    ok(
        &home,
        &["client", "set", "laptop", "all_exports=rw", "all_paths=ro"],
    );
    ok(&home, &["client", "set", "phone", "all_paths=rw"]);
    let dir = d.path().join("x");
    std::fs::create_dir(&dir).unwrap();
    ok(&home, &["export", "add", &format!("x:{}", dir.display())]);
    ok(&home, &["grant", "add", "x", "laptop"]);
    let o = irsync(&home, &["grant", "remove", "x", "laptop"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert!(
        err.contains("laptop still has all_exports=rw; remove it with: irsync client set laptop all_exports=none")
            && err.contains("laptop still has all_paths=ro; remove it with: irsync client set laptop all_paths=none"),
        "{err}"
    );
    // Nothing left to remove: the note comes with the error.
    let before = std::fs::read(&file).unwrap();
    let o = irsync(&home, &["grant", "remove", "all", "laptop"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "{err}");
    assert!(err.contains("laptop still has all_exports=rw"), "{err}");
    assert!(!err.contains("phone still has"), "{err}");
    assert_eq!(std::fs::read(&file).unwrap(), before);
    // All peers: every client that holds a right.
    let o = irsync(&home, &["grant", "remove", "all", "all"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("laptop still has all_exports=rw")
            && err.contains("phone still has all_paths=rw"),
        "{err}"
    );
    // Once the rights are gone, no note.
    ok(
        &home,
        &[
            "client",
            "set",
            "laptop",
            "all_exports=none",
            "all_paths=none",
        ],
    );
    let o = irsync(&home, &["grant", "remove", "all", "laptop"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!err.contains("still has"), "{err}");
}

#[test]
fn grant_add_all_points_to_client_set() {
    let d = tempfile::tempdir().unwrap();
    let (home, file) = home_with(&d, "");
    ok(&home, &["client", "add", "laptop", &peer_id(5)]);
    let before = std::fs::read(&file).unwrap();
    let o = irsync(&home, &["grant", "add", "all", "laptop"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !o.status.success() && err.contains("irsync client set NAME all_exports=rw"),
        "{err}"
    );
    let o = irsync(&home, &["grant", "add", "all", "laptop", "--ro"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !o.status.success() && err.contains("irsync client set NAME all_exports=ro"),
        "{err}"
    );
    assert_eq!(std::fs::read(&file).unwrap(), before);
}

#[test]
fn grant_list_shows_client_rights() {
    let d = tempfile::tempdir().unwrap();
    let (home, _) = home_with(&d, "");
    let id = peer_id(5);
    ok(&home, &["client", "add", "laptop", &id]);
    ok(&home, &["client", "set", "laptop", "all_paths=rw"]);
    let list = ok(&home, &["grant", "list"]);
    assert!(
        list.contains(&format!("all_paths:\n    rw: laptop ({id})\n")),
        "{list}"
    );
    assert_eq!(
        ok(&home, &["grant", "list", "all_paths"]),
        format!("all_paths:\n    rw: laptop ({id})\n")
    );
    assert_eq!(
        ok(&home, &["grant", "list", "laptop"]),
        format!("laptop ({id}):\n    all_paths: rw\n")
    );
}
