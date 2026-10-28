use irsync::config::exports::Exports;
use irsync::manage::grants::{grant_add, grant_remove};
use irsync::manage::{ManageError, PeerSpec};
use std::path::Path;
use toml_edit::DocumentMut;

fn id(n: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
}
fn peer(n: u8) -> PeerSpec {
    PeerSpec::Id {
        id: id(n),
        comment: None,
    }
}

const INJECTED: &str = "laptop\n]\n[etc]\npath = \"/etc\"\nrw = [\"x\"";

#[test]
fn grant_add_moves_between_arrays_replaces_other_spelling_and_keeps_comment() {
    let mut doc: DocumentMut = format!("[exports.p]\npath = \"/p\"\nrw = [\"{}\"]\n", id(1))
        .parse()
        .unwrap();
    let resolve = |s: &str| (s == "laptop").then(|| id(1));
    grant_add(
        &mut doc,
        Some("p"),
        &PeerSpec::Alias("laptop".into()),
        false,
        &resolve,
    )
    .unwrap(); // replaces the raw id
    assert_eq!(doc["exports"]["p"]["rw"].as_array().unwrap().len(), 1);
    assert_eq!(
        doc["exports"]["p"]["rw"]
            .as_array()
            .unwrap()
            .get(0)
            .unwrap()
            .as_str(),
        Some("laptop")
    );
    grant_add(
        &mut doc,
        Some("p"),
        &PeerSpec::Alias("laptop".into()),
        true,
        &resolve,
    )
    .unwrap(); // rw -> ro
    assert!(doc["exports"]["p"]["rw"].as_array().unwrap().is_empty());
    assert_eq!(doc["exports"]["p"]["ro"].as_array().unwrap().len(), 1);
    grant_add(
        &mut doc,
        Some("p"),
        &PeerSpec::Id {
            id: id(2),
            comment: Some("mike@laptop".into()),
        },
        false,
        &resolve,
    )
    .unwrap();
    assert!(
        doc.to_string()
            .contains(&format!("rw = [\"{}\", # mike@laptop\n]", id(2))),
        "{doc}"
    );
    grant_add(&mut doc, None, &peer(3), true, &resolve).unwrap(); // [all]
    assert_eq!(
        doc["all"]["ro"]
            .as_array()
            .unwrap()
            .get(0)
            .unwrap()
            .as_str(),
        Some(id(3).to_string().as_str())
    );
    assert!(matches!(
        grant_add(&mut doc, Some("zz"), &peer(3), false, &resolve),
        Err(ManageError::NotFound(_))
    ));
}

#[test]
fn grant_remove_by_export_by_all_and_everywhere() {
    let text = format!("[all]\nrw = [\"{a}\"]\n[exports.p]\npath = \"/p\"\nrw = [\"{a}\", \"{b}\"]\nro = [\"lap\"]\n[exports.q]\npath = \"/q\"\nro = [\"{a}\"]\n", a = id(1), b = id(2));
    let resolve = |s: &str| (s == "lap").then(|| id(1));
    let mut doc: DocumentMut = text.parse().unwrap();
    assert_eq!(
        grant_remove(&mut doc, Some("p"), Some(&peer(1)), &resolve).unwrap(),
        2
    ); // raw id and the alias that resolves to it
    assert_eq!(doc["exports"]["p"]["rw"].as_array().unwrap().len(), 1);
    assert!(doc["exports"]["p"]["ro"].as_array().unwrap().is_empty());
    let mut doc: DocumentMut = text.parse().unwrap();
    assert_eq!(
        grant_remove(&mut doc, None, Some(&peer(1)), &resolve).unwrap(),
        4
    ); // everywhere
    let mut doc: DocumentMut = text.parse().unwrap();
    assert_eq!(
        grant_remove(&mut doc, Some("p"), None, &resolve).unwrap(),
        3
    ); // all of p
    assert!(matches!(
        grant_remove(&mut doc, Some("p"), None, &resolve),
        Err(ManageError::NothingToRemove(_))
    ));
}

#[test]
fn a_second_grant_add_after_a_comment_keeps_the_comment_and_valid_toml() {
    let mut doc: DocumentMut = "[exports.p]\npath = \"/p\"\n".parse().unwrap();
    let none = |_: &str| None;
    grant_add(
        &mut doc,
        Some("p"),
        &PeerSpec::Id {
            id: id(1),
            comment: Some("c1".into()),
        },
        false,
        &none,
    )
    .unwrap();
    grant_add(&mut doc, Some("p"), &peer(2), false, &none).unwrap();
    let text = doc.to_string();
    let x = Exports::parse(&text, Path::new("e")).unwrap();
    assert_eq!(x.exports["p"].rw.len(), 2, "{text}");
    assert!(text.contains(&format!("\"{}\", # c1\n", id(1))), "{text}");
}

#[test]
fn grant_remove_matches_a_dangling_alias_by_literal_text() {
    let mut doc: DocumentMut = "[exports.p]\npath = \"/p\"\nrw = [\"gone\"]\n"
        .parse()
        .unwrap();
    let none = |_: &str| None;
    assert_eq!(
        grant_remove(&mut doc, None, Some(&PeerSpec::Alias("gone".into())), &none).unwrap(),
        1
    );
}

#[test]
fn grant_add_refuses_a_comment_with_control_characters_and_leaves_the_doc() {
    let mut doc: DocumentMut = "[exports.p]\npath = \"/p\"\n".parse().unwrap();
    let before = doc.to_string();
    let none = |_: &str| None;
    for c in [INJECTED, "a\rb", "tab\there"] {
        let evil = PeerSpec::Id {
            id: id(1),
            comment: Some(c.into()),
        };
        let r = grant_add(&mut doc, Some("p"), &evil, false, &none);
        assert!(matches!(r, Err(ManageError::BadComment(_))), "{c:?}: {r:?}");
        assert_eq!(doc.to_string(), before);
    }
}

#[test]
fn grant_add_to_all_creates_the_all_table() {
    let mut doc: DocumentMut = "[exports.p]\npath = \"/p\"\n".parse().unwrap();
    grant_add(&mut doc, None, &peer(1), false, &|_| None).unwrap();
    let s = doc.to_string();
    assert!(
        s.contains("[all]\nrw = [") && s.contains(&id(1).to_string()),
        "{s}"
    );
    assert!(Exports::parse(&s, Path::new("e")).unwrap().rw == vec![id(1).to_string()]);
}
