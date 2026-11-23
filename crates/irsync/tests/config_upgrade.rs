//! Files of layout 1, with `[hosts]`, read into the message of layout 2,
//! with `[servers]` and `[clients]` (`config::legacy_v1`).
//!
//! The corpus is in `tests/fixtures/config_v1`: each `NAME.toml` of
//! layout 1 has beside it `NAME.v2.toml`, the text the writer makes of its
//! conversion, and `NAME.grants`, the access its ids had as the stage 1
//! code read the old file. A `.grants` file has one line per id named in
//! `NAME.toml` (a host's `endpoint_id` or `client_id`, or an id in a grant
//! list), sorted by id: the id, then a space and `EXPORT=rw` or
//! `EXPORT=ro` for each export it has its own grant for, in export order,
//! then `all=rw` or `all=ro` for its `[all]` grant and `immediate` for
//! its immediate grant, when it has them. `all` and `immediate` are no
//! export names. An id with no grant is alone on its line. Run the tests
//! with `IRSYNC_WRITE_GRANTS=1` to write the `.grants` files from the
//! stage 1 code again.

use iroh::EndpointId;
use irsync::authz::{Access, Grants, Snapshot};
use irsync::config::clients::Clients;
use irsync::config::file::Config;
use irsync::config::hosts::Hosts;
use irsync::config::legacy_v1;
use irsync::config::reader::read_values;
use irsync::config::writer::write;
use irsync::identity::parse_endpoint_id;
use irsync_proto::config as pb;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn p() -> &'static Path {
    Path::new("irsync.toml")
}

fn id(n: u8) -> EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
}

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config_v1")
}

/// The files of layout 1 of the corpus, by name without `.toml`.
fn corpus() -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = std::fs::read_dir(dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter_map(|p| {
            let name = p.file_name()?.to_str()?.to_owned();
            let stem = name.strip_suffix(".toml")?;
            (!stem.ends_with(".v2"))
                .then(|| (stem.to_owned(), std::fs::read_to_string(&p).unwrap()))
        })
        .collect();
    files.sort();
    assert!(files.len() >= 10, "the corpus is in {}", dir().display());
    files
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(dir().join(format!("{name}.toml"))).unwrap()
}

fn converted(text: &str) -> legacy_v1::Converted {
    legacy_v1::read(text, p()).unwrap()
}

/// Every comment line of `text`: each line from its `#` on.
fn comment_lines(text: &str) -> Vec<&str> {
    text.lines()
        .filter_map(|l| l.find('#').map(|i| l[i..].trim_end()))
        .collect()
}

/// Every id `text` names: those of its hosts and those in grant lists.
fn ids_named(old: &pb::Config) -> BTreeSet<EndpointId> {
    let key =
        |b: &Vec<u8>| EndpointId::from_bytes(&<[u8; 32]>::try_from(b.as_slice()).unwrap()).unwrap();
    let hosts = old
        .hosts
        .iter()
        .flat_map(|h| [h.endpoint_id.as_ref(), h.client_id.as_ref()])
        .flatten()
        .map(key);
    let lists = old
        .all
        .iter()
        .chain(&old.immediate)
        .flat_map(|g| [&g.rw, &g.ro])
        .chain(old.exports.iter().flat_map(|e| [&e.rw, &e.ro]))
        .flatten()
        .filter_map(|e| parse_endpoint_id(&e.value).ok());
    hosts.chain(lists).collect()
}

/// The access as the stage 1 code reads the old file.
fn old_snapshot(text: &str) -> Snapshot {
    let c = Config::parse(text, p()).unwrap();
    Snapshot::from_files(&c.hosts, &c.exports, true)
}

/// The access of the converted message: grants resolve against its
/// clients only. Until `Snapshot` takes clients, each client is a host
/// with only a `client_id`.
fn new_snapshot(msg: &pb::Config) -> Snapshot {
    let c = Config::from_message(msg, p()).unwrap();
    let hosts: Vec<pb::Host> = msg
        .clients
        .iter()
        .map(|c| pb::Host {
            name: c.name.clone(),
            client_id: Some(c.client_id.clone()),
            ..Default::default()
        })
        .collect();
    let hosts = Hosts::from_message(&hosts, p()).unwrap();
    Snapshot::from_files(&hosts, &c.exports, true)
}

fn access(a: Access) -> &'static str {
    match a {
        Access::ReadWrite => "rw",
        Access::ReadOnly => "ro",
    }
}

/// The lines of a `.grants` file (see the top of this file).
fn grants_lines(snap: &Snapshot, ids: &BTreeSet<EndpointId>) -> Vec<String> {
    let mut lines: Vec<String> = ids
        .iter()
        .map(|id| {
            let g: Grants = snap.grants(id);
            let mut line = id.to_string();
            for (export, a) in &g.exports {
                line += &format!(" {export}={}", access(*a));
            }
            if let Some(a) = g.all {
                line += &format!(" all={}", access(a));
            }
            if g.immediate {
                line += " immediate";
            }
            line
        })
        .collect();
    lines.sort();
    lines
}

/// Every file of the corpus converts to the text beside it.
#[test]
fn v1_corpus_converts_to_its_v2_text() {
    for (name, text) in corpus() {
        let want = std::fs::read_to_string(dir().join(format!("{name}.v2.toml"))).unwrap();
        let c = converted(&text);
        assert_eq!(c.config.config_version, Some(2), "{name}");
        assert!(c.config.hosts.is_empty(), "{name}");
        assert_eq!(write(&c.config), want, "{name}");
    }
}

/// Every comment line of each old file is in the converted one.
#[test]
fn conversion_keeps_every_comment() {
    for (name, text) in corpus() {
        let out = write(&converted(&text).config);
        let mut rest: Vec<&str> = comment_lines(&out);
        for line in comment_lines(&text) {
            let at = rest.iter().position(|l| *l == line);
            let at = at.unwrap_or_else(|| panic!("{name}: lost {line:?} in\n{out}"));
            rest.remove(at);
        }
    }
}

/// The grants of each old file, read the stage 1 way, equal those of the
/// converted file, and those in its `.grants` file.
#[test]
fn conversion_keeps_the_access() {
    let rewrite = std::env::var_os("IRSYNC_WRITE_GRANTS").is_some();
    for (name, text) in corpus() {
        let ids = ids_named(&read_values(&text, p()).unwrap());
        assert!(!ids.is_empty(), "{name}");
        let old = old_snapshot(&text);
        let new = new_snapshot(&converted(&text).config);
        for id in &ids {
            assert_eq!(new.grants(id), old.grants(id), "{name}: {id}");
        }
        assert_eq!(new.exports, old.exports, "{name}");
        let lines = grants_lines(&old, &ids);
        let file = dir().join(format!("{name}.grants"));
        if rewrite {
            std::fs::write(&file, lines.join("\n") + "\n").unwrap();
        }
        let pinned = std::fs::read_to_string(&file).unwrap();
        assert_eq!(pinned.lines().collect::<Vec<_>>(), lines, "{name}");
    }
}

/// Review Focus 1: one id as the `endpoint_id` of one host and the
/// `client_id` of another, both granted by name.
#[test]
fn shared_id_keeps_its_grants() {
    let text = fixture("shared_id");
    let x = parse_endpoint_id("069ffbb125300737c2767fed918cd5f4bc12b3102fdd9009a2a1320e2ebd3191")
        .unwrap();
    let c = converted(&text).config;
    let clients = Clients::from_message(&c.clients, p()).unwrap();
    assert_eq!(clients.resolve_peer("a").unwrap(), x);
    assert_eq!(clients.resolve_peer("b").unwrap(), x);
    assert_eq!(clients.alias_of(&x), Some("a"));
    let old = Config::parse(&text, p()).unwrap();
    assert_eq!(old.hosts.alias_of(&x), Some("a"));
    let (old, new) = (old_snapshot(&text), new_snapshot(&c));
    assert_eq!(new.grants(&x), old.grants(&x));
    assert_eq!(new.grants(&x).access("photos"), Some(Access::ReadWrite));
}

/// Review Focus 2: a host with only an `endpoint_id` gets its client
/// entry when any grant list names it, and none when no list does.
#[test]
fn every_grant_list_counts_for_keep_the_access() {
    let host = format!("[hosts.lap]\nendpoint_id = \"{}\"\n", id(1));
    let lists = [
        "[all]\nrw = [\"lap\"]\n",
        "[all]\nro = [\"lap\"]\n",
        "[immediate]\nrw = [\"lap\"]\n",
        "[exports.p]\npath = \"/p\"\nrw = [\"lap\"]\n",
        "[exports.p]\npath = \"/p\"\nro = [\"lap\"]\n",
    ];
    for list in lists {
        let text = format!("{host}{list}");
        let c = converted(&text).config;
        let names: Vec<_> = c
            .clients
            .iter()
            .map(|e| (e.name.as_str(), e.client_id.as_slice()))
            .collect();
        assert_eq!(names, [("lap", id(1).as_bytes().as_slice())], "{list}");
        let (old, new) = (old_snapshot(&text), new_snapshot(&c));
        assert!(!old.grants(&id(1)).is_empty(), "{list}");
        assert_eq!(new.grants(&id(1)), old.grants(&id(1)), "{list}");
    }
    let text = format!("{host}[exports.p]\npath = \"/p\"\nrw = [\"other\"]\n");
    assert!(converted(&text).config.clients.is_empty());
}

/// Review Focus 3: the comments of a bare `[hosts]`, or of
/// `hosts = { … }`, go to the first entry made from the first host.
#[test]
fn group_comments_go_to_the_first_entry() {
    let c = converted(&fixture("group_comments")).config;
    let above = |c: &Option<pb::Comment>| c.as_ref().map(|c| c.above.clone()).unwrap_or_default();
    assert_eq!(c.clients[0].name, "phone");
    assert_eq!(
        above(&c.clients[0].comment),
        [
            "# about my hosts",
            "# all of them",
            "# the first is the phone"
        ]
    );
    assert_eq!(above(&c.servers[0].comment), Vec::<String>::new());

    let text = format!(
        "# my hosts\nhosts = {{ nas = {{ endpoint_id = \"{}\", client_id = \"{}\" }}, lap = {{ client_id = \"{}\" }} }} # inline\n",
        id(1),
        id(2),
        id(3)
    );
    let c = converted(&text).config;
    assert_eq!(above(&c.servers[0].comment), ["# my hosts", "# inline"]);
    assert_eq!(above(&c.clients[0].comment), Vec::<String>::new());
    assert!(c.header.is_empty() && c.footer.is_empty(), "{c:?}");
}

#[test]
fn dial_keys_of_a_client_only_host_are_named() {
    let c = legacy_v1::read(
        &format!(
            "[hosts.phone]\nclient_id = \"{}\"\nrelay = \"https://r.example/\"\naddress = [\"p:1\"]\n",
            id(1)
        ),
        p(),
    )
    .unwrap();
    assert_eq!(
        c.dropped,
        ["[hosts.phone] had a client_id and no endpoint_id: dropped relay, address (they only apply to a server)"]
    );
    assert_eq!(converted(&fixture("client_with_dial_keys")).dropped, [
        "[hosts.phone] had a client_id and no endpoint_id: dropped relay, address, ssh (they only apply to a server)"
    ]);
    assert!(converted(&fixture("both_ids")).dropped.is_empty());
    assert!(converted(&fixture("client_only")).dropped.is_empty());
}

/// A bad file of layout 1 fails as the stage 1 reader failed it: same
/// message, same line.
#[test]
fn errors_are_those_of_the_stage_1_reader() {
    let bad = [
        "[hosts.\"a:b\"]\nendpoint_id = \"x\"\n".to_string(),
        "[hosts.nas]\nssh = \"me@nas\"\n".to_string(),
        "[hosts.nas]\n\nendpoint_id = \"not an id\"\n".to_string(),
        "[hosts.nas]\nendpoint_id = 1\n".to_string(),
        format!("[hosts.nas]\nendpoint_id = \"{}\"\nport = 1\n", id(1)),
        format!("[nas]\nendpoint_id = \"{}\"\n", id(1)),
        "[photos]\npath = \"/p\"\n".to_string(),
        "rw = [\"a\"]\n".to_string(),
        "[immediate]\nro = [\"a\"]\n".to_string(),
        "[exports.p]\npath = \"rel\"\n".to_string(),
        "[exports.all]\npath = \"/p\"\n".to_string(),
        "[client]\nrelay = \"not a url\"\n".to_string(),
        "[daemon]\nidle_timeout = \"5x\"\n".to_string(),
        "[servers.nas]\nendpoint_id = \"x\"\n".to_string(),
        "\n[clients.lap]\nclient_id = \"x\"\n".to_string(),
        "servers = 1\n".to_string(),
        "hosts = 1\n".to_string(),
        "[hosts\n".to_string(),
    ];
    for text in bad {
        let want = read_values(&text, p()).unwrap_err().to_string();
        let got = legacy_v1::read(&text, p()).unwrap_err().to_string();
        assert_eq!(got, want, "{text}");
    }
}

/// A file of layout 1 may say so: `config_version` is checked before this
/// layout is chosen, and ignored here.
#[test]
fn config_version_is_ignored() {
    let text = format!(
        "config_version = 1\n\n[hosts.nas]\nendpoint_id = \"{}\"\n",
        id(1)
    );
    let c = converted(&text).config;
    assert_eq!(c.config_version, Some(2));
    assert_eq!(c.servers.len(), 1);
}
