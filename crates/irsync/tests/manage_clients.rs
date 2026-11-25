use irsync::authz::Access;
use irsync::config::clients::Right;
use irsync::config::reader;
use irsync::config::writer::write;
use irsync::manage::clients::{
    client_add, client_list, client_names, client_remove, client_rename, client_set, parse_setting,
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

fn named_everywhere() -> String {
    format!(
        "[clients.box]\nclient_id = \"{}\"\n[exports.a]\npath = \"/a\"\nrw = [\"box\", \"x\"]\n\
         [exports.b]\npath = \"/b\"\nrw = [\"box\"]\n\
         [exports.p]\npath = \"/p\"\nro = [\"y\", \"box\"]\n\
         [exports.q]\npath = \"/q\"\nrw = [\"y\"]\n",
        id(1)
    )
}

#[test]
fn add_creates_and_updates() {
    let mut m = pb::Config::default();
    assert_eq!(client_add(&mut m, "c", id(1)).unwrap(), None);
    assert_eq!(
        write(&m),
        format!("[clients.c]\nclient_id = \"{}\"\n", id(1))
    );
    assert_eq!(client_add(&mut m, "c", id(2)).unwrap(), Some(id(1)));
    assert_eq!(client_add(&mut m, "c", id(2)).unwrap(), None);
    assert_eq!(m.clients.len(), 1);
    assert_eq!(client_list(&m), vec![format!("c: {}", id(2))]);
}

#[test]
fn add_refuses_an_invalid_name() {
    let mut m = pb::Config::default();
    for bad in ["a:b", "a/b", "", "all", "*", &id(1).to_string()] {
        assert!(
            matches!(client_add(&mut m, bad, id(1)), Err(ManageError::BadName(_))),
            "{bad:?}"
        );
    }
    assert!(m.clients.is_empty());
}

#[test]
fn remove_without_grants() {
    let mut m = msg(&format!(
        "[clients.box]\nclient_id = \"{}\"\n[exports.p]\npath = \"/p\"\nrw = [\"other\"]\n",
        id(1)
    ));
    assert_eq!(client_remove(&mut m, "box", false).unwrap(), 0);
    assert_eq!(write(&m), "[exports.p]\npath = \"/p\"\nrw = [\"other\"]\n");
    assert!(matches!(
        client_remove(&mut m, "box", false),
        Err(ManageError::NotFound(_))
    ));
}

#[test]
fn remove_is_refused_while_grants_name_the_client() {
    let mut m = msg(&named_everywhere());
    let before = write(&m);
    let e = client_remove(&mut m, "box", false).unwrap_err();
    assert!(matches!(&e, ManageError::ClientInUse { name, lists }
        if name == "box" && lists == &["exports.a", "exports.b", "exports.p"]));
    assert_eq!(
        e.to_string(),
        "client \"box\" is named by grants in exports.a, exports.b, exports.p: \
         remove those grants first, or pass --force"
    );
    assert_eq!(write(&m), before);
}

#[test]
fn force_removes_the_grants_then_the_client() {
    let mut m = msg(&named_everywhere());
    assert_eq!(client_remove(&mut m, "box", true).unwrap(), 3);
    assert!(m.clients.is_empty());
    assert_eq!(
        write(&m),
        "[exports.a]\npath = \"/a\"\nrw = [\"x\"]\n\n\
         [exports.b]\npath = \"/b\"\nrw = []\n\n\
         [exports.p]\npath = \"/p\"\nro = [\"y\"]\n\n\
         [exports.q]\npath = \"/q\"\nrw = [\"y\"]\n"
    );
}

#[test]
fn rename_renames_the_client_and_every_grant() {
    let mut m = msg(&named_everywhere());
    assert_eq!(client_rename(&mut m, "box", "crate").unwrap(), 3);
    assert_eq!(
        write(&m),
        write(&msg(&named_everywhere().replace("box", "crate")))
    );
    assert_eq!(client_names(&m), ["crate"]);
}

#[test]
fn rename_keeps_place_and_comments() {
    let mut m = msg(&format!(
        "[clients.a]\nclient_id = \"{}\"\n\n# the old box\n[clients.box] # header\n\
         client_id = \"{}\" # id\n",
        id(1),
        id(2)
    ));
    client_rename(&mut m, "box", "crate").unwrap();
    assert_eq!(client_names(&m), ["a", "crate"]);
    let s = write(&m);
    assert!(s.contains("# the old box\n[clients.crate] # header"), "{s}");
    assert!(s.contains("# id"), "{s}");
}

#[test]
fn rename_to_an_existing_name_or_bad_name_is_refused() {
    let mut m = msg(&format!(
        "{}[clients.taken]\nclient_id = \"{}\"\n",
        named_everywhere(),
        id(2)
    ));
    let before = write(&m);
    let e = client_rename(&mut m, "box", "taken").unwrap_err();
    assert!(matches!(&e, ManageError::ClientExists(n) if n == "taken"));
    assert_eq!(e.to_string(), "client \"taken\" already exists");
    assert!(matches!(
        client_rename(&mut m, "box", "a:b"),
        Err(ManageError::BadName(_))
    ));
    assert!(matches!(
        client_rename(&mut m, "nobody", "c"),
        Err(ManageError::NotFound(_))
    ));
    assert_eq!(write(&m), before);
}

#[test]
fn rename_to_itself_changes_nothing() {
    let mut m = msg(&named_everywhere());
    let before = write(&m);
    assert_eq!(client_rename(&mut m, "box", "box").unwrap(), 0);
    assert_eq!(write(&m), before);
}

#[test]
fn list_marks_a_missing_or_invalid_id() {
    let m = pb::Config {
        clients: vec![
            pb::ClientEntry {
                name: "none".into(),
                ..Default::default()
            },
            pb::ClientEntry {
                name: "bad".into(),
                client_id: vec![1, 2, 3],
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    assert_eq!(client_list(&m), ["none: (missing)", "bad: (invalid)"]);
}

fn client_file() -> pb::Config {
    reader::read(
        &format!("[clients.laptop]\nclient_id = \"{}\"\n", id(1)),
        Path::new("f"),
    )
    .unwrap()
}

#[test]
fn client_set_sets_and_removes_rights() {
    let mut m = client_file();
    client_set(
        &mut m,
        "laptop",
        &[
            (Right::AllExports, Some(Access::ReadWrite)),
            (Right::AllPaths, Some(Access::ReadOnly)),
        ],
    )
    .unwrap();
    let text = write(&m);
    assert!(text.contains("all_exports = \"rw\""), "{text}");
    assert!(text.contains("all_paths = \"ro\""), "{text}");
    client_set(&mut m, "laptop", &[(Right::AllPaths, None)]).unwrap();
    let text = write(&m);
    assert!(text.contains("all_exports = \"rw\""), "{text}");
    assert!(!text.contains("all_paths"), "{text}");
}

#[test]
fn client_set_on_an_unknown_client_is_refused() {
    let mut m = client_file();
    let before = write(&m);
    assert!(matches!(
        client_set(&mut m, "nope", &[(Right::AllPaths, Some(Access::ReadWrite))]),
        Err(ManageError::NotFound(n)) if n == "nope"
    ));
    assert_eq!(write(&m), before);
}

#[test]
fn parse_setting_takes_rw_ro_none() {
    assert_eq!(
        parse_setting("all_paths=ro").unwrap(),
        (Right::AllPaths, Some(Access::ReadOnly))
    );
    assert_eq!(
        parse_setting("all_exports=rw").unwrap(),
        (Right::AllExports, Some(Access::ReadWrite))
    );
    assert_eq!(
        parse_setting("all_paths=none").unwrap(),
        (Right::AllPaths, None)
    );
    for bad in [
        "all_paths=yes",
        "admin=yes",
        "all_paths",
        "all_paths=",
        "=rw",
    ] {
        assert!(
            matches!(parse_setting(bad), Err(ManageError::BadValue(_))),
            "{bad:?}"
        );
    }
}

#[test]
fn client_list_shows_rights() {
    let mut m = client_file();
    client_set(
        &mut m,
        "laptop",
        &[
            (Right::AllExports, Some(Access::ReadWrite)),
            (Right::AllPaths, Some(Access::ReadOnly)),
        ],
    )
    .unwrap();
    assert_eq!(
        client_list(&m),
        vec![format!("laptop: {} all_exports=rw all_paths=ro", id(1))]
    );
    client_set(&mut m, "laptop", &[(Right::AllExports, None)]).unwrap();
    assert_eq!(
        client_list(&m),
        vec![format!("laptop: {} all_paths=ro", id(1))]
    );
}

#[test]
fn rights_alone_do_not_hold_a_client_back_from_removal() {
    let mut m = client_file();
    client_set(
        &mut m,
        "laptop",
        &[(Right::AllPaths, Some(Access::ReadWrite))],
    )
    .unwrap();
    assert_eq!(client_remove(&mut m, "laptop", false).unwrap(), 0);
    assert!(m.clients.is_empty());
}

#[test]
fn client_set_takes_admin() {
    assert_eq!(
        parse_setting("admin=rw").unwrap(),
        (Right::Admin, Some(Access::ReadWrite))
    );
    let mut m = client_file();
    client_set(&mut m, "laptop", &[(Right::Admin, Some(Access::ReadWrite))]).unwrap();
    assert_eq!(client_list(&m), vec![format!("laptop: {} admin=rw", id(1))]);
}
