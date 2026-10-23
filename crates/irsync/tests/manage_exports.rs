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
    assert!(doc.to_string().contains(&format!("\"{}\", # mike@laptop", id(2))) || doc.to_string().contains(&format!("\"{}\" # mike@laptop", id(2))), "{doc}");
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
