use irsync::config::exports::Exports;
use irsync::config::hosts::{valid_alias, Hosts};
use irsync::config::settings::{RelayMode, Settings};
use irsync::config::ConfigError;
use std::path::Path;
use std::time::Duration;

fn id(n: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
}
fn p() -> &'static Path {
    Path::new("/x/f.toml")
}

#[test]
fn settings_defaults_when_empty() {
    let s = Settings::parse("", p()).unwrap();
    assert!(s.identity.ends_with(".config/irsync/id_ed25519"));
    assert!(s.daemon.identity.ends_with(".config/irsync/daemon_ed25519"));
    assert_eq!(s.daemon.idle_timeout, Duration::from_secs(30 * 60));
    assert_eq!(
        s.daemon.dormant_session_timeout,
        Duration::from_secs(7 * 86400)
    );
    assert!(
        s.daemon.portmap
            && s.daemon.discovery.dht
            && s.daemon.discovery.dns
            && s.daemon.discovery.mdns
    );
    assert_eq!(s.client.parallel, None);
}

#[test]
fn settings_tables_parse_and_expand_tilde() {
    let t = "identity = \"~/k\"\n[client]\nparallel = 8\nreconnect = \"1h\"\n[daemon]\nidentity = \"~/d\"\nrelay_mode = \"disabled\"\nlisten = [\"127.0.0.1:0\"]\n[daemon.discovery]\ndht = false\n";
    let s = Settings::parse(t, p()).unwrap();
    assert_eq!(s.identity, dirs::home_dir().unwrap().join("k"));
    assert_eq!(s.daemon.identity, dirs::home_dir().unwrap().join("d"));
    assert_eq!(s.client.parallel, Some(8));
    assert_eq!(s.client.reconnect, Some(Duration::from_secs(3600)));
    assert_eq!(s.daemon.relay_mode, RelayMode::Disabled);
    assert_eq!(s.daemon.listen.len(), 1);
    assert!(!s.daemon.discovery.dht && s.daemon.discovery.dns);
}

#[test]
fn settings_unknown_key_names_file_and_key() {
    let e = Settings::parse("[client]\nbogus = 1\n", p()).unwrap_err();
    let msg = e.to_string();
    assert!(msg.contains("/x/f.toml") && msg.contains("bogus"), "{msg}");
}

#[test]
fn settings_missing_file_is_default() {
    let d = tempfile::tempdir().unwrap();
    let s = Settings::load(&d.path().join("none.toml")).unwrap();
    assert_eq!(s.client.parallel, None);
}

#[test]
fn hosts_parse_and_resolve_alias_before_id() {
    let t = format!("[home]\nendpoint_id = \"{}\"\nparallel = 4\naddress = [\"192.168.1.10:4000\"]\n[laptop]\nendpoint_id = \"{}\"\n", id(1), id(2));
    let h = Hosts::parse(&t, p()).unwrap();
    assert_eq!(h.get("home").unwrap().endpoint_id, id(1));
    assert_eq!(h.get("home").unwrap().parallel, Some(4));
    assert_eq!(h.get("home").unwrap().address.len(), 1);
    assert_eq!(h.resolve_peer("laptop").unwrap(), id(2));
    assert_eq!(h.resolve_peer(&id(3).to_string()).unwrap(), id(3));
    match h.resolve_peer("nope") {
        Err(ConfigError::UnknownAlias { alias, hosts }) => {
            assert_eq!(alias, "nope");
            assert_eq!(hosts, p());
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn hosts_entry_without_endpoint_id_is_error_with_line() {
    let e = Hosts::parse("[home]\nparallel = 4\n", p())
        .unwrap_err()
        .to_string();
    assert!(e.contains("/x/f.toml") && e.contains("endpoint_id"), "{e}");
}

#[test]
fn alias_validity() {
    assert!(valid_alias("home-server"));
    assert!(!valid_alias("") && !valid_alias("a:b") && !valid_alias("a/b"));
    assert!(!valid_alias(&id(1).to_string()));
}

#[test]
fn exports_parse_top_level_arrays_and_tables() {
    let t = "rw = [\"admin\"]\n[photos]\npath = \"/srv/photos\"\nrw = [\"laptop\"]\nro = [\"b2e1\"]\n[docs]\npath = \"/srv/docs\"\nread_only = true\nwarm_index = true\n";
    let x = Exports::parse(t, p()).unwrap();
    assert_eq!(x.rw, vec!["admin"]);
    assert_eq!(x.exports["photos"].rw, vec!["laptop"]);
    assert_eq!(x.exports["photos"].ro, vec!["b2e1"]);
    assert!(x.exports["docs"].read_only && x.exports["docs"].warm_index);
    assert_eq!(x.exports["docs"].path, Path::new("/srv/docs"));
}

#[test]
fn exports_reserved_table_name_is_error() {
    let e = Exports::parse("[all]\npath = \"/x\"\n", p()).unwrap_err();
    assert!(
        matches!(e, ConfigError::ReservedName(ref n) if n == "all"),
        "{e:?}"
    );
}

#[test]
fn exports_missing_path_and_unknown_key_are_errors_with_file() {
    assert!(Exports::parse("[a]\nread_only = true\n", p())
        .unwrap_err()
        .to_string()
        .contains("/x/f.toml"));
    assert!(Exports::parse("[a]\npath = \"/x\"\nbogus = 1\n", p())
        .unwrap_err()
        .to_string()
        .contains("bogus"));
}

#[test]
fn exports_missing_file_is_empty() {
    let d = tempfile::tempdir().unwrap();
    let x = Exports::load(&d.path().join("none.toml")).unwrap();
    assert!(x.exports.is_empty() && x.rw.is_empty());
}
