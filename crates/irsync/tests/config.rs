use irsync::config::client::ClientConfig;
use irsync::config::daemon::{DaemonConfig, RelayMode};
use irsync::config::ConfigError;
use std::path::PathBuf;
use std::time::Duration;

fn id() -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[7u8; 32]).public()
}

fn client_text() -> String {
    format!(
        "Host home\n  EndpointId {}  # my box\n  IdentityFile /keys/id_ed25519\n  Parallel 32\n  Reconnect 24h\n\n\
         Host nas-*\n  IdentityFile=/keys/id_ed25519_nas\n  Dht no\n\n\
         Host *\n  identityfile /keys/other\n  Mdns yes\n  Relay https://relay.example.com/\n",
        id()
    )
}

#[test]
fn first_match_wins_like_ssh_config() {
    let cfg = ClientConfig::parse(&client_text()).unwrap();
    let home = cfg.resolve("home");
    assert_eq!(home.endpoint_id, Some(id()));
    assert_eq!(home.identity_file, Some(PathBuf::from("/keys/id_ed25519")));
    assert_eq!(home.parallel, Some(32));
    assert_eq!(home.reconnect, Some(Duration::from_secs(24 * 3600)));
    assert_eq!(home.mdns, Some(true));
    assert_eq!(home.dht, None);
    assert_eq!(home.relay.unwrap().as_str(), "https://relay.example.com/");

    let nas = cfg.resolve("nas-1");
    assert_eq!(
        nas.identity_file,
        Some(PathBuf::from("/keys/id_ed25519_nas"))
    );
    assert_eq!(nas.dht, Some(false));
    assert_eq!(nas.endpoint_id, None);

    let none = ClientConfig::parse("").unwrap().resolve("x");
    assert_eq!(none.parallel, None);
}

#[test]
fn negated_pattern_excludes() {
    let cfg = ClientConfig::parse("Host * !secret ?b\nParallel 4\n").unwrap();
    assert_eq!(cfg.resolve("abc").parallel, Some(4));
    assert_eq!(cfg.resolve("ab").parallel, Some(4));
    assert_eq!(cfg.resolve("secret").parallel, None);
}

#[test]
fn global_options_before_host_apply_everywhere() {
    let cfg = ClientConfig::parse("Parallel 2\nHost a\nParallel 9\n").unwrap();
    assert_eq!(cfg.resolve("a").parallel, Some(2));
    assert_eq!(cfg.resolve("zzz").parallel, Some(2));
}

#[test]
fn unknown_keyword_is_error() {
    match ClientConfig::parse("Host a\nBogus 1\n") {
        Err(ConfigError::UnknownKeyword { line, keyword }) => {
            assert_eq!(line, 2);
            assert_eq!(keyword, "Bogus");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn bad_value_is_error() {
    assert!(matches!(
        ClientConfig::parse("Parallel lots\n"),
        Err(ConfigError::BadValue { line: 1, .. })
    ));
    assert!(matches!(
        ClientConfig::parse("Dht maybe\n"),
        Err(ConfigError::BadValue { .. })
    ));
}

#[test]
fn load_explicit_path_and_tilde() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("cfg");
    std::fs::write(&p, "IdentityFile ~/k\n").unwrap();
    let cfg = ClientConfig::load(Some(&p)).unwrap();
    let got = cfg.resolve("h").identity_file.unwrap();
    assert_eq!(got, dirs::home_dir().unwrap().join("k"));
}

#[test]
fn duration_parse() {
    use irsync::config::duration::parse;
    assert_eq!(parse("24h").unwrap(), Duration::from_secs(86400));
    assert_eq!(parse("30m").unwrap(), Duration::from_secs(1800));
    assert_eq!(parse("7d").unwrap(), Duration::from_secs(7 * 86400));
    assert_eq!(parse("45").unwrap(), Duration::from_secs(45));
    assert_eq!(parse("10s").unwrap(), Duration::from_secs(10));
    assert_eq!(parse("0").unwrap(), Duration::ZERO);
    assert!(parse("").is_err());
    assert!(parse("5x").is_err());
    assert!(parse("-1h").is_err());
}

#[test]
fn daemon_toml_from_spec_parses() {
    let text = r#"
identity = "/etc/irsync/key"
authorized_keys = "/etc/irsync/authorized_keys"
state_dir = "/var/lib/irsync"
relay_mode = "disabled"
relay = "https://relay.example.com"
dormant_session_timeout = "3d"

[discovery]
dht = true
dns = false
mdns = false

[modules.backup]
path = "/srv/backup"

[modules.docs]
path = "/srv/docs"
read_only = true
"#;
    let cfg = DaemonConfig::parse(text).unwrap();
    assert_eq!(cfg.identity, PathBuf::from("/etc/irsync/key"));
    assert_eq!(cfg.state_dir, PathBuf::from("/var/lib/irsync"));
    assert_eq!(cfg.relay_mode, RelayMode::Disabled);
    assert!(cfg.relay.is_some());
    assert_eq!(cfg.dormant_session_timeout, Duration::from_secs(3 * 86400));
    assert!(cfg.discovery.dht && !cfg.discovery.dns && !cfg.discovery.mdns);
    assert_eq!(cfg.modules.len(), 2);
    assert!(!cfg.modules["backup"].read_only);
    assert!(cfg.modules["docs"].read_only);
    assert_eq!(cfg.modules["docs"].path, PathBuf::from("/srv/docs"));
}

#[test]
fn daemon_defaults_when_sections_missing() {
    let cfg = DaemonConfig::parse("").unwrap();
    let home = dirs::home_dir().unwrap();
    assert_eq!(cfg.identity, home.join(".config/irsync/daemon_ed25519"));
    assert_eq!(
        cfg.authorized_keys,
        home.join(".config/irsync/authorized_keys")
    );
    assert_eq!(cfg.state_dir, home.join(".local/state/irsync"));
    assert!(cfg.discovery.dht && cfg.discovery.dns && cfg.discovery.mdns);
    assert_eq!(cfg.relay, None);
    assert_eq!(cfg.relay_mode, RelayMode::Default);
    assert_eq!(cfg.dormant_session_timeout, Duration::from_secs(7 * 86400));
    assert!(cfg.modules.is_empty());
}

#[test]
fn daemon_load_explicit_path() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("d.toml");
    std::fs::write(&p, "state_dir = \"/x\"\n").unwrap();
    assert_eq!(
        DaemonConfig::load(Some(&p)).unwrap().state_dir,
        PathBuf::from("/x")
    );
    assert!(DaemonConfig::load(Some(&dir.path().join("missing"))).is_err());
}
