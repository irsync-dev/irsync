#[path = "common/fixture.rs"]
mod fixture;

use irsync::authz::Access;
use irsync::config::file::Config;
use irsync::config::reader;
use irsync::config::writer::write;
use irsync::manage::exports::parse_add_target;
use irsync::manage::exports::{
    add_auto_export, add_export, export_set, parse_setting, remove_export, rename_export, set_line,
    ExportFlags,
};
use irsync::manage::{ClientSpec, ManageError};
use irsync_proto::config as pb;
use std::path::Path;

fn id(n: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
}
fn msg(text: &str) -> pb::Config {
    reader::read(&fixture::toml(text), Path::new("f")).unwrap()
}
fn typed(m: &pb::Config) -> Config {
    Config::from_message(m, Path::new("f")).unwrap()
}

#[test]
fn add_writes_only_what_was_asked_updates_an_existing_one_and_refuses_reserved() {
    let mut m = pb::Config::default();
    add_export(
        &mut m,
        "photos",
        &fixture::abs_path("/srv/photos"),
        ExportFlags::default(),
    )
    .unwrap();
    add_export(
        &mut m,
        "docs",
        &fixture::abs_path("/srv/docs"),
        ExportFlags {
            read_only: true,
            index_at_start: true,
            fsync: true,
        },
    )
    .unwrap();
    let x = typed(&m).exports;
    assert!(!x.exports["photos"].read_only && !x.exports["photos"].index_at_start);
    assert!(x.exports["docs"].read_only && x.exports["docs"].index_at_start);
    assert_eq!(m.exports[0].read_only, None);
    assert_eq!(m.exports[1].read_only, Some(true));
    assert!(
        !write(&m).contains("read_only = false"),
        "only true flags are written:\n{}",
        write(&m)
    );
    assert!(typed(&m).exports.exports["docs"].fsync);
    // An existing name: the new path, the flags given turned on, the
    // rest as it was.
    let old = add_export(
        &mut m,
        "docs",
        &fixture::abs_path("/x"),
        ExportFlags {
            fsync: true,
            ..ExportFlags::default()
        },
    )
    .unwrap();
    assert_eq!(old.as_deref(), fixture::abs_path("/srv/docs").to_str());
    let docs = &typed(&m).exports.exports["docs"];
    assert_eq!(docs.path, fixture::abs_path("/x"));
    assert!(docs.read_only && docs.index_at_start && docs.fsync);
    assert!(matches!(
        add_export(&mut m, "all", &fixture::abs_path("/x"), ExportFlags::default()),
        Err(ManageError::Reserved(_))
    ));
}

#[test]
fn set_and_rename_change_one_export_and_keep_its_grants() {
    let mut m = msg("[exports.p]\npath = \"/p\"\nrw = [\"x\"]\n[exports.q]\npath = \"/q\"\n");
    let settings: Vec<_> = ["read_only=true", "fsync=true"]
        .iter()
        .map(|s| parse_setting(s).unwrap())
        .collect();
    export_set(&mut m, "p", &settings).unwrap();
    assert_eq!(
        set_line(&m, "p").unwrap(),
        "p: read_only=true index_at_start=false fsync=true"
    );
    export_set(&mut m, "p", &[parse_setting("read_only=false").unwrap()]).unwrap();
    assert_eq!(m.exports[0].read_only, None, "false is the default: no key");
    for bad in ["read_only", "rw=true", "fsync=yes"] {
        assert!(
            matches!(parse_setting(bad), Err(ManageError::BadValue(_))),
            "{bad}"
        );
    }
    assert!(matches!(
        export_set(&mut m, "nope", &settings),
        Err(ManageError::NotFound(_))
    ));

    rename_export(&mut m, "p", "photos").unwrap();
    let x = typed(&m).exports;
    assert_eq!(x.exports["photos"].rw, vec!["x"]);
    assert!(x.exports["photos"].fsync && !x.exports.contains_key("p"));
    assert_eq!(m.exports[0].name, "photos", "it keeps its place");
    for (old, new) in [
        ("nope", "z"),
        ("photos", "q"),
        ("photos", "all"),
        ("photos", "a/b"),
        ("photos", ""),
    ] {
        assert!(rename_export(&mut m, old, new).is_err(), "{old} -> {new}");
    }
}

#[test]
fn add_writes_under_exports_as_a_dotted_header_and_refuses_all() {
    let mut m = pb::Config::default();
    add_export(
        &mut m,
        "photos",
        &fixture::abs_path("/srv/photos"),
        ExportFlags::default(),
    )
    .unwrap();
    assert_eq!(
        write(&m),
        fixture::toml("[exports.photos]\npath = \"/srv/photos\"\n")
    );
    let e = add_export(&mut m, "all", &fixture::abs_path("/srv/all"), ExportFlags::default()).unwrap_err();
    assert_eq!(
        e.to_string(),
        "\"all\" is reserved: it is the word `grant remove all WHO` and `index --export all` use for every export"
    );
    assert!(add_export(&mut m, "rw", &fixture::abs_path("/srv/rw"), ExportFlags::default()).is_ok());
}

#[test]
fn a_new_export_goes_last_and_the_path_stays_as_written() {
    let mut m = msg("[exports.b]\npath = \"/b\"\n[exports.a]\npath = \"/a\"\n");
    add_export(&mut m, "c", Path::new("~/c"), ExportFlags::default()).unwrap();
    let names: Vec<_> = m.exports.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["b", "a", "c"]);
    assert_eq!(m.exports[2].path, "~/c");
}

#[test]
fn remove_last_export_leaves_a_loadable_file() {
    let mut m = msg(&format!(
        "[clients.x]\nclient_id = \"{}\"\nall_exports = \"rw\"\n[exports.a]\npath = \"/a\"\n",
        id(1)
    ));
    remove_export(&mut m, "a").unwrap();
    let c = typed(&m);
    assert!(c.exports.exports.is_empty() && c.clients.get("x").is_some());
    assert!(matches!(
        remove_export(&mut m, "all"),
        Err(ManageError::NotFound(_))
    ));
}

#[test]
fn remove_drops_table_and_its_grants() {
    let mut m = msg("[exports.a]\npath = \"/a\"\nrw = [\"x\"]\n[exports.b]\npath = \"/b\"\n");
    remove_export(&mut m, "a").unwrap();
    assert_eq!(write(&m), fixture::toml("[exports.b]\npath = \"/b\"\n"));
    assert!(matches!(
        remove_export(&mut m, "a"),
        Err(ManageError::NotFound(_))
    ));
}

#[test]
fn remove_only_removes_an_export() {
    let text = format!(
        "[clients.c]\nclient_id = \"{}\"\nall_exports = \"rw\"\n[exports.p]\npath = \"/p\"\nro = []\n",
        id(1)
    );
    let mut m = msg(&text);
    for r in ["all", "exports", "rw"] {
        assert!(
            matches!(remove_export(&mut m, r), Err(ManageError::NotFound(_))),
            "{r}"
        );
    }
    // Nothing was removed: the message is as it was read.
    assert_eq!(m, msg(&text));
}

#[test]
fn auto_export_names() {
    let mut m = pb::Config::default();
    let mut n =
        |p: &str| add_auto_export(&mut m, &fixture::abs_path(p), id(1), Access::ReadWrite).unwrap();
    assert_eq!(n("/srv/photos"), "auto-srv-photos");
    assert_eq!(n("/"), "auto-root");
    assert_eq!(n("/a b/\u{e7}"), "auto-a_b-__");
    assert_eq!(n("/srv/photos"), "auto-srv-photos-2");
    let e = &m.exports[0];
    assert_eq!(e.auto, Some(true));
    assert!(e.rw_set);
    assert_eq!(e.rw[0].value, id(1).to_string());
    assert!(typed(&m).exports.exports["auto-srv-photos"].auto);
    let text = write(&m);
    let want = fixture::toml(&format!(
        "[exports.auto-srv-photos]\npath = \"/srv/photos\"\nauto = true\nrw = [\"{}\"]\n",
        id(1)
    ));
    assert!(text.contains(&want), "{text}");
}

#[test]
fn auto_export_for_read_only_lists_the_peer_under_ro() {
    let mut m = pb::Config::default();
    add_auto_export(&mut m, Path::new("/srv/photos"), id(1), Access::ReadOnly).unwrap();
    let text = write(&m);
    let want = format!(
        "[exports.auto-srv-photos]\npath = \"/srv/photos\"\nauto = true\nro = [\"{}\"]\n",
        id(1)
    );
    assert!(text.contains(&want), "{text}");
}

#[test]
fn add_target_forms() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("docs")).unwrap();
    std::fs::write(d.path().join("f"), b"").unwrap();
    assert_eq!(
        parse_add_target("photos:docs", d.path()).unwrap(),
        ("photos".into(), d.path().join("docs"))
    );
    assert_eq!(
        parse_add_target("docs", d.path()).unwrap(),
        ("docs".into(), d.path().join("docs"))
    );
    assert_eq!(
        parse_add_target(
            d.path().join("docs").to_str().unwrap(),
            Path::new("/elsewhere")
        )
        .unwrap()
        .0,
        "docs"
    );
    assert!(matches!(
        parse_add_target("x:f", d.path()),
        Err(ManageError::NotADirectory(_))
    ));
    assert!(matches!(
        parse_add_target("x:missing", d.path()),
        Err(ManageError::NotADirectory(_))
    ));
}

/// A directory name with a colon: Unix only, as NTFS holds no `:` in a name.
#[cfg(unix)]
#[test]
fn add_target_of_an_absolute_path_with_a_colon_in_a_name() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("a:b")).unwrap();
    let abs = d.path().join("a:b");
    assert_eq!(
        parse_add_target(abs.to_str().unwrap(), Path::new("/elsewhere")).unwrap(),
        ("a:b".into(), abs)
    );
}

/// On Windows a drive path is a bare `PATH`, never `NAME:PATH`; on Unix
/// `C:` before the path is a name.
#[test]
fn a_drive_path_is_a_bare_path_on_windows_only() {
    use irsync::manage::exports::parse_add_target_on;
    use irsync::platform::Platform::{Unix, Windows};
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("srv").join("photos")).unwrap();
    let photos = d.path().join("srv").join("photos");
    let abs = photos.to_str().unwrap();
    if cfg!(windows) {
        assert_eq!(
            parse_add_target_on(abs, Path::new("/elsewhere"), Windows).unwrap(),
            ("photos".into(), photos.clone())
        );
        let slashed = abs.replace('\\', "/");
        assert_eq!(
            parse_add_target_on(&slashed, Path::new("/elsewhere"), Windows)
                .unwrap()
                .0,
            "photos"
        );
        assert_eq!(
            parse_add_target_on(&format!("p:{abs}"), Path::new("/elsewhere"), Windows).unwrap(),
            ("p".into(), photos.clone())
        );
    }
    // Read as on Unix, `C:` before a path is a name, wherever this runs.
    let cwd = d.path().join("srv");
    assert_eq!(
        parse_add_target_on("C:photos", &cwd, Unix).unwrap(),
        ("C".into(), photos.clone())
    );
    assert_eq!(
        parse_add_target_on("C:photos", &cwd, Windows).unwrap(),
        ("C".into(), photos)
    );
}

#[test]
fn add_target_edge_cases() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("sub/x")).unwrap();
    assert!(matches!(
        parse_add_target(":docs", d.path()),
        Err(ManageError::BadExportName(_))
    ));
    let sub = d.path().join("sub");
    assert_eq!(
        parse_add_target("..", &sub.join("x")).unwrap(),
        ("sub".into(), sub.clone())
    );
    assert_eq!(
        parse_add_target("./x", &sub).unwrap(),
        ("x".into(), sub.join("x"))
    );
    assert_eq!(
        parse_add_target("../sub/x", &sub).unwrap(),
        ("x".into(), sub.join("x"))
    );
}

#[test]
fn add_target_expands_a_leading_tilde() {
    let home = dirs::home_dir().unwrap();
    let (name, path) = parse_add_target("home:~", Path::new("/cwd")).unwrap();
    assert_eq!((name.as_str(), path), ("home", home.clone()));
    let (_, path) = parse_add_target("x:~/", Path::new("/cwd")).unwrap();
    assert_eq!(path, home);
    // `~x` is a plain name, not a user.
    assert!(matches!(
        parse_add_target("x:~nobody", Path::new("/cwd")),
        Err(ManageError::NotADirectory(_))
    ));
}

const INJECTED: &str = "laptop\n]\n[etc]\npath = \"/etc\"\nrw = [\"x\"";

#[test]
fn peer_spec_refuses_a_key_line_whose_comment_has_a_newline() {
    let c = irsync::config::file::Config::parse("", Path::new("h")).unwrap();
    let line = irsync::identity::public_key_line(&iroh::SecretKey::from_bytes(&[7; 32]), "x");
    let key = line.rsplit_once(' ').unwrap().0;
    let r = ClientSpec::parse(&format!("{key} {INJECTED}"), &c.clients, &c.servers);
    assert!(matches!(r, Err(ManageError::BadComment(_))), "{r:?}");
    let msg = r.unwrap_err().to_string();
    assert!(msg.contains("control characters"), "{msg}");
    // A file holding such a line is refused the same way.
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("k.pub");
    std::fs::write(&f, format!("{key} a\x07b\n")).unwrap();
    assert!(matches!(
        ClientSpec::parse(f.to_str().unwrap(), &c.clients, &c.servers),
        Err(ManageError::BadComment(_))
    ));
}

#[test]
fn an_edit_that_makes_the_message_invalid_is_refused_on_load() {
    let mut m = msg("[exports.p]\npath = \"/p\"\n");
    assert!(Config::from_message(&m, Path::new("f")).is_ok());
    m.exports[0].path = "rel/p".to_string();
    let e = Config::from_message(&m, Path::new("f")).unwrap_err();
    assert_eq!(
        e.to_string(),
        "f: [exports.p]: path \"rel/p\" must be absolute"
    );
}

#[test]
fn add_and_remove_keep_an_inline_exports_table() {
    let mut m = msg("exports = { photos = { path = \"/p\", rw = [\"x\"] } }\n");
    add_export(&mut m, "q", &fixture::abs_path("/q"), ExportFlags::default()).unwrap();
    let c = typed(&m).exports;
    assert_eq!(c.exports["photos"].rw, vec!["x"]);
    assert_eq!(c.exports["q"].path, fixture::abs_path("/q"));
    assert_eq!(
        write(&m),
        fixture::toml(
            "[exports.photos]\npath = \"/p\"\nrw = [\"x\"]\n\n[exports.q]\npath = \"/q\"\n"
        )
    );
    remove_export(&mut m, "photos").unwrap();
    assert_eq!(write(&m), fixture::toml("[exports.q]\npath = \"/q\"\n"));
}

#[test]
#[cfg(unix)]
fn save_config_refuses_an_export_message_that_does_not_load() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("irsync.toml");
    std::fs::write(&path, "[exports.p]\npath = \"/p\"\n").unwrap();
    // save_config reads the file first, and refuses one open to others.
    irsync::platform::testutil::set_mode(&path, 0o600).unwrap();
    let mut bad = msg("[exports.p]\npath = \"/p\"\n");
    bad.exports[0].path = "rel/p".into();
    let e = irsync::manage::save_config(&path, &mut bad).unwrap_err();
    assert!(e.to_string().contains(path.to_str().unwrap()), "{e}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[exports.p]\npath = \"/p\"\n"
    );
    // A state folder of the test's own: the save looks for a running
    // daemon there, not in the default one of whoever runs the tests (#275).
    let state = d.path().join("state").display().to_string();
    let mut ok = msg("[exports.q]\npath = \"/q\"\n");
    ok.state_dir = Some(state.clone());
    irsync::manage::save_config(&path, &mut ok).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!("config_version = 2\nstate_dir = {state:?}\n\n[exports.q]\npath = \"/q\"\n")
    );
}
