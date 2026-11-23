use irsync::config::file::Config;
use irsync::config::reader::read;
use irsync::config::writer::write;
use irsync::manage::exports::parse_add_target;
use irsync::manage::exports::{add_auto_export, add_export, remove_export};
use irsync::manage::{ManageError, PeerSpec};
use irsync_proto::config as pb;
use std::path::Path;

fn id(n: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
}
fn msg(text: &str) -> pb::Config {
    read(text, Path::new("f")).unwrap()
}
fn typed(m: &pb::Config) -> Config {
    Config::from_message(m, Path::new("f")).unwrap()
}

#[test]
fn add_writes_only_what_was_asked_and_refuses_duplicates_and_reserved() {
    let mut m = pb::Config::default();
    add_export(&mut m, "photos", Path::new("/srv/photos"), false, false).unwrap();
    add_export(&mut m, "docs", Path::new("/srv/docs"), true, true).unwrap();
    let x = typed(&m).exports;
    assert!(!x.exports["photos"].read_only && !x.exports["photos"].warm_index);
    assert!(x.exports["docs"].read_only && x.exports["docs"].warm_index);
    assert_eq!(m.exports[0].read_only, None);
    assert_eq!(m.exports[1].read_only, Some(true));
    assert!(
        !write(&m).contains("read_only = false"),
        "only true flags are written:\n{}",
        write(&m)
    );
    assert!(matches!(
        add_export(&mut m, "photos", Path::new("/x"), false, false),
        Err(ManageError::Exists(_))
    ));
    assert!(matches!(
        add_export(&mut m, "all", Path::new("/x"), false, false),
        Err(ManageError::Reserved(_))
    ));
}

#[test]
fn add_writes_under_exports_as_a_dotted_header_and_refuses_all() {
    let mut m = pb::Config::default();
    add_export(&mut m, "photos", Path::new("/srv/photos"), false, false).unwrap();
    assert_eq!(write(&m), "[exports.photos]\npath = \"/srv/photos\"\n");
    let e = add_export(&mut m, "all", Path::new("/srv/all"), false, false).unwrap_err();
    assert_eq!(
        e.to_string(),
        "\"all\" is reserved: it is the word `grant add all WHO` uses for every export"
    );
    assert!(add_export(&mut m, "rw", Path::new("/srv/rw"), false, false).is_ok());
}

#[test]
fn a_new_export_goes_last_and_the_path_stays_as_written() {
    let mut m = msg("[exports.b]\npath = \"/b\"\n[exports.a]\npath = \"/a\"\n");
    add_export(&mut m, "c", Path::new("~/c"), false, false).unwrap();
    let names: Vec<_> = m.exports.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["b", "a", "c"]);
    assert_eq!(m.exports[2].path, "~/c");
}

#[test]
fn remove_last_export_leaves_a_loadable_file() {
    let mut m = msg("[all]\nrw = [\"x\"]\n[exports.a]\npath = \"/a\"\n");
    remove_export(&mut m, "a").unwrap();
    let c = typed(&m);
    assert!(c.exports.exports.is_empty() && c.exports.rw == vec!["x"]);
    assert!(matches!(
        remove_export(&mut m, "all"),
        Err(ManageError::NotFound(_))
    ));
}

#[test]
fn remove_drops_table_and_its_grants() {
    let mut m = msg("[exports.a]\npath = \"/a\"\nrw = [\"x\"]\n[exports.b]\npath = \"/b\"\n");
    remove_export(&mut m, "a").unwrap();
    assert_eq!(write(&m), "[exports.b]\npath = \"/b\"\n");
    assert!(matches!(
        remove_export(&mut m, "a"),
        Err(ManageError::NotFound(_))
    ));
}

#[test]
fn remove_only_removes_an_export() {
    let text = format!(
        "[all]\nrw = [\"{}\"]\nro = []\n[exports.p]\npath = \"/p\"\n",
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
    let mut n = |p: &str| add_auto_export(&mut m, Path::new(p), id(1)).unwrap();
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
    let want = format!(
        "[exports.auto-srv-photos]\npath = \"/srv/photos\"\nauto = true\nrw = [\"{}\"]\n",
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

#[test]
fn add_target_edge_cases() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("a:b")).unwrap();
    std::fs::create_dir_all(d.path().join("sub/x")).unwrap();
    assert!(matches!(
        parse_add_target(":docs", d.path()),
        Err(ManageError::BadName(_))
    ));
    let abs = d.path().join("a:b");
    assert_eq!(
        parse_add_target(abs.to_str().unwrap(), Path::new("/elsewhere")).unwrap(),
        ("a:b".into(), abs)
    );
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
    let hosts = irsync::config::file::Config::parse("", Path::new("h"))
        .unwrap()
        .hosts;
    let line = irsync::identity::public_key_line(&iroh::SecretKey::from_bytes(&[7; 32]), "x");
    let key = line.rsplit_once(' ').unwrap().0;
    let r = PeerSpec::parse(&format!("{key} {INJECTED}"), &hosts);
    assert!(matches!(r, Err(ManageError::BadComment(_))), "{r:?}");
    let msg = r.unwrap_err().to_string();
    assert!(msg.contains("control characters"), "{msg}");
    // A file holding such a line is refused the same way.
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("k.pub");
    std::fs::write(&f, format!("{key} a\x07b\n")).unwrap();
    assert!(matches!(
        PeerSpec::parse(f.to_str().unwrap(), &hosts),
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
    add_export(&mut m, "q", Path::new("/q"), false, false).unwrap();
    let c = typed(&m).exports;
    assert_eq!(c.exports["photos"].rw, vec!["x"]);
    assert_eq!(c.exports["q"].path, Path::new("/q"));
    assert!(matches!(
        add_export(&mut m, "photos", Path::new("/x"), false, false),
        Err(ManageError::Exists(_))
    ));
    assert_eq!(
        write(&m),
        "[exports.photos]\npath = \"/p\"\nrw = [\"x\"]\n\n[exports.q]\npath = \"/q\"\n"
    );
    remove_export(&mut m, "photos").unwrap();
    assert_eq!(write(&m), "[exports.q]\npath = \"/q\"\n");
}

#[test]
fn save_config_refuses_an_export_message_that_does_not_load() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("irsync.toml");
    std::fs::write(&path, "[exports.p]\npath = \"/p\"\n").unwrap();
    let mut bad = msg("[exports.p]\npath = \"/p\"\n");
    bad.exports[0].path = "rel/p".into();
    let e = irsync::manage::save_config(&path, &mut bad).unwrap_err();
    assert!(e.to_string().contains(path.to_str().unwrap()), "{e}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[exports.p]\npath = \"/p\"\n"
    );
    let mut ok = msg("[exports.q]\npath = \"/q\"\n");
    irsync::manage::save_config(&path, &mut ok).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[exports.q]\npath = \"/q\"\n"
    );
}
