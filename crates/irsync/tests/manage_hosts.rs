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
        ..Default::default()
    };
    assert_eq!(host_add(&mut doc, "home", id(1), &f).unwrap(), None);
    assert_eq!(
        doc.to_string(),
        format!(
            "[hosts.home]\nendpoint_id = \"{}\"\nparallel = 4\nreconnect = \"1h\"\n",
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
        host_remove(&mut doc, "x"),
        Err(ManageError::NotFound(_))
    ));
    host_add(&mut doc, "x", id(1), &HostFlags::default()).unwrap();
    host_remove(&mut doc, "x").unwrap();
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
    let path = d.path().join("hosts.toml");
    let doc: DocumentMut = "[hosts.x]\nendpoint_id = \"nope\"\n".parse().unwrap();
    let e = irsync::manage::save_config(&path, &doc).unwrap_err();
    assert!(e.to_string().contains(path.to_str().unwrap()), "{e}");
    assert!(!path.exists());
}

#[test]
fn host_remove_needs_a_table() {
    let mut doc: DocumentMut = "[hosts]\nx = 1\n".parse().unwrap();
    assert!(matches!(
        host_remove(&mut doc, "x"),
        Err(ManageError::NotFound(_))
    ));
    assert_eq!(doc.to_string(), "[hosts]\nx = 1\n");
    let mut doc: DocumentMut = format!("[hosts]\ny = {{ endpoint_id = \"{}\" }}\n", id(1))
        .parse()
        .unwrap();
    host_remove(&mut doc, "y").unwrap();
    assert!(doc["hosts"].as_table().unwrap().is_empty());
}

#[test]
fn host_add_keeps_the_other_keys_of_an_inline_table() {
    let mut doc: DocumentMut = format!(
        "[hosts]\nhome = {{ endpoint_id = \"{}\", parallel = 4 }}\n",
        id(1)
    )
    .parse()
    .unwrap();
    let f = HostFlags {
        mdns: Some(true),
        ..Default::default()
    };
    assert_eq!(host_add(&mut doc, "home", id(2), &f).unwrap(), Some(id(1)));
    let h =
        irsync::config::hosts::Hosts::parse(&doc.to_string(), std::path::Path::new("h")).unwrap();
    let e = h.get("home").unwrap();
    assert_eq!(
        (e.endpoint_id, e.parallel, e.mdns),
        (id(2), Some(4), Some(true)),
        "{doc}"
    );
}
