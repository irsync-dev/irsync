use irsync::config::file::Config;
use irsync::config::reader::read;
use irsync::config::writer::write;
use irsync::manage::msg::exports::{add_auto_export, add_export, remove_export};
use irsync::manage::ManageError;
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
}
