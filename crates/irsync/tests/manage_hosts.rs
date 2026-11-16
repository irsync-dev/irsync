use irsync::manage::hosts::{host_add, host_list, host_remove, HostFlags};
use irsync::manage::ManageError;
use toml_edit::DocumentMut;

fn id(n: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
}

#[test]
fn host_add_writes_given_keys_updates_id_and_keeps_others() {
    let mut doc = DocumentMut::new();
    let f = HostFlags {
        parallel: Some(4),
        reconnect: Some("1h".into()),
        dial_retries: Some(2),
        ..Default::default()
    };
    assert_eq!(host_add(&mut doc, "home", id(1), &f).unwrap(), None);
    assert_eq!(
        doc.to_string(),
        format!(
            "[hosts.home]\nendpoint_id = \"{}\"\nparallel = 4\nreconnect = \"1h\"\ndial_retries = 2\n",
            id(1)
        )
    );
    let f = HostFlags {
        address: Some(vec!["10.0.0.1:4000".parse().unwrap()]),
        relay_mode: Some(irsync::config::settings::RelayMode::Disabled),
        ..Default::default()
    };
    assert_eq!(host_add(&mut doc, "home", id(2), &f).unwrap(), Some(id(1)));
    let s = doc.to_string();
    assert!(
        s.contains(&id(2).to_string())
            && s.contains("parallel = 4")
            && s.contains("address = [\"10.0.0.1:4000\"]")
            && s.contains("relay_mode = \"disabled\""),
        "{s}"
    );
    assert_eq!(
        host_add(&mut doc, "home", id(2), &HostFlags::default()).unwrap(),
        None
    );
}

#[test]
fn host_add_rejects_bad_alias_and_remove_needs_existing() {
    let mut doc = DocumentMut::new();
    for bad in ["a:b", "a/b", "", &id(1).to_string()] {
        assert!(
            matches!(
                host_add(&mut doc, bad, id(1), &HostFlags::default()),
                Err(ManageError::BadAlias(_))
            ),
            "{bad:?}"
        );
    }
    assert!(matches!(
        host_remove(&mut doc, "x", false),
        Err(ManageError::NotFound(_))
    ));
    host_add(&mut doc, "x", id(1), &HostFlags::default()).unwrap();
    host_remove(&mut doc, "x", false).unwrap();
    assert!(host_list(&doc).is_empty(), "{doc}");
}

#[test]
fn host_add_writes_a_dotted_header_and_refuses_all() {
    let mut doc = DocumentMut::new();
    host_add(&mut doc, "home", id(1), &HostFlags::default()).unwrap();
    assert_eq!(
        doc.to_string(),
        format!("[hosts.home]\nendpoint_id = \"{}\"\n", id(1))
    );
    for bad in ["all", "*"] {
        assert!(matches!(
            host_add(&mut doc, bad, id(1), &HostFlags::default()),
            Err(ManageError::BadAlias(_))
        ));
    }
    assert_eq!(host_list(&doc), vec![format!("home: {}", id(1))]);
}

#[test]
fn save_config_refuses_a_hosts_document_that_does_not_parse() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("irsync.toml");
    let mut doc: DocumentMut = "[hosts.x]\nendpoint_id = \"nope\"\n".parse().unwrap();
    let e = irsync::manage::save_config(&path, &mut doc).unwrap_err();
    assert!(e.to_string().contains(path.to_str().unwrap()), "{e}");
    assert!(!path.exists());
}

#[test]
fn host_remove_needs_a_table() {
    let mut doc: DocumentMut = "[hosts]\nx = 1\n".parse().unwrap();
    assert!(matches!(
        host_remove(&mut doc, "x", false),
        Err(ManageError::NotFound(_))
    ));
    assert_eq!(doc.to_string(), "[hosts]\nx = 1\n");
    let mut doc: DocumentMut = format!("[hosts]\ny = {{ endpoint_id = \"{}\" }}\n", id(1))
        .parse()
        .unwrap();
    host_remove(&mut doc, "y", false).unwrap();
    assert!(doc["hosts"].as_table().unwrap().is_empty());
}

#[test]
fn host_add_keeps_the_other_keys_of_an_inline_table() {
    let mut doc: DocumentMut = format!(
        "[hosts]\nhome = {{ endpoint_id = \"{}\", parallel = 4, dial_retries = 3 }}\n",
        id(1)
    )
    .parse()
    .unwrap();
    let f = HostFlags {
        mdns: Some(true),
        ..Default::default()
    };
    assert_eq!(host_add(&mut doc, "home", id(2), &f).unwrap(), Some(id(1)));
    let h = parse_hosts(&doc.to_string(), std::path::Path::new("h")).unwrap();
    let e = h.get("home").unwrap();
    assert_eq!(
        (e.endpoint_id, e.parallel, e.dial_retries, e.mdns),
        (id(2), Some(4), Some(3), Some(true)),
        "{doc}"
    );
}

#[test]
fn host_add_writes_ssh() {
    let mut doc = DocumentMut::new();
    let f = HostFlags {
        ssh: Some("u@h".into()),
        ..Default::default()
    };
    host_add(&mut doc, "h", id(1), &f).unwrap();
    assert!(doc.to_string().contains("ssh = \"u@h\""));
    let h = parse_hosts(&doc.to_string(), std::path::Path::new("h")).unwrap();
    assert_eq!(h.get("h").unwrap().ssh.as_deref(), Some("u@h"));
}

fn parse_hosts(
    text: &str,
    path: &std::path::Path,
) -> Result<irsync::config::hosts::Hosts, irsync::config::ConfigError> {
    irsync::config::file::Config::parse(text, path).map(|c| c.hosts)
}

/// `[hosts.box]` and grants that name it in three tables, each beside
/// another entry.
fn named_everywhere() -> String {
    format!(
        "[hosts.box]\nendpoint_id = \"{}\"\n[all]\nrw = [\"box\", \"x\"]\n\
         [immediate]\nrw = [\"box\"]\n\
         [exports.p]\npath = \"/p\"\nro = [\"y\", \"box\"]\n\
         [exports.q]\npath = \"/q\"\nrw = [\"y\"]\n",
        id(1)
    )
}

#[test]
fn remove_without_grants() {
    let mut doc: DocumentMut = format!(
        "[hosts.box]\nendpoint_id = \"{}\"\n[all]\nrw = [\"other\"]\n",
        id(1)
    )
    .parse()
    .unwrap();
    assert_eq!(host_remove(&mut doc, "box", false).unwrap(), 0);
    assert_eq!(doc.to_string(), "[all]\nrw = [\"other\"]\n");
}

#[test]
fn remove_is_refused_while_grants_name_the_host() {
    let text = named_everywhere();
    let mut doc: DocumentMut = text.parse().unwrap();
    let e = host_remove(&mut doc, "box", false).unwrap_err();
    match &e {
        ManageError::HostInUse { alias, lists } => {
            assert_eq!(alias, "box");
            assert_eq!(lists, &["all", "immediate", "exports.p"]);
        }
        other => panic!("{other}"),
    }
    assert_eq!(
        e.to_string(),
        "host \"box\" is named by grants in all, immediate, exports.p: \
         remove those grants first, or pass --force"
    );
    assert_eq!(doc.to_string(), text);
}

#[test]
fn force_removes_the_grants_then_the_host() {
    let mut doc: DocumentMut = named_everywhere().parse().unwrap();
    assert_eq!(host_remove(&mut doc, "box", true).unwrap(), 3);
    assert_eq!(
        doc.to_string(),
        "[all]\nrw = [\"x\"]\n[immediate]\nrw = []\n\
         [exports.p]\npath = \"/p\"\nro = [\"y\"]\n\
         [exports.q]\npath = \"/q\"\nrw = [\"y\"]\n"
    );
}

#[test]
fn force_on_a_missing_host_is_not_found() {
    let text = "[all]\nrw = [\"box\"]\n";
    let mut doc: DocumentMut = text.parse().unwrap();
    assert!(matches!(
        host_remove(&mut doc, "box", true),
        Err(ManageError::NotFound(_))
    ));
    assert_eq!(doc.to_string(), text);
}

#[test]
fn lists_naming_and_remove_named_match_the_text_only() {
    use irsync::manage::grants::{lists_naming, remove_named};
    let mut doc: DocumentMut = named_everywhere().parse().unwrap();
    assert!(lists_naming(&doc, "y") == ["exports.p", "exports.q"]);
    assert!(lists_naming(&doc, "nobody").is_empty());
    assert_eq!(remove_named(&mut doc, "y"), 2);
    assert!(lists_naming(&doc, "y").is_empty());
    assert_eq!(remove_named(&mut doc, "y"), 0);
}
