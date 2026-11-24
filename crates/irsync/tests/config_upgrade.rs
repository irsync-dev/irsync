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
//! export names. An id with no grant is alone on its line. The stage 1
//! code that wrote them is gone: they are the record of what it granted.

mod common;

use iroh::EndpointId;
use irsync::authz::{Access, Grants, Snapshot};
use irsync::config::clients::Clients;
use irsync::config::file::Config;
use irsync::config::legacy_v1;
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

/// Every id a converted message names: those of its servers and clients
/// (the ids of the old hosts) and those in grant lists.
fn ids_named(msg: &pb::Config) -> BTreeSet<EndpointId> {
    let key =
        |b: &Vec<u8>| EndpointId::from_bytes(&<[u8; 32]>::try_from(b.as_slice()).unwrap()).unwrap();
    let servers = msg.servers.iter().map(|s| key(&s.endpoint_id));
    let clients = msg.clients.iter().map(|c| key(&c.client_id));
    let lists = msg
        .all
        .iter()
        .chain(&msg.immediate)
        .flat_map(|g| [&g.rw, &g.ro])
        .chain(msg.exports.iter().flat_map(|e| [&e.rw, &e.ro]))
        .flatten()
        .filter_map(|e| parse_endpoint_id(&e.value).ok());
    servers.chain(clients).chain(lists).collect()
}

/// The access of the converted message: grants resolve against its
/// clients only.
fn new_snapshot(msg: &pb::Config) -> Snapshot {
    let c = Config::from_message(msg, p()).unwrap();
    Snapshot::from_files(&c.clients, &c.exports, true)
}

/// The pinned lines of `NAME.grants`.
fn pinned(name: &str) -> Vec<String> {
    std::fs::read_to_string(dir().join(format!("{name}.grants")))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
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
            if let Some(a) = g.all_exports {
                line += &format!(" all={}", access(a));
            }
            match g.all_paths {
                Some(Access::ReadWrite) => line += " immediate",
                Some(Access::ReadOnly) => line += " immediate=ro",
                None => {}
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

/// The grants of each converted file, read through its clients, are those
/// the stage 1 code gave the old file: its `.grants` file. The same holds
/// for the file as `Config::parse` reads it, and for the file the
/// conversion writes.
#[test]
fn conversion_keeps_the_access() {
    for (name, text) in corpus() {
        let msg = converted(&text).config;
        let ids = ids_named(&msg);
        assert!(!ids.is_empty(), "{name}");
        let lines = grants_lines(&new_snapshot(&msg), &ids);
        assert_eq!(lines, pinned(&name), "{name}");
        let parsed = Config::parse(&text, p()).unwrap();
        let snap = Snapshot::from_files(&parsed.clients, &parsed.exports, true);
        assert_eq!(grants_lines(&snap, &ids), lines, "{name}: parsed");
        let rewritten = Config::parse(&write(&msg), p()).unwrap();
        let snap = Snapshot::from_files(&rewritten.clients, &rewritten.exports, true);
        assert_eq!(grants_lines(&snap, &ids), lines, "{name}: rewritten");
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
    let new = new_snapshot(&c);
    let line = grants_lines(&new, &BTreeSet::from([x]));
    assert!(pinned("shared_id").contains(&line[0]), "{line:?}");
    assert_eq!(new.grants(&x).access("photos"), Some(Access::ReadWrite));
}

/// Review Focus 2: a host with only an `endpoint_id` gets its client
/// entry when any grant list names it, and none when no list does.
#[test]
fn every_grant_list_counts_for_keep_the_access() {
    let host = format!("[hosts.lap]\nendpoint_id = \"{}\"\n", id(1));
    // What the stage 1 code granted the host's one id for each list.
    let lists = [
        ("[all]\nrw = [\"lap\"]\n", " all=rw"),
        ("[all]\nro = [\"lap\"]\n", " all=ro"),
        ("[immediate]\nrw = [\"lap\"]\n", " immediate"),
        ("[exports.p]\npath = \"/p\"\nrw = [\"lap\"]\n", " p=rw"),
        ("[exports.p]\npath = \"/p\"\nro = [\"lap\"]\n", " p=ro"),
    ];
    for (list, granted) in lists {
        let text = format!("{host}{list}");
        let c = converted(&text).config;
        let names: Vec<_> = c
            .clients
            .iter()
            .map(|e| (e.name.as_str(), e.client_id.as_slice()))
            .collect();
        assert_eq!(names, [("lap", id(1).as_bytes().as_slice())], "{list}");
        let new = new_snapshot(&c);
        assert_eq!(
            grants_lines(&new, &BTreeSet::from([id(1)])),
            [format!("{}{granted}", id(1))],
            "{list}"
        );
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
/// message, same line. The messages are those that reader gave.
#[test]
fn errors_are_those_of_the_stage_1_reader() {
    let bad = [
        (
            "[hosts.\"a:b\"]\nendpoint_id = \"x\"\n".to_string(),
            "1: invalid host alias \"a:b\": must be non-empty, contain no ':' or '/', and be neither all, * nor an EndpointId",
        ),
        (
            "[hosts.nas]\nssh = \"me@nas\"\n".to_string(),
            "1: host \"nas\": needs an endpoint_id (its daemon), a client_id, or both",
        ),
        (
            "[hosts.nas]\n\nendpoint_id = \"not an id\"\n".to_string(),
            "3: host \"nas\": invalid endpoint id: not an id",
        ),
        (
            "[hosts.nas]\nendpoint_id = 1\n".to_string(),
            "2: invalid type: integer `1`, expected a string",
        ),
        (
            format!("[hosts.nas]\nendpoint_id = \"{}\"\nport = 1\n", id(1)),
            "3: unknown field `port`, expected one of `endpoint_id`, `client_id`, `relay`, `relay_mode`, `congestion`, `pq_key_exchange`, `address`, `parallel`, `reconnect`, `dial_retries`, `dht`, `mdns`, `ssh`, `ssh_aliases`",
        ),
        (
            format!("[nas]\nendpoint_id = \"{}\"\n", id(1)),
            "1: unknown table \"nas\"; hosts live under [hosts.nas]",
        ),
        (
            "[photos]\npath = \"/p\"\n".to_string(),
            "1: unknown table \"photos\"; exports live under [exports.photos]",
        ),
        (
            "rw = [\"a\"]\n".to_string(),
            "1: unknown key \"rw\"; grants for every export live under [all]",
        ),
        (
            "[immediate]\nro = [\"a\"]\n".to_string(),
            "1: [immediate]: unknown key \"ro\"; only `rw` is allowed (immediate grants are read-write)",
        ),
        (
            "[exports.p]\npath = \"rel\"\n".to_string(),
            "1: [exports.p]: path \"rel\" must be absolute",
        ),
        (
            "[exports.all]\npath = \"/p\"\n".to_string(),
            "1: \"all\" is reserved: it is the word `grant add all WHO` uses for every export",
        ),
        (
            "[client]\nrelay = \"not a url\"\n".to_string(),
            "2: relative URL without a base: \"not a url\"",
        ),
        (
            "[daemon]\nidle_timeout = \"5x\"\n".to_string(),
            "2: bad duration \"5x\": expected <number>[s|m|h|d]",
        ),
        (
            "[servers.nas]\nendpoint_id = \"x\"\n".to_string(),
            "1: unknown table \"servers\"; hosts live under [hosts.servers], exports under [exports.servers]",
        ),
        (
            "\n[clients.lap]\nclient_id = \"x\"\n".to_string(),
            "2: unknown table \"clients\"; hosts live under [hosts.clients], exports under [exports.clients]",
        ),
        ("servers = 1\n".to_string(), "1: unknown key \"servers\""),
        (
            "hosts = 1\n".to_string(),
            "1: hosts: expected a table, found integer",
        ),
        ("[hosts\n".to_string(), "1: unclosed table, expected `]`"),
    ];
    for (text, want) in bad {
        let got = legacy_v1::read(&text, p()).unwrap_err().to_string();
        assert_eq!(got, format!("irsync.toml:{want}"), "{text}");
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

/// `reader::read` picks the reader by the top-level keys: one case per
/// row of the table in the plan.
#[test]
fn reader_choice() {
    use irsync::config::reader::{read, CONFIG_VERSION};
    assert_eq!(CONFIG_VERSION, 2);
    let server = format!("[servers.nas]\nendpoint_id = \"{}\"\n", id(1));
    let host = format!("[hosts.box]\nendpoint_id = \"{}\"\n", id(2));
    let error = |text: &str| read(text, p()).unwrap_err().to_string();

    // config_version 2 with [hosts]: an error at [hosts].
    assert_eq!(
        error(&format!("config_version = 2\n\n{server}\n{host}")),
        "irsync.toml:6: [hosts] is not read since config_version 2: hosts are now [servers.NAME] (to connect to) and [clients.NAME] (to let in)"
    );
    // config_version 2 without [hosts]: the current reader.
    let r = read(&format!("config_version = 2\n\n{server}"), p()).unwrap();
    assert_eq!((r.version, r.config.config_version), (2, Some(2)));
    assert_eq!(r.config.servers[0].name, "nas");
    assert!(r.dropped.is_empty());
    // config_version 1: legacy_v1, whatever else the file has.
    let r = read(&format!("config_version = 1\n\n{host}"), p()).unwrap();
    assert_eq!((r.version, r.config.config_version), (1, Some(2)));
    assert_eq!(r.config.servers[0].name, "box");
    let e = error(&format!("config_version = 1\n\n{server}"));
    assert_eq!(
        e,
        legacy_v1::read(&format!("config_version = 1\n\n{server}"), p())
            .unwrap_err()
            .to_string()
    );
    // No config_version, [hosts] and nothing new: legacy_v1.
    let r = read(&host, p()).unwrap();
    assert_eq!(r.version, 1);
    assert_eq!(r.config, legacy_v1::read(&host, p()).unwrap().config);
    let r = read(
        &format!(
            "[hosts.phone]\nclient_id = \"{}\"\naddress = [\"p:1\"]\n",
            id(3)
        ),
        p(),
    )
    .unwrap();
    assert_eq!(
        r.dropped,
        ["[hosts.phone] had a client_id and no endpoint_id: dropped address (they only apply to a server)"]
    );
    // No config_version, [hosts] and [servers] or [clients]: an error at [hosts].
    let both = "this file has [hosts] and also [servers] or [clients]: move each [hosts.NAME] into [servers.NAME] (its endpoint_id and how to reach it) or [clients.NAME] (its client_id)";
    assert_eq!(
        error(&format!("{server}\n{host}")),
        format!("irsync.toml:4: {both}")
    );
    assert_eq!(
        error(&format!(
            "# top\n{host}\n[clients.lap]\nclient_id = \"{}\"\n",
            id(4)
        )),
        format!("irsync.toml:2: {both}")
    );
    // No config_version and no [hosts]: the current reader.
    for text in [
        String::new(),
        server.clone(),
        "[daemon]\nportmap = false\n".into(),
    ] {
        let r = read(&text, p()).unwrap();
        assert_eq!((r.version, r.config.config_version), (2, None), "{text}");
    }
    // Above 2, or not 1 or 2: an error at the key.
    assert_eq!(
        error(&format!("\nconfig_version = 3\n{server}")),
        "irsync.toml:2: config_version 3: this file was written by a newer irsync, which reads up to config_version 2; upgrade irsync"
    );
    assert_eq!(
        error(&format!("config_version = \"x\"\n{host}")),
        "irsync.toml:1: config_version: expected 1 or 2, found string"
    );
    assert_eq!(
        error("config_version = 0\n"),
        "irsync.toml:1: config_version: expected 1 or 2, found 0"
    );
}

/// Review Focus 4: a `config_version` of the wrong type or value is an
/// error at its line, never a silent fallback to `legacy_v1`.
#[test]
fn bad_config_version_values() {
    use irsync::config::reader::read;
    let host = format!("[hosts.box]\nendpoint_id = \"{}\"\n", id(2));
    for (value, want) in [
        ("\"2\"", "config_version: expected 1 or 2, found string"),
        ("2.0", "config_version: expected 1 or 2, found float"),
        ("0", "config_version: expected 1 or 2, found 0"),
        ("-1", "config_version: expected 1 or 2, found -1"),
        ("true", "config_version: expected 1 or 2, found boolean"),
        (
            "3",
            "config_version 3: this file was written by a newer irsync, which reads up to config_version 2; upgrade irsync",
        ),
    ] {
        for rest in [String::new(), host.clone()] {
            let text = format!("# the version\nconfig_version = {value}\n{rest}");
            let e = read(&text, p()).unwrap_err().to_string();
            assert_eq!(e, format!("irsync.toml:2: {want}"), "{text}");
        }
    }
}

/// A daemon started on a file of layout 1 adds an automatic export, which
/// saves the file in layout 2; its clients keep their access, and the
/// converted file grants what the old one did.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_daemon_on_a_v1_file_converts_it_and_still_lets_clients_in() {
    use common::{archive_opts, Flow, Harness};
    use irsync::authz::{Authorizer, FileAuthorizer};
    use irsync::config::paths::Paths;
    use std::sync::Arc;

    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        let opts = archive_opts();
        let client = common::client_endpoint().await;
        let other = id(77);
        let config_dir = tempfile::tempdir().unwrap();
        let config = config_dir.path().join("irsync.toml");
        let shared = tempfile::tempdir().unwrap();
        // Its own state_dir: the conversion looks for a running daemon
        // there, not in the default one of whoever runs the tests (#275).
        let old = format!(
            "state_dir = {:?}\n\n\
             # who may push anywhere\n[immediate]\nrw = [\"laptop\"]\n\n\
             [daemon]\nimmediate = true\n\n\
             # the laptop\n[hosts.laptop]\nclient_id = \"{}\"\n\n\
             # the nas\n[hosts.nas]\nendpoint_id = \"{}\"\n\n\
             [exports.shared]\npath = \"{}\"\nrw = [\"nas\"]\n",
            config_dir.path().join("state").display().to_string(),
            client.id(),
            other,
            shared.path().display()
        );
        irsync::fs::private::write(&config, &old).unwrap();

        // What the old file grants, read before the daemon touches it.
        let before_dir = tempfile::tempdir().unwrap();
        let before_path = before_dir.path().join("irsync.toml");
        irsync::fs::private::write(&before_path, &old).unwrap();
        let before = FileAuthorizer::new(Paths::resolve(Some(&before_path)))
            .snapshot()
            .unwrap();
        assert!(
            before.grants(&client.id()).all_paths == Some(Access::ReadWrite),
            "the old file grants the client immediate access"
        );

        let authz = Arc::new(FileAuthorizer::new(Paths::resolve(Some(&config))));
        let h = Harness {
            daemon: common::spawn_daemon_with(authz).await,
            client,
            export: tempfile::tempdir().unwrap(),
            local: tempfile::tempdir().unwrap(),
            client_state: tempfile::tempdir().unwrap(),
        };
        std::fs::write(h.local.path().join("a.txt"), b"hello").unwrap();
        let mut hello = Harness::hello(Flow::Push);
        hello.path = irsync::platform::testutil::name_bytes(h.export.path().as_os_str()).to_vec();

        let first = h
            .run_hello(&opts, hello.clone(), h.local_side())
            .await
            .unwrap();
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        assert_eq!(
            std::fs::read(h.export.path().join("a.txt")).unwrap(),
            b"hello"
        );

        let written = std::fs::read_to_string(&config).unwrap();
        assert!(written.starts_with("config_version = 2\n"), "{written}");
        for want in [
            "[clients.laptop]",
            "[servers.nas]",
            "[clients.nas]",
            "auto = true",
            "# the laptop",
            "# the nas",
            "# who may push anywhere",
        ] {
            assert!(written.contains(want), "no {want} in\n{written}");
        }
        // (A generated comment names `[hosts.nas]`; no table is left.)
        assert!(!written.contains("\n[hosts."), "{written}");

        let second = h.run_hello(&opts, hello, h.local_side()).await.unwrap();
        assert!(second.errors.is_empty(), "{:?}", second.errors);
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            written,
            "the second push changed the file"
        );

        // The same access for both ids, the automatic export aside.
        let after = FileAuthorizer::new(Paths::resolve(Some(&config)))
            .snapshot()
            .unwrap();
        for who in [h.client.id(), other] {
            let (b, a) = (before.grants(&who), after.grants(&who));
            assert_eq!(b.exports.get("shared"), a.exports.get("shared"));
            assert_eq!(b.all_exports, a.all_exports);
            assert_eq!(b.all_paths, a.all_paths);
        }
        assert_eq!(
            after.grants(&other).access("shared"),
            Some(Access::ReadWrite)
        );
        h.daemon.shutdown.cancel();
    })
    .await
    .expect("test timed out");
}
