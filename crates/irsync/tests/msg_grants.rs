use irsync::config::file::Config;
use irsync::config::hosts::Hosts;
use irsync::config::reader::read;
use irsync::config::writer::write;
use irsync::manage::msg::grants::{
    canonicalize, grant_add, grant_immediate, grant_remove, grant_remove_immediate, lists_naming,
    remove_named, rename_named,
};
use irsync::manage::{ManageError, PeerSpec};
use irsync_proto::config as pb;
use std::path::Path;

fn id(n: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
}
fn peer(n: u8) -> PeerSpec {
    PeerSpec::Id {
        id: id(n),
        comment: None,
    }
}
fn msg(text: &str) -> pb::Config {
    read(text, Path::new("f")).unwrap()
}
/// The typed config the message builds; panics when it does not load.
fn typed(m: &pb::Config) -> Config {
    Config::from_message(m, Path::new("f")).unwrap()
}
fn values(l: &[pb::Entry]) -> Vec<&str> {
    l.iter().map(|e| e.value.as_str()).collect()
}
fn trailing(e: &pb::Entry) -> &str {
    e.comment.as_ref().map_or("", |c| c.trailing.as_str())
}
fn export<'a>(m: &'a pb::Config, name: &str) -> &'a pb::Export {
    m.exports.iter().find(|e| e.name == name).unwrap()
}

const INJECTED: &str = "laptop\n]\n[etc]\npath = \"/etc\"\nrw = [\"x\"";

#[test]
fn grant_add_moves_between_arrays_replaces_other_spelling_and_keeps_comment() {
    let mut m = msg(&format!(
        "[exports.p]\npath = \"/p\"\nrw = [\"{}\"]\n",
        id(1)
    ));
    let resolve = |s: &str| (s == "laptop").then(|| id(1));
    let laptop = PeerSpec::Alias("laptop".into());
    grant_add(&mut m, Some("p"), &laptop, false, &resolve).unwrap(); // replaces the raw id
    assert_eq!(values(&export(&m, "p").rw), ["laptop"]);
    grant_add(&mut m, Some("p"), &laptop, true, &resolve).unwrap(); // rw -> ro
    assert!(export(&m, "p").rw.is_empty() && export(&m, "p").rw_set);
    assert_eq!(values(&export(&m, "p").ro), ["laptop"]);
    let keyed = PeerSpec::Id {
        id: id(2),
        comment: Some("mike@laptop".into()),
    };
    grant_add(&mut m, Some("p"), &keyed, false, &resolve).unwrap();
    let rw = &export(&m, "p").rw;
    assert_eq!(values(rw), [id(2).to_string()]);
    assert_eq!(trailing(&rw[0]), "# mike@laptop");
    assert!(
        write(&m).contains(&format!("rw = [\n    \"{}\", # mike@laptop\n]", id(2))),
        "{}",
        write(&m)
    );
    grant_add(&mut m, None, &peer(3), true, &resolve).unwrap(); // [all]
    assert_eq!(values(&m.all.as_ref().unwrap().ro), [id(3).to_string()]);
    assert!(matches!(
        grant_add(&mut m, Some("zz"), &peer(3), false, &resolve),
        Err(ManageError::NotFound(_))
    ));
}

#[test]
fn grant_remove_by_export_by_all_and_everywhere() {
    let text = format!("[all]\nrw = [\"{a}\"]\n[exports.p]\npath = \"/p\"\nrw = [\"{a}\", \"{b}\"]\nro = [\"lap\"]\n[exports.q]\npath = \"/q\"\nro = [\"{a}\"]\n", a = id(1), b = id(2));
    let resolve = |s: &str| (s == "lap").then(|| id(1));
    let mut m = msg(&text);
    assert_eq!(
        grant_remove(&mut m, Some("p"), Some(&peer(1)), &resolve).unwrap(),
        2
    ); // raw id and the alias that resolves to it
    assert_eq!(export(&m, "p").rw.len(), 1);
    assert!(export(&m, "p").ro.is_empty());
    let mut m = msg(&text);
    assert_eq!(
        grant_remove(&mut m, None, Some(&peer(1)), &resolve).unwrap(),
        4
    ); // everywhere
    let mut m = msg(&text);
    assert_eq!(grant_remove(&mut m, Some("p"), None, &resolve).unwrap(), 3); // all of p
    assert!(matches!(
        grant_remove(&mut m, Some("p"), None, &resolve),
        Err(ManageError::NothingToRemove(_))
    ));
}

#[test]
fn a_second_grant_add_after_a_comment_keeps_the_comment_and_valid_toml() {
    let mut m = msg("[exports.p]\npath = \"/p\"\n");
    let none = |_: &str| None;
    let keyed = PeerSpec::Id {
        id: id(1),
        comment: Some("c1".into()),
    };
    grant_add(&mut m, Some("p"), &keyed, false, &none).unwrap();
    grant_add(&mut m, Some("p"), &peer(2), false, &none).unwrap();
    let text = write(&m);
    let back = msg(&text);
    assert_eq!(typed(&back).exports.exports["p"].rw.len(), 2, "{text}");
    assert!(text.contains(&format!("\"{}\", # c1\n", id(1))), "{text}");
    assert_eq!(back, m, "{text}");
}

#[test]
fn removing_an_entry_takes_its_comment_and_leaves_the_others() {
    let mut m = msg(&format!(
        "[exports.p]\npath = \"/p\"\nrw = [\n    # above a\n    \"a\", # one\n    \"{}\", # two\n    \"c\", # three\n]\n",
        id(1)
    ));
    assert_eq!(
        grant_remove(&mut m, Some("p"), Some(&peer(1)), &|_| None).unwrap(),
        1
    );
    let rw = &export(&m, "p").rw;
    assert_eq!(values(rw), ["a", "c"]);
    assert_eq!((trailing(&rw[0]), trailing(&rw[1])), ("# one", "# three"));
    assert_eq!(rw[0].comment.as_ref().unwrap().above, ["# above a"]);
    assert!(!write(&m).contains("# two"), "{}", write(&m));
}

#[test]
fn grant_remove_matches_a_dangling_alias_by_literal_text() {
    let mut m = msg("[exports.p]\npath = \"/p\"\nrw = [\"gone\"]\n");
    let none = |_: &str| None;
    assert_eq!(
        grant_remove(&mut m, None, Some(&PeerSpec::Alias("gone".into())), &none).unwrap(),
        1
    );
    // The emptied list stays in the file, as today.
    assert_eq!(write(&m), "[exports.p]\npath = \"/p\"\nrw = []\n");
}

#[test]
fn grant_add_refuses_a_comment_with_control_characters_and_leaves_the_message() {
    let mut m = msg("[exports.p]\npath = \"/p\"\n");
    let before = m.clone();
    let none = |_: &str| None;
    for c in [INJECTED, "a\rb", "tab\there"] {
        let evil = PeerSpec::Id {
            id: id(1),
            comment: Some(c.into()),
        };
        let r = grant_add(&mut m, Some("p"), &evil, false, &none);
        assert!(matches!(r, Err(ManageError::BadComment(_))), "{c:?}: {r:?}");
        assert_eq!(m, before);
        let r = grant_immediate(&mut m, &evil, &none);
        assert!(matches!(r, Err(ManageError::BadComment(_))), "{c:?}: {r:?}");
        assert_eq!(m, before);
    }
}

#[test]
fn a_name_or_comment_with_brackets_makes_no_second_table() {
    let mut m = msg("[exports.p]\npath = \"/p\"\n");
    let none = |_: &str| None;
    grant_add(
        &mut m,
        Some("p"),
        &PeerSpec::Alias(INJECTED.into()),
        false,
        &none,
    )
    .unwrap();
    let bracket = PeerSpec::Id {
        id: id(1),
        comment: Some("x ] [etc] path = \"/etc\"".into()),
    };
    grant_add(&mut m, Some("p"), &bracket, true, &none).unwrap();
    let text = write(&m);
    let back = msg(&text);
    assert_eq!(
        back.exports
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>(),
        ["p"],
        "{text}"
    );
    assert_eq!(values(&export(&back, "p").rw), [INJECTED]);
    assert_eq!(
        trailing(&export(&back, "p").ro[0]),
        "# x ] [etc] path = \"/etc\""
    );
}

#[test]
fn grant_add_to_all_creates_the_all_table() {
    let mut m = msg("[exports.p]\npath = \"/p\"\n");
    grant_add(&mut m, None, &peer(1), false, &|_| None).unwrap();
    let s = write(&m);
    assert!(s.contains(&format!("[all]\nrw = [\"{}\"]", id(1))), "{s}");
    assert_eq!(typed(&msg(&s)).exports.rw, vec![id(1).to_string()]);
}

#[test]
fn grant_remove_all_all_empties_every_array_in_the_file() {
    let mut m = msg(&format!(
        "[all]\nrw = [\"{a}\"]\n[exports.p]\npath = \"/p\"\nrw = [\"{a}\"]\nro = [\"{b}\"]\n[exports.q]\npath = \"/q\"\nro = [\"lap\"]\n",
        a = id(1),
        b = id(2)
    ));
    assert_eq!(grant_remove(&mut m, None, None, &|_| None).unwrap(), 4);
    let x = typed(&msg(&write(&m))).exports;
    assert!(x.rw.is_empty() && x.ro.is_empty());
    assert!(
        x.exports
            .values()
            .all(|e| e.rw.is_empty() && e.ro.is_empty()),
        "{}",
        write(&m)
    );
    assert!(matches!(
        grant_remove(&mut m, None, None, &|_| None),
        Err(ManageError::NothingToRemove(_))
    ));
}

#[test]
fn grant_edits_reach_inline_tables() {
    // Inline tables read into the same message as headers do.
    let text = format!(
        "all = {{ rw = [\"{a}\"] }}\nexports = {{ photos = {{ path = \"/p\", rw = [\"{a}\"] }} }}\n",
        a = id(1)
    );
    let mut m = msg(&text);
    assert_eq!(
        grant_remove(&mut m, None, Some(&peer(1)), &|_| None).unwrap(),
        2
    );
    let x = typed(&m).exports;
    assert!(x.rw.is_empty() && x.exports["photos"].rw.is_empty());
    grant_add(&mut m, Some("photos"), &peer(2), true, &|_| None).unwrap();
    grant_add(&mut m, None, &peer(2), false, &|_| None).unwrap();
    let x = typed(&m).exports;
    assert_eq!(x.exports["photos"].ro, vec![id(2).to_string()]);
    assert_eq!(x.rw, vec![id(2).to_string()]);
    let mut m = msg(&format!(
        "[exports]\nphotos = {{ path = \"/p\", rw = [\"{a}\"] }}\n[exports.q]\npath = \"/q\"\nrw = [\"{a}\"]\n",
        a = id(1)
    ));
    assert_eq!(
        grant_remove(&mut m, None, Some(&peer(1)), &|_| None).unwrap(),
        2
    );
    assert_eq!(
        grant_remove(&mut m, None, None, &|_| None)
            .unwrap_err()
            .to_string(),
        "nothing to remove for \"all\""
    );
}

#[test]
fn grant_immediate_adds_once() {
    let mut m = pb::Config::default();
    let resolve = |_: &str| None;
    assert!(grant_immediate(&mut m, &peer(1), &resolve).unwrap());
    assert!(!grant_immediate(&mut m, &peer(1), &resolve).unwrap());
    assert_eq!(m.immediate.as_ref().unwrap().rw.len(), 1);
    assert_eq!(write(&m), format!("[immediate]\nrw = [\"{}\"]\n", id(1)));
    assert_eq!(
        grant_remove_immediate(&mut m, Some(&peer(1)), &resolve).unwrap(),
        1
    );
    assert_eq!(write(&m), "[immediate]\nrw = []\n");
    assert!(matches!(
        grant_remove_immediate(&mut m, None, &resolve),
        Err(ManageError::NothingToRemove(w)) if w == "immediate"
    ));
}

#[test]
fn grant_immediate_knows_the_other_spelling() {
    let mut m = msg("[immediate]\nrw = [\"lap\"]\n");
    let resolve = |s: &str| (s == "lap").then(|| id(1));
    assert!(!grant_immediate(&mut m, &peer(1), &resolve).unwrap());
    assert_eq!(grant_remove_immediate(&mut m, None, &resolve).unwrap(), 1);
}

/// `[hosts.box]` and grants that name it in three tables.
fn named_everywhere() -> pb::Config {
    msg(
        "[all]\nrw = [\"box\", \"x\"]\n[immediate]\nrw = [\"box\"]\n\
         [exports.p]\npath = \"/p\"\nro = [\"y\", \"box\"]\n\
         [exports.q]\npath = \"/q\"\nrw = [\"y\"]\n",
    )
}

#[test]
fn lists_naming_remove_named_and_rename_named_walk_every_table() {
    let m = named_everywhere();
    assert_eq!(lists_naming(&m, "box"), ["all", "immediate", "exports.p"]);
    assert_eq!(lists_naming(&m, "y"), ["exports.p", "exports.q"]);
    assert!(lists_naming(&m, "nobody").is_empty());
    let mut r = m.clone();
    assert_eq!(rename_named(&mut r, "box", "crate"), 3);
    assert!(lists_naming(&r, "box").is_empty());
    assert_eq!(lists_naming(&r, "crate"), ["all", "immediate", "exports.p"]);
    let mut r = m.clone();
    assert_eq!(remove_named(&mut r, "box"), 3);
    assert!(lists_naming(&r, "box").is_empty());
    assert_eq!(values(&r.all.as_ref().unwrap().rw), ["x"]);
}

/// `[hosts.NAME]` tables for `(alias, id number)` pairs.
fn host_tables(hosts: &[(&str, u8)]) -> String {
    hosts
        .iter()
        .map(|(a, n)| format!("[hosts.{a}]\nendpoint_id = \"{}\"\n", id(*n)))
        .collect()
}

fn known(hosts: &[(&str, u8)]) -> Hosts {
    typed(&msg(&host_tables(hosts))).hosts
}

/// Runs `canonicalize` on the hosts tables followed by `body`; the file
/// written afterwards without the hosts, and whether anything changed.
fn canon(hosts: &[(&str, u8)], body: &str) -> (String, bool) {
    let mut m = msg(&format!("{}{body}", host_tables(hosts)));
    let changed = canonicalize(&mut m, &known(hosts));
    m.hosts.clear();
    (write(&m), changed)
}

/// `body` in the writer's form.
fn written(body: &str) -> String {
    write(&msg(body))
}

const BOX: &[(&str, u8)] = &[("box", 1)];

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
        written(
            "[all]\nro = [\"box\"]\n[immediate]\nrw = [\"box\"]\n\
             [exports.p]\npath = \"/p\"\nrw = [\"box\"]\n\
             [exports.q]\npath = \"/q\"\nro = [\"box\"]\n"
        )
    );
    // Inline tables count, as for the other editors; top-level keys come
    // before the hosts tables.
    let mut m = msg(&format!(
        "exports = {{ p = {{ path = \"/p\", rw = [\"{i}\"] }} }}\nall = {{ ro = [\"{i}\"] }}\n{}",
        host_tables(BOX)
    ));
    assert!(canonicalize(&mut m, &known(BOX)));
    m.hosts.clear();
    assert_eq!(
        write(&m),
        written("[exports.p]\npath = \"/p\"\nrw = [\"box\"]\n[all]\nro = [\"box\"]\n")
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
        written("[exports.p]\npath = \"/p\"\nrw = [\"box\"]\nro = [\"x\"]\n")
    );
    // Each table on its own: rw in [all] does not touch ro of an export.
    let body = "[all]\nrw = [\"box\"]\n[exports.p]\npath = \"/p\"\nro = [\"box\"]\n";
    assert_eq!(canon(BOX, body), (written(body), false));
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
fn comments_are_kept() {
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
            "# who may write\n[all]\nrw = [\n    \"box\", # laptop\n    \"{}\", # other\n] # end\n",
            id(2)
        )
    );
}

#[test]
fn canonicalize_keeps_the_comment_of_the_entry_that_stays() {
    // rw holds the id (with a comment) and its alias: the first stays, renamed to the alias, with its comment.
    let mut m = read(&format!("[hosts.laptop]\nclient_id = \"{0}\"\n[all]\nrw = [\n    \"{0}\", # mine\n    \"laptop\",\n]\n", id(1)), Path::new("f")).unwrap();
    let hosts = Config::from_message(&m, Path::new("f")).unwrap().hosts;
    assert!(canonicalize(&mut m, &hosts));
    let rw = &m.all.as_ref().unwrap().rw;
    assert_eq!(rw.len(), 1);
    assert_eq!(
        (
            rw[0].value.as_str(),
            rw[0].comment.as_ref().unwrap().trailing.as_str()
        ),
        ("laptop", "# mine")
    );
}

#[test]
fn second_run_changes_nothing() {
    let mut m = msg(&format!(
        "{}[all]\nrw = [\"{1}\", \"box\"]\nro = [\"{1}\"]\n[immediate]\nrw = [\"{1}\"]\n",
        host_tables(BOX),
        id(1)
    ));
    let hosts = known(BOX);
    assert!(canonicalize(&mut m, &hosts));
    let once = m.clone();
    assert!(!canonicalize(&mut m, &hosts));
    assert_eq!(m, once);
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
    assert_eq!(
        text,
        format!(
            "[exports.p]\npath = \"/p\"\nrw = [\n    \"box\", # one\n    \"{}\", # two\n]\n",
            id(2)
        )
    );
}
