use irsync::config::exports::Exports;
use irsync::config::hosts::Hosts;
use irsync::manage::grants::{
    canonicalize, grant_add, grant_remove, list_all, list_export, list_peer,
};
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
    let x = parse_exports(&text, Path::new("e")).unwrap();
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
    assert!(parse_exports(&s, Path::new("e")).unwrap().rw == vec![id(1).to_string()]);
}

fn fixture() -> (Hosts, Exports) {
    let h = parse_hosts(
        &format!("[hosts.laptop]\nendpoint_id = \"{}\"\n", id(1)),
        Path::new("h"),
    )
    .unwrap();
    let e = parse_exports(
        &format!(
            "[all]\nrw = [\"laptop\"]\n[exports.backups]\npath = \"/b\"\n[exports.docs]\npath = \"/d\"\nread_only = true\nrw = [\"laptop\"]\n[exports.photos]\npath = \"/p\"\nro = [\"{}\"]\n",
            id(2)
        ),
        Path::new("e"),
    )
    .unwrap();
    (h, e)
}

#[test]
fn list_all_names_every_export_and_the_all_table() {
    let (h, e) = fixture();
    let s = list_all(&e, &h);
    assert_eq!(
        s,
        format!(
            "all:\n    rw: laptop ({l})\nimmediate:\nbackups:\ndocs:\n    rw: laptop ({l})\nphotos:\n    ro: {o}\n",
            l = id(1),
            o = id(2)
        )
    );
    assert_eq!(
        list_export(&e, &h, "docs").unwrap(),
        format!("docs:\n    rw: laptop ({})\n", id(1))
    );
    assert_eq!(
        list_export(&e, &h, "all").unwrap(),
        format!("all:\n    rw: laptop ({})\n", id(1))
    );
    assert!(list_export(&e, &h, "nope").is_none());
}

/// `[immediate]` grants show up in every form of `grant list`; `[all]`
/// is not shown as reaching an automatic export.
#[test]
fn grant_list_shows_immediate_grants() {
    let h = parse_hosts(
        &format!("[hosts.laptop]\nendpoint_id = \"{}\"\n", id(1)),
        Path::new("h"),
    )
    .unwrap();
    let e = parse_exports(
        &format!(
            "[all]\nrw = [\"{o}\"]\n[immediate]\nrw = [\"laptop\"]\n\
             [exports.auto-h]\npath = \"/h\"\nauto = true\nrw = [\"laptop\"]\n",
            o = id(2)
        ),
        Path::new("e"),
    )
    .unwrap();
    assert_eq!(
        list_all(&e, &h),
        format!(
            "all:\n    rw: {o}\nimmediate:\n    rw: laptop ({l})\nauto-h:\n    rw: laptop ({l})\n",
            l = id(1),
            o = id(2)
        )
    );
    assert_eq!(
        list_export(&e, &h, "immediate").unwrap(),
        format!("immediate:\n    rw: laptop ({})\n", id(1))
    );
    assert_eq!(
        list_peer(&e, &h, "laptop", &id(1)),
        "laptop:\n    immediate: rw\n    auto-h: rw\n"
    );
    assert_eq!(list_peer(&e, &h, "o", &id(2)), "o:\n");
}

#[test]
fn list_peer_marks_inherited_and_capped_grants() {
    let (h, e) = fixture();
    let s = list_peer(&e, &h, &format!("laptop ({})", id(1)), &id(1));
    assert_eq!(
        s,
        format!(
            "laptop ({}):\n    backups: rw (all)\n    docs: ro (read_only export)\n    photos: rw (all)\n",
            id(1)
        )
    );
    assert_eq!(
        list_peer(&e, &h, &id(2).to_string(), &id(2)),
        format!("{}:\n    photos: ro\n", id(2))
    );
    assert_eq!(list_peer(&e, &h, "x", &id(3)), "x:\n");
}

#[test]
fn grant_remove_all_all_empties_every_array_in_the_file() {
    let mut doc: DocumentMut = format!(
        "[all]\nrw = [\"{a}\"]\n[exports.p]\npath = \"/p\"\nrw = [\"{a}\"]\nro = [\"{b}\"]\n[exports.q]\npath = \"/q\"\nro = [\"lap\"]\n",
        a = id(1),
        b = id(2)
    )
    .parse()
    .unwrap();
    assert_eq!(grant_remove(&mut doc, None, None, &|_| None).unwrap(), 4);
    let x = parse_exports(&doc.to_string(), Path::new("e")).unwrap();
    assert!(x.rw.is_empty() && x.ro.is_empty());
    assert!(
        x.exports
            .values()
            .all(|e| e.rw.is_empty() && e.ro.is_empty()),
        "{doc}"
    );
    assert!(matches!(
        grant_remove(&mut doc, None, None, &|_| None),
        Err(ManageError::NothingToRemove(_))
    ));
}

#[test]
fn grant_edits_reach_inline_tables() {
    // `exports` as an inline table, one export inline under a header, and
    // `all` inline: every form the parser accepts, the editors edit.
    let text = format!(
        "all = {{ rw = [\"{a}\"] }}\nexports = {{ photos = {{ path = \"/p\", rw = [\"{a}\"] }} }}\n",
        a = id(1)
    );
    let mut doc: DocumentMut = text.parse().unwrap();
    assert_eq!(
        grant_remove(&mut doc, None, Some(&peer(1)), &|_| None).unwrap(),
        2
    );
    let x = parse_exports(&doc.to_string(), Path::new("e")).unwrap();
    assert!(
        x.rw.is_empty() && x.exports["photos"].rw.is_empty(),
        "{doc}"
    );
    grant_add(&mut doc, Some("photos"), &peer(2), true, &|_| None).unwrap();
    grant_add(&mut doc, None, &peer(2), false, &|_| None).unwrap();
    let x = parse_exports(&doc.to_string(), Path::new("e")).unwrap();
    assert_eq!(x.exports["photos"].ro, vec![id(2).to_string()]);
    assert_eq!(x.rw, vec![id(2).to_string()]);
    let mut doc: DocumentMut = format!(
        "[exports]\nphotos = {{ path = \"/p\", rw = [\"{a}\"] }}\n[exports.q]\npath = \"/q\"\nrw = [\"{a}\"]\n",
        a = id(1)
    )
    .parse()
    .unwrap();
    assert_eq!(
        grant_remove(&mut doc, None, Some(&peer(1)), &|_| None).unwrap(),
        2
    );
    assert_eq!(
        grant_remove(&mut doc, None, None, &|_| None)
            .unwrap_err()
            .to_string(),
        "nothing to remove for \"all\""
    );
}

#[test]
fn grant_immediate_adds_once() {
    let mut doc = DocumentMut::new();
    let resolve = |_: &str| None;
    assert!(irsync::manage::grants::grant_immediate(&mut doc, &peer(1), &resolve).unwrap());
    assert!(!irsync::manage::grants::grant_immediate(&mut doc, &peer(1), &resolve).unwrap());
    assert_eq!(doc["immediate"]["rw"].as_array().unwrap().len(), 1);
    let n =
        irsync::manage::grants::grant_remove_immediate(&mut doc, Some(&peer(1)), &resolve).unwrap();
    assert_eq!(n, 1);
}

fn parse_hosts(
    text: &str,
    path: &std::path::Path,
) -> Result<irsync::config::hosts::Hosts, irsync::config::ConfigError> {
    irsync::config::file::Config::parse(text, path).map(|c| c.hosts)
}

fn parse_exports(
    text: &str,
    path: &std::path::Path,
) -> Result<irsync::config::exports::Exports, irsync::config::ConfigError> {
    irsync::config::file::Config::parse(text, path).map(|c| c.exports)
}

/// `[hosts.NAME]` tables for `(alias, id number)` pairs.
fn host_tables(hosts: &[(&str, u8)]) -> String {
    hosts
        .iter()
        .map(|(a, n)| format!("[hosts.{a}]\nendpoint_id = \"{}\"\n", id(*n)))
        .collect()
}

/// Runs `canonicalize` on the hosts tables followed by `body`; the text of
/// `body` afterwards, and whether anything changed.
fn canon(hosts: &[(&str, u8)], body: &str) -> (String, bool) {
    let head = host_tables(hosts);
    let known = parse_hosts(&head, Path::new("irsync.toml")).unwrap();
    let mut doc: DocumentMut = format!("{head}{body}").parse().unwrap();
    let changed = canonicalize(&mut doc, &known);
    let text = doc.to_string();
    (text.strip_prefix(&head).unwrap().to_string(), changed)
}

const BOX: &[(&str, u8)] = &[("box", 1)];

#[test]
fn alias_of_is_the_first_alias_in_order() {
    let h = parse_hosts(
        &host_tables(&[("zed", 1), ("amy", 1), ("bob", 2)]),
        Path::new("h"),
    )
    .unwrap();
    assert_eq!(h.alias_of(&id(1)), Some("amy"));
    assert_eq!(h.alias_of(&id(2)), Some("bob"));
    assert_eq!(h.alias_of(&id(3)), None);
}

#[test]
fn id_becomes_alias() {
    let (text, changed) = canon(BOX, &format!("[all]\nrw = [\"{}\"]\n", id(1)));
    assert_eq!(text, "[all]\nrw = [\"box\"]\n");
    assert!(changed);
}

#[test]
fn every_grant_list_is_visited() {
    let i = id(1);
    let (text, _) = canon(
        BOX,
        &format!(
            "[all]\nro = [\"{i}\"]\n[immediate]\nrw = [\"{i}\"]\n\
             [exports.p]\npath = \"/p\"\nrw = [\"{i}\"]\n\
             [exports.q]\npath = \"/q\"\nro = [\"{i}\"]\n"
        ),
    );
    assert_eq!(
        text,
        "[all]\nro = [\"box\"]\n[immediate]\nrw = [\"box\"]\n\
         [exports.p]\npath = \"/p\"\nrw = [\"box\"]\n\
         [exports.q]\npath = \"/q\"\nro = [\"box\"]\n"
    );
    // Inline tables count, as for the other editors.
    let known = parse_hosts(&host_tables(BOX), Path::new("h")).unwrap();
    let mut doc: DocumentMut = format!(
        "exports = {{ p = {{ path = \"/p\", rw = [\"{i}\"] }} }}\nall = {{ ro = [\"{i}\"] }}\n"
    )
    .parse()
    .unwrap();
    assert!(canonicalize(&mut doc, &known));
    assert_eq!(
        doc.to_string(),
        "exports = { p = { path = \"/p\", rw = [\"box\"] } }\nall = { ro = [\"box\"] }\n"
    );
}

#[test]
fn first_alias_in_order_wins() {
    let (text, _) = canon(
        &[("zed", 1), ("amy", 1)],
        &format!("[all]\nrw = [\"{}\"]\n", id(1)),
    );
    assert_eq!(text, "[all]\nrw = [\"amy\"]\n");
}

#[test]
fn existing_alias_is_left_alone() {
    let body = "[all]\nrw = [\"zed\"]\n";
    assert_eq!(
        canon(&[("zed", 1), ("amy", 1)], body),
        (body.to_string(), false)
    );
}

#[test]
fn unknown_entries_are_left_alone() {
    let body = format!("[all]\nrw = [\"{}\", \"ghost\"]\n", id(2));
    assert_eq!(canon(BOX, &body), (body.clone(), false));
}

#[test]
fn duplicate_in_one_list_keeps_the_first() {
    let (text, changed) = canon(BOX, &format!("[all]\nrw = [\"box\", \"{}\"]\n", id(1)));
    assert_eq!(text, "[all]\nrw = [\"box\"]\n");
    assert!(changed);
    let (text, _) = canon(BOX, &format!("[all]\nrw = [\"{}\", \"box\"]\n", id(1)));
    assert_eq!(text, "[all]\nrw = [\"box\"]\n");
    // Duplicates that were already there go too.
    let (text, changed) = canon(BOX, "[all]\nrw = [\"box\", \"x\", \"box\"]\n");
    assert_eq!(text, "[all]\nrw = [\"box\", \"x\"]\n");
    assert!(changed);
}

#[test]
fn rw_wins_over_ro() {
    let (text, _) = canon(
        BOX,
        &format!(
            "[exports.p]\npath = \"/p\"\nro = [\"box\", \"x\"]\nrw = [\"{}\"]\n",
            id(1)
        ),
    );
    assert_eq!(
        text,
        "[exports.p]\npath = \"/p\"\nro = [\"x\"]\nrw = [\"box\"]\n"
    );
    // Each table on its own: rw in [all] does not touch ro of an export.
    let body = "[all]\nrw = [\"box\"]\n[exports.p]\npath = \"/p\"\nro = [\"box\"]\n";
    assert_eq!(canon(BOX, body), (body.to_string(), false));
}

#[test]
fn two_aliases_of_one_id_both_stay() {
    let body = "[all]\nrw = [\"amy\", \"zed\"]\n";
    assert_eq!(
        canon(&[("zed", 1), ("amy", 1)], body),
        (body.to_string(), false)
    );
}

#[test]
fn comment_and_layout_are_kept() {
    let (text, _) = canon(
        BOX,
        &format!(
            "# who may write\n[all]\nrw = [\n    \"{}\", # laptop\n    \"{}\",   # other\n] # end\n",
            id(1),
            id(2)
        ),
    );
    assert_eq!(
        text,
        format!(
            "# who may write\n[all]\nrw = [\n    \"box\", # laptop\n    \"{}\",   # other\n] # end\n",
            id(2)
        )
    );
}

#[test]
fn second_run_changes_nothing() {
    let known = parse_hosts(&host_tables(BOX), Path::new("h")).unwrap();
    let mut doc: DocumentMut = format!(
        "[all]\nrw = [\"{0}\", \"box\"]\nro = [\"{0}\"]\n[immediate]\nrw = [\"{0}\"]\n",
        id(1)
    )
    .parse()
    .unwrap();
    assert!(canonicalize(&mut doc, &known));
    let once = doc.to_string();
    assert!(!canonicalize(&mut doc, &known));
    assert_eq!(doc.to_string(), once);
}

#[test]
fn odd_shapes_do_not_panic() {
    let (text, changed) = canon(
        BOX,
        &format!("[all]\nrw = [1, \"{}\"]\nro = \"x\"\n", id(1)),
    );
    assert_eq!(text, "[all]\nrw = [1, \"box\"]\nro = \"x\"\n");
    assert!(changed);
    for body in [
        "all = 5\n",
        "exports = \"x\"\n",
        "[exports]\np = 1\n",
        "immediate = []\n",
        "[[all]]\nrw = []\n",
    ] {
        assert_eq!(canon(BOX, body), (body.to_string(), false), "{body:?}");
    }
}

#[test]
fn other_spelling_of_an_id_is_rewritten() {
    let z32 = id(1).to_z32();
    assert_ne!(z32, id(1).to_string());
    let (text, _) = canon(BOX, &format!("[all]\nrw = [\"{z32}\"]\n"));
    assert_eq!(text, "[all]\nrw = [\"box\"]\n");
}

#[test]
fn duplicate_removal_keeps_other_comments() {
    let (text, _) = canon(
        BOX,
        &format!(
            "[exports.p]\npath = \"/p\"\nrw = [\n    \"box\", # one\n    \"{}\", # two\n    \"{}\", # three\n]\n",
            id(2),
            id(1)
        ),
    );
    assert!(
        text.contains("\"box\", # one") && text.contains("# two"),
        "{text}"
    );
    assert_eq!(text.matches("box").count(), 1, "{text}");
    assert!(!text.contains(&id(1).to_string()), "{text}");
    let x = parse_exports(&text, Path::new("e")).unwrap();
    assert_eq!(x.exports["p"].rw, ["box".to_string(), id(2).to_string()]);
}
