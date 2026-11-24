use irsync::config::reader;
use irsync::config::writer::write;
use irsync::manage::{load_message, lock_config, save_config, ManageError, PeerSpec};
use irsync_proto::config as pb;
use std::path::Path;

fn id(n: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
}

fn msg(text: &str) -> pb::Config {
    reader::read(text, Path::new("irsync.toml")).unwrap().config
}

/// A `state_dir` line for a config in `dir`: a save then looks for a
/// running daemon in a state folder of the test's own, never in the default
/// one of whoever runs the tests (#275).
fn state_line(dir: &Path) -> String {
    format!("state_dir = {:?}\n", state(dir))
}

fn state(dir: &Path) -> String {
    dir.join("state").display().to_string()
}

fn export(name: &str, path: &str) -> pb::Export {
    pb::Export {
        name: name.into(),
        path: path.into(),
        ..Default::default()
    }
}

/// A server without its id: the message holds it, the config refuses it.
fn server_without_id(name: &str) -> pb::ServerEntry {
    pb::ServerEntry {
        name: name.into(),
        ..Default::default()
    }
}

#[test]
fn load_missing_is_empty_and_save_creates_parent_atomically() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("sub/irsync.toml");
    let mut m = load_message(&p).unwrap();
    assert_eq!(m, pb::Config::default());
    m.state_dir = Some(state(d.path()));
    m.exports.push(export("x", "/x"));
    save_config(&p, &mut m).unwrap();
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        format!(
            "config_version = 2\n{}\n[exports.x]\npath = \"/x\"\n",
            state_line(d.path())
        )
    );
    assert!(
        std::fs::read_dir(p.parent().unwrap()).unwrap().count() == 1,
        "no temp file or lock left behind"
    );
}

#[test]
fn save_preserves_comments_and_order() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("e.toml");
    irsync::fs::private::write(
        &p,
        format!(
            "{}# top\n[exports.b]\npath = \"/b\" # keep\n\n[exports.a]\npath = \"/a\"\n",
            state_line(d.path())
        ),
    )
    .unwrap();
    let mut m = load_message(&p).unwrap();
    m.exports[1].read_only = Some(true);
    save_config(&p, &mut m).unwrap();
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        format!(
            "config_version = 2\n{}\n# top\n[exports.b]\npath = \"/b\" # keep\n\n[exports.a]\npath = \"/a\"\nread_only = true\n",
            state_line(d.path())
        )
    );
}

#[test]
fn a_save_keeps_comments_and_writes_the_writers_form() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("irsync.toml");
    irsync::fs::private::write(
        &path,
        format!(
            "{}[exports.p]\npath=\"/p\"\nrw=[]   # nobody yet\n\n\n# the daemon\n[daemon]\nportmap=false\n",
            state_line(dir.path())
        ),
    )
    .unwrap();
    let mut lock = lock_config(&path).unwrap();
    let mut m = lock.load().unwrap();
    lock.save(&mut m).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!(
            "config_version = 2\n{}\n# the daemon\n[daemon]\nportmap = false\n\n[exports.p]\npath = \"/p\"\nrw = [] # nobody yet\n",
            state_line(dir.path())
        )
    );
}

#[test]
fn a_save_that_would_not_load_leaves_the_file_and_frees_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("irsync.toml");
    irsync::fs::private::write(&path, "[exports.p]\npath = \"/p\"\n").unwrap();
    let mut lock = lock_config(&path).unwrap();
    let mut m = lock.load().unwrap();
    m.exports[0].path = "rel".into();
    assert!(matches!(
        lock.save(&mut m),
        Err(ManageError::Invalid { .. })
    ));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[exports.p]\npath = \"/p\"\n"
    );
    lock_config(&path).unwrap();
}

#[test]
fn a_file_that_does_not_load_cannot_be_edited() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("irsync.toml");
    irsync::fs::private::write(&path, "[hosts.nas]\nendpoint_id = \"nope\"\n").unwrap();
    let e = lock_config(&path).unwrap().load().unwrap_err();
    assert!(e.to_string().contains("irsync.toml:2:"), "{e}");
}

#[test]
fn peer_spec_forms() {
    let c = irsync::config::file::Config::parse(
        &format!(
            "[clients.laptop]\nclient_id = \"{}\"\n[servers.nas]\nendpoint_id = \"{}\"\n",
            id(1),
            id(3)
        ),
        Path::new("h"),
    )
    .unwrap();
    let hosts = (&c.clients, &c.servers);
    assert!(
        matches!(PeerSpec::parse("laptop", hosts.0, hosts.1).unwrap(), PeerSpec::Alias(ref a) if a == "laptop")
    );
    assert!(
        matches!(PeerSpec::parse(&id(2).to_string(), hosts.0, hosts.1).unwrap(), PeerSpec::Id { id: got, .. } if got == id(2))
    );
    let mut rng = ssh_key::rand_core::UnwrapErr(ssh_key::getrandom::SysRng);
    let key = ssh_key::PrivateKey::random(&mut rng, ssh_key::Algorithm::Ed25519).unwrap();
    let mut pk = key.public_key().clone();
    pk.set_comment("mike@laptop");
    let line = pk.to_openssh().unwrap();
    let want = irsync::identity::endpoint_id_of_public_key(&pk).unwrap();
    assert!(
        matches!(PeerSpec::parse(&line, hosts.0, hosts.1).unwrap(), PeerSpec::Id { id, comment: Some(ref c) } if id == want && c == "mike@laptop")
    );
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("k.pub"), &line).unwrap();
    assert!(matches!(
        PeerSpec::parse(d.path().join("k.pub").to_str().unwrap(), hosts.0, hosts.1).unwrap(),
        PeerSpec::Id {
            comment: Some(_),
            ..
        }
    ));
    assert!(matches!(
        PeerSpec::parse("all", hosts.0, hosts.1),
        Err(ManageError::NoOpenAccess)
    ));
    assert!(matches!(
        PeerSpec::parse("*", hosts.0, hosts.1),
        Err(ManageError::NoOpenAccess)
    ));
    assert!(PeerSpec::parse("nobody", hosts.0, hosts.1).is_err());
    // Grants name clients: a server's name is refused, with what to do.
    let e = PeerSpec::parse("nas", hosts.0, hosts.1).unwrap_err();
    assert!(matches!(e, ManageError::NotAClient(_)), "{e:?}");
    assert_eq!(
        e.to_string(),
        "\"nas\" is a server here, not a client; to grant that machine, add it as a client: irsync client add nas ID"
    );
    assert_eq!(
        PeerSpec::parse("laptop", hosts.0, hosts.1).unwrap().entry(),
        "laptop"
    );
    assert_eq!(
        PeerSpec::parse(&id(2).to_string(), hosts.0, hosts.1)
            .unwrap()
            .entry(),
        id(2).to_string()
    );
}

#[test]
fn save_config_refuses_a_message_that_does_not_load() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("irsync.toml");
    let mut m = pb::Config {
        servers: vec![server_without_id("x")],
        ..Default::default()
    };
    let e = save_config(&path, &mut m).unwrap_err();
    assert!(e.to_string().contains("refusing to save"), "{e}");
    assert!(!path.exists());
    let mut m = msg("[client]\nparallel = 2\n[exports.p]\npath = \"/p\"\n");
    m.state_dir = Some(state(d.path()));
    save_config(&path, &mut m).unwrap();
    assert!(path.exists());
}

#[test]
fn lock_config_excludes_a_second_editor_until_save_or_drop() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("sub/irsync.toml");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    irsync::fs::private::write(
        &p,
        format!("{}# keep\n[client]\nparallel = 4\n", state_line(d.path())),
    )
    .unwrap();
    let kept = format!(
        "config_version = 2\n{}\n# keep\n[client]\nparallel = 8\n",
        state_line(d.path())
    );
    let lock_file = d.path().join("sub/irsync.toml.lock");

    let mut lock = lock_config(&p).unwrap();
    assert!(lock_file.exists());
    let e = lock_config(&p).unwrap_err();
    assert!(matches!(e, ManageError::Locked { .. }), "{e}");
    assert!(
        e.to_string().contains("irsync.toml.lock") && e.to_string().contains("another irsync"),
        "{e}"
    );
    let mut m = lock.load().unwrap();
    m.client.as_mut().unwrap().parallel = Some(8);
    lock.save(&mut m).unwrap();
    assert!(!lock_file.exists(), "save releases the lock");
    assert_eq!(std::fs::read_to_string(&p).unwrap(), kept);

    // Dropped without saving: the lock goes and the file is untouched.
    let lock = lock_config(&p).unwrap();
    drop(lock);
    assert!(!lock_file.exists());
    assert_eq!(std::fs::read_to_string(&p).unwrap(), kept);

    // An invalid message is refused and the lock released.
    let mut lock = lock_config(&p).unwrap();
    let mut m = lock.load().unwrap();
    m.servers.push(server_without_id("x"));
    assert!(matches!(
        lock.save(&mut m),
        Err(ManageError::Invalid { .. })
    ));
    assert!(!lock_file.exists());
}

fn parse_clients(
    text: &str,
    path: &std::path::Path,
) -> Result<irsync::config::clients::Clients, irsync::config::ConfigError> {
    irsync::config::file::Config::parse(text, path).map(|c| c.clients)
}

#[test]
fn save_writes_aliases_for_ids() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("irsync.toml");
    let mut m = msg(&format!(
        "[clients.box]\nclient_id = \"{0}\"\n[exports.p]\npath = \"/p\"\nrw = [\"{0}\"]\n",
        id(1)
    ));
    m.state_dir = Some(state(d.path()));
    lock_config(&path).unwrap().save(&mut m).unwrap();
    let want = format!(
        "config_version = 2\n{}\n[clients.box]\nclient_id = \"{}\"\n\n[exports.p]\npath = \"/p\"\nrw = [\"box\"]\n",
        state_line(d.path()),
        id(1)
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), want);
    // The caller's message is the one that was written.
    assert_eq!(write(&m), want);
}

#[test]
fn canonicalize_doc_uses_the_clients_of_the_message() {
    use irsync::manage::canonicalize_doc;
    let path = Path::new("irsync.toml");
    let mut m = msg(&format!(
        "[clients.box]\nclient_id = \"{0}\"\n[exports.p]\npath = \"/p\"\nrw = [\"{0}\"]\n",
        id(1)
    ));
    assert!(canonicalize_doc(&mut m, path).unwrap());
    assert_eq!(m.exports[0].rw[0].value, "box");
    assert!(!canonicalize_doc(&mut m, path).unwrap());
    let mut bad = pb::Config {
        servers: vec![server_without_id("box")],
        ..Default::default()
    };
    assert!(matches!(
        canonicalize_doc(&mut bad, path),
        Err(ManageError::Invalid { .. })
    ));
}

#[test]
fn written_is_the_alias_of_an_id_that_has_one() {
    let hosts = parse_clients(
        &format!(
            "[clients.zed]\nclient_id = \"{0}\"\n[clients.amy]\nclient_id = \"{0}\"\n",
            id(1)
        ),
        Path::new("h"),
    )
    .unwrap();
    let spec = |n| PeerSpec::Id {
        id: id(n),
        comment: None,
    };
    assert_eq!(spec(1).written(&hosts), "amy");
    assert_eq!(spec(2).written(&hosts), id(2).to_string());
    assert_eq!(PeerSpec::Alias("zed".into()).written(&hosts), "zed");
}

/// A save keeps the mode the file has, so it never opens up a config its
/// owner closed. A new config is for the owner only.
#[test]
fn save_keeps_the_files_mode_and_a_new_file_is_private() {
    use irsync::platform::{mode_of, testutil::set_mode};
    let d = tempfile::tempdir().unwrap();
    let mode = |p: &std::path::Path| mode_of(&std::fs::metadata(p).unwrap()) & 0o7777;

    let fresh = d.path().join("new.toml");
    let mut m = load_message(&fresh).unwrap();
    m.state_dir = Some(state(d.path()));
    m.client = Some(Default::default());
    save_config(&fresh, &mut m).unwrap();
    assert_eq!(mode(&fresh), 0o600);

    let locked = d.path().join("locked.toml");
    let mut lock = lock_config(&locked).unwrap();
    let mut m = lock.load().unwrap();
    m.state_dir = Some(state(d.path()));
    m.client = Some(Default::default());
    lock.save(&mut m).unwrap();
    assert_eq!(mode(&locked), 0o600);

    // A file open to other users loads only when it says so itself;
    // save_config reads the file first too.
    m.unsafe_config_allowed = Some(true);
    save_config(&fresh, &mut m).unwrap();
    for kept in [0o640, 0o644, 0o600] {
        set_mode(&fresh, kept).unwrap();
        m.client.as_mut().unwrap().parallel = Some(kept);
        save_config(&fresh, &mut m).unwrap();
        assert_eq!(mode(&fresh), kept, "save_config");
        let mut lock = lock_config(&fresh).unwrap();
        let mut m = lock.load().unwrap();
        m.client.as_mut().unwrap().parallel = Some(1);
        lock.save(&mut m).unwrap();
        assert_eq!(mode(&fresh), kept, "lock.save");
    }
}

/// A file of layout 1 is converted when it is loaded for an edit and saved
/// in the current layout, with the same comments; a file of the current
/// layout gets `config_version` on its first save.
#[test]
fn a_save_writes_the_current_layout() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("irsync.toml");
    irsync::fs::private::write(
        &path,
        format!(
            "{2}# the nas\n[hosts.nas]\nendpoint_id = \"{0}\"\nclient_id = \"{1}\" # its client\n\n[all]\nrw = [\"nas\"]\n",
            id(1),
            id(2),
            state_line(d.path())
        ),
    )
    .unwrap();
    let mut lock = lock_config(&path).unwrap();
    let mut m = lock.load().unwrap();
    assert_eq!(m.config_version, Some(2));
    lock.save(&mut m).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!(
            "config_version = 2\n{2}\n# the nas\n[servers.nas]\nendpoint_id = \"{0}\"\n\n[clients.nas]\nclient_id = \"{1}\" # its client\nall_exports = \"rw\"\n",
            id(1),
            id(2),
            state_line(d.path())
        )
    );

    irsync::fs::private::write(
        &path,
        format!("{}[client]\nparallel = 2\n", state_line(d.path())),
    )
    .unwrap();
    let mut lock = lock_config(&path).unwrap();
    let mut m = lock.load().unwrap();
    assert_eq!(m.config_version, None);
    lock.save(&mut m).unwrap();
    assert_eq!(m.config_version, Some(2));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!(
            "config_version = 2\n{}\n[client]\nparallel = 2\n",
            state_line(d.path())
        )
    );
}
