use irsync::config::reader;
use irsync::config::writer::write;
use irsync::manage::servers::{
    server_add, server_add_ssh_alias, server_list, server_names, server_remove, server_rename,
    ServerFlags,
};
use irsync::manage::ManageError;
use irsync_proto::config as pb;
use std::path::Path;

fn id(n: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
}

/// The message of `text`; `config_version` stays unset, so `write` does
/// not print it.
fn msg(text: &str) -> pb::Config {
    reader::read(text, Path::new("f")).unwrap()
}
use std::time::Duration;

/// A server and a client of the same name, and grants naming the client.
fn both() -> String {
    format!(
        "[servers.box]\nendpoint_id = \"{}\"\n[clients.box]\nclient_id = \"{}\"\n[exports.p]\npath = \"/p\"\nrw = [\"box\"]\n",
        id(1),
        id(2)
    )
}

#[test]
fn add_creates_then_updates_keeping_other_keys() {
    let mut m = pb::Config::default();
    let f = ServerFlags {
        parallel: Some(4),
        reconnect: Some(Duration::from_secs(3600)),
        dial_retries: Some(2),
        ..Default::default()
    };
    assert_eq!(server_add(&mut m, "home", id(1), &f).unwrap(), None);
    assert_eq!(
        write(&m),
        format!(
            "[servers.home]\nendpoint_id = \"{}\"\nparallel = 4\nreconnect = \"1h\"\ndial_retries = 2\n",
            id(1)
        )
    );
    let f = ServerFlags {
        address: Some(vec!["10.0.0.1:4000".parse().unwrap()]),
        ssh: Some("u@h".into()),
        ..Default::default()
    };
    assert_eq!(server_add(&mut m, "home", id(2), &f).unwrap(), Some(id(1)));
    let s = write(&m);
    assert!(
        s.contains(&id(2).to_string())
            && s.contains("parallel = 4")
            && s.contains("address = [\"10.0.0.1:4000\"]")
            && s.contains("ssh = \"u@h\""),
        "{s}"
    );
    assert_eq!(
        server_add(&mut m, "home", id(2), &ServerFlags::default()).unwrap(),
        None
    );
}

#[test]
fn add_refuses_an_invalid_name() {
    let mut m = pb::Config::default();
    for bad in ["a:b", "a/b", "", "all", "*", &id(1).to_string()] {
        assert!(
            matches!(
                server_add(&mut m, bad, id(1), &ServerFlags::default()),
                Err(ManageError::BadName(_))
            ),
            "{bad:?}"
        );
    }
    assert!(m.servers.is_empty());
}

#[test]
fn ssh_aliases_are_added_once_and_only_to_a_server() {
    let mut m = pb::Config::default();
    server_add_ssh_alias(&mut m, "h", "x");
    assert!(m.servers.is_empty());
    server_add(&mut m, "h", id(1), &ServerFlags::default()).unwrap();
    server_add_ssh_alias(&mut m, "h", "u@h");
    server_add_ssh_alias(&mut m, "h", "u@h");
    assert_eq!(
        write(&m),
        format!(
            "[servers.h]\nendpoint_id = \"{}\"\nssh_aliases = [\"u@h\"]\n",
            id(1)
        )
    );
}

#[test]
fn remove_ignores_grants_and_keeps_the_client() {
    let mut m = msg(&both());
    server_remove(&mut m, "box").unwrap();
    assert!(m.servers.is_empty());
    assert_eq!(m.clients.len(), 1);
    assert_eq!(m.exports[0].rw.len(), 1);
    assert!(matches!(
        server_remove(&mut m, "box"),
        Err(ManageError::NotFound(_))
    ));
}

#[test]
fn rename_keeps_place_and_comments_and_touches_no_grant() {
    let mut m = msg(&format!(
        "[servers.a]\nendpoint_id = \"{}\"\n\n# the old box\n[servers.box] # header\n\
         endpoint_id = \"{}\" # id\n[clients.c]\nclient_id = \"{}\"\n[exports.p]\npath = \"/p\"\nrw = [\"c\"]\n",
        id(1),
        id(2),
        id(3)
    ));
    let before = write(&m);
    server_rename(&mut m, "box", "crate").unwrap();
    assert_eq!(server_names(&m), ["a", "crate"]);
    assert_eq!(m.clients[0].name, "c");
    assert_eq!(m.exports[0].rw[0].value, "c");
    let s = write(&m);
    assert!(s.contains("# the old box\n[servers.crate] # header"), "{s}");
    assert!(s.contains("# id"), "{s}");
    assert_eq!(
        s.matches("[clients.c]").count(),
        before.matches("[clients.c]").count()
    );
}

#[test]
fn rename_to_an_existing_name_or_bad_name_is_refused_and_to_itself_is_a_no_op() {
    let mut m = msg(&format!(
        "[servers.a]\nendpoint_id = \"{}\"\n[servers.b]\nendpoint_id = \"{}\"\n",
        id(1),
        id(2)
    ));
    let before = write(&m);
    let e = server_rename(&mut m, "a", "b").unwrap_err();
    assert!(matches!(&e, ManageError::ServerExists(n) if n == "b"));
    assert_eq!(e.to_string(), "server \"b\" already exists");
    assert!(matches!(
        server_rename(&mut m, "a", "a:b"),
        Err(ManageError::BadName(_))
    ));
    assert!(matches!(
        server_rename(&mut m, "nobody", "c"),
        Err(ManageError::NotFound(_))
    ));
    server_rename(&mut m, "a", "a").unwrap();
    assert_eq!(write(&m), before);
}

#[test]
fn a_server_may_share_its_name_with_a_client() {
    let mut m = msg(&both());
    server_add(&mut m, "other", id(5), &ServerFlags::default()).unwrap();
    server_rename(&mut m, "other", "box2").unwrap();
    assert_eq!(server_names(&m), ["box", "box2"]);
}

#[test]
fn list_shows_the_keys_and_not_config_version() {
    let mut m = pb::Config {
        config_version: Some(2),
        ..Default::default()
    };
    let f = ServerFlags {
        parallel: Some(2),
        address: Some(vec!["10.0.0.1:4000".parse().unwrap()]),
        ..Default::default()
    };
    server_add(&mut m, "b", id(1), &f).unwrap();
    server_add(&mut m, "a", id(2), &ServerFlags::default()).unwrap();
    assert_eq!(
        server_list(&m),
        vec![
            format!("b: {}", id(1)),
            "    address = [\"10.0.0.1:4000\"]".to_string(),
            "    parallel = 2".to_string(),
            format!("a: {}", id(2)),
        ]
    );
}

#[test]
fn add_writes_relay_dht_dns_and_mdns() {
    let mut m = pb::Config::default();
    let f = ServerFlags {
        relay: Some("https://relay.example.net".parse().unwrap()),
        dht: Some(false),
        dns: Some(false),
        mdns: Some(true),
        ..Default::default()
    };
    server_add(&mut m, "home", id(1), &f).unwrap();
    assert_eq!(
        write(&m),
        format!(
            "[servers.home]\nendpoint_id = \"{}\"\nrelay = \"https://relay.example.net/\"\ndht = false\ndns = false\nmdns = true\n",
            id(1)
        )
    );
    let f = ServerFlags {
        relay: Some(irsync::config::settings::Relay::Disabled),
        ..Default::default()
    };
    server_add(&mut m, "home", id(1), &f).unwrap();
    assert!(write(&m).contains("relay = \"disabled\"\n"));
}

#[test]
fn list_marks_a_missing_or_invalid_id() {
    let m = pb::Config {
        servers: vec![
            pb::ServerEntry {
                name: "none".into(),
                ..Default::default()
            },
            pb::ServerEntry {
                name: "bad".into(),
                endpoint_id: vec![1, 2, 3],
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    assert_eq!(server_list(&m), ["none: (missing)", "bad: (invalid)"]);
}

/// `server add` refuses `--address` beside `--relay` or a lookup flag,
/// also when one of them is in the file already.
#[test]
fn add_refuses_address_beside_relay_or_lookups() {
    let mut m = pb::Config::default();
    let both = ServerFlags {
        address: Some(vec!["10.0.0.2:4433".parse().unwrap()]),
        relay: Some(irsync::config::settings::Relay::Disabled),
        ..Default::default()
    };
    let e = server_add(&mut m, "home", id(1), &both).unwrap_err();
    assert!(
        e.to_string().contains("relay cannot be set beside address"),
        "{e}"
    );
    let mut m = pb::Config::default();
    let dht = ServerFlags {
        dht: Some(false),
        ..Default::default()
    };
    server_add(&mut m, "home", id(1), &dht).unwrap();
    let address = ServerFlags {
        address: Some(vec!["10.0.0.2:4433".parse().unwrap()]),
        ..Default::default()
    };
    let e = server_add(&mut m, "home", id(1), &address).unwrap_err();
    assert!(
        e.to_string().contains("dht cannot be set beside address"),
        "{e}"
    );
}
