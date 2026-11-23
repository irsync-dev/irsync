use irsync::config::legacy_v1;
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

/// A message from a file of layout 1 (`[hosts]`), which `legacy_v1` splits
/// into servers and clients; `config_version` is cleared so `write` does
/// not print it.
fn msg(text: &str) -> pb::Config {
    let mut m = legacy_v1::read(text, Path::new("f")).unwrap().config;
    m.config_version = None;
    m
}
use std::time::Duration;

/// A server and a client of the same name, and grants naming the client.
fn both() -> String {
    format!(
        "[hosts.box]\nendpoint_id = \"{}\"\nclient_id = \"{}\"\n[all]\nrw = [\"box\"]\n",
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
                Err(ManageError::BadAlias(_))
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
    assert_eq!(m.all.as_ref().map(|a| a.rw.len()), Some(1));
    assert!(matches!(
        server_remove(&mut m, "box"),
        Err(ManageError::NotFound(_))
    ));
}

#[test]
fn rename_keeps_place_and_comments_and_touches_no_grant() {
    let mut m = msg(&format!(
        "[hosts.a]\nendpoint_id = \"{}\"\n\n# the old box\n[hosts.box] # header\n\
         endpoint_id = \"{}\" # id\n[hosts.c]\nclient_id = \"{}\"\n[all]\nrw = [\"c\"]\n",
        id(1),
        id(2),
        id(3)
    ));
    let before = write(&m);
    server_rename(&mut m, "box", "crate").unwrap();
    assert_eq!(server_names(&m), ["a", "crate"]);
    assert_eq!(m.clients[0].name, "c");
    assert_eq!(m.all.as_ref().unwrap().rw[0].value, "c");
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
        "[hosts.a]\nendpoint_id = \"{}\"\n[hosts.b]\nendpoint_id = \"{}\"\n",
        id(1),
        id(2)
    ));
    let before = write(&m);
    let e = server_rename(&mut m, "a", "b").unwrap_err();
    assert!(matches!(&e, ManageError::ServerExists(n) if n == "b"));
    assert_eq!(e.to_string(), "server \"b\" already exists");
    assert!(matches!(
        server_rename(&mut m, "a", "a:b"),
        Err(ManageError::BadAlias(_))
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
