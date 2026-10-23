use irsync::manage::exports::{add_export, allow, parse_add_target, remove_export, revoke};
use irsync::manage::{ManageError, PeerSpec};
use std::path::Path;
use toml_edit::DocumentMut;

fn id(n: u8) -> iroh::EndpointId { iroh::SecretKey::from_bytes(&[n; 32]).public() }
fn peer(n: u8) -> PeerSpec { PeerSpec::Id { id: id(n), comment: None } }

#[test]
fn add_writes_only_what_was_asked_and_refuses_duplicates_and_reserved() {
    let mut doc = DocumentMut::new();
    add_export(&mut doc, "photos", Path::new("/srv/photos"), false, false).unwrap();
    add_export(&mut doc, "docs", Path::new("/srv/docs"), true, true).unwrap();
    let x = irsync::config::exports::Exports::parse(&doc.to_string(), Path::new("e")).unwrap();
    assert!(!x.exports["photos"].read_only && !x.exports["photos"].warm_index);
    assert!(x.exports["docs"].read_only && x.exports["docs"].warm_index);
    assert!(!doc.to_string().contains("read_only = false"), "only true flags are written:\n{doc}");
    assert!(matches!(add_export(&mut doc, "photos", Path::new("/x"), false, false), Err(ManageError::Exists(_))));
    for r in ["rw", "ro", "all"] {
        assert!(matches!(add_export(&mut doc, r, Path::new("/x"), false, false), Err(ManageError::Reserved(_))));
    }
}

#[test]
fn remove_drops_table_and_its_grants() {
    let mut doc: DocumentMut = "[a]\npath = \"/a\"\nrw = [\"x\"]\n[b]\npath = \"/b\"\n".parse().unwrap();
    remove_export(&mut doc, "a").unwrap();
    assert_eq!(doc.to_string(), "[b]\npath = \"/b\"\n");
    assert!(matches!(remove_export(&mut doc, "a"), Err(ManageError::NotFound(_))));
}

#[test]
fn allow_moves_between_arrays_replaces_other_spelling_and_keeps_comment() {
    let mut doc: DocumentMut = format!("[p]\npath = \"/p\"\nrw = [\"{}\"]\n", id(1)).parse().unwrap();
    let resolve = |s: &str| (s == "laptop").then(|| id(1));
    allow(&mut doc, Some("p"), &PeerSpec::Alias("laptop".into()), false, &resolve).unwrap();   // replaces the raw id
    assert_eq!(doc["p"]["rw"].as_array().unwrap().len(), 1);
    assert_eq!(doc["p"]["rw"].as_array().unwrap().get(0).unwrap().as_str(), Some("laptop"));
    allow(&mut doc, Some("p"), &PeerSpec::Alias("laptop".into()), true, &resolve).unwrap();    // rw -> ro
    assert!(doc["p"]["rw"].as_array().unwrap().is_empty());
    assert_eq!(doc["p"]["ro"].as_array().unwrap().len(), 1);
    allow(&mut doc, Some("p"), &PeerSpec::Id { id: id(2), comment: Some("mike@laptop".into()) }, false, &resolve).unwrap();
    assert!(doc.to_string().contains(&format!("rw = [\"{}\", # mike@laptop\n]", id(2))), "{doc}");
    allow(&mut doc, None, &peer(3), true, &resolve).unwrap();                                   // top level
    assert_eq!(doc["ro"].as_array().unwrap().get(0).unwrap().as_str(), Some(id(3).to_string().as_str()));
    assert!(matches!(allow(&mut doc, Some("zz"), &peer(3), false, &resolve), Err(ManageError::NotFound(_))));
}

#[test]
fn revoke_by_export_by_all_and_everywhere() {
    let text = format!("rw = [\"{a}\"]\n[p]\npath = \"/p\"\nrw = [\"{a}\", \"{b}\"]\nro = [\"lap\"]\n[q]\npath = \"/q\"\nro = [\"{a}\"]\n", a = id(1), b = id(2));
    let resolve = |s: &str| (s == "lap").then(|| id(1));
    let mut doc: DocumentMut = text.parse().unwrap();
    assert_eq!(revoke(&mut doc, Some("p"), Some(&peer(1)), &resolve).unwrap(), 2);   // raw id and the alias that resolves to it
    assert_eq!(doc["p"]["rw"].as_array().unwrap().len(), 1);
    assert!(doc["p"]["ro"].as_array().unwrap().is_empty());
    let mut doc: DocumentMut = text.parse().unwrap();
    assert_eq!(revoke(&mut doc, None, Some(&peer(1)), &resolve).unwrap(), 4);        // everywhere
    let mut doc: DocumentMut = text.parse().unwrap();
    assert_eq!(revoke(&mut doc, Some("p"), None, &resolve).unwrap(), 3);              // all of p
    assert!(matches!(revoke(&mut doc, Some("p"), None, &resolve), Err(ManageError::NothingToRemove(_))));
}

#[test]
fn add_target_forms() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("docs")).unwrap();
    std::fs::write(d.path().join("f"), b"").unwrap();
    assert_eq!(parse_add_target("photos:docs", d.path()).unwrap(), ("photos".into(), d.path().join("docs")));
    assert_eq!(parse_add_target("docs", d.path()).unwrap(), ("docs".into(), d.path().join("docs")));
    assert_eq!(parse_add_target(d.path().join("docs").to_str().unwrap(), Path::new("/elsewhere")).unwrap().0, "docs");
    assert!(matches!(parse_add_target("x:f", d.path()), Err(ManageError::NotADirectory(_))));
    assert!(matches!(parse_add_target("x:missing", d.path()), Err(ManageError::NotADirectory(_))));
}

#[test]
fn a_second_allow_after_a_comment_keeps_the_comment_and_valid_toml() {
    let mut doc: DocumentMut = "[p]\npath = \"/p\"\n".parse().unwrap();
    let none = |_: &str| None;
    allow(&mut doc, Some("p"), &PeerSpec::Id { id: id(1), comment: Some("c1".into()) }, false, &none).unwrap();
    allow(&mut doc, Some("p"), &peer(2), false, &none).unwrap();
    let text = doc.to_string();
    let x = irsync::config::exports::Exports::parse(&text, Path::new("e")).unwrap();
    assert_eq!(x.exports["p"].rw.len(), 2, "{text}");
    assert!(text.contains(&format!("\"{}\", # c1\n", id(1))), "{text}");
}

#[test]
fn revoke_matches_a_dangling_alias_by_literal_text() {
    let mut doc: DocumentMut = "[p]\npath = \"/p\"\nrw = [\"gone\"]\n".parse().unwrap();
    let none = |_: &str| None;
    assert_eq!(revoke(&mut doc, None, Some(&PeerSpec::Alias("gone".into())), &none).unwrap(), 1);
}

#[test]
fn add_target_edge_cases() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("a:b")).unwrap();
    std::fs::create_dir_all(d.path().join("sub/x")).unwrap();
    assert!(matches!(parse_add_target(":docs", d.path()), Err(ManageError::BadName(_))));
    let abs = d.path().join("a:b");
    assert_eq!(parse_add_target(abs.to_str().unwrap(), Path::new("/elsewhere")).unwrap(), ("a:b".into(), abs));
    let sub = d.path().join("sub");
    assert_eq!(parse_add_target("..", &sub.join("x")).unwrap(), ("sub".into(), sub.clone()));
    assert_eq!(parse_add_target("./x", &sub).unwrap(), ("x".into(), sub.join("x")));
    assert_eq!(parse_add_target("../sub/x", &sub).unwrap(), ("x".into(), sub.join("x")));
}

const INJECTED: &str = "laptop\n]\n[etc]\npath = \"/etc\"\nrw = [\"x\"";

#[test]
fn allow_refuses_a_comment_with_control_characters_and_leaves_the_doc() {
    let mut doc: DocumentMut = "[p]\npath = \"/p\"\n".parse().unwrap();
    let before = doc.to_string();
    let none = |_: &str| None;
    for c in [INJECTED, "a\rb", "tab\there"] {
        let evil = PeerSpec::Id { id: id(1), comment: Some(c.into()) };
        let r = allow(&mut doc, Some("p"), &evil, false, &none);
        assert!(matches!(r, Err(ManageError::BadComment(_))), "{c:?}: {r:?}");
        assert_eq!(doc.to_string(), before);
    }
}

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
    assert!(matches!(PeerSpec::parse(f.to_str().unwrap(), &hosts), Err(ManageError::BadComment(_))));
}

#[test]
fn save_exports_refuses_a_document_that_does_not_parse() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("exports.toml");
    std::fs::write(&path, "[p]\npath = \"/p\"\n").unwrap();
    let doc: DocumentMut = "[p]\npath = \"/p\"\nbogus = 1\n".parse().unwrap();
    let e = irsync::manage::save_exports(&path, &doc).unwrap_err();
    assert!(e.to_string().contains(path.to_str().unwrap()), "{e}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[p]\npath = \"/p\"\n");
    let ok: DocumentMut = "[q]\npath = \"/q\"\n".parse().unwrap();
    irsync::manage::save_exports(&path, &ok).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[q]\npath = \"/q\"\n");
}
