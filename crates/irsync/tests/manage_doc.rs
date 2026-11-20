use irsync::manage::{load_doc, save_config, save_doc, ManageError, PeerSpec};
use std::path::Path;
use toml_edit::DocumentMut;

fn id(n: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
}

#[test]
fn load_missing_is_empty_and_save_creates_parent_atomically() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("sub/irsync.toml");
    let mut doc = load_doc(&p).unwrap();
    assert!(doc.as_table().is_empty());
    doc["x"] = toml_edit::table();
    doc["x"]["path"] = toml_edit::value("/x");
    save_doc(&p, &doc).unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "[x]\npath = \"/x\"\n");
    assert!(
        std::fs::read_dir(p.parent().unwrap()).unwrap().count() == 1,
        "no temp file left behind"
    );
}

#[test]
fn save_preserves_comments_and_order() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("e.toml");
    std::fs::write(
        &p,
        "# top\n[b]\npath = \"/b\" # keep\n\n[a]\npath = \"/a\"\n",
    )
    .unwrap();
    let mut doc = load_doc(&p).unwrap();
    doc["a"]["read_only"] = toml_edit::value(true);
    save_doc(&p, &doc).unwrap();
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "# top\n[b]\npath = \"/b\" # keep\n\n[a]\npath = \"/a\"\nread_only = true\n"
    );
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
fn save_config_refuses_a_document_that_does_not_load() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("irsync.toml");
    let mut doc: DocumentMut = "[hosts.x]\nendpoint_id = \"nope\"\n".parse().unwrap();
    let e = save_config(&path, &mut doc).unwrap_err();
    assert!(e.to_string().contains("refusing to save"), "{e}");
    assert!(!path.exists());
    let mut doc: DocumentMut = "[client]\nparallel = 2\n[exports.p]\npath = \"/p\"\n"
        .parse()
        .unwrap();
    save_config(&path, &mut doc).unwrap();
    assert!(path.exists());
}

#[test]
fn lock_config_excludes_a_second_editor_until_save_or_drop() {
    use irsync::manage::lock_config;
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("sub/irsync.toml");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "# keep\n[client]\nparallel = 4\n").unwrap();
    let lock_file = d.path().join("sub/irsync.toml.lock");

    let lock = lock_config(&p).unwrap();
    assert!(lock_file.exists());
    let e = lock_config(&p).unwrap_err();
    assert!(matches!(e, ManageError::Locked { .. }), "{e}");
    assert!(
        e.to_string().contains("irsync.toml.lock") && e.to_string().contains("another irsync"),
        "{e}"
    );
    let mut doc = lock.load().unwrap();
    doc["client"]["parallel"] = toml_edit::value(8);
    lock.save(&mut doc).unwrap();
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

    // An invalid document is refused and the lock released.
    let lock = lock_config(&p).unwrap();
    let mut doc = lock.load().unwrap();
    doc["client"]["bogus"] = toml_edit::value(1);
    assert!(matches!(
        lock.save(&mut doc),
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
    use irsync::manage::lock_config;
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("irsync.toml");
    let mut doc: DocumentMut = format!(
        "[hosts.box]\nendpoint_id = \"{0}\"\n[all]\nrw = [\"{0}\"]\n",
        id(1)
    )
    .parse()
    .unwrap();
    lock_config(&path).unwrap().save(&mut doc).unwrap();
    let want = format!(
        "[hosts.box]\nendpoint_id = \"{}\"\n[all]\nrw = [\"box\"]\n",
        id(1)
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), want);
    // The caller's document is the one that was written.
    assert_eq!(doc.to_string(), want);
}

#[test]
fn canonicalize_doc_uses_the_hosts_of_the_document() {
    use irsync::manage::canonicalize_doc;
    let path = Path::new("irsync.toml");
    let mut doc: DocumentMut = format!(
        "[hosts.box]\nendpoint_id = \"{0}\"\n[immediate]\nrw = [\"{0}\"]\n",
        id(1)
    )
    .parse()
    .unwrap();
    assert!(canonicalize_doc(&mut doc, path).unwrap());
    assert!(doc.to_string().ends_with("rw = [\"box\"]\n"));
    assert!(!canonicalize_doc(&mut doc, path).unwrap());
    let mut bad: DocumentMut = "[hosts.box]\nendpoint_id = \"nonsense\"\n".parse().unwrap();
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
    use irsync::manage::lock_config;
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o7777;

    let fresh = d.path().join("new.toml");
    let mut doc = load_doc(&fresh).unwrap();
    doc["client"] = toml_edit::table();
    save_doc(&fresh, &doc).unwrap();
    assert_eq!(mode(&fresh), 0o600);

    let locked = d.path().join("locked.toml");
    let lock = lock_config(&locked).unwrap();
    let mut doc = lock.load().unwrap();
    doc["client"] = toml_edit::table();
    lock.save(&mut doc).unwrap();
    assert_eq!(mode(&locked), 0o600);

    for kept in [0o640, 0o644, 0o600] {
        std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(kept)).unwrap();
        doc["client"]["parallel"] = toml_edit::value(i64::from(kept));
        save_doc(&fresh, &doc).unwrap();
        assert_eq!(mode(&fresh), kept, "save_doc");
        let lock = lock_config(&fresh).unwrap();
        let mut doc = lock.load().unwrap();
        doc["client"]["parallel"] = toml_edit::value(1);
        lock.save(&mut doc).unwrap();
        assert_eq!(mode(&fresh), kept, "lock.save");
    }
}
