use irsync::config::reader::read;
use irsync::config::writer::write;
use irsync::manage::msg::hosts::{
    host_add, host_add_ssh_alias, host_aliases, host_list, host_remove, host_rename,
    host_set_client, HostFlags,
};
use irsync::manage::msg::{id_bytes, id_of};
use irsync::manage::ManageError;
use irsync_proto::config as pb;
use std::path::Path;
use std::time::Duration;

fn id(n: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
}

fn msg(text: &str) -> pb::Config {
    read(text, Path::new("f")).unwrap()
}

#[test]
fn host_add_writes_given_keys_updates_id_and_keeps_others() {
    let mut m = pb::Config::default();
    let f = HostFlags {
        parallel: Some(4),
        reconnect: Some(Duration::from_secs(3600)),
        dial_retries: Some(2),
        ..Default::default()
    };
    assert_eq!(host_add(&mut m, "home", id(1), &f).unwrap(), None);
    assert_eq!(
        write(&m),
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
    assert_eq!(host_add(&mut m, "home", id(2), &f).unwrap(), Some(id(1)));
    let s = write(&m);
    assert!(
        s.contains(&id(2).to_string())
            && s.contains("parallel = 4")
            && s.contains("address = [\"10.0.0.1:4000\"]")
            && s.contains("relay_mode = \"disabled\""),
        "{s}"
    );
    assert_eq!(
        host_add(&mut m, "home", id(2), &HostFlags::default()).unwrap(),
        None
    );
}

#[test]
fn host_add_rejects_bad_alias_and_remove_needs_existing() {
    let mut m = pb::Config::default();
    for bad in ["a:b", "a/b", "", &id(1).to_string()] {
        assert!(
            matches!(
                host_add(&mut m, bad, id(1), &HostFlags::default()),
                Err(ManageError::BadAlias(_))
            ),
            "{bad:?}"
        );
    }
    assert!(matches!(
        host_remove(&mut m, "x", false),
        Err(ManageError::NotFound(_))
    ));
    host_add(&mut m, "x", id(1), &HostFlags::default()).unwrap();
    host_remove(&mut m, "x", false).unwrap();
    assert!(host_list(&m).is_empty(), "{}", write(&m));
}

#[test]
fn host_add_writes_a_dotted_header_and_refuses_all() {
    let mut m = pb::Config::default();
    host_add(&mut m, "home", id(1), &HostFlags::default()).unwrap();
    assert_eq!(
        write(&m),
        format!("[hosts.home]\nendpoint_id = \"{}\"\n", id(1))
    );
    for bad in ["all", "*"] {
        assert!(matches!(
            host_add(&mut m, bad, id(1), &HostFlags::default()),
            Err(ManageError::BadAlias(_))
        ));
    }
    assert_eq!(host_list(&m), vec![format!("home: {}", id(1))]);
}

#[test]
fn a_hosts_message_that_does_not_parse_is_refused_with_the_path() {
    let e = read(
        "[hosts.x]\nendpoint_id = \"nope\"\n",
        Path::new("irsync.toml"),
    )
    .unwrap_err();
    assert!(e.to_string().contains("irsync.toml"), "{e}");
}

#[test]
fn host_remove_needs_a_host() {
    // A host that is no table never gets into the message.
    assert!(read("[hosts]\nx = 1\n", Path::new("f")).is_err());
    let mut m = msg(&format!("[hosts]\ny = {{ endpoint_id = \"{}\" }}\n", id(1)));
    host_remove(&mut m, "y", false).unwrap();
    assert!(m.hosts.is_empty());
}

#[test]
fn host_add_keeps_the_other_keys_of_an_inline_table() {
    let mut m = msg(&format!(
        "[hosts]\nhome = {{ endpoint_id = \"{}\", parallel = 4, dial_retries = 3 }}\n",
        id(1)
    ));
    let f = HostFlags {
        mdns: Some(true),
        ..Default::default()
    };
    assert_eq!(host_add(&mut m, "home", id(2), &f).unwrap(), Some(id(1)));
    let h = &m.hosts[0];
    assert_eq!(
        (
            h.endpoint_id.as_deref().and_then(id_of),
            h.parallel,
            h.dial_retries,
            h.mdns
        ),
        (Some(id(2)), Some(4), Some(3), Some(true)),
        "{}",
        write(&m)
    );
}

#[test]
fn host_add_writes_ssh() {
    let mut m = pb::Config::default();
    let f = HostFlags {
        ssh: Some("u@h".into()),
        ..Default::default()
    };
    host_add(&mut m, "h", id(1), &f).unwrap();
    assert!(write(&m).contains("ssh = \"u@h\""));
    let back = msg(&write(&m));
    assert_eq!(back.hosts[0].ssh.as_deref(), Some("u@h"));
}

#[test]
fn ssh_aliases_are_added_once_and_only_to_a_host() {
    let mut m = pb::Config::default();
    host_add_ssh_alias(&mut m, "h", "x");
    assert!(m.hosts.is_empty());
    host_add(&mut m, "h", id(1), &HostFlags::default()).unwrap();
    host_add_ssh_alias(&mut m, "h", "u@h");
    host_add_ssh_alias(&mut m, "h", "u@h");
    host_add_ssh_alias(&mut m, "h", "v@h");
    assert_eq!(
        write(&m),
        format!(
            "[hosts.h]\nendpoint_id = \"{}\"\nssh_aliases = [\"u@h\", \"v@h\"]\n",
            id(1)
        )
    );
}

#[test]
fn set_client_creates_a_host_and_keeps_the_endpoint_id() {
    let mut m = pb::Config::default();
    host_set_client(&mut m, "c", id(3)).unwrap();
    assert_eq!(write(&m), format!("[hosts.c]\nclient_id = \"{}\"\n", id(3)));
    assert_eq!(
        host_list(&m),
        vec![
            "c: (client only)".to_string(),
            format!("    client_id = \"{}\"", id(3))
        ]
    );
    host_add(&mut m, "c", id(4), &HostFlags::default()).unwrap();
    host_set_client(&mut m, "c", id(5)).unwrap();
    assert_eq!(m.hosts.len(), 1);
    assert_eq!(m.hosts[0].endpoint_id, Some(id_bytes(id(4))));
    assert!(matches!(
        host_set_client(&mut m, "a:b", id(1)),
        Err(ManageError::BadAlias(_))
    ));
}

#[test]
fn host_list_shows_the_other_keys_and_aliases_are_in_order() {
    let mut m = pb::Config::default();
    let f = HostFlags {
        parallel: Some(2),
        address: Some(vec!["10.0.0.1:4000".parse().unwrap()]),
        ..Default::default()
    };
    host_add(&mut m, "b", id(1), &f).unwrap();
    host_add(&mut m, "a", id(2), &HostFlags::default()).unwrap();
    assert_eq!(host_aliases(&m), ["b", "a"]);
    assert_eq!(
        host_list(&m),
        vec![
            format!("b: {}", id(1)),
            "    address = [\"10.0.0.1:4000\"]".to_string(),
            "    parallel = 2".to_string(),
            format!("a: {}", id(2)),
        ]
    );
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
    let mut m = msg(&format!(
        "[hosts.box]\nendpoint_id = \"{}\"\n[all]\nrw = [\"other\"]\n",
        id(1)
    ));
    assert_eq!(host_remove(&mut m, "box", false).unwrap(), 0);
    assert_eq!(write(&m), "[all]\nrw = [\"other\"]\n");
}

#[test]
fn remove_is_refused_while_grants_name_the_host() {
    let mut m = msg(&named_everywhere());
    let before = write(&m);
    let e = host_remove(&mut m, "box", false).unwrap_err();
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
    assert_eq!(write(&m), before);
}

#[test]
fn force_removes_the_grants_then_the_host() {
    let mut m = msg(&named_everywhere());
    assert_eq!(host_remove(&mut m, "box", true).unwrap(), 3);
    assert_eq!(
        write(&m),
        "[exports.p]\npath = \"/p\"\nro = [\"y\"]\n\n\
         [exports.q]\npath = \"/q\"\nrw = [\"y\"]\n\n\
         [all]\nrw = [\"x\"]\n\n\
         [immediate]\nrw = []\n"
    );
}

#[test]
fn force_on_a_missing_host_is_not_found() {
    let mut m = msg("[all]\nrw = [\"box\"]\n");
    let before = write(&m);
    assert!(matches!(
        host_remove(&mut m, "box", true),
        Err(ManageError::NotFound(_))
    ));
    assert_eq!(write(&m), before);
}

#[test]
fn rename_renames_the_host_and_every_grant() {
    let mut m = msg(&named_everywhere());
    assert_eq!(host_rename(&mut m, "box", "crate").unwrap(), 3);
    assert_eq!(
        write(&m),
        write(&msg(&named_everywhere().replace("box", "crate")))
    );
}

#[test]
fn rename_keeps_the_hosts_place_keys_and_comments() {
    let text = format!(
        "[hosts.a]\nendpoint_id = \"{a}\"\n\n\
         # the old box\n[hosts.box] # header\nendpoint_id = \"{b}\" # id\nparallel = 2\n\n\
         [hosts.c]\nendpoint_id = \"{c}\"\n\
         [all]\nrw = [\n  \"x\",\n  \"box\", # mine\n]\n",
        a = id(1),
        b = id(2),
        c = id(3)
    );
    let mut m = msg(&text);
    let before = write(&m);
    assert_eq!(host_rename(&mut m, "box", "crate").unwrap(), 1);
    assert_eq!(
        write(&m),
        before
            .replace("box", "crate")
            .replace("old crate", "old box")
    );
    assert_eq!(host_aliases(&m), ["a", "crate", "c"]);
}

#[test]
fn rename_refuses_a_missing_host_a_taken_name_and_a_bad_alias() {
    let text = format!(
        "{}[hosts.taken]\nendpoint_id = \"{}\"\n",
        named_everywhere(),
        id(2)
    );
    let mut m = msg(&text);
    let before = write(&m);
    assert!(matches!(
        host_rename(&mut m, "nobody", "crate"),
        Err(ManageError::NotFound(a)) if a == "nobody"
    ));
    // `x` is named by a grant, but is no host.
    assert!(matches!(
        host_rename(&mut m, "x", "crate"),
        Err(ManageError::NotFound(_))
    ));
    let e = host_rename(&mut m, "box", "taken").unwrap_err();
    assert!(matches!(&e, ManageError::HostExists(a) if a == "taken"));
    assert_eq!(e.to_string(), "host \"taken\" already exists");
    for bad in ["a:b", "a/b", "all", "*", "", &id(5).to_string()] {
        assert!(
            matches!(
                host_rename(&mut m, "box", bad),
                Err(ManageError::BadAlias(_))
            ),
            "{bad:?}"
        );
    }
    assert_eq!(write(&m), before);
}

#[test]
fn rename_to_the_same_name_changes_nothing() {
    let mut m = msg(&named_everywhere());
    let before = write(&m);
    assert_eq!(host_rename(&mut m, "box", "box").unwrap(), 0);
    assert_eq!(write(&m), before);
    assert!(matches!(
        host_rename(&mut m, "nobody", "nobody"),
        Err(ManageError::NotFound(_))
    ));
}

#[test]
fn rename_and_remove_take_the_comments_along() {
    let mut m = read(
        &format!(
            "# the nas\n[hosts.nas]\nendpoint_id = \"{}\" # its daemon\n",
            id(1)
        ),
        Path::new("f"),
    )
    .unwrap();
    host_rename(&mut m, "nas", "box").unwrap();
    assert_eq!(
        write(&m),
        format!(
            "# the nas\n[hosts.box]\nendpoint_id = \"{}\" # its daemon\n",
            id(1)
        )
    );
    host_remove(&mut m, "box", false).unwrap();
    assert_eq!(write(&m), "");
}
