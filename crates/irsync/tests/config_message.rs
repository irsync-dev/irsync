//! The config message as `config::reader` reads it from irsync.toml.

use irsync::config::file::Config;
use irsync::config::reader::{read, read_values};
use irsync::config::settings::DaemonSettings;
use irsync::config::writer::write;
use irsync_proto::config as pb;
use std::path::Path;

fn id(n: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
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
        "identity = \"~/k\"\n[client]\nreconnect = \"90\"\n[hosts.nas]\nendpoint_id = \"{}\"\nrelay_mode = \"disabled\"\nreconnect = 3600\n",
        id(1)
    ));
    assert_eq!(c.identity.as_deref(), Some("~/k")); // as written
    assert_eq!(
        c.client
            .as_ref()
            .unwrap()
            .reconnect
            .as_ref()
            .unwrap()
            .seconds,
        90
    );
    let h = &c.hosts[0];
    assert_eq!(h.endpoint_id.as_deref(), Some(id(1).as_bytes().as_slice()));
    assert_eq!(h.relay_mode, Some(pb::RelayMode::Disabled as i32));
    assert_eq!(h.reconnect.as_ref().unwrap().seconds, 3600);
}

#[test]
fn urls_are_kept_as_written() {
    let text = format!(
        "[client]\nrelay = \"https://r.example\"\n\n[daemon]\nrelay = \"https://d.example\"\n\n\
         [telemetry]\notlp_endpoint = \"http://c:4318\"\n\n[hosts.nas]\nendpoint_id = \"{}\"\nrelay = \"https://h.example\"\n",
        id(1)
    );
    let c = values(&text);
    assert_eq!(
        c.client.as_ref().unwrap().relay.as_deref(),
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
    assert_eq!(c.hosts[0].relay.as_deref(), Some("https://h.example"));
    assert_eq!(write(&c), text);
}

#[test]
fn entries_keep_file_order() {
    let c = values(&format!(
        "[hosts.zed]\nendpoint_id = \"{0}\"\n[exports.b]\npath = \"/b\"\n[hosts.abe]\nendpoint_id = \"{0}\"\n[exports.a]\npath = \"/a\"\n",
        id(1)
    ));
    assert_eq!(
        c.hosts.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
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
    let c = values("[all]\nrw = []\n");
    let all = c.all.unwrap();
    assert!(all.rw.is_empty() && all.rw_set && !all.ro_set);
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
    let text = |aliases: &str| format!("[hosts.nas]\nendpoint_id = \"{}\"\n{aliases}", id(1));
    let h = values(&text("")).hosts.remove(0);
    assert!(h.ssh_aliases.is_empty() && !h.ssh_aliases_set);
    let h = values(&text("ssh_aliases = []\n")).hosts.remove(0);
    assert!(h.ssh_aliases.is_empty() && h.ssh_aliases_set);
    let h = values(&text("ssh_aliases = [\"10.0.0.2\"]\n"))
        .hosts
        .remove(0);
    assert_eq!(h.ssh_aliases[0].value, "10.0.0.2");
    assert!(h.ssh_aliases_set);
}

#[test]
fn a_client_id_is_32_bytes() {
    let h = values(&format!("[hosts.lap]\nclient_id = \"{}\"\n", id(2)))
        .hosts
        .remove(0);
    assert_eq!(h.client_id.as_deref(), Some(id(2).as_bytes().as_slice()));
    assert_eq!(h.endpoint_id, None);
}

#[test]
fn the_immediate_table() {
    assert_eq!(values("").immediate, None);
    let i = values("[immediate]\nrw = [\"lap\"]\n").immediate.unwrap();
    assert_eq!(
        i.rw.iter().map(|e| e.value.as_str()).collect::<Vec<_>>(),
        ["lap"]
    );
    assert!(i.rw_set && !i.ro_set);
}

#[test]
fn from_message_applies_defaults_and_expands_tilde() {
    let c =
        Config::from_message(&values("identity = \"~/k\"\n[daemon]\n"), Path::new("f")).unwrap();
    assert_eq!(c.settings.identity, dirs::home_dir().unwrap().join("k"));
    assert_eq!(c.settings.ssh_identity, None); // the file names an identity
    assert_eq!(c.settings.daemon, DaemonSettings::default());
    assert_eq!(c.hosts.path, Path::new("f"));
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
    m.hosts.push(pb::Host {
        name: "nas".into(),
        endpoint_id: Some(vec![1, 2, 3]),
        ..Default::default()
    });
    assert!(Config::from_message(&m, Path::new("f")).is_err());
}

#[test]
fn from_message_checks_each_edit_again() {
    let host = |name: &str, n: u8| pb::Host {
        name: name.into(),
        endpoint_id: Some(id(n).as_bytes().to_vec()),
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
        refusal(&|m| m.hosts.push(host("a:b", 1))),
        "f: invalid host alias \"a:b\": must be non-empty, contain no ':' or '/', and be neither all, * nor an EndpointId"
    );
    assert_eq!(
        refusal(&|m| m.hosts.push(pb::Host {
            name: "nas".into(),
            ..Default::default()
        })),
        "f: host \"nas\": needs an endpoint_id (its daemon), a client_id, or both"
    );
    assert_eq!(
        refusal(&|m| m.hosts.extend([host("nas", 1), host("nas", 2)])),
        "f: duplicate host alias \"nas\""
    );
    assert_eq!(
        refusal(&|m| m.exports.push(export("all"))),
        "f: \"all\" is reserved: it is the word `grant add all WHO` uses for every export"
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
            m.client = Some(pb::Client {
                relay: Some("not a url".into()),
                ..Default::default()
            })
        }),
        "f: [client]: relative URL without a base: \"not a url\""
    );
}

#[test]
fn errors_are_those_of_config_parse() {
    // The messages `Config::parse` gave before it read through the message.
    for (text, message) in [
        (
            "[hosts.nas]\nendpoint_id = \"nope\"\n",
            "f:2: host \"nas\": invalid endpoint id: nope",
        ),
        (
            "[exports.p]\npath = \"rel\"\n",
            "f:1: [exports.p]: path \"rel\" must be absolute",
        ),
        (
            "[nas]\nendpoint_id = \"x\"\n",
            "f:1: unknown table \"nas\"; hosts live under [hosts.nas]",
        ),
        (
            "rw = [\"x\"]\n",
            "f:1: unknown key \"rw\"; grants for every export live under [all]",
        ),
        (
            "[daemon]\nlisten = [\"1.2.3.4:1\", \"1.2.3.5:1\"]\n",
            "f:2: at most one IPv4 and one IPv6 listen address",
        ),
        (
            "[immediate]\nro = [\"x\"]\n",
            "f:1: [immediate]: unknown key \"ro\"; only `rw` is allowed (immediate grants are read-write)",
        ),
        (
            "[hosts.all]\nendpoint_id = \"x\"\n",
            "f:1: invalid host alias \"all\": must be non-empty, contain no ':' or '/', and be neither all, * nor an EndpointId",
        ),
        (
            "[client]\nbogus = 1\n",
            "f:2: unknown field `bogus`, expected one of `parallel`, `reconnect`, `dial_retries`, `dht`, `ssh_install`, `mdns`, `relay`, `relay_mode`, `congestion`",
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
            "[daemon.discovery]\nbogus = true\n",
            "f:2: unknown field `bogus`, expected one of `dht`, `dns`, `mdns`",
        ),
        (
            "[client]\nrelay = \"not a url\"\n",
            "f:2: relative URL without a base: \"not a url\"",
        ),
        (
            "[telemetry]\notlp_protocol = \"udp\"\n",
            "f:2: unknown variant `udp`, expected `grpc` or `http`",
        ),
        (
            "[hosts.nas]\naddress = [\"nope\"]\n",
            "f:2: invalid address \"nope\": expected ip:port, [ipv6]:port or host:port",
        ),
        (
            "[hosts.nas]\nssh = \"nas\"\n",
            "f:1: host \"nas\": needs an endpoint_id (its daemon), a client_id, or both",
        ),
        (
            "[hosts.b]\nendpoint_id = \"x\"\n[hosts.a]\nendpoint_id = \"y\"\n",
            "f:4: host \"a\": invalid endpoint id: y",
        ),
        (
            "[exports.p]\nread_only = true\n",
            "f:1: [exports.p]: missing field `path`",
        ),
        (
            "[exports.p]\npath = \"/p\"\nbogus = 1\n",
            "f:1: [exports.p]: unknown field `bogus`, expected one of `path`, `read_only`, `warm_index`, `fsync`, `auto`, `rw`, `ro`",
        ),
        (
            "[exports.b]\npath = \"b\"\n[exports.a]\npath = \"a\"\n",
            "f:3: [exports.a]: path \"a\" must be absolute",
        ),
        (
            "[exports]\np = 1\n",
            "f:2: exports.p: expected a table (an export), found integer",
        ),
        (
            "[all]\nrw = \"x\"\n",
            "f:2: invalid type: string \"x\", expected a sequence",
        ),
        (
            "identity = 1\n",
            "f:1: identity: expected a string, found integer",
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
            "[client]\nrelay_mode = 1\n",
            "f:2: wanted string or table",
        ),
        (
            "[client]\nrelay_mode = \"x\"\n",
            "f:2: unknown variant `x`, expected `default` or `disabled`",
        ),
        (
            "[daemon]\ncongestion = \"reno\"\n",
            "f:2: unknown variant `reno`, expected `bbr` or `cubic`",
        ),
        (
            "[telemetry]\notlp_protocol = 1\n",
            "f:2: wanted string or table",
        ),
        (
            "[client]\nreconnect = true\n",
            "f:2: data did not match any variant of untagged enum Raw",
        ),
        (
            "[client]\nreconnect = \"soon\"\n",
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
            "[daemon]\ndiscovery = 1\n",
            "f:2: invalid type: integer `1`, expected struct Discovery",
        ),
        (
            "[hosts.nas]\nreconnect = \"x\"\n",
            "f:2: bad duration \"x\": expected <number>[s|m|h|d]",
        ),
        (
            "[daemon]\nbogus = 1\n",
            "f:2: unknown field `bogus`, expected one of `identity`, `idle_timeout`, `dormant_session_timeout`, `max_dormant_sessions_per_key`, `listen`, `portmap`, `relay`, `relay_mode`, `congestion`, `discovery`, `debug_addr`, `immediate`, `allow_root`",
        ),
        (
            "[telemetry]\nbogus = 1\n",
            "f:2: unknown field `bogus`, expected `otlp_endpoint` or `otlp_protocol`",
        ),
        (
            "[hosts.nas]\nbogus = 1\n",
            "f:2: unknown field `bogus`, expected one of `endpoint_id`, `client_id`, `relay`, `relay_mode`, `congestion`, `address`, `parallel`, `reconnect`, `dial_retries`, `dht`, `mdns`, `ssh`, `ssh_aliases`",
        ),
        (
            "[all]\nbogus = 1\n",
            "f:2: unknown field `bogus`, expected `rw` or `ro`",
        ),
        (
            "[daemon.discovery]\ndht = 1\n",
            "f:2: invalid type: integer `1`, expected a boolean",
        ),
        (
            "[hosts]\nnas = 1\n",
            "f:2: invalid type: integer `1`, expected struct RawHost",
        ),
        (
            "[daemon]\nrelay = 1\n",
            "f:2: invalid type: integer `1`, expected a string representing an URL",
        ),
        (
            "[hosts.nas]\naddress = 1\n",
            "f:2: invalid type: integer `1`, expected a sequence",
        ),
        (
            "[exports.p]\npath = 1\n",
            "f:1: [exports.p]: invalid type: integer `1`, expected path string",
        ),
        (
            "[hosts.nas]\nssh_aliases = [1]\n",
            "f:2: invalid type: integer `1`, expected a string",
        ),
        (
            "[daemon]\nmax_dormant_sessions_per_key = -1\n",
            "f:2: invalid value: integer `-1`, expected u32",
        ),
        (
            "unsafe_config_allowed = 1\n",
            "f:1: unsafe_config_allowed: expected a boolean, found integer",
        ),
        (
            "[client]\nrelay_mode = {}\n",
            "f:2: wanted exactly 1 element, found 0 elements",
        ),
        (
            "[client]\nrelay_mode = { default = 1 }\n",
            "f:2: expected table, found integer",
        ),
        (
            "[daemon]\ncongestion = { x = 1 }\n",
            "f:2: unknown variant `x`, expected `bbr` or `cubic`",
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
            "[hosts.nas]\nendpoint_id = 1\n",
            "f:2: invalid type: integer `1`, expected a string",
        ),
        (
            "not toml",
            "f:1: key with no value, expected `=`",
        ),
    ] {
        let e = read_values(text, Path::new("f")).unwrap_err();
        assert_eq!(e.to_string(), message, "{text}");
    }
}

const COMMENTED: &str = "\
# irsync on the nas
# second header line

identity = \"~/k\" # my key
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
    assert_eq!(c.key_comments["identity"].trailing, "# my key");
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
    assert_eq!(c.footer, ["# the end"]);
}

#[test]
fn hash_in_a_string_is_not_a_comment() {
    let c = read(
        &format!(
            "[hosts.nas]\nendpoint_id = \"{}\"\nssh = \"me@host#1\"\n",
            id(1)
        ),
        Path::new("f"),
    )
    .unwrap();
    assert_eq!(c.hosts[0].ssh.as_deref(), Some("me@host#1"));
    assert!(c.hosts[0].key_comments.is_empty());
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
    let c =
        commented("[daemon]\nportmap = true\n\n# a\n\n# b\n\n[client]\n# first\n\nparallel = 2\n");
    let d = c.daemon.unwrap();
    assert_eq!(d.tail, ["# a", "", "# b"]);
    let cl = c.client.unwrap();
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
    let c = commented("[hosts] # none yet\n[daemon]\n# end");
    assert_eq!(c.footer, ["# none yet", "# end"]);
}

#[test]
fn comments_in_arrays() {
    let c = commented(
        "[all]\nrw = [ # first\n  \"a\" # no comma\n  # after\n]\nro = [\n  # nothing yet\n] # ro\n",
    );
    let all = c.all.unwrap();
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
        "[daemon]\n# on dht\ndiscovery.dht = false # off\n[hosts]\n# the nas\nnas = {{ endpoint_id = \"{}\" }} # inline\n",
        id(1)
    ));
    let disc = c.daemon.unwrap().discovery.unwrap();
    assert_eq!(disc.key_comments["dht"].above, ["# on dht"]);
    assert_eq!(disc.key_comments["dht"].trailing, "# off");
    let h = c.hosts[0].comment.as_ref().unwrap();
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

identity = \"~/k\" # my key
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
        "[hosts.\"my nas\"]",
        "[hosts.\"käse\"]",
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
[hosts.nas] # inline host
endpoint_id = \"{a}\"
address = [\"10.0.0.2:4433\"]

[hosts.lap]
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
identity = \"~/k\"

[daemon]
# on portmap
portmap = false # off

[daemon.discovery]
dht = false # dht off

[hosts.nas]
endpoint_id = \"{a}\" # the id
",
        a = "069ffbb125300737c2767fed918cd5f4bc12b3102fdd9009a2a1320e2ebd3191",
    );
    for (name, twin, header) in [
        ("inline.toml", inline_twin, "[hosts.nas]"),
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
