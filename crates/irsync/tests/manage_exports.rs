use irsync::config::exports::Exports;
use irsync::manage::exports::{add_export, parse_add_target, remove_export};
use irsync::manage::{ManageError, PeerSpec};
use std::path::Path;
use toml_edit::DocumentMut;

fn id(n: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
}

#[test]
fn add_writes_only_what_was_asked_and_refuses_duplicates_and_reserved() {
    let mut doc = DocumentMut::new();
    add_export(&mut doc, "photos", Path::new("/srv/photos"), false, false).unwrap();
    add_export(&mut doc, "docs", Path::new("/srv/docs"), true, true).unwrap();
    let x = Exports::parse(&doc.to_string(), Path::new("e")).unwrap();
    assert!(!x.exports["photos"].read_only && !x.exports["photos"].warm_index);
    assert!(x.exports["docs"].read_only && x.exports["docs"].warm_index);
    assert!(
        !doc.to_string().contains("read_only = false"),
        "only true flags are written:\n{doc}"
    );
    assert!(matches!(
        add_export(&mut doc, "photos", Path::new("/x"), false, false),
        Err(ManageError::Exists(_))
    ));
    assert!(matches!(
        add_export(&mut doc, "all", Path::new("/x"), false, false),
        Err(ManageError::Reserved(_))
    ));
}

#[test]
fn add_writes_under_exports_as_a_dotted_header_and_refuses_all() {
    let mut doc = DocumentMut::new();
    add_export(&mut doc, "photos", Path::new("/srv/photos"), false, false).unwrap();
    assert_eq!(
        doc.to_string(),
        "[exports.photos]\npath = \"/srv/photos\"\n"
    );
    let e = add_export(&mut doc, "all", Path::new("/srv/all"), false, false).unwrap_err();
    assert_eq!(
        e.to_string(),
        "\"all\" is reserved: it is the word `grant add all WHO` uses for every export"
    );
    assert!(add_export(&mut doc, "rw", Path::new("/srv/rw"), false, false).is_ok());
}

#[test]
fn remove_last_export_leaves_a_loadable_file() {
    let mut doc: DocumentMut = "[all]\nrw = [\"x\"]\n[exports.a]\npath = \"/a\"\n"
        .parse()
        .unwrap();
    remove_export(&mut doc, "a").unwrap();
    let x = Exports::parse(&doc.to_string(), Path::new("e")).unwrap();
    assert!(x.exports.is_empty() && x.rw == vec!["x"]);
    assert!(matches!(
        remove_export(&mut doc, "all"),
        Err(ManageError::NotFound(_))
    ));
}

#[test]
fn remove_drops_table_and_its_grants() {
    let mut doc: DocumentMut =
        "[exports.a]\npath = \"/a\"\nrw = [\"x\"]\n[exports.b]\npath = \"/b\"\n"
            .parse()
            .unwrap();
    remove_export(&mut doc, "a").unwrap();
    assert_eq!(doc.to_string(), "[exports.b]\npath = \"/b\"\n");
    assert!(matches!(
        remove_export(&mut doc, "a"),
        Err(ManageError::NotFound(_))
    ));
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

const INJECTED: &str = "laptop\n]\n[etc]\npath = \"/etc\"\nrw = [\"x\"";

#[test]
fn peer_spec_refuses_a_key_line_whose_comment_has_a_newline() {
    let hosts = irsync::config::hosts::Hosts::parse("", Path::new("h")).unwrap();
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
fn save_exports_refuses_a_document_that_does_not_parse() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("exports.toml");
    std::fs::write(&path, "[exports.p]\npath = \"/p\"\n").unwrap();
    let doc: DocumentMut = "[exports.p]\npath = \"/p\"\nbogus = 1\n".parse().unwrap();
    let e = irsync::manage::save_exports(&path, &doc).unwrap_err();
    assert!(e.to_string().contains(path.to_str().unwrap()), "{e}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[exports.p]\npath = \"/p\"\n"
    );
    let ok: DocumentMut = "[exports.q]\npath = \"/q\"\n".parse().unwrap();
    irsync::manage::save_exports(&path, &ok).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[exports.q]\npath = \"/q\"\n"
    );
}

#[test]
fn remove_only_removes_an_export_table() {
    let text = format!(
        "[all]\nrw = [\"{}\"]\nro = []\n[exports.p]\npath = \"/p\"\n",
        id(1)
    );
    let mut doc: DocumentMut = text.parse().unwrap();
    for r in ["all", "exports", "rw"] {
        assert!(
            matches!(remove_export(&mut doc, r), Err(ManageError::NotFound(_))),
            "{r}"
        );
    }
    let mut odd: DocumentMut = "[exports]\nx = 1\n[exports.p]\npath = \"/p\"\n"
        .parse()
        .unwrap();
    assert!(matches!(
        remove_export(&mut odd, "x"),
        Err(ManageError::NotFound(_))
    ));
    assert_eq!(doc.to_string(), text);
    assert_eq!(
        odd.to_string(),
        "[exports]\nx = 1\n[exports.p]\npath = \"/p\"\n"
    );
}

#[test]
fn add_and_remove_keep_an_inline_exports_table() {
    let mut doc: DocumentMut = "exports = { photos = { path = \"/p\", rw = [\"x\"] } }\n"
        .parse()
        .unwrap();
    add_export(&mut doc, "q", Path::new("/q"), false, false).unwrap();
    let x = Exports::parse(&doc.to_string(), Path::new("e")).unwrap();
    assert_eq!(x.exports["photos"].rw, vec!["x"], "{doc}");
    assert_eq!(x.exports["q"].path, Path::new("/q"));
    assert!(matches!(
        add_export(&mut doc, "photos", Path::new("/x"), false, false),
        Err(ManageError::Exists(_))
    ));
    remove_export(&mut doc, "photos").unwrap();
    let x = Exports::parse(&doc.to_string(), Path::new("e")).unwrap();
    assert!(
        !x.exports.contains_key("photos") && x.exports.contains_key("q"),
        "{doc}"
    );
    // A non-table `exports` is refused, never overwritten.
    let mut odd: DocumentMut = "exports = 5\n".parse().unwrap();
    assert!(add_export(&mut odd, "q", Path::new("/q"), false, false).is_err());
    assert_eq!(odd.to_string(), "exports = 5\n");
}
