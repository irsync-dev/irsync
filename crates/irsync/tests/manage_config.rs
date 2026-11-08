use irsync::manage::config::{default_of, get, kind_of, list, parse_key, Got, Kind};
use toml_edit::DocumentMut;

fn doc(s: &str) -> DocumentMut {
    s.parse().unwrap()
}
fn p(s: &str) -> Vec<String> {
    parse_key(s).unwrap()
}
fn id() -> String {
    iroh::SecretKey::from_bytes(&[5; 32]).public().to_string()
}
fn lines(v: &[&str]) -> Got {
    Got::Lines(v.iter().map(|s| s.to_string()).collect())
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
    let d = doc(
        "[client]\nparallel = 4\n[daemon]\nlisten = [\"0.0.0.0:0\", \"[::]:0\"]\n\
         [hosts.home]\nendpoint_id = \"abc\"\nrelay = \"https://r/\"\n[daemon.discovery]\nmdns = false\n",
    );
    assert_eq!(get(&d, &p("client.parallel")).unwrap(), lines(&["4"]));
    assert_eq!(
        get(&d, &p("daemon.listen")).unwrap(),
        lines(&["0.0.0.0:0", "[::]:0"])
    );
    assert_eq!(
        get(&d, &p("hosts.home")).unwrap(),
        lines(&["endpoint_id = \"abc\"", "relay = \"https://r/\""])
    );
    assert_eq!(
        get(&d, &p("daemon")).unwrap(),
        lines(&[
            "listen = [\"0.0.0.0:0\", \"[::]:0\"]",
            "discovery.mdns = false"
        ])
    );
    assert_eq!(get(&d, &p("client.reconnect")).unwrap(), Got::Unset);
    assert_eq!(get(&d, &p("hosts.nope")).unwrap(), Got::Unset);
    assert!(get(&d, &p("daemon.nope")).is_err());
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
    assert_eq!(default_of(&p("client.parallel")), Some(vec!["32".into()]));
    assert_eq!(default_of(&p("client.reconnect")), Some(vec!["1d".into()]));
    assert_eq!(default_of(&p("client.dht")), Some(vec!["true".into()]));
    assert_eq!(default_of(&p("client.mdns")), Some(vec!["true".into()]));
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
fn list_is_dotted_keys_in_file_order_with_quoting() {
    let d = doc(
        "identity = \"/k\"\n[daemon]\nlisten = [\"a:1\"]\n[daemon.discovery]\ndht = false\n\
         [exports.\"www.example.com\"]\npath = \"/w\"\n",
    );
    assert_eq!(
        list(&d),
        vec![
            "identity = \"/k\"",
            "daemon.listen = [\"a:1\"]",
            "daemon.discovery.dht = false",
            "exports.\"www.example.com\".path = \"/w\"",
        ]
    );
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
    use irsync::manage::config::parse_value;
    let v = |k, vals: &[&str]| {
        parse_value(k, &vals.iter().map(|s| s.to_string()).collect::<Vec<_>>())
            .map(|v| v.to_string().trim().to_string())
    };
    assert_eq!(v(Kind::Bool, &["true"]).unwrap(), "true");
    assert_eq!(v(Kind::Int, &["8"]).unwrap(), "8");
    assert_eq!(v(Kind::Duration, &["30m"]).unwrap(), "\"30m\"");
    assert_eq!(
        v(Kind::AddrArray, &["a:1", "b:2"]).unwrap(),
        "[\"a:1\", \"b:2\"]"
    );
    assert_eq!(v(Kind::StrArray, &[]).unwrap(), "[]");
    assert_eq!(v(Kind::Id, &[&id()]).unwrap(), format!("{:?}", id()));
    assert_eq!(v(Kind::Path, &["/x"]).unwrap(), "\"/x\"");
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
fn set_and_unset_edit_the_document_and_keep_the_rest() {
    use irsync::manage::config::{parse_value, set, unset};
    use irsync::manage::ManageError;
    let mut d = doc("# keep me\n[client]\nparallel = 4   # inline\n");
    let val = |k, s: &str| parse_value(k, &[s.to_string()]).unwrap();
    set(&mut d, &p("client.parallel"), val(Kind::Int, "8")).unwrap();
    set(&mut d, &p("hosts.home.endpoint_id"), val(Kind::Id, &id())).unwrap();
    set(
        &mut d,
        &p("daemon.discovery.mdns"),
        val(Kind::Bool, "false"),
    )
    .unwrap();
    assert_eq!(
        d.to_string(),
        format!(
            "# keep me\n[client]\nparallel = 8   # inline\n\n[hosts.home]\nendpoint_id = \"{}\"\n\n[daemon.discovery]\nmdns = false\n",
            id()
        )
    );
    unset(&mut d, &p("client.parallel")).unwrap();
    unset(&mut d, &p("hosts.home")).unwrap();
    let text = d.to_string();
    assert!(
        !text.contains("parallel") && !text.contains("hosts"),
        "{text}"
    );
    assert!(matches!(
        unset(&mut d, &p("client.parallel")),
        Err(ManageError::NothingToRemove(_))
    ));
    assert!(set(&mut d, &p("hosts.home"), toml_edit::Value::from(1)).is_err());
}

#[test]
fn get_and_list_ignore_comments_and_layout() {
    let d = doc(
        "[client]\nparallel = 4 # four streams\n[daemon]\nlisten = [\n  \"a:1\", # v4\n  \"b:2\",\n] # both\nportmap = false # no\n",
    );
    assert_eq!(get(&d, &p("client.parallel")).unwrap(), lines(&["4"]));
    assert_eq!(
        get(&d, &p("daemon.listen")).unwrap(),
        lines(&["a:1", "b:2"])
    );
    assert_eq!(
        get(&d, &p("daemon")).unwrap(),
        lines(&["listen = [\"a:1\", \"b:2\"]", "portmap = false"])
    );
    assert_eq!(
        list(&d),
        vec![
            "client.parallel = 4",
            "daemon.listen = [\"a:1\", \"b:2\"]",
            "daemon.portmap = false",
        ]
    );
}

#[test]
fn bare_seconds_set_a_duration_as_an_integer() {
    use irsync::manage::config::parse_value;
    let v = parse_value(Kind::Duration, &["600".to_string()]).unwrap();
    assert_eq!(v.as_integer(), Some(600), "{v}");
    let v = parse_value(Kind::Duration, &["10m".to_string()]).unwrap();
    assert_eq!(v.as_str(), Some("10m"), "{v}");
    assert!(parse_value(Kind::Duration, &["-5".to_string()]).is_err());
}

#[test]
fn unset_removes_tables_it_leaves_empty() {
    use irsync::manage::config::unset;
    let mut d = doc(
        "[client]\nparallel = 4\nrelay = \"https://r\"\n\n[hosts.home]\nendpoint_id = \"x\"\n\n[daemon.discovery]\nmdns = false\n",
    );
    unset(&mut d, &p("client.parallel")).unwrap();
    unset(&mut d, &p("hosts.home.endpoint_id")).unwrap();
    unset(&mut d, &p("daemon.discovery.mdns")).unwrap();
    assert_eq!(d.to_string(), "[client]\nrelay = \"https://r\"\n");
}

#[test]
fn set_telemetry_endpoint() {
    use irsync::manage::config::{parse_value, set};
    let mut d = doc("");
    let key = p("telemetry.otlp_endpoint");
    assert_eq!(kind_of(&key), Some(Kind::Url));
    assert_eq!(kind_of(&p("telemetry.otlp_protocol")), Some(Kind::Str));
    assert_eq!(kind_of(&p("daemon.debug_addr")), Some(Kind::Str));
    let v = parse_value(Kind::Url, &["http://c:4318".to_string()]).unwrap();
    set(&mut d, &key, v).unwrap();
    assert_eq!(get(&d, &key).unwrap(), lines(&["http://c:4318"]));
}
