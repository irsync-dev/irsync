use irsync::config::clients::Clients;
use irsync::config::exports::Exports;
use irsync::config::file::Config;
use irsync::config::reader;
use irsync::config::writer::write;
use irsync::manage::grants::{
    canonicalize, grant_add, grant_remove, list_all, list_export, list_peer, lists_naming,
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
    reader::read(text, Path::new("f")).unwrap()
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
    grant_add(&mut m, "p", &laptop, false, &resolve).unwrap(); // replaces the raw id
    assert_eq!(values(&export(&m, "p").rw), ["laptop"]);
    grant_add(&mut m, "p", &laptop, true, &resolve).unwrap(); // rw -> ro
    assert!(export(&m, "p").rw.is_empty() && export(&m, "p").rw_set);
    assert_eq!(values(&export(&m, "p").ro), ["laptop"]);
    let keyed = PeerSpec::Id {
        id: id(2),
        comment: Some("mike@laptop".into()),
    };
    grant_add(&mut m, "p", &keyed, false, &resolve).unwrap();
    let rw = &export(&m, "p").rw;
    assert_eq!(values(rw), [id(2).to_string()]);
    assert_eq!(trailing(&rw[0]), "# mike@laptop");
    assert!(
        write(&m).contains(&format!("rw = [\n    \"{}\", # mike@laptop\n]", id(2))),
        "{}",
        write(&m)
    );
    assert!(matches!(
        grant_add(&mut m, "zz", &peer(3), false, &resolve),
        Err(ManageError::NotFound(_))
    ));
}

#[test]
fn grant_remove_by_export_by_all_and_everywhere() {
    let text = format!("[exports.p]\npath = \"/p\"\nrw = [\"{a}\", \"{b}\"]\nro = [\"lap\"]\n[exports.q]\npath = \"/q\"\nro = [\"{a}\"]\n", a = id(1), b = id(2));
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
        3
    ); // every export
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
    grant_add(&mut m, "p", &keyed, false, &none).unwrap();
    grant_add(&mut m, "p", &peer(2), false, &none).unwrap();
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
        let r = grant_add(&mut m, "p", &evil, false, &none);
        assert!(matches!(r, Err(ManageError::BadComment(_))), "{c:?}: {r:?}");
        assert_eq!(m, before);
    }
}

#[test]
fn a_name_or_comment_with_brackets_makes_no_second_table() {
    let mut m = msg("[exports.p]\npath = \"/p\"\n");
    let none = |_: &str| None;
    grant_add(&mut m, "p", &PeerSpec::Alias(INJECTED.into()), false, &none).unwrap();
    let bracket = PeerSpec::Id {
        id: id(1),
        comment: Some("x ] [etc] path = \"/etc\"".into()),
    };
    grant_add(&mut m, "p", &bracket, true, &none).unwrap();
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
fn grant_remove_all_all_empties_every_array_in_the_file() {
    let mut m = msg(&format!(
        "[exports.p]\npath = \"/p\"\nrw = [\"{a}\"]\nro = [\"{b}\"]\n[exports.q]\npath = \"/q\"\nro = [\"lap\"]\n",
        a = id(1),
        b = id(2)
    ));
    assert_eq!(grant_remove(&mut m, None, None, &|_| None).unwrap(), 3);
    let x = typed(&msg(&write(&m))).exports;
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
        "exports = {{ photos = {{ path = \"/p\", rw = [\"{a}\"] }} }}\n",
        a = id(1)
    );
    let mut m = msg(&text);
    assert_eq!(
        grant_remove(&mut m, None, Some(&peer(1)), &|_| None).unwrap(),
        1
    );
    let x = typed(&m).exports;
    assert!(x.exports["photos"].rw.is_empty());
    grant_add(&mut m, "photos", &peer(2), true, &|_| None).unwrap();
    let x = typed(&m).exports;
    assert_eq!(x.exports["photos"].ro, vec![id(2).to_string()]);
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

/// Grants that name `box` in three exports.
fn named_everywhere() -> pb::Config {
    msg(
        "[exports.a]\npath = \"/a\"\nrw = [\"box\", \"x\"]\n[exports.b]\npath = \"/b\"\nrw = [\"box\"]\n\
         [exports.p]\npath = \"/p\"\nro = [\"y\", \"box\"]\n\
         [exports.q]\npath = \"/q\"\nrw = [\"y\"]\n",
    )
}

#[test]
fn lists_naming_remove_named_and_rename_named_walk_every_table() {
    let m = named_everywhere();
    assert_eq!(
        lists_naming(&m, "box"),
        ["exports.a", "exports.b", "exports.p"]
    );
    assert_eq!(lists_naming(&m, "y"), ["exports.p", "exports.q"]);
    assert!(lists_naming(&m, "nobody").is_empty());
    let mut r = m.clone();
    assert_eq!(rename_named(&mut r, "box", "crate"), 3);
    assert!(lists_naming(&r, "box").is_empty());
    assert_eq!(
        lists_naming(&r, "crate"),
        ["exports.a", "exports.b", "exports.p"]
    );
    let mut r = m.clone();
    assert_eq!(remove_named(&mut r, "box"), 3);
    assert!(lists_naming(&r, "box").is_empty());
    assert_eq!(values(&export(&r, "a").rw), ["x"]);
}

/// `[clients.NAME]` tables for `(name, id number)` pairs.
fn host_tables(hosts: &[(&str, u8)]) -> String {
    hosts
        .iter()
        .map(|(a, n)| format!("[clients.{a}]\nclient_id = \"{}\"\n", id(*n)))
        .collect()
}

fn known(hosts: &[(&str, u8)]) -> Clients {
    typed(&msg(&host_tables(hosts))).clients
}

/// Runs `canonicalize` on the clients tables followed by `body`; the file
/// written afterwards without the clients, and whether anything changed.
fn canon(hosts: &[(&str, u8)], body: &str) -> (String, bool) {
    let mut m = msg(&format!("{}{body}", host_tables(hosts)));
    let changed = canonicalize(&mut m, &known(hosts));
    m.clients.clear();
    (write(&m), changed)
}

/// `body` in the writer's form.
fn written(body: &str) -> String {
    write(&msg(body))
}

const BOX: &[(&str, u8)] = &[("box", 1)];

#[test]
fn id_becomes_alias() {
    let (text, changed) = canon(
        BOX,
        &format!("[exports.p]\npath = \"/p\"\nrw = [\"{}\"]\n", id(1)),
    );
    assert_eq!(text, "[exports.p]\npath = \"/p\"\nrw = [\"box\"]\n");
    assert!(changed);
}

#[test]
fn every_grant_list_is_visited() {
    let i = id(1);
    let (text, _) = canon(
        BOX,
        &format!(
            "[exports.a]\npath = \"/a\"\nro = [\"{i}\"]\n[exports.b]\npath = \"/b\"\nrw = [\"{i}\"]\n\
             [exports.p]\npath = \"/p\"\nrw = [\"{i}\"]\n\
             [exports.q]\npath = \"/q\"\nro = [\"{i}\"]\n"
        ),
    );
    assert_eq!(
        text,
        written(
            "[exports.a]\npath = \"/a\"\nro = [\"box\"]\n[exports.b]\npath = \"/b\"\nrw = [\"box\"]\n\
             [exports.p]\npath = \"/p\"\nrw = [\"box\"]\n\
             [exports.q]\npath = \"/q\"\nro = [\"box\"]\n"
        )
    );
    // Inline tables count, as for the other editors; top-level keys come
    // before the clients tables.
    let mut m = msg(&format!(
        "exports = {{ p = {{ path = \"/p\", rw = [\"{i}\"] }}, q = {{ path = \"/q\", ro = [\"{i}\"] }} }}\n{}",
        host_tables(BOX)
    ));
    assert!(canonicalize(&mut m, &known(BOX)));
    m.clients.clear();
    assert_eq!(
        write(&m),
        written("[exports.p]\npath = \"/p\"\nrw = [\"box\"]\n[exports.q]\npath = \"/q\"\nro = [\"box\"]\n")
    );
}

#[test]
fn first_alias_in_order_wins() {
    let (text, _) = canon(
        &[("zed", 1), ("amy", 1)],
        &format!("[exports.p]\npath = \"/p\"\nrw = [\"{}\"]\n", id(1)),
    );
    assert_eq!(text, "[exports.p]\npath = \"/p\"\nrw = [\"amy\"]\n");
}

#[test]
fn existing_alias_is_left_alone() {
    let body = "[exports.p]\npath = \"/p\"\nrw = [\"zed\"]\n";
    assert_eq!(
        canon(&[("zed", 1), ("amy", 1)], body),
        (body.to_string(), false)
    );
}

#[test]
fn unknown_entries_are_left_alone() {
    let body = format!(
        "[exports.p]\npath = \"/p\"\nrw = [\"{}\", \"ghost\"]\n",
        id(2)
    );
    assert_eq!(canon(BOX, &body), (body.clone(), false));
}

#[test]
fn duplicate_in_one_list_keeps_the_first() {
    let (text, changed) = canon(
        BOX,
        &format!(
            "[exports.p]\npath = \"/p\"\nrw = [\"box\", \"{}\"]\n",
            id(1)
        ),
    );
    assert_eq!(text, "[exports.p]\npath = \"/p\"\nrw = [\"box\"]\n");
    assert!(changed);
    let (text, _) = canon(
        BOX,
        &format!(
            "[exports.p]\npath = \"/p\"\nrw = [\"{}\", \"box\"]\n",
            id(1)
        ),
    );
    assert_eq!(text, "[exports.p]\npath = \"/p\"\nrw = [\"box\"]\n");
    // Duplicates that were already there go too.
    let (text, changed) = canon(
        BOX,
        "[exports.p]\npath = \"/p\"\nrw = [\"box\", \"x\", \"box\"]\n",
    );
    assert_eq!(text, "[exports.p]\npath = \"/p\"\nrw = [\"box\", \"x\"]\n");
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
    // Each export on its own: rw in one does not touch ro of another.
    let body =
        "[exports.a]\npath = \"/a\"\nrw = [\"box\"]\n[exports.p]\npath = \"/p\"\nro = [\"box\"]\n";
    assert_eq!(canon(BOX, body), (written(body), false));
}

#[test]
fn two_aliases_of_one_id_both_stay() {
    let body = "[exports.p]\npath = \"/p\"\nrw = [\"amy\", \"zed\"]\n";
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
            "# who may write\n[exports.p]\npath = \"/p\"\nrw = [\n    \"{}\", # laptop\n    \"{}\",   # other\n] # end\n",
            id(1),
            id(2)
        ),
    );
    assert_eq!(
        text,
        format!(
            "# who may write\n[exports.p]\npath = \"/p\"\nrw = [\n    \"box\", # laptop\n    \"{}\", # other\n] # end\n",
            id(2)
        )
    );
}

#[test]
fn canonicalize_keeps_the_comment_of_the_entry_that_stays() {
    // rw holds the id (with a comment) and its alias: the first stays, renamed to the alias, with its comment.
    let mut m = msg(&format!("[clients.laptop]\nclient_id = \"{0}\"\n[exports.p]\npath = \"/p\"\nrw = [\n    \"{0}\", # mine\n    \"laptop\",\n]\n", id(1)));
    let hosts = Config::from_message(&m, Path::new("f")).unwrap().clients;
    assert!(canonicalize(&mut m, &hosts));
    let rw = &export(&m, "p").rw;
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
        "{}[exports.p]\npath = \"/p\"\nrw = [\"{1}\", \"box\"]\nro = [\"{1}\"]\n[exports.q]\npath = \"/q\"\nrw = [\"{1}\"]\n",
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
    let (text, _) = canon(
        BOX,
        &format!("[exports.p]\npath = \"/p\"\nrw = [\"{z32}\"]\n"),
    );
    assert_eq!(text, "[exports.p]\npath = \"/p\"\nrw = [\"box\"]\n");
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

/// The written form of `body` after `f` ran on it.
fn after(body: &str, f: impl FnOnce(&mut pb::Config)) -> String {
    let mut m = msg(body);
    f(&mut m);
    write(&m)
}

#[test]
fn removing_a_middle_entry_keeps_the_lines_above_it_on_the_next() {
    let text = after(
        "[exports.p]\npath = \"/p\"\nrw = [\n    \"a\",\n    # about b\n    \"b\", # bee\n    \"c\",\n]\n",
        |m| {
            grant_remove(m, Some("p"), Some(&PeerSpec::Alias("b".into())), &|_| None).unwrap();
        },
    );
    assert_eq!(
        text,
        "[exports.p]\npath = \"/p\"\nrw = [\n    \"a\",\n    # about b\n    \"c\",\n]\n"
    );
}

#[test]
fn removing_the_last_entry_moves_the_lines_before_the_bracket_above_the_one_before() {
    let text = after(
        "[exports.p]\npath = \"/p\"\nrw = [\n    \"a\",\n    \"b\", # bee\n    # tail\n]\n",
        |m| {
            grant_remove(m, Some("p"), Some(&PeerSpec::Alias("b".into())), &|_| None).unwrap();
        },
    );
    assert_eq!(
        text,
        "[exports.p]\npath = \"/p\"\nrw = [\n    # tail\n    \"a\",\n]\n"
    );
}

#[test]
fn removing_the_only_entry_moves_its_lines_above_the_key() {
    let text = after(
        "[exports.p]\npath = \"/p\"\nrw = [\n    # heading\n    \"b\", # bee\n]\n",
        |m| {
            assert_eq!(remove_named(m, "b"), 1);
        },
    );
    assert_eq!(text, "[exports.p]\npath = \"/p\"\n# heading\nrw = []\n");
}

#[test]
fn a_duplicate_dropped_by_canonicalize_leaves_its_lines_above() {
    let mut m = msg(&format!(
        "{}[exports.p]\npath = \"/p\"\nrw = [\n    \"box\", # mine\n    # the id\n    \"{}\", # dup\n    \"x\",\n]\n",
        host_tables(BOX),
        id(1)
    ));
    assert!(canonicalize(&mut m, &known(BOX)));
    m.clients.clear();
    assert_eq!(
        write(&m),
        "[exports.p]\npath = \"/p\"\nrw = [\n    \"box\", # mine\n    # the id\n    \"x\",\n]\n"
    );
}

#[test]
fn grant_add_moves_an_entry_with_its_comment_and_replaces_another_spelling() {
    let resolve = |s: &str| (s == "lap").then(|| id(1));
    // Same text, rw to ro: the whole comment goes along.
    let text = after(
        "[exports.p]\npath = \"/p\"\nrw = [\n    \"x\",\n    # the laptop\n    \"lap\", # mine\n]\n",
        |m| grant_add(m, "p", &PeerSpec::Alias("lap".into()), true, &resolve).unwrap(),
    );
    assert_eq!(
        text,
        "[exports.p]\npath = \"/p\"\nrw = [\"x\"]\nro = [\n    # the laptop\n    \"lap\", # mine\n]\n"
    );
    // Another spelling: replaced, the lines above it stay.
    let text = after(
        &format!(
            "[exports.p]\npath = \"/p\"\nrw = [\n    \"x\",\n    # the laptop\n    \"{}\", # old\n]\n",
            id(1)
        ),
        |m| grant_add(m, "p", &PeerSpec::Alias("lap".into()), false, &resolve).unwrap(),
    );
    assert_eq!(
        text,
        "[exports.p]\npath = \"/p\"\nrw = [\n    # the laptop\n    \"x\",\n    \"lap\",\n]\n"
    );
}

fn fixture() -> (Clients, Exports) {
    let h = parse_clients(
        &format!(
            "[clients.laptop]\nclient_id = \"{}\"\nall_exports = \"rw\"\n",
            id(1)
        ),
        Path::new("h"),
    )
    .unwrap();
    let e = parse_exports(
        &format!(
            "[exports.backups]\npath = \"/b\"\n[exports.docs]\npath = \"/d\"\nread_only = true\nrw = [\"laptop\"]\n[exports.photos]\npath = \"/p\"\nro = [\"{}\"]\n",
            id(2)
        ),
        Path::new("e"),
    )
    .unwrap();
    (h, e)
}

#[test]
fn list_all_names_the_rights_then_every_export() {
    let (h, e) = fixture();
    let s = list_all(&e, &h);
    assert_eq!(
        s,
        format!(
            "all_exports:\n    rw: laptop ({l})\nall_paths:\nadmin:\nbackups:\ndocs:\n    rw: laptop ({l})\nphotos:\n    ro: {o}\n",
            l = id(1),
            o = id(2)
        )
    );
    assert_eq!(
        list_export(&e, &h, "docs").unwrap(),
        format!("docs:\n    rw: laptop ({})\n", id(1))
    );
    assert_eq!(
        list_export(&e, &h, "all_exports").unwrap(),
        format!("all_exports:\n    rw: laptop ({})\n", id(1))
    );
    assert!(list_export(&e, &h, "all").is_none());
    assert!(list_export(&e, &h, "nope").is_none());
}

#[test]
fn grant_list_shows_all_paths_rights() {
    let h = parse_clients(
        &format!(
            "[clients.laptop]\nclient_id = \"{}\"\nall_paths = \"rw\"\n\
             [clients.other]\nclient_id = \"{}\"\nall_exports = \"ro\"\n",
            id(1),
            id(2)
        ),
        Path::new("h"),
    )
    .unwrap();
    let e = parse_exports(
        "[exports.auto-h]\npath = \"/h\"\nauto = true\nrw = [\"laptop\"]\n",
        Path::new("e"),
    )
    .unwrap();
    assert_eq!(
        list_all(&e, &h),
        format!(
            "all_exports:\n    ro: other ({o})\nall_paths:\n    rw: laptop ({l})\nadmin:\nauto-h:\n    rw: laptop ({l})\n",
            l = id(1),
            o = id(2)
        )
    );
    assert_eq!(
        list_export(&e, &h, "all_paths").unwrap(),
        format!("all_paths:\n    rw: laptop ({})\n", id(1))
    );
    assert_eq!(
        list_peer(&e, &h, true, "laptop", &id(1)),
        "laptop:\n    all_paths: rw\n    auto-h: rw\n"
    );
    // all_exports never reaches an automatic export.
    assert_eq!(list_peer(&e, &h, true, "other", &id(2)), "other:\n");
}

#[test]
fn list_peer_marks_inherited_and_capped_grants() {
    let (h, e) = fixture();
    let s = list_peer(&e, &h, true, &format!("laptop ({})", id(1)), &id(1));
    assert_eq!(
        s,
        format!(
            "laptop ({}):\n    backups: rw (all_exports)\n    docs: ro (read_only export)\n    photos: rw (all_exports)\n",
            id(1)
        )
    );
    assert_eq!(
        list_peer(&e, &h, true, &id(2).to_string(), &id(2)),
        format!("{}:\n    photos: ro\n", id(2))
    );
    assert_eq!(list_peer(&e, &h, true, "x", &id(3)), "x:\n");
}

fn parse_clients(
    text: &str,
    path: &std::path::Path,
) -> Result<Clients, irsync::config::ConfigError> {
    irsync::config::file::Config::parse(text, path).map(|c| c.clients)
}

fn parse_exports(
    text: &str,
    path: &std::path::Path,
) -> Result<irsync::config::exports::Exports, irsync::config::ConfigError> {
    irsync::config::file::Config::parse(text, path).map(|c| c.exports)
}

#[test]
fn alias_of_is_the_first_alias_in_order() {
    let h = parse_clients(
        &host_tables(&[("zed", 1), ("amy", 1), ("bob", 2)]),
        Path::new("h"),
    )
    .unwrap();
    assert_eq!(h.alias_of(&id(1)), Some("amy"));
    assert_eq!(h.alias_of(&id(2)), Some("bob"));
    assert_eq!(h.alias_of(&id(3)), None);
}

#[test]
fn a_comment_line_after_a_removed_last_entry_stays() {
    // The reader hangs the lines before `]` on the last entry; when that
    // entry goes they stay, above the entry before it.
    let (text, _) = canon(
        BOX,
        &format!(
            "[exports.p]\npath = \"/p\"\nrw = [\n    \"box\",\n    \"{}\"\n    # tail\n]\n",
            id(1)
        ),
    );
    assert_eq!(
        text,
        "[exports.p]\npath = \"/p\"\nrw = [\n    # tail\n    \"box\",\n]\n"
    );
    // The removed entry's own comment goes with it; the line after stays.
    let (text, _) = canon(
        BOX,
        &format!(
            "[exports.p]\npath = \"/p\"\nrw = [\n    \"box\", # one\n    \"{}\" # two\n    # tail\n]\n",
            id(1)
        ),
    );
    assert_eq!(
        text,
        "[exports.p]\npath = \"/p\"\nrw = [\n    # tail\n    \"box\", # one\n]\n"
    );
    // On one line there is nothing to keep.
    let (text, _) = canon(
        BOX,
        &format!(
            "[exports.p]\npath = \"/p\"\nrw = [\"box\", \"{}\" ]\n",
            id(1)
        ),
    );
    assert_eq!(text, "[exports.p]\npath = \"/p\"\nrw = [\"box\"]\n");
}

/// Shapes the old editor met in a file and had to step around: the reader
/// refuses them, with an error and no panic, so they never reach an edit.
#[test]
fn odd_shapes_are_refused_by_the_reader() {
    for body in [
        format!("[exports.p]\npath = \"/p\"\nrw = [1, \"{}\"]\n", id(1)),
        "[exports.p]\npath = \"/p\"\nro = \"x\"\n".to_string(),
        "all = 5\n".to_string(),
        "exports = \"x\"\n".to_string(),
        "[exports]\np = 1\n".to_string(),
        "[[all]]\nrw = []\n".to_string(),
    ] {
        assert!(reader::read(&body, Path::new("f")).is_err(), "{body:?}");
    }
}
