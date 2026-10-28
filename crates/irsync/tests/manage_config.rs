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
    assert_eq!(
        default_of(&p("identity")),
        Some(vec![irsync::config::settings::Settings::default()
            .identity
            .display()
            .to_string()])
    );
    assert_eq!(default_of(&p("client.parallel")), None);
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
