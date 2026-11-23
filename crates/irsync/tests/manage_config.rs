use irsync::config::file::Config;
use irsync::config::reader::read;
use irsync::config::writer::write;
use irsync::manage::config::{default_of, kind_of, parse_key, Got, Kind};
use irsync::manage::config::{get, list, parse_value, set, unset, Value};
use irsync::manage::ManageError;
use irsync_proto::config as pb;
use std::path::Path;

fn msg(s: &str) -> pb::Config {
    read(s, Path::new("f")).unwrap()
}
fn p(s: &str) -> Vec<String> {
    parse_key(s).unwrap()
}
fn id() -> String {
    iroh::SecretKey::from_bytes(&[5; 32]).public().to_string()
}
fn id2() -> String {
    iroh::SecretKey::from_bytes(&[6; 32]).public().to_string()
}
fn lines(v: &[&str]) -> Got {
    Got::Lines(v.iter().map(|s| s.to_string()).collect())
}
fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn keys_parse_with_toml_grammar() {
    assert_eq!(p("daemon.listen"), ["daemon", "listen"]);
    assert_eq!(
        p("exports.\"www.example.com\".path"),
        ["exports", "www.example.com", "path"]
    );
    assert!(parse_key("a..b").is_err() && parse_key("").is_err());
}

#[test]
fn kinds_of_known_keys() {
    assert_eq!(kind_of(&p("daemon.listen")), Some(Kind::AddrArray));
    assert_eq!(kind_of(&p("hosts.home.reconnect")), Some(Kind::Duration));
    assert_eq!(kind_of(&p("hosts.home.dial_retries")), Some(Kind::Int));
    assert_eq!(kind_of(&p("client.dial_retries")), Some(Kind::Int));
    assert_eq!(kind_of(&p("exports.photos.read_only")), Some(Kind::Bool));
    assert_eq!(kind_of(&p("exports.photos.fsync")), Some(Kind::Bool));
    assert_eq!(kind_of(&p("hosts.home")), Some(Kind::Table));
    assert_eq!(kind_of(&p("daemon.discovery")), Some(Kind::Table));
    assert_eq!(kind_of(&p("all.rw")), Some(Kind::StrArray));
    assert_eq!(kind_of(&p("identity")), Some(Kind::Path));
    assert!(kind_of(&p("daemon.nope")).is_none() && kind_of(&p("nope")).is_none());
    assert!(kind_of(&p("hosts.home.nope")).is_none());
}

#[test]
fn get_prints_scalars_arrays_and_tables() {
    let m = msg(&format!(
        "[client]\nparallel = 4\n[daemon]\nlisten = [\"0.0.0.0:0\", \"[::]:0\"]\n\
         [hosts.home]\nendpoint_id = \"{}\"\nrelay = \"https://r/\"\n[daemon.discovery]\nmdns = false\n",
        id()
    ));
    assert_eq!(get(&m, &p("client.parallel")).unwrap(), lines(&["4"]));
    assert_eq!(
        get(&m, &p("daemon.listen")).unwrap(),
        lines(&["0.0.0.0:0", "[::]:0"])
    );
    assert_eq!(
        get(&m, &p("hosts.home")).unwrap(),
        Got::Lines(vec![
            format!("endpoint_id = \"{}\"", id()),
            "relay = \"https://r/\"".to_string()
        ])
    );
    assert_eq!(
        get(&m, &p("daemon")).unwrap(),
        lines(&[
            "listen = [\"0.0.0.0:0\", \"[::]:0\"]",
            "discovery.mdns = false"
        ])
    );
    assert_eq!(
        get(&m, &p("hosts")).unwrap(),
        Got::Lines(vec![
            format!("home.endpoint_id = \"{}\"", id()),
            "home.relay = \"https://r/\"".to_string()
        ])
    );
    assert_eq!(get(&m, &p("client.reconnect")).unwrap(), Got::Unset);
    assert_eq!(get(&m, &p("hosts.nope")).unwrap(), Got::Unset);
    assert_eq!(get(&m, &p("telemetry")).unwrap(), Got::Unset);
    assert_eq!(get(&m, &p("exports")).unwrap(), Got::Unset);
    assert!(matches!(
        get(&m, &p("daemon.nope")),
        Err(ManageError::UnknownKey(_))
    ));
}

#[test]
fn defaults_for_settings_keys() {
    assert_eq!(
        default_of(&p("daemon.idle_timeout")),
        Some(vec!["30m".into()])
    );
    assert_eq!(
        default_of(&p("daemon.dormant_session_timeout")),
        Some(vec!["7d".into()])
    );
    assert_eq!(
        default_of(&p("daemon.max_dormant_sessions_per_key")),
        Some(vec!["25".into()])
    );
    assert_eq!(default_of(&p("daemon.portmap")), Some(vec!["true".into()]));
    assert_eq!(default_of(&p("daemon.listen")), Some(vec![]));
    assert_eq!(
        default_of(&p("daemon.relay_mode")),
        Some(vec!["default".into()])
    );
    assert_eq!(
        default_of(&p("daemon.discovery.dht")),
        Some(vec!["true".into()])
    );
    // Path defaults print as the file would hold them: home as `~`.
    assert_eq!(
        default_of(&p("identity")),
        Some(vec!["~/.config/irsync/id_ed25519".into()])
    );
    assert_eq!(
        default_of(&p("daemon.identity")),
        Some(vec!["~/.config/irsync/daemon_ed25519".into()])
    );
    // The [client] keys book/src/reference/configuration.md lists with defaults: the effective ones.
    assert_eq!(default_of(&p("client.parallel")), Some(vec!["128".into()]));
    assert_eq!(default_of(&p("client.reconnect")), Some(vec!["1d".into()]));
    assert_eq!(
        default_of(&p("client.dial_retries")),
        Some(vec!["4".into()])
    );
    assert_eq!(default_of(&p("client.dht")), Some(vec!["true".into()]));
    assert_eq!(default_of(&p("client.mdns")), Some(vec!["true".into()]));
    assert_eq!(
        default_of(&p("client.ssh_install")),
        Some(vec!["true".into()])
    );
    assert_eq!(default_of(&p("client.relay")), None);
    assert_eq!(
        default_of(&p("client.relay_mode")),
        Some(vec!["default".to_string()])
    );
    assert!(kind_of(&p("hosts.home.relay_mode")).is_some());
    assert_eq!(
        default_of(&p("client.congestion")),
        Some(vec!["bbr".into()])
    );
    assert_eq!(
        default_of(&p("daemon.congestion")),
        Some(vec!["bbr".into()])
    );
    assert!(kind_of(&p("hosts.home.congestion")).is_some());
    assert_eq!(default_of(&p("daemon.relay")), None);
    assert_eq!(default_of(&p("hosts.home.relay")), None);
    let _ = id();
}

#[test]
fn list_is_dotted_keys_in_the_writers_order_with_quoting() {
    let m = msg(
        "[exports.\"www.example.com\"]\npath = \"/w\"\n[daemon.discovery]\ndht = false\n\
         [daemon]\nlisten = [\"a:1\"]\n\nidentity = \"/k\"\n",
    );
    assert_eq!(
        list(&m),
        vec![
            "daemon.identity = \"/k\"",
            "daemon.listen = [\"a:1\"]",
            "daemon.discovery.dht = false",
            "exports.\"www.example.com\".path = \"/w\"",
        ]
    );
    let m = msg("identity = \"/k\"\n[client]\nparallel = 2\n");
    assert_eq!(list(&m), vec!["identity = \"/k\"", "client.parallel = 2"]);
}

#[test]
fn list_has_the_keys_and_values_the_writer_writes() {
    let mut m = base();
    for (key, values) in ALL_KEYS {
        let path = parse_key(key).unwrap();
        let value = parse_value(kind_of(&path).unwrap(), &strings(values)).unwrap();
        set(&mut m, &path, value).unwrap();
    }
    // Every key the writer writes, as `[table]` headers and `key = value` lines.
    let mut written = Vec::new();
    let mut table = String::new();
    for line in write(&m).lines().filter(|l| !l.is_empty()) {
        match line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            Some(t) => table = format!("{t}."),
            None => written.push(format!("{table}{line}")),
        }
    }
    assert_eq!(list(&m), written);
    assert_eq!(list(&m).len(), ALL_KEYS.len());
}

#[test]
fn durations_format_in_the_largest_even_unit() {
    use irsync::config::duration::format;
    use std::time::Duration;
    assert_eq!(format(Duration::from_secs(1800)), "30m");
    assert_eq!(format(Duration::from_secs(7 * 86400)), "7d");
    assert_eq!(format(Duration::from_secs(90)), "90s");
    assert_eq!(format(Duration::from_secs(0)), "0s");
    assert_eq!(format(Duration::from_secs(7200)), "2h");
}

#[test]
fn values_are_typed_by_kind() {
    let v = |k, vals: &[&str]| parse_value(k, &strings(vals));
    assert_eq!(v(Kind::Bool, &["true"]).unwrap(), Value::Bool(true));
    assert_eq!(v(Kind::Int, &["8"]).unwrap(), Value::Int(8));
    assert_eq!(
        v(Kind::Duration, &["30m"]).unwrap(),
        Value::Text("30m".into())
    );
    assert_eq!(
        v(Kind::AddrArray, &["a:1", "b:2"]).unwrap(),
        Value::List(strings(&["a:1", "b:2"]))
    );
    assert_eq!(v(Kind::StrArray, &[]).unwrap(), Value::List(vec![]));
    assert_eq!(v(Kind::Id, &[&id()]).unwrap(), Value::Text(id()));
    assert_eq!(v(Kind::Path, &["/x"]).unwrap(), Value::Text("/x".into()));
    for (kind, vals) in [
        (Kind::Bool, &["yes"][..]),
        (Kind::Int, &["x"]),
        (Kind::Duration, &["5x"]),
        (Kind::Bool, &[]),
        (Kind::Bool, &["true", "false"]),
        (Kind::Id, &["nope"]),
        (Kind::AddrArray, &["nope"]),
        (Kind::Url, &["not a url"]),
        (Kind::Table, &["x"]),
    ] {
        assert!(v(kind, vals).is_err(), "{kind:?} {vals:?}");
    }
}

#[test]
fn set_and_unset_edit_the_message_and_keep_the_rest() {
    let mut m = msg("# keep me\n[client]\nparallel = 4   # inline\n");
    let val = |k, s: &str| parse_value(k, &[s.to_string()]).unwrap();
    set(&mut m, &p("client.parallel"), val(Kind::Int, "8")).unwrap();
    set(&mut m, &p("hosts.home.endpoint_id"), val(Kind::Id, &id())).unwrap();
    set(
        &mut m,
        &p("daemon.discovery.mdns"),
        val(Kind::Bool, "false"),
    )
    .unwrap();
    assert_eq!(
        write(&m),
        format!(
            "# keep me\n[client]\nparallel = 8 # inline\n\n[daemon.discovery]\nmdns = false\n\n[hosts.home]\nendpoint_id = \"{}\"\n",
            id()
        )
    );
    unset(&mut m, &p("client.parallel")).unwrap();
    unset(&mut m, &p("hosts.home")).unwrap();
    let text = write(&m);
    assert!(
        !text.contains("parallel") && !text.contains("hosts"),
        "{text}"
    );
    assert!(matches!(
        unset(&mut m, &p("client.parallel")),
        Err(ManageError::NothingToRemove(_))
    ));
    assert!(matches!(
        unset(&mut m, &p("hosts.home")),
        Err(ManageError::NothingToRemove(_))
    ));
    assert!(set(&mut m, &p("hosts.home"), Value::Int(1)).is_err());
    assert!(matches!(
        set(&mut m, &p("daemon.nope"), Value::Int(1)),
        Err(ManageError::UnknownKey(_))
    ));
    assert!(matches!(
        unset(&mut m, &p("daemon.nope")),
        Err(ManageError::UnknownKey(_))
    ));
}

#[test]
fn get_and_list_ignore_comments_and_layout() {
    let m = msg(
        "[client]\nparallel = 4 # four streams\n[daemon]\nlisten = [\n  \"a:1\", # v4\n  \"b:2\",\n] # both\nportmap = false # no\n",
    );
    assert_eq!(get(&m, &p("client.parallel")).unwrap(), lines(&["4"]));
    assert_eq!(
        get(&m, &p("daemon.listen")).unwrap(),
        lines(&["a:1", "b:2"])
    );
    assert_eq!(
        get(&m, &p("daemon")).unwrap(),
        lines(&["listen = [\"a:1\", \"b:2\"]", "portmap = false"])
    );
    assert_eq!(
        list(&m),
        vec![
            "client.parallel = 4",
            "daemon.listen = [\"a:1\", \"b:2\"]",
            "daemon.portmap = false",
        ]
    );
}

#[test]
fn bare_seconds_set_a_duration_as_an_integer() {
    let v = parse_value(Kind::Duration, &["600".to_string()]).unwrap();
    assert_eq!(v, Value::Int(600));
    let v = parse_value(Kind::Duration, &["10m".to_string()]).unwrap();
    assert_eq!(v, Value::Text("10m".into()));
    assert!(parse_value(Kind::Duration, &["-5".to_string()]).is_err());
    // Either way the message holds a duration, which prints as the writer spells it.
    let mut m = pb::Config::default();
    let key = p("client.reconnect");
    set(
        &mut m,
        &key,
        parse_value(Kind::Duration, &["90".to_string()]).unwrap(),
    )
    .unwrap();
    assert_eq!(get(&m, &key).unwrap(), lines(&["90s"]));
    set(
        &mut m,
        &key,
        parse_value(Kind::Duration, &["600".to_string()]).unwrap(),
    )
    .unwrap();
    assert_eq!(get(&m, &key).unwrap(), lines(&["10m"]));
    assert_eq!(write(&m), "[client]\nreconnect = \"10m\"\n");
}

#[test]
fn unset_removes_tables_it_leaves_empty() {
    let mut m = msg(&format!(
        "[client]\nparallel = 4\nrelay = \"https://r/\"\n\n[hosts.home]\nendpoint_id = \"{}\"\n\n[daemon.discovery]\nmdns = false\n",
        id()
    ));
    unset(&mut m, &p("client.parallel")).unwrap();
    unset(&mut m, &p("hosts.home.endpoint_id")).unwrap();
    unset(&mut m, &p("daemon.discovery.mdns")).unwrap();
    assert_eq!(write(&m), "[client]\nrelay = \"https://r/\"\n");
    assert!(m.daemon.is_none() && m.hosts.is_empty());
}

#[test]
fn set_telemetry_endpoint() {
    let mut m = pb::Config::default();
    let key = p("telemetry.otlp_endpoint");
    assert_eq!(kind_of(&key), Some(Kind::Url));
    assert_eq!(kind_of(&p("telemetry.otlp_protocol")), Some(Kind::Str));
    assert_eq!(kind_of(&p("daemon.debug_addr")), Some(Kind::Str));
    let v = parse_value(Kind::Url, &["http://c:4318".to_string()]).unwrap();
    set(&mut m, &key, v).unwrap();
    // Kept as typed, as the reader keeps a URL as written.
    assert_eq!(get(&m, &key).unwrap(), lines(&["http://c:4318"]));
    assert_eq!(
        write(&m),
        "[telemetry]\notlp_endpoint = \"http://c:4318\"\n"
    );
}

/// One key per arm of `kind_of` that is not a table, with a value
/// that loads.
const ALL_KEYS: &[(&str, &[&str])] = &[
    ("unsafe_config_allowed", &["false"]),
    ("identity", &["/k"]),
    ("state_dir", &["~/state"]),
    ("client.parallel", &["4"]),
    ("client.reconnect", &["1h"]),
    ("client.dial_retries", &["2"]),
    ("client.dht", &["false"]),
    ("client.ssh_install", &["false"]),
    ("client.mdns", &["false"]),
    ("client.relay", &["https://r/"]),
    ("client.relay_mode", &["disabled"]),
    ("client.congestion", &["cubic"]),
    ("daemon.identity", &["/d"]),
    ("daemon.idle_timeout", &["10m"]),
    ("daemon.dormant_session_timeout", &["1d"]),
    ("daemon.max_dormant_sessions_per_key", &["3"]),
    ("daemon.listen", &["0.0.0.0:0", "[::]:0"]),
    ("daemon.portmap", &["false"]),
    ("daemon.relay", &["https://r/"]),
    ("daemon.relay_mode", &["disabled"]),
    ("daemon.congestion", &["cubic"]),
    ("daemon.debug_addr", &["127.0.0.1:9000"]),
    ("daemon.immediate", &["true"]),
    ("daemon.allow_root", &["true"]),
    ("daemon.discovery.dht", &["false"]),
    ("daemon.discovery.dns", &["false"]),
    ("daemon.discovery.mdns", &["false"]),
    ("telemetry.otlp_endpoint", &["http://c:4318/"]),
    ("telemetry.otlp_protocol", &["http"]),
    ("hosts.nas.endpoint_id", &[ID5]),
    ("hosts.nas.client_id", &[ID5]),
    ("hosts.nas.relay", &["https://r/"]),
    ("hosts.nas.relay_mode", &["disabled"]),
    ("hosts.nas.congestion", &["cubic"]),
    ("hosts.nas.address", &["10.0.0.1:4000"]),
    ("hosts.nas.parallel", &["4"]),
    ("hosts.nas.reconnect", &["1h"]),
    ("hosts.nas.dial_retries", &["2"]),
    ("hosts.nas.dht", &["false"]),
    ("hosts.nas.mdns", &["false"]),
    ("hosts.nas.ssh", &["me@nas"]),
    ("hosts.nas.ssh_aliases", &["box"]),
    ("exports.p.path", &["/srv/q"]),
    ("exports.p.read_only", &["true"]),
    ("exports.p.warm_index", &["true"]),
    ("exports.p.fsync", &["true"]),
    ("exports.p.auto", &["true"]),
    ("exports.p.rw", &["nas"]),
    ("exports.p.ro", &["nas"]),
    ("all.rw", &["nas"]),
    ("all.ro", &["nas"]),
    ("immediate.rw", &["nas"]),
];

/// The id of `[5; 32]`, as `id()` prints it.
const ID5: &str = "6e7a1cdd29b0b78fd13af4c5598feff4ef2a97166e3ca6f2e4fbfccd80505bf1";

/// A message with a host `nas` and an export `p`.
fn base() -> pb::Config {
    msg(&format!(
        "[hosts.nas]\nendpoint_id = \"{}\"\n\n[exports.p]\npath = \"/srv/p\"\n",
        id2()
    ))
}

#[test]
fn the_id_constant_is_the_test_id() {
    assert_eq!(ID5, id());
}

#[test]
fn every_known_key_can_be_set_read_and_unset() {
    for (key, values) in ALL_KEYS {
        let mut m = base();
        let path = parse_key(key).unwrap();
        set(
            &mut m,
            &path,
            parse_value(kind_of(&path).unwrap(), &strings(values)).unwrap(),
        )
        .unwrap();
        assert!(matches!(get(&m, &path).unwrap(), Got::Lines(_)), "{key}");
        Config::from_message(&m, Path::new("f")).unwrap();
        unset(&mut m, &path).unwrap();
        assert_eq!(get(&m, &path).unwrap(), Got::Unset, "{key}");
    }
}

#[test]
fn unset_takes_the_keys_comment_and_set_keeps_it() {
    let mut m = read("[daemon]\n# why\nportmap = false\n", Path::new("f")).unwrap();
    let p = parse_key("daemon.portmap").unwrap();
    set(&mut m, &p, Value::Bool(true)).unwrap();
    assert_eq!(write(&m), "[daemon]\n# why\nportmap = true\n");
    unset(&mut m, &p).unwrap();
    assert_eq!(write(&m), "");
}

#[test]
fn unset_of_a_table_takes_its_comments() {
    let mut m = msg(&format!(
        "# the nas\n[hosts.nas]\nendpoint_id = \"{}\"\n\n# the tv\n[hosts.tv]\nendpoint_id = \"{}\"\n\n\
         # client side\n[client]\nparallel = 2\n\n[daemon]\nportmap = false\n\n# found how\n[daemon.discovery]\ndns = false\n",
        id(),
        id2()
    ));
    unset(&mut m, &p("hosts.nas")).unwrap();
    unset(&mut m, &p("client")).unwrap();
    unset(&mut m, &p("daemon.discovery")).unwrap();
    assert_eq!(
        write(&m),
        format!(
            "[daemon]\nportmap = false\n\n# the tv\n[hosts.tv]\nendpoint_id = \"{}\"\n",
            id2()
        )
    );
    unset(&mut m, &p("hosts")).unwrap();
    unset(&mut m, &p("daemon")).unwrap();
    assert_eq!(write(&m), "");
    for key in [
        "hosts",
        "daemon",
        "client",
        "exports",
        "all",
        "immediate",
        "telemetry",
    ] {
        assert!(
            matches!(unset(&mut m, &p(key)), Err(ManageError::NothingToRemove(_))),
            "{key}"
        );
    }
}

#[test]
fn set_refuses_what_the_message_cannot_hold_and_changes_nothing() {
    let mut m = pb::Config::default();
    for (key, value) in [
        ("hosts.nas.relay_mode", Value::Text("bogus".into())),
        ("client.congestion", Value::Text("reno".into())),
        ("telemetry.otlp_protocol", Value::Text("udp".into())),
        ("client.parallel", Value::Int(-1)),
        ("client.parallel", Value::Int(1 << 40)),
        ("client.parallel", Value::Text("4".into())),
    ] {
        assert!(
            matches!(set(&mut m, &p(key), value), Err(ManageError::BadValue(_))),
            "{key}"
        );
    }
    assert_eq!(m, pb::Config::default());
    let e = set(&mut m, &p("client.relay_mode"), Value::Text("bogus".into())).unwrap_err();
    assert_eq!(
        e.to_string(),
        "unknown variant `bogus`, expected `default` or `disabled`"
    );
}

#[test]
fn unsetting_the_only_id_of_a_host_with_other_keys_does_not_load() {
    let mut m = msg(&format!(
        "[hosts.nas]\nendpoint_id = \"{}\"\nrelay = \"https://r/\"\n",
        id()
    ));
    unset(&mut m, &p("hosts.nas.endpoint_id")).unwrap();
    assert_eq!(m.hosts.len(), 1);
    assert!(Config::from_message(&m, Path::new("f")).is_err());
}
