use irsync::config::reader::read;
use irsync::config::writer::write;
use irsync::manage::{load_message, lock_config, save_config, ManageError, PeerSpec};
use irsync_proto::config as pb;
use std::path::Path;

fn id(n: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
}

fn msg(text: &str) -> pb::Config {
    read(text, Path::new("irsync.toml")).unwrap()
}

fn export(name: &str, path: &str) -> pb::Export {
    pb::Export {
        name: name.into(),
        path: path.into(),
        ..Default::default()
    }
}

/// A host with neither id: the message holds it, the config refuses it.
fn host_without_ids(name: &str) -> pb::Host {
    pb::Host {
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
    m.exports.push(export("x", "/x"));
    save_config(&p, &mut m).unwrap();
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "[exports.x]\npath = \"/x\"\n"
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
        "# top\n[exports.b]\npath = \"/b\" # keep\n\n[exports.a]\npath = \"/a\"\n",
    )
    .unwrap();
    let mut m = load_message(&p).unwrap();
    m.exports[1].read_only = Some(true);
    save_config(&p, &mut m).unwrap();
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "# top\n[exports.b]\npath = \"/b\" # keep\n\n[exports.a]\npath = \"/a\"\nread_only = true\n"
    );
}

#[test]
fn a_save_keeps_comments_and_writes_the_writers_form() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("irsync.toml");
    irsync::fs::private::write(
        &path,
        "[all]\nrw=[]   # nobody yet\n\n\n# the daemon\n[daemon]\nportmap=false\n",
    )
    .unwrap();
    let lock = lock_config(&path).unwrap();
    let mut m = lock.load().unwrap();
    lock.save(&mut m).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "# the daemon\n[daemon]\nportmap = false\n\n[all]\nrw = [] # nobody yet\n"
    );
}

#[test]
fn a_save_that_would_not_load_leaves_the_file_and_frees_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("irsync.toml");
    irsync::fs::private::write(&path, "[exports.p]\npath = \"/p\"\n").unwrap();
    let lock = lock_config(&path).unwrap();
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
    let hosts = parse_hosts(
        &format!("[hosts.laptop]\nendpoint_id = \"{}\"\n", id(1)),
        Path::new("h"),
    )
    .unwrap();
    assert!(
        matches!(PeerSpec::parse("laptop", &hosts).unwrap(), PeerSpec::Alias(ref a) if a == "laptop")
    );
    assert!(
        matches!(PeerSpec::parse(&id(2).to_string(), &hosts).unwrap(), PeerSpec::Id { id: got, .. } if got == id(2))
    );
    let mut rng = ssh_key::rand_core::UnwrapErr(ssh_key::getrandom::SysRng);
    let key = ssh_key::PrivateKey::random(&mut rng, ssh_key::Algorithm::Ed25519).unwrap();
    let mut pk = key.public_key().clone();
    pk.set_comment("mike@laptop");
    let line = pk.to_openssh().unwrap();
    let want = irsync::identity::endpoint_id_of_public_key(&pk).unwrap();
    assert!(
        matches!(PeerSpec::parse(&line, &hosts).unwrap(), PeerSpec::Id { id, comment: Some(ref c) } if id == want && c == "mike@laptop")
    );
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("k.pub"), &line).unwrap();
    assert!(matches!(
        PeerSpec::parse(d.path().join("k.pub").to_str().unwrap(), &hosts).unwrap(),
        PeerSpec::Id {
            comment: Some(_),
            ..
        }
    ));
    assert!(matches!(
        PeerSpec::parse("all", &hosts),
        Err(ManageError::NoOpenAccess)
    ));
    assert!(matches!(
        PeerSpec::parse("*", &hosts),
        Err(ManageError::NoOpenAccess)
    ));
    assert!(PeerSpec::parse("nobody", &hosts).is_err());
    assert_eq!(PeerSpec::parse("laptop", &hosts).unwrap().entry(), "laptop");
    assert_eq!(
        PeerSpec::parse(&id(2).to_string(), &hosts).unwrap().entry(),
        id(2).to_string()
    );
}

#[test]
fn save_config_refuses_a_message_that_does_not_load() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("irsync.toml");
    let mut m = pb::Config {
        hosts: vec![host_without_ids("x")],
        ..Default::default()
    };
    let e = save_config(&path, &mut m).unwrap_err();
    assert!(e.to_string().contains("refusing to save"), "{e}");
    assert!(!path.exists());
    let mut m = msg("[client]\nparallel = 2\n[exports.p]\npath = \"/p\"\n");
    save_config(&path, &mut m).unwrap();
    assert!(path.exists());
}

#[test]
fn lock_config_excludes_a_second_editor_until_save_or_drop() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("sub/irsync.toml");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    irsync::fs::private::write(&p, "# keep\n[client]\nparallel = 4\n").unwrap();
    let lock_file = d.path().join("sub/irsync.toml.lock");

    let lock = lock_config(&p).unwrap();
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
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "# keep\n[client]\nparallel = 8\n"
    );

    // Dropped without saving: the lock goes and the file is untouched.
    let lock = lock_config(&p).unwrap();
    drop(lock);
    assert!(!lock_file.exists());
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "# keep\n[client]\nparallel = 8\n"
    );

    // An invalid message is refused and the lock released.
    let lock = lock_config(&p).unwrap();
    let mut m = lock.load().unwrap();
    m.hosts.push(host_without_ids("x"));
    assert!(matches!(
        lock.save(&mut m),
        Err(ManageError::Invalid { .. })
    ));
    assert!(!lock_file.exists());
}

fn parse_hosts(
    text: &str,
    path: &std::path::Path,
) -> Result<irsync::config::hosts::Hosts, irsync::config::ConfigError> {
    irsync::config::file::Config::parse(text, path).map(|c| c.hosts)
}

#[test]
fn save_writes_aliases_for_ids() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("irsync.toml");
    let mut m = msg(&format!(
        "[hosts.box]\nendpoint_id = \"{0}\"\n[all]\nrw = [\"{0}\"]\n",
        id(1)
    ));
    lock_config(&path).unwrap().save(&mut m).unwrap();
    let want = format!(
        "[hosts.box]\nendpoint_id = \"{}\"\n\n[all]\nrw = [\"box\"]\n",
        id(1)
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), want);
    // The caller's message is the one that was written.
    assert_eq!(write(&m), want);
}

#[test]
fn canonicalize_doc_uses_the_hosts_of_the_message() {
    use irsync::manage::canonicalize_doc;
    let path = Path::new("irsync.toml");
    let mut m = msg(&format!(
        "[hosts.box]\nendpoint_id = \"{0}\"\n[immediate]\nrw = [\"{0}\"]\n",
        id(1)
    ));
    assert!(canonicalize_doc(&mut m, path).unwrap());
    assert_eq!(m.immediate.as_ref().unwrap().rw[0].value, "box");
    assert!(!canonicalize_doc(&mut m, path).unwrap());
    let mut bad = pb::Config {
        hosts: vec![host_without_ids("box")],
        ..Default::default()
    };
    assert!(matches!(
        canonicalize_doc(&mut bad, path),
        Err(ManageError::Invalid { .. })
    ));
}

#[test]
fn written_is_the_alias_of_an_id_that_has_one() {
    let hosts = parse_hosts(
        &format!(
            "[hosts.zed]\nendpoint_id = \"{0}\"\n[hosts.amy]\nendpoint_id = \"{0}\"\n",
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
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o7777;

    let fresh = d.path().join("new.toml");
    let mut m = load_message(&fresh).unwrap();
    m.client = Some(Default::default());
    save_config(&fresh, &mut m).unwrap();
    assert_eq!(mode(&fresh), 0o600);

    let locked = d.path().join("locked.toml");
    let lock = lock_config(&locked).unwrap();
    let mut m = lock.load().unwrap();
    m.client = Some(Default::default());
    lock.save(&mut m).unwrap();
    assert_eq!(mode(&locked), 0o600);

    // A file open to other users loads only when it says so itself.
    m.unsafe_config_allowed = Some(true);
    for kept in [0o640, 0o644, 0o600] {
        std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(kept)).unwrap();
        m.client.as_mut().unwrap().parallel = Some(kept);
        save_config(&fresh, &mut m).unwrap();
        assert_eq!(mode(&fresh), kept, "save_config");
        let lock = lock_config(&fresh).unwrap();
        let mut m = lock.load().unwrap();
        m.client.as_mut().unwrap().parallel = Some(1);
        lock.save(&mut m).unwrap();
        assert_eq!(mode(&fresh), kept, "lock.save");
    }
}
