//! The admin engine: a transaction of changes applied all or nothing on
//! a config message, the self-protection of the committing client, and
//! the reads.

use irsync::authz::Access;
use irsync::config::reader::read;
use irsync::manage::admin::{admin_of, apply, config_text, summary, view, Committer};
use irsync_proto::config as pb;
use irsync_proto::v1;
use irsync_proto::v1::change::Op;
use std::collections::BTreeMap;
use std::path::Path;

const FILE: &str = "/srv/irsync.toml";

fn id(n: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
}

fn msg(text: &str) -> pb::Config {
    read(text, Path::new(FILE)).unwrap()
}

/// "me" (id 1) with the given admin line, "laptop" (id 2) without rights.
fn config_with(admin: &str) -> pb::Config {
    msg(&format!(
        "[clients.me]\nclient_id = \"{}\"\n{admin}\n[clients.laptop]\nclient_id = \"{}\"\n",
        id(1),
        id(2)
    ))
}

fn me() -> Committer {
    Committer { id: id(1) }
}

fn change(op: Op) -> v1::Change {
    v1::Change { op: Some(op) }
}

fn export_add(target: &str) -> v1::Change {
    change(Op::ExportAdd(v1::ExportAdd {
        target: target.to_string(),
        read_only: false,
        index_at_start: false,
    }))
}

fn client_set(name: &str, settings: &[&str]) -> v1::Change {
    change(Op::ClientSet(v1::ClientSet {
        name: name.to_string(),
        settings: settings.iter().map(|s| s.to_string()).collect(),
    }))
}

fn run(m: &mut pb::Config, changes: &[v1::Change]) -> Result<Vec<String>, v1::Nack> {
    apply(m, Path::new(FILE), None, &me(), changes)
}

#[test]
fn a_transaction_applies_every_change_in_order() {
    let d = tempfile::tempdir().unwrap();
    let mut m = config_with("admin = \"rw\"");
    let lines = run(
        &mut m,
        &[
            export_add(&format!("photos:{}", d.path().display())),
            change(Op::GrantAdd(v1::GrantAdd {
                export: "photos".into(),
                who: "laptop".into(),
                ro: true,
            })),
        ],
    )
    .unwrap();
    assert_eq!(
        lines,
        [
            format!("added export photos ({}) to {FILE}", d.path().display()),
            "laptop: ro on photos".to_string(),
        ]
    );
    let e = m.exports.iter().find(|e| e.name == "photos").unwrap();
    assert_eq!(e.path, d.path().to_string_lossy());
    let ro: Vec<&str> = e.ro.iter().map(|e| e.value.as_str()).collect();
    assert_eq!(ro, ["laptop"]);
    assert!(e.rw.is_empty());
}

#[test]
fn a_failing_last_change_applies_nothing() {
    let d = tempfile::tempdir().unwrap();
    let original = config_with("admin = \"rw\"");
    let mut m = original.clone();
    let nack = run(
        &mut m,
        &[
            export_add(&format!("a:{}", d.path().display())),
            change(Op::ClientRemove(v1::ClientRemove {
                name: "nobody".into(),
                force: false,
            })),
        ],
    )
    .unwrap_err();
    assert_eq!(nack.change, 2);
    assert!(nack.reason.contains("nobody"), "{}", nack.reason);
    // Neither the message handed in nor the caller's copy holds the first change.
    assert_eq!(m, original);
    assert!(m.exports.is_empty());
}

#[test]
fn a_client_without_admin_rw_is_not_allowed() {
    let d = tempfile::tempdir().unwrap();
    let add = [export_add(&format!("a:{}", d.path().display()))];
    for admin in ["admin = \"ro\"", ""] {
        let original = config_with(admin);
        let mut m = original.clone();
        let nack = run(&mut m, &add).unwrap_err();
        assert_eq!(
            (nack.change, nack.reason.as_str()),
            (0, "not allowed"),
            "{admin}"
        );
        assert_eq!(m, original);
    }
    // A key that is no client at all.
    let mut m = config_with("admin = \"rw\"");
    let nack = apply(
        &mut m,
        Path::new(FILE),
        None,
        &Committer { id: id(7) },
        &add,
    )
    .unwrap_err();
    assert_eq!((nack.change, nack.reason.as_str()), (0, "not allowed"));
}

#[test]
fn admin_of_is_the_highest_over_the_entries_with_that_id() {
    let m = msg(&format!(
        "[clients.a]\nclient_id = \"{0}\"\nadmin = \"ro\"\n\
         [clients.b]\nclient_id = \"{0}\"\nadmin = \"rw\"\n\
         [clients.c]\nclient_id = \"{1}\"\nadmin = \"ro\"\n",
        id(1),
        id(2)
    ));
    let p = Path::new(FILE);
    assert_eq!(admin_of(&m, p, &id(1)), Some(Access::ReadWrite));
    assert_eq!(admin_of(&m, p, &id(2)), Some(Access::ReadOnly));
    assert_eq!(admin_of(&m, p, &id(3)), None);
}

#[test]
fn the_committer_cannot_take_its_own_admin_away() {
    let refused = [
        change(Op::ClientRemove(v1::ClientRemove {
            name: "me".into(),
            force: true,
        })),
        change(Op::ClientRename(v1::ClientRename {
            old: "me".into(),
            new: "other".into(),
        })),
        client_set("me", &["admin=ro"]),
        client_set("me", &["admin=none"]),
        change(Op::ClientAdd(v1::ClientAdd {
            name: "me".into(),
            id: id(3).as_bytes().to_vec(),
        })),
    ];
    for c in refused {
        let original = config_with("admin = \"rw\"");
        let mut m = original.clone();
        let nack = run(&mut m, std::slice::from_ref(&c)).unwrap_err();
        assert_eq!(nack.change, 0, "{c:?}");
        assert_eq!(
            nack.reason, "this would take away your own admin right",
            "{c:?}"
        );
        assert_eq!(m, original, "{c:?}");
    }
    let mut m = config_with("admin = \"rw\"");
    assert_eq!(
        run(&mut m, &[client_set("me", &["all_paths=ro"])]).unwrap(),
        ["me: all_exports=none all_paths=ro admin=rw"]
    );
    // What counts is the end result, whatever the order of the changes.
    let mut m = config_with("admin = \"rw\"");
    run(
        &mut m,
        &[
            client_set("me", &["admin=none"]),
            client_set("me", &["admin=rw"]),
        ],
    )
    .unwrap();
    let mut m = config_with("admin = \"rw\"");
    let nack = run(
        &mut m,
        &[
            client_set("me", &["admin=rw"]),
            client_set("laptop", &["admin=rw"]),
            client_set("me", &["admin=ro"]),
        ],
    )
    .unwrap_err();
    assert_eq!(nack.change, 0);
}

#[test]
fn export_add_needs_an_absolute_or_home_path() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join("Photos")).unwrap();
    for target in ["photos:Photos", "photos:./Photos", "photos:~nobody/Photos"] {
        let mut m = config_with("admin = \"rw\"");
        let nack = apply(
            &mut m,
            Path::new(FILE),
            Some(home.path()),
            &me(),
            &[export_add(target)],
        )
        .unwrap_err();
        assert_eq!(nack.change, 1, "{target}");
        assert!(
            nack.reason
                .contains("the path must be absolute or start with ~"),
            "{target}: {}",
            nack.reason
        );
    }
    let mut m = config_with("admin = \"rw\"");
    let lines = apply(
        &mut m,
        Path::new(FILE),
        Some(home.path()),
        &me(),
        &[export_add("photos:~/Photos")],
    )
    .unwrap();
    assert_eq!(lines, [format!("added export photos (~/Photos) to {FILE}")]);
    assert_eq!(m.exports[0].path, "~/Photos");
    // A path under ~ must be a directory there too.
    let mut m = config_with("admin = \"rw\"");
    let nack = apply(
        &mut m,
        Path::new(FILE),
        Some(home.path()),
        &me(),
        &[export_add("music:~/Music")],
    )
    .unwrap_err();
    assert_eq!(nack.change, 1);
    assert!(nack.reason.contains("not a directory"), "{}", nack.reason);
}

#[test]
fn grant_add_does_not_read_files_on_the_server() {
    let d = tempfile::tempdir().unwrap();
    let key = d.path().join("k.pub");
    let line = irsync::identity::public_key_line(&iroh::SecretKey::from_bytes(&[9; 32]), "x");
    std::fs::write(&key, format!("{line}\n")).unwrap();
    let mut m = config_with("admin = \"rw\"");
    let nack = run(
        &mut m,
        &[
            export_add(&format!("a:{}", d.path().display())),
            change(Op::GrantAdd(v1::GrantAdd {
                export: "a".into(),
                who: key.to_string_lossy().into_owned(),
                ro: false,
            })),
        ],
    )
    .unwrap_err();
    assert_eq!(nack.change, 2, "{}", nack.reason);
    // The key line itself is taken.
    let mut m = config_with("admin = \"rw\"");
    run(
        &mut m,
        &[
            export_add(&format!("a:{}", d.path().display())),
            change(Op::GrantAdd(v1::GrantAdd {
                export: "a".into(),
                who: line,
                ro: false,
            })),
        ],
    )
    .unwrap();
}

#[test]
fn grant_remove_refuses_as_the_cli_does() {
    let mut m = config_with("admin = \"rw\"");
    let nack = run(
        &mut m,
        &[change(Op::GrantRemove(v1::GrantRemove {
            export: "photos".into(),
            who: "*".into(),
        }))],
    )
    .unwrap_err();
    assert_eq!(nack.change, 1);
    assert!(nack.reason.contains("names no peer"), "{}", nack.reason);
}

#[test]
fn view_lists_exports_and_clients_with_rights() {
    let m = msg(&format!(
        "[clients.me]\nclient_id = \"{}\"\nadmin = \"rw\"\n\
         [clients.laptop]\nclient_id = \"{}\"\nall_paths = \"ro\"\nall_exports = \"rw\"\n\
         [clients.nas]\nclient_id = \"{}\"\n\
         [exports.photos]\npath = \"/srv/photos\"\nread_only = true\nrw = [\"me\"]\nro = [\"laptop\"]\n",
        id(1),
        id(2),
        id(3)
    ));
    let v = view(&m, id(9), true, None);
    assert_eq!(v.daemon_id, id(9).as_bytes().to_vec());
    assert!(v.allow_all_paths);
    assert_eq!(
        v.exports,
        [v1::ExportView {
            name: "photos".into(),
            path: "/srv/photos".into(),
            read_only: true,
            index_at_start: false,
            fsync: false,
            is_auto: false,
            rw: vec!["me".into()],
            ro: vec!["laptop".into()],
        }]
    );
    let rights = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    assert_eq!(
        v.clients,
        [
            v1::ClientView {
                name: "me".into(),
                id: id(1).as_bytes().to_vec(),
                rights: rights(&[("admin", "rw")]),
            },
            v1::ClientView {
                name: "laptop".into(),
                id: id(2).as_bytes().to_vec(),
                rights: rights(&[("all_exports", "rw"), ("all_paths", "ro")]),
            },
            v1::ClientView {
                name: "nas".into(),
                id: id(3).as_bytes().to_vec(),
                rights: rights(&[]),
            },
        ]
    );
}

#[test]
fn view_expands_a_tilde_path_with_the_daemons_home() {
    let m = msg("[exports.home]\npath = \"~\"\n[exports.photos]\npath = \"~/Photos\"\n");
    let paths = |home: Option<&Path>| -> Vec<String> {
        view(&m, id(9), false, home)
            .exports
            .into_iter()
            .map(|e| e.path)
            .collect()
    };
    assert_eq!(
        paths(Some(Path::new("/srv/daemon"))),
        ["/srv/daemon", "/srv/daemon/Photos"]
    );
    assert_eq!(paths(None), ["~", "~/Photos"]);
}

#[test]
fn config_text_gets_and_lists() {
    let m = config_with("admin = \"rw\"");
    // Unset and without a default: no lines, as irsync config get prints
    // none (and exits 1).
    assert!(config_text(&m, "daemon.debug_addr")
        .unwrap()
        .lines
        .is_empty());
    // Unset with a default: the default.
    assert_eq!(config_text(&m, "daemon.portmap").unwrap().lines, ["true"]);
    let set = msg("[daemon]\nallow_all_paths = false\n");
    assert_eq!(
        config_text(&set, "daemon.allow_all_paths").unwrap().lines,
        ["false"]
    );
    assert_eq!(
        config_text(&m, "daemon.allow_all_paths").unwrap().lines,
        ["true"]
    );
    assert_eq!(
        config_text(&m, "").unwrap().lines,
        irsync::manage::config::list(&m)
    );
    assert_eq!(config_text(&m, "clients.me.admin").unwrap().lines, ["rw"]);
    let e = config_text(&m, "nosuch.key").unwrap_err();
    assert!(e.contains("unknown key"), "{e}");
}

#[test]
fn summaries_say_what_a_change_does() {
    assert_eq!(
        summary(&export_add("photos:~/Photos")),
        "added export photos at ~/Photos"
    );
    assert_eq!(
        summary(&client_set("laptop", &["admin=ro"])),
        "set laptop admin=ro"
    );
    // Text from the client cannot start a new log line.
    assert_eq!(
        summary(&client_set("laptop\nforged", &["admin=ro"])),
        "set laptop\\nforged admin=ro"
    );
}
