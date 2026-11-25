//! The config message as `config::reader` reads it from irsync.toml.

use irsync::config::file::Config;
use irsync::config::reader::{self, read_values};
use irsync::config::settings::DaemonSettings;
use irsync::config::writer::write;
use irsync_proto::config as pb;
use std::path::Path;

fn id(n: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
}
/// The message `reader::read` reads.
fn read(text: &str, path: &Path) -> Result<pb::Config, irsync::config::ConfigError> {
    reader::read(text, path)
}
fn values(text: &str) -> pb::Config {
    read_values(text, Path::new("irsync.toml")).unwrap()
}

#[test]
fn unset_and_set_to_the_default_differ() {
    assert_eq!(values("").daemon, None);
    assert_eq!(values("[daemon]\n").daemon.unwrap().portmap, None);
    assert_eq!(
        values("[daemon]\nportmap = true\n").daemon.unwrap().portmap,
        Some(true)
    );
}

#[test]
fn values_not_spellings() {
    let c = values(&format!(
        "[defaults]\nidentity = \"~/k\"\nreconnect = \"90\"\n[servers.nas]\nendpoint_id = \"{}\"\nrelay = \"disabled\"\nreconnect = 3600\n",
        id(1)
    ));
    assert_eq!(
        c.defaults.as_ref().unwrap().identity.as_deref(),
        Some("~/k")
    ); // as written
    assert_eq!(
        c.defaults
            .as_ref()
            .unwrap()
            .reconnect
            .as_ref()
            .unwrap()
            .seconds,
        90
    );
    let h = &c.servers[0];
    assert_eq!(h.endpoint_id, id(1).as_bytes());
    assert_eq!(h.relay.as_deref(), Some("disabled"));
    assert_eq!(h.reconnect.as_ref().unwrap().seconds, 3600);
}

#[test]
fn urls_are_kept_as_written() {
    let text = format!(
        "[defaults]\nrelay = \"https://r.example\"\n\n[daemon]\nrelay = \"https://d.example\"\n\n\
         [telemetry]\notlp_endpoint = \"http://c:4318\"\n\n[servers.nas]\nendpoint_id = \"{}\"\nrelay = \"https://h.example\"\n",
        id(1)
    );
    let c = values(&text);
    assert_eq!(
        c.defaults.as_ref().unwrap().relay.as_deref(),
        Some("https://r.example")
    );
    assert_eq!(
        c.daemon.as_ref().unwrap().relay.as_deref(),
        Some("https://d.example")
    );
    assert_eq!(
        c.telemetry.as_ref().unwrap().otlp_endpoint.as_deref(),
        Some("http://c:4318")
    );
    assert_eq!(c.servers[0].relay.as_deref(), Some("https://h.example"));
    assert_eq!(write(&c), text);
}

#[test]
fn entries_keep_file_order() {
    let c = values(&format!(
        "[servers.zed]\nendpoint_id = \"{0}\"\n[exports.b]\npath = \"/b\"\n[clients.zed]\nclient_id = \"{0}\"\n[servers.abe]\nendpoint_id = \"{0}\"\n[exports.a]\npath = \"/a\"\n[clients.abe]\nclient_id = \"{0}\"\n",
        id(1)
    ));
    assert_eq!(
        c.servers
            .iter()
            .map(|h| h.name.as_str())
            .collect::<Vec<_>>(),
        ["zed", "abe"]
    );
    assert_eq!(
        c.clients
            .iter()
            .map(|h| h.name.as_str())
            .collect::<Vec<_>>(),
        ["zed", "abe"]
    );
    assert_eq!(
        c.exports
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>(),
        ["b", "a"]
    );
}

#[test]
fn an_empty_array_is_set() {
    let c = values("[exports.p]\npath = \"/p\"\nrw = []\n");
    let p = &c.exports[0];
    assert!(p.rw.is_empty() && p.rw_set && !p.ro_set);
}

#[test]
fn an_export_path_is_kept_as_written() {
    let c = values("[exports.p]\npath = \"~/p\"\n");
    assert_eq!(c.exports[0].path, "~/p");
}

#[test]
fn addresses_are_kept_as_written_or_as_printed() {
    let c = values("[daemon]\ndebug_addr = \"9000\"\nlisten = [\"[0:0::1]:1\", \"nas.lan:2\"]\n");
    let d = c.daemon.unwrap();
    assert_eq!(d.debug_addr.as_deref(), Some("9000"));
    assert_eq!(
        d.listen
            .iter()
            .map(|e| e.value.as_str())
            .collect::<Vec<_>>(),
        ["[::1]:1", "nas.lan:2"]
    );
    assert!(d.listen_set);
}

#[test]
fn ssh_aliases_set_says_whether_the_key_is_there() {
    let text = |aliases: &str| format!("[servers.nas]\nendpoint_id = \"{}\"\n{aliases}", id(1));
    let h = values(&text("")).servers.remove(0);
    assert!(h.ssh_aliases.is_empty() && !h.ssh_aliases_set);
    let h = values(&text("ssh_aliases = []\n")).servers.remove(0);
    assert!(h.ssh_aliases.is_empty() && h.ssh_aliases_set);
    let h = values(&text("ssh_aliases = [\"10.0.0.2\"]\n"))
        .servers
        .remove(0);
    assert_eq!(h.ssh_aliases[0].value, "10.0.0.2");
    assert!(h.ssh_aliases_set);
}

#[test]
fn a_client_id_is_32_bytes() {
    let c = values(&format!("[clients.lap]\nclient_id = \"{}\"\n", id(2)));
    assert_eq!(c.clients[0].client_id, id(2).as_bytes());
    assert!(c.servers.is_empty());
}

#[test]
fn from_message_applies_defaults_and_expands_tilde() {
    let c = Config::from_message(
        &values("[defaults]\nidentity = \"~/k\"\n[daemon]\n"),
        Path::new("f"),
    )
    .unwrap();
    assert_eq!(c.settings.identity, dirs::home_dir().unwrap().join("k"));
    assert_eq!(c.settings.ssh_identity, None); // the file names an identity
    assert_eq!(c.settings.daemon, DaemonSettings::default());
    assert_eq!(c.servers.path, Path::new("f"));
    assert_eq!(c.clients.path, Path::new("f"));
}

#[test]
fn from_message_refuses_what_a_command_can_break() {
    let mut m = values("[exports.p]\npath = \"/p\"\n");
    m.exports[0].path = "rel".into();
    let e = Config::from_message(&m, Path::new("f")).unwrap_err();
    assert_eq!(
        e.to_string(),
        "f: [exports.p]: path \"rel\" must be absolute"
    );
    let mut m = values("");
    m.servers.push(pb::ServerEntry {
        name: "nas".into(),
        endpoint_id: vec![1, 2, 3],
        ..Default::default()
    });
    assert!(Config::from_message(&m, Path::new("f")).is_err());
}

#[test]
fn from_message_checks_each_edit_again() {
    let server = |name: &str, n: u8| pb::ServerEntry {
        name: name.into(),
        endpoint_id: id(n).as_bytes().to_vec(),
        ..Default::default()
    };
    let export = |name: &str| pb::Export {
        name: name.into(),
        path: "/p".into(),
        ..Default::default()
    };
    let addr = |s: &str| pb::Entry {
        value: s.into(),
        comment: None,
    };
    let refusal = |edit: &dyn Fn(&mut pb::Config)| {
        let mut m = values("");
        edit(&mut m);
        Config::from_message(&m, Path::new("f"))
            .unwrap_err()
            .to_string()
    };
    assert_eq!(
        refusal(&|m| m.servers.push(server("a:b", 1))),
        "f: invalid name \"a:b\": must be non-empty, contain no ':' or '/', and be neither all, * nor an EndpointId"
    );
    assert_eq!(
        refusal(&|m| m.servers.push(pb::ServerEntry {
            name: "nas".into(),
            ..Default::default()
        })),
        "f: server \"nas\": needs an endpoint_id"
    );
    assert_eq!(
        refusal(&|m| m.servers.extend([server("nas", 1), server("nas", 2)])),
        "f: duplicate server name \"nas\""
    );
    assert_eq!(
        refusal(&|m| m.exports.push(export("all"))),
        "f: \"all\" is reserved: it is the word `grant remove all WHO` and `export update all` use for every export"
    );
    assert_eq!(
        refusal(&|m| m.exports.push(export("a/b"))),
        "f: \"a/b\" is not a valid export name: it must not be empty or contain '/'"
    );
    assert_eq!(
        refusal(&|m| m.exports.extend([export("p"), export("p")])),
        "f: duplicate export \"p\""
    );
    assert_eq!(
        refusal(&|m| {
            m.daemon = Some(pb::Daemon {
                listen: vec![addr("1.2.3.4:1"), addr("1.2.3.5:1")],
                ..Default::default()
            })
        }),
        "f: [daemon]: at most one IPv4 and one IPv6 listen address"
    );
    assert_eq!(
        refusal(&|m| {
            m.daemon = Some(pb::Daemon {
                debug_addr: Some("x".into()),
                ..Default::default()
            })
        }),
        "f: [daemon]: bad debug address \"x\": expected PORT or IP:PORT"
    );
    assert_eq!(
        refusal(&|m| {
            m.defaults = Some(pb::Defaults {
                relay: Some("not a url".into()),
                ..Default::default()
            })
        }),
        "f: [defaults]: \"not a url\": expected default, disabled or a URL (relative URL without a base)"
    );
}

#[test]
fn errors_are_those_of_config_parse() {
    for (text, message) in [
        (
            "[servers.nas]\nendpoint_id = \"nope\"\n",
            "f:2: server \"nas\": invalid endpoint id: nope",
        ),
        (
            "[exports.p]\npath = \"rel\"\n",
            "f:1: [exports.p]: path \"rel\" must be absolute",
        ),
        (
            "[nas]\nendpoint_id = \"x\"\n",
            "f:1: unknown key \"nas\"",
        ),
        (
            "rw = [\"x\"]\n",
            "f:1: unknown key \"rw\"",
        ),
        (
            "[daemon]\nlisten = [\"1.2.3.4:1\", \"1.2.3.5:1\"]\n",
            "f:2: at most one IPv4 and one IPv6 listen address",
        ),
        (
            "[servers.all]\nendpoint_id = \"x\"\n",
            "f:1: invalid name \"all\": must be non-empty, contain no ':' or '/', and be neither all, * nor an EndpointId",
        ),
        (
            "[defaults]\nbogus = 1\n",
            "f:2: unknown key \"defaults.bogus\"",
        ),
        (
            "[daemon]\nidentity = 1\n",
            "f:2: invalid type: integer `1`, expected path string",
        ),
        (
            "[daemon]\nidle_timeout = \"soon\"\n",
            "f:2: bad duration \"soon\": expected <number>[s|m|h|d]",
        ),
        (
            "[daemon]\ndebug_addr = \"x\"\n",
            "f:2: bad debug address \"x\": expected PORT or IP:PORT",
        ),
        (
            "[daemon]\ndiscovery = true\n",
            "f:2: unknown key \"daemon.discovery\"",
        ),
        (
            "[defaults]\nrelay = \"not a url\"\n",
            "f:2: \"not a url\": expected default, disabled or a URL (relative URL without a base)",
        ),
        (
            "[telemetry]\notlp_protocol = \"udp\"\n",
            "f:2: \"udp\": expected grpc or http",
        ),
        (
            "[servers.nas]\naddress = [\"nope\"]\n",
            "f:2: invalid address \"nope\": expected ip:port, [ipv6]:port or host:port",
        ),
        (
            "[servers.nas]\nssh = \"nas\"\n",
            "f:1: server \"nas\": needs an endpoint_id",
        ),
        (
            "[servers.b]\nendpoint_id = \"x\"\n[servers.a]\nendpoint_id = \"y\"\n",
            "f:4: server \"a\": invalid endpoint id: y",
        ),
        (
            "[exports.p]\nread_only = true\n",
            "f:1: [exports.p]: missing field `path`",
        ),
        (
            "[exports.p]\npath = \"/p\"\nbogus = 1\n",
            "f:3: unknown key \"exports.p.bogus\"",
        ),
        (
            "[exports.b]\npath = \"b\"\n[exports.a]\npath = \"a\"\n",
            "f:3: [exports.a]: path \"a\" must be absolute",
        ),
        (
            "[exports]\np = 1\n",
            "f:2: exports.p: expected a table, found integer",
        ),
        (
            "[all]\nrw = \"x\"\n",
            "f:1: unknown key \"all\"",
        ),
        (
            "state_dir = 1\n",
            "f:1: invalid type: integer `1`, expected a string",
        ),
        (
            "[daemon]\nidle_timeout = true\n",
            "f:2: data did not match any variant of untagged enum Raw",
        ),
        (
            "[daemon]\ndebug_addr = 1\n",
            "f:2: invalid type: integer `1`, expected a string",
        ),
        (
            "[defaults]\nrelay = 1\n",
            "f:2: invalid type: integer `1`, expected a string",
        ),
        (
            "[defaults]\nrelay = \"x\"\n",
            "f:2: \"x\": expected default, disabled or a URL (relative URL without a base)",
        ),
        (
            "[daemon]\ncongestion = \"reno\"\n",
            "f:2: \"reno\": expected bbr or cubic",
        ),
        (
            "[servers.h]\npq_key_exchange = \"only\"\n",
            "f:2: \"only\": expected prefer or require",
        ),
        (
            "[telemetry]\notlp_protocol = 1\n",
            "f:2: invalid type: integer `1`, expected a string",
        ),
        (
            "[defaults]\nreconnect = true\n",
            "f:2: data did not match any variant of untagged enum Raw",
        ),
        (
            "[defaults]\nreconnect = \"soon\"\n",
            "f:2: bad duration \"soon\": expected <number>[s|m|h|d]",
        ),
        (
            "[daemon]\ndormant_session_timeout = -1\n",
            "f:2: data did not match any variant of untagged enum Raw",
        ),
        (
            "[daemon]\nlisten = \"1.2.3.4:1\"\n",
            "f:2: invalid type: string \"1.2.3.4:1\", expected a sequence",
        ),
        (
            "[daemon]\nlisten = [\"[::1]:1\", \"[::2]:1\"]\n",
            "f:2: at most one IPv4 and one IPv6 listen address",
        ),
        (
            "[servers.nas]\nreconnect = \"x\"\n",
            "f:2: bad duration \"x\": expected <number>[s|m|h|d]",
        ),
        (
            "[daemon]\nbogus = 1\n",
            "f:2: unknown key \"daemon.bogus\"",
        ),
        (
            "[telemetry]\nbogus = 1\n",
            "f:2: unknown key \"telemetry.bogus\"",
        ),
        (
            "[servers.nas]\nbogus = 1\n",
            "f:2: unknown key \"servers.nas.bogus\"",
        ),
        (
            "[daemon]\ndht = 1\n",
            "f:2: invalid type: integer `1`, expected a boolean",
        ),
        (
            "[servers]\nnas = 1\n",
            "f:2: servers.nas: expected a table, found integer",
        ),
        (
            "[daemon]\nrelay = 1\n",
            "f:2: invalid type: integer `1`, expected a string",
        ),
        (
            "[servers.nas]\naddress = 1\n",
            "f:2: invalid type: integer `1`, expected a sequence",
        ),
        (
            "[exports.p]\npath = 1\n",
            "f:1: [exports.p]: invalid type: integer `1`, expected path string",
        ),
        (
            "[servers.nas]\nssh_aliases = [1]\n",
            "f:2: invalid type: integer `1`, expected a string",
        ),
        (
            "[daemon]\nmax_dormant_sessions_per_key = -1\n",
            "f:2: invalid value: integer `-1`, expected u32",
        ),
        (
            "unsafe_config_allowed = 1\n",
            "f:1: invalid type: integer `1`, expected a boolean",
        ),
        (
            "[defaults]\nrelay = {}\n",
            "f:2: invalid type: map, expected a string",
        ),
        (
            "[defaults]\nrelay = { default = 1 }\n",
            "f:2: invalid type: map, expected a string",
        ),
        (
            "[daemon]\ncongestion = { x = 1 }\n",
            "f:2: invalid type: map, expected a string",
        ),
        (
            "[daemon]\nidle_timeout = {}\n",
            "f:2: data did not match any variant of untagged enum Raw",
        ),
        (
            "[daemon]\nlisten = [1]\n",
            "f:2: invalid type: integer `1`, expected a string",
        ),
        (
            "[daemon]\ndebug_addr = {}\n",
            "f:2: invalid type: map, expected a string",
        ),
        (
            "[servers.nas]\nendpoint_id = 1\n",
            "f:2: invalid type: integer `1`, expected a string",
        ),
        (
            "not toml",
            "f:1: key with no value, expected `=`",
        ),
    ] {
        let e = read(text, Path::new("f")).unwrap_err();
        assert_eq!(e.to_string(), message, "{text}");
    }
}

const COMMENTED: &str = "\
# irsync on the nas
# second header line

state_dir = \"~/k\" # my key
# after the top keys

# about the daemon
[daemon] # trailing on header
# above portmap
portmap = false # why off
# daemon tail

# the photos
[exports.photos]
path = \"/photos\"
rw = [
    # the laptop
    \"laptop\", # mine
    \"phone\",
    # after the last
]
# the end
";

#[test]
fn every_comment_has_a_home() {
    let c = read(COMMENTED, Path::new("f")).unwrap();
    assert_eq!(c.header, ["# irsync on the nas", "# second header line"]);
    assert_eq!(c.key_comments["state_dir"].trailing, "# my key");
    assert_eq!(c.tail, ["# after the top keys"]);
    let d = c.daemon.unwrap();
    assert_eq!(d.comment.as_ref().unwrap().above, ["# about the daemon"]);
    assert_eq!(d.comment.unwrap().trailing, "# trailing on header");
    assert_eq!(d.key_comments["portmap"].above, ["# above portmap"]);
    assert_eq!(d.key_comments["portmap"].trailing, "# why off");
    assert_eq!(d.tail, ["# daemon tail"]);
    let e = &c.exports[0];
    assert_eq!(e.comment.as_ref().unwrap().above, ["# the photos"]);
    assert_eq!(e.rw[0].comment.as_ref().unwrap().above, ["# the laptop"]);
    assert_eq!(e.rw[0].comment.as_ref().unwrap().trailing, "# mine");
    assert_eq!(
        e.rw[1].comment.as_ref().unwrap().above,
        ["# after the last"]
    );
    // No blank line before it: the tail of the table it ends.
    assert_eq!(e.tail, ["# the end"]);
    assert!(c.footer.is_empty());
}

#[test]
fn the_footer_starts_at_the_first_blank_line_after_the_last_key() {
    let c = commented("[daemon]\nportmap = false\n# daemon tail\n\n# the footer\n\n# more\n");
    assert_eq!(c.daemon.unwrap().tail, ["# daemon tail"]);
    assert_eq!(c.footer, ["# the footer", "", "# more"]);
    // Top-level keys and no table: the top level's tail.
    let c = commented("state_dir = \"~/k\"\n# top tail\n\n# the footer\n");
    assert_eq!(c.tail, ["# top tail"]);
    assert_eq!(c.footer, ["# the footer"]);
}

#[test]
fn hash_in_a_string_is_not_a_comment() {
    let c = read(
        &format!(
            "[servers.nas]\nendpoint_id = \"{}\"\nssh = \"me@host#1\"\n",
            id(1)
        ),
        Path::new("f"),
    )
    .unwrap();
    assert_eq!(c.servers[0].ssh.as_deref(), Some("me@host#1"));
    assert!(c.servers[0].key_comments.is_empty());
}

#[test]
fn a_file_without_comments_has_none() {
    let text = "[daemon]\nportmap = false\n";
    assert_eq!(
        read(text, Path::new("f")).unwrap(),
        read_values(text, Path::new("f")).unwrap()
    );
}

fn commented(text: &str) -> pb::Config {
    read(text, Path::new("irsync.toml")).unwrap()
}

#[test]
fn a_file_that_starts_with_a_table_has_a_header() {
    let c = commented("# top\n\n# one\n\n# two\n\n\n# the daemon\n[daemon]\n");
    assert_eq!(c.header, ["# top", "", "# one", "", "# two"]);
    assert_eq!(c.daemon.unwrap().comment.unwrap().above, ["# the daemon"]);
    assert!(c.tail.is_empty() && c.footer.is_empty());
}

#[test]
fn a_tail_keeps_its_inner_blank_lines() {
    let c = commented(
        "[daemon]\nportmap = true\n\n# a\n\n# b\n\n[defaults]\n# first\n\nparallel = 2\n",
    );
    let d = c.daemon.unwrap();
    assert_eq!(d.tail, ["# a", "", "# b"]);
    let cl = c.defaults.unwrap();
    assert_eq!(cl.comment, None);
    assert_eq!(cl.key_comments["parallel"].above, ["# first"]);
}

#[test]
fn a_bare_group_header_gives_its_comments_to_the_first_entry() {
    let c = commented("# all exports\n[exports] # mine\n# the photos\n[exports.p]\npath = \"/p\"\n[exports.q]\npath = \"/q\"\n");
    assert_eq!(
        c.exports[0].comment.as_ref().unwrap().above,
        ["# all exports", "# mine", "# the photos"]
    );
    assert_eq!(c.exports[1].comment, None);
}

#[test]
fn a_comment_without_a_place_goes_to_the_footer() {
    let c = commented("[servers] # none yet\n[daemon]\n# end");
    assert_eq!(c.footer, ["# none yet"]);
    assert_eq!(c.daemon.unwrap().tail, ["# end"]);
}

#[test]
fn comments_in_arrays() {
    let c = commented(
        "[exports.p]\npath = \"/p\"\nrw = [ # first\n  \"a\" # no comma\n  # after\n]\nro = [\n  # nothing yet\n] # ro\n",
    );
    let all = &c.exports[0];
    let a = all.rw[0].comment.as_ref().unwrap();
    assert_eq!(a.above, ["# first", "# after"]);
    assert_eq!(a.trailing, "# no comma");
    assert!(all.ro.is_empty());
    assert_eq!(all.key_comments["ro"].above, ["# nothing yet"]);
    assert_eq!(all.key_comments["ro"].trailing, "# ro");
}

#[test]
fn comments_on_dotted_keys_and_inline_tables() {
    let c = commented(&format!(
        "# on dht\ndaemon.dht = false # off\n[servers]\n# the nas\nnas = {{ endpoint_id = \"{}\" }} # inline\n",
        id(1)
    ));
    let disc = c.daemon.unwrap();
    assert_eq!(disc.key_comments["dht"].above, ["# on dht"]);
    assert_eq!(disc.key_comments["dht"].trailing, "# off");
    let h = c.servers[0].comment.as_ref().unwrap();
    assert_eq!(h.above, ["# the nas"]);
    assert_eq!(h.trailing, "# inline");
}

fn corpus() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config_corpus");
    let mut files: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read_to_string(&p).unwrap())
        })
        .collect();
    files.sort();
    assert!(files.len() >= 7, "the corpus is in {}", dir.display());
    files
}

fn fixture(name: &str) -> String {
    corpus()
        .into_iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("no fixture {name}"))
        .1
}

fn rewritten(text: &str) -> String {
    write(&read(text, Path::new("f")).unwrap())
}

/// Every comment of `text` is in `out`.
fn assert_no_comment_lost(name: &str, text: &str, out: &str) {
    for line in text
        .lines()
        .filter_map(|l| l.find('#').map(|i| l[i..].trim_end()))
    {
        assert!(out.contains(line), "{name}: lost {line:?} in\n{out}");
    }
}

#[test]
fn the_corpus_is_commented_as_the_tests_say() {
    assert_eq!(fixture("commented.toml"), COMMENTED);
}

#[test]
fn the_writers_form() {
    // COMMENTED, as the writer lays it out.
    let out = write(&read(COMMENTED, Path::new("f")).unwrap());
    assert_eq!(
        out,
        "\
# irsync on the nas
# second header line

state_dir = \"~/k\" # my key
# after the top keys

# about the daemon
[daemon] # trailing on header
# above portmap
portmap = false # why off
# daemon tail

# the photos
[exports.photos]
path = \"/photos\"
rw = [
    # the laptop
    \"laptop\", # mine
    # after the last
    \"phone\",
]
# the end
"
    );
}

#[test]
fn read_write_read_is_the_same_message() {
    for (name, text) in corpus() {
        let m = read(&text, Path::new("f")).unwrap();
        assert_eq!(read(&write(&m), Path::new("f")).unwrap(), m, "{name}");
    }
}

#[test]
fn the_form_is_stable() {
    for (name, text) in corpus() {
        let once = rewritten(&text);
        assert_eq!(rewritten(&once), once, "{name}");
    }
}

#[test]
fn no_comment_is_lost() {
    for (name, text) in corpus() {
        assert_no_comment_lost(&name, &text, &rewritten(&text));
    }
}

#[test]
fn the_typed_config_is_the_same_after_a_rewrite() {
    for (name, text) in corpus() {
        let out = rewritten(&text);
        assert_eq!(
            Config::parse(&out, Path::new("f")).unwrap(),
            Config::parse(&text, Path::new("f")).unwrap(),
            "{name}"
        );
    }
}

/// `parallel = 0` could send nothing: refused at its line, as
/// `--parallel 0` is, and in an edited message.
#[test]
fn parallel_must_be_at_least_one() {
    let e = read("[defaults]\nreconnect = 1\nparallel = 0\n", Path::new("f")).unwrap_err();
    assert_eq!(e.to_string(), "f:3: parallel must be at least 1");
    let text = format!("[servers.nas]\nendpoint_id = \"{}\"\nparallel = 0\n", id(1));
    let e = read(&text, Path::new("f")).unwrap_err();
    assert_eq!(e.to_string(), "f:3: parallel must be at least 1");
    assert!(read("[defaults]\nparallel = 1\n", Path::new("f")).is_ok());
    let mut m = values("[defaults]\nparallel = 1\n");
    m.defaults.as_mut().unwrap().parallel = Some(0);
    let e = Config::from_message(&m, Path::new("f")).unwrap_err();
    assert_eq!(e.to_string(), "f: [defaults]: parallel must be at least 1");
    let mut m = values(&text.replace("parallel = 0", "parallel = 2"));
    m.servers[0].parallel = Some(0);
    let e = Config::from_message(&m, Path::new("f")).unwrap_err();
    assert_eq!(
        e.to_string(),
        "f: server \"nas\": parallel must be at least 1"
    );
}

/// A server's `address` dials it directly: `relay` and the lookups
/// beside it are refused, at the line of `address`.
#[test]
fn address_refuses_relay_and_lookups_beside_it() {
    let text = format!(
        "[servers.nas]\nendpoint_id = \"{}\"\ndht = false\naddress = [\"10.0.0.2:4433\"]\nrelay = \"disabled\"\n",
        id(1)
    );
    let e = read(&text, Path::new("f")).unwrap_err().to_string();
    assert_eq!(
        e,
        "f:4: server \"nas\": relay and dht cannot be set beside address, which dials the server directly with the relay and the lookups off"
    );
    // An empty list dials nothing directly.
    let text = format!(
        "[servers.nas]\nendpoint_id = \"{}\"\naddress = []\nmdns = false\n",
        id(1)
    );
    assert!(read(&text, Path::new("f")).is_ok());
    // The typed view checks an edited message again.
    let mut m = values(&format!(
        "[servers.nas]\nendpoint_id = \"{}\"\naddress = [\"10.0.0.2:4433\"]\n",
        id(1)
    ));
    m.servers[0].mdns = Some(true);
    let e = Config::from_message(&m, Path::new("f"))
        .unwrap_err()
        .to_string();
    assert!(e.contains("mdns cannot be set beside address"), "{e}");
}

/// full.toml sets every key of the key table, and only those: the
/// reader reads each of them, the writer writes each, and `config list`
/// (which goes through `config get`'s fields) shows each.
#[test]
fn full_toml_sets_every_key_of_the_table() {
    use irsync::config::keys::{Kind, KEYS};
    use std::collections::BTreeSet;
    let m = read(&fixture("full.toml"), Path::new("f")).unwrap();
    let listed: BTreeSet<String> = irsync::manage::config::list(&m)
        .iter()
        .map(|line| {
            let key = line.split(" = ").next().unwrap();
            let mut parts: Vec<&str> = key.split('.').collect();
            if parts.len() == 3 {
                parts[1] = "*";
            }
            parts.join(".")
        })
        .collect();
    let table: BTreeSet<String> = KEYS
        .iter()
        .filter(|k| k.kind != Kind::Table)
        .map(|k| k.path.to_string())
        .collect();
    assert_eq!(listed, table);
}

/// A key the table does not know is refused at its line, in every table.
#[test]
fn a_key_the_table_does_not_know_is_refused() {
    for (text, message) in [
        ("bogus = 1\n", "f:1: unknown key \"bogus\""),
        (
            "[defaults]\nparallel = 2\nbogus = 1\n",
            "f:3: unknown key \"defaults.bogus\"",
        ),
        ("daemon.bogus.x = 1\n", "f:1: unknown key \"daemon.bogus\""),
        (
            "[exports.p]\npath = \"/p\"\nrw = []\nx = 1\n",
            "f:4: unknown key \"exports.p.x\"",
        ),
        (
            "[[servers]]\nx = 1\n",
            "f:1: servers: expected a table, found array",
        ),
    ] {
        let e = read(text, Path::new("f")).unwrap_err().to_string();
        assert_eq!(e, message, "{text}");
    }
}

#[test]
fn every_key_is_written() {
    // full.toml sets every key; the rewrite holds each of them again.
    let text = fixture("full.toml");
    assert_eq!(rewritten(&text), text);
}

#[test]
fn odd_comment_places_survive() {
    let text = fixture("odd_comments.toml");
    assert_no_comment_lost("odd_comments.toml", &text, &rewritten(&text));
}

#[test]
fn quoted_names_round_trip() {
    let text = fixture("quoted_names.toml");
    let m = read(&text, Path::new("f")).unwrap();
    let out = write(&m);
    for header in [
        "[servers.\"my nas\"]",
        "[clients.\"käse\"]",
        "[exports.\"www.example.com\"]",
    ] {
        assert!(out.contains(header), "{header} in\n{out}");
    }
    assert_eq!(read(&out, Path::new("f")).unwrap(), m);
}

#[test]
fn file_edges_round_trip() {
    assert_eq!(write(&pb::Config::default()), "");
    for text in [
        "",
        "# only\n",
        "[daemon]\nportmap = false",
        "[daemon]\r\nportmap = false\r\n",
    ] {
        let m = read(text, Path::new("f")).unwrap();
        let out = write(&m);
        assert!(
            out.is_empty() || out.ends_with('\n') && !out.ends_with("\n\n"),
            "{text:?}"
        );
        assert_eq!(read(&out, Path::new("f")).unwrap(), m, "{text:?}");
    }
    // A file of only comments is all footer.
    let only = read("# only\n", Path::new("f")).unwrap();
    assert_eq!(only.footer, ["# only"]);
    assert!(only.header.is_empty());
    assert_eq!(
        read("[daemon]\r\nportmap = false\r\n", Path::new("f")).unwrap(),
        read("[daemon]\nportmap = false\n", Path::new("f")).unwrap()
    );
}

#[test]
fn inline_and_dotted_forms_become_tables() {
    let inline_twin = format!(
        "\
# the nas
[servers.nas] # inline host
endpoint_id = \"{a}\"
address = [\"10.0.0.2:4433\"]

[clients.lap]
client_id = \"{b}\"

# the photos
[exports.photos] # inline export
path = \"/photos\"
rw = [\"nas\", \"lap\"]
",
        a = "069ffbb125300737c2767fed918cd5f4bc12b3102fdd9009a2a1320e2ebd3191",
        b = "6e7a1cdd29b0b78fd13af4c5598feff4ef2a97166e3ca6f2e4fbfccd80505bf1",
    );
    let dotted_twin = format!(
        "\
# top
state_dir = \"~/k\"

[daemon]
# on portmap
portmap = false # off
dht = false # dht off

[servers.nas]
endpoint_id = \"{a}\" # the id
",
        a = "069ffbb125300737c2767fed918cd5f4bc12b3102fdd9009a2a1320e2ebd3191",
    );
    for (name, twin, header) in [
        ("inline.toml", inline_twin, "[servers.nas]"),
        ("dotted.toml", dotted_twin, "[daemon]"),
    ] {
        let m = read(&fixture(name), Path::new("f")).unwrap();
        assert_eq!(m, read(&twin, Path::new("f")).unwrap(), "{name}");
        let out = write(&m);
        assert!(out.contains(header), "{name}: {out}");
        assert_eq!(out, twin, "{name}");
    }
}

#[test]
fn a_top_level_group_key_gives_its_comments_to_the_first_entry() {
    let c = commented("# the exports\nexports = { p = { path = \"/p\" } } # inline\n");
    assert!(c.key_comments.is_empty());
    assert_eq!(
        c.exports[0].comment.as_ref().unwrap().above,
        ["# the exports", "# inline"]
    );
}

#[test]
fn a_bare_group_tail_gives_no_blank_lines_to_the_first_entry() {
    let c = commented("[exports]\n# a\n\n# b\n\n[exports.p]\npath = \"/p\"\n");
    assert_eq!(c.exports[0].comment.as_ref().unwrap().above, ["# a", "# b"]);
}

#[test]
fn servers_and_clients_build_the_typed_view() {
    let msg = pb::Config {
        servers: vec![pb::ServerEntry {
            name: "nas".into(),
            endpoint_id: id(1).as_bytes().to_vec(),
            ..Default::default()
        }],
        clients: vec![pb::ClientEntry {
            name: "nas".into(),
            client_id: id(2).as_bytes().to_vec(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let c = Config::from_message(&msg, Path::new("irsync.toml")).unwrap();
    assert_eq!(c.servers.get("nas").unwrap().endpoint_id, id(1));
    assert_eq!(c.clients.get("nas").unwrap().client_id, id(2));
    assert_eq!(c.clients.alias_of(&id(2)), Some("nas"));
    assert_eq!(c.clients.alias_of(&id(1)), None);
    assert_eq!(c.clients.resolve_peer("nas").unwrap(), id(2));
    assert_eq!(c.clients.resolve_peer(&id(3).to_string()).unwrap(), id(3));
    assert_eq!(
        c.clients.resolve_peer("zed").unwrap_err().to_string(),
        "unknown client \"zed\" (and not an EndpointId); clients are defined in irsync.toml"
    );
}

#[test]
fn a_server_needs_an_endpoint_id_and_a_client_a_client_id() {
    let p = Path::new("irsync.toml");
    let s = pb::Config {
        servers: vec![pb::ServerEntry {
            name: "nas".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    assert_eq!(
        Config::from_message(&s, p).unwrap_err().to_string(),
        "irsync.toml: server \"nas\": needs an endpoint_id"
    );
    let c = pb::Config {
        clients: vec![pb::ClientEntry {
            name: "lap".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    assert_eq!(
        Config::from_message(&c, p).unwrap_err().to_string(),
        "irsync.toml: client \"lap\": needs a client_id"
    );
}

#[test]
fn names_are_checked_and_unique_per_table() {
    let p = Path::new("irsync.toml");
    let server = |n: &str| pb::ServerEntry {
        name: n.into(),
        endpoint_id: id(1).as_bytes().to_vec(),
        ..Default::default()
    };
    let client = |n: &str| pb::ClientEntry {
        name: n.into(),
        client_id: id(2).as_bytes().to_vec(),
        ..Default::default()
    };
    let bad = [
        String::new(),
        "all".into(),
        "*".into(),
        "a:b".into(),
        "a/b".into(),
        id(5).to_string(),
    ];
    for name in &bad {
        let want = format!(
            "irsync.toml: invalid name {name:?}: must be non-empty, contain no ':' or '/', and be neither all, * nor an EndpointId"
        );
        let s = pb::Config {
            servers: vec![server(name)],
            ..Default::default()
        };
        assert_eq!(Config::from_message(&s, p).unwrap_err().to_string(), want);
        let c = pb::Config {
            clients: vec![client(name)],
            ..Default::default()
        };
        assert_eq!(Config::from_message(&c, p).unwrap_err().to_string(), want);
    }
    let s = pb::Config {
        servers: vec![server("x"), server("x")],
        ..Default::default()
    };
    assert_eq!(
        Config::from_message(&s, p).unwrap_err().to_string(),
        "irsync.toml: duplicate server name \"x\""
    );
    let c = pb::Config {
        clients: vec![client("x"), client("x")],
        ..Default::default()
    };
    assert_eq!(
        Config::from_message(&c, p).unwrap_err().to_string(),
        "irsync.toml: duplicate client name \"x\""
    );
}

#[test]
fn server_keys_are_read_like_host_keys() {
    let msg = pb::Config {
        servers: vec![pb::ServerEntry {
            name: "nas".into(),
            endpoint_id: id(1).as_bytes().to_vec(),
            ssh: Some("me@nas".into()),
            ssh_aliases: vec![pb::Entry {
                value: "10.0.0.2".into(),
                ..Default::default()
            }],
            parallel: Some(4),
            ..Default::default()
        }],
        ..Default::default()
    };
    let c = Config::from_message(&msg, Path::new("irsync.toml")).unwrap();
    assert_eq!(c.servers.by_ssh("me@nas"), Ok(Some("nas")));
    assert_eq!(c.servers.by_ssh("10.0.0.2"), Ok(Some("nas")));
    assert_eq!(c.servers.by_ssh("other"), Ok(None));
    assert_eq!(c.servers.get("nas").unwrap().parallel, Some(4));
}

/// A message with one server and one client, each with comments, as
/// `irsync.toml` of version 2 holds them.
fn split_message() -> pb::Config {
    let comment = |above: &[&str], trailing: &str| pb::Comment {
        above: above.iter().map(|s| s.to_string()).collect(),
        trailing: trailing.into(),
    };
    pb::Config {
        config_version: Some(2),
        servers: vec![pb::ServerEntry {
            comment: Some(comment(&["# the nas"], "")),
            name: "nas".into(),
            endpoint_id: id(1).as_bytes().to_vec(),
            address: vec![pb::Entry {
                value: "nas.lan:4433".into(),
                ..Default::default()
            }],
            address_set: true,
            ssh: Some("me@nas".into()),
            ..Default::default()
        }],
        clients: vec![pb::ClientEntry {
            name: "laptop".into(),
            client_id: id(2).as_bytes().to_vec(),
            key_comments: [("client_id".to_string(), comment(&[], "# my laptop"))].into(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[test]
fn writer_writes_servers_then_clients() {
    assert_eq!(
        write(&split_message()),
        format!(
            "config_version = 2\n\n# the nas\n[servers.nas]\nendpoint_id = \"{}\"\naddress = [\"nas.lan:4433\"]\nssh = \"me@nas\"\n\n[clients.laptop]\nclient_id = \"{}\" # my laptop\n",
            id(1),
            id(2)
        )
    );
}

#[test]
fn the_new_tables_sit_between_telemetry_and_exports() {
    let mut msg = split_message();
    msg.telemetry = Some(pb::Telemetry {
        otlp_endpoint: Some("http://c:4318".into()),
        ..Default::default()
    });
    msg.exports = vec![pb::Export {
        name: "data".into(),
        path: "/data".into(),
        ..Default::default()
    }];
    let text = write(&msg);
    let at = |s: &str| text.find(s).unwrap_or_else(|| panic!("{s} in {text}"));
    assert!(at("[telemetry]") < at("[servers.nas]"));
    assert!(at("[servers.nas]") < at("[clients.laptop]"));
    assert!(at("[clients.laptop]") < at("[exports.data]"));
}

/// The current reader reads `[servers]` and `[clients]` with their
/// comments, and `config_version`.
#[test]
fn servers_and_clients_are_read_with_their_comments() {
    let text = format!(
        "config_version = 2 # the layout\n\n# the nas\n[servers.nas] # mine\nendpoint_id = \"{}\"\n# how to reach it\naddress = [\"nas.lan:4433\"]\nssh = \"me@nas\"\n# nas tail\n\n[clients.nas]\nclient_id = \"{}\" # the nas as a client\n\n[clients.lap]\nclient_id = \"{}\"\n",
        id(1),
        id(2),
        id(3)
    );
    let c = commented(&text);
    assert_eq!(c.config_version, Some(2));
    assert_eq!(c.key_comments["config_version"].trailing, "# the layout");
    let s = &c.servers[0];
    assert_eq!(s.name, "nas");
    assert_eq!(s.endpoint_id, id(1).as_bytes().to_vec());
    assert_eq!(s.comment.as_ref().unwrap().above, ["# the nas"]);
    assert_eq!(s.comment.as_ref().unwrap().trailing, "# mine");
    assert_eq!(s.key_comments["address"].above, ["# how to reach it"]);
    assert_eq!(s.tail, ["# nas tail"]);
    assert_eq!(s.ssh.as_deref(), Some("me@nas"));
    let names: Vec<_> = c.clients.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["nas", "lap"]);
    assert_eq!(c.clients[0].client_id, id(2).as_bytes().to_vec());
    assert_eq!(
        c.clients[0].key_comments["client_id"].trailing,
        "# the nas as a client"
    );
    assert_eq!(write(&c), text);
    let typed = Config::parse(&text, Path::new("f")).unwrap();
    assert_eq!(typed.servers.get("nas").unwrap().endpoint_id, id(1));
    assert_eq!(typed.clients.get("nas").unwrap().client_id, id(2));
}

#[test]
fn a_file_without_config_version_reads_as_unset() {
    assert_eq!(values("").config_version, None);
    assert_eq!(values("config_version = 2\n").config_version, Some(2));
}

/// A server without `endpoint_id` and a client without `client_id` are
/// errors at their line, as are bad ids and unknown keys.
#[test]
fn the_current_reader_checks_servers_and_clients_at_their_line() {
    let error = |text: &str| read_values(text, Path::new("f")).unwrap_err().to_string();
    assert_eq!(
        error("[daemon]\n\n[servers.nas]\nssh = \"me@nas\"\n"),
        "f:3: server \"nas\": needs an endpoint_id"
    );
    assert_eq!(
        error("\n[clients.lap]\n"),
        "f:2: client \"lap\": needs a client_id"
    );
    assert_eq!(
        error("[servers.nas]\n\nendpoint_id = \"nope\"\n"),
        "f:3: server \"nas\": invalid endpoint id: nope"
    );
    assert_eq!(
        error("[clients.lap]\nclient_id = \"nope\"\n"),
        "f:2: client \"lap\": invalid endpoint id: nope"
    );
    assert_eq!(
        error(&format!(
            "[clients.lap]\nclient_id = \"{}\"\nendpoint_id = \"x\"\n",
            id(1)
        )),
        "f:3: unknown key \"clients.lap.endpoint_id\""
    );
    assert_eq!(
        error(&format!(
            "[servers.nas]\nendpoint_id = \"{}\"\nclient_id = \"x\"\n",
            id(1)
        )),
        "f:3: unknown key \"servers.nas.client_id\""
    );
    assert_eq!(
        error(&format!("[servers.all]\nendpoint_id = \"{}\"\n", id(1))),
        "f:1: invalid name \"all\": must be non-empty, contain no ':' or '/', and be neither all, * nor an EndpointId"
    );
    assert_eq!(
        error(&format!("\n[nas]\nendpoint_id = \"{}\"\n", id(1))),
        "f:2: unknown key \"nas\""
    );
    assert_eq!(error("[nothing]\nx = 1\n"), "f:1: unknown key \"nothing\"");
    // A server and a client may have the same name.
    let both = format!(
        "[servers.nas]\nendpoint_id = \"{}\"\n[clients.nas]\nclient_id = \"{}\"\n",
        id(1),
        id(2)
    );
    assert_eq!(values(&both).servers[0].name, values(&both).clients[0].name);
}
