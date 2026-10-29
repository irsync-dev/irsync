use irsync::config::exports::Exports;
use irsync::config::file::Config;
use irsync::config::hosts::{valid_alias, Hosts};
use irsync::config::paths::Paths;
use irsync::config::settings::{RelayMode, Settings};
use irsync::config::ConfigError;
use std::path::{Path, PathBuf};
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
    let t = "identity = \"~/k\"\n[client]\nparallel = 8\nreconnect = \"1h\"\nrelay_mode = \"disabled\"\n[daemon]\nidentity = \"~/d\"\nrelay_mode = \"disabled\"\nlisten = [\"127.0.0.1:0\"]\n[daemon.discovery]\ndht = false\n";
    let s = Settings::parse(t, p()).unwrap();
    assert_eq!(s.identity, dirs::home_dir().unwrap().join("k"));
    assert_eq!(s.daemon.identity, dirs::home_dir().unwrap().join("d"));
    assert_eq!(s.client.parallel, Some(8));
    assert_eq!(s.client.relay_mode, Some(RelayMode::Disabled));
    assert_eq!(Settings::default().client.relay_mode, None);
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
    let paths = Paths::resolve(Some(&d.path().join("none.toml")));
    let s = Config::load(&paths).unwrap().settings;
    assert_eq!(s.client.parallel, None);
}

#[test]
fn hosts_parse_and_resolve_alias_before_id() {
    let t = format!("[hosts.home]\nendpoint_id = \"{}\"\nparallel = 4\naddress = [\"192.168.1.10:4000\"]\n[hosts.laptop]\nendpoint_id = \"{}\"\n", id(1), id(2));
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
    let e = Hosts::parse("[hosts.home]\nparallel = 4\n", p())
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("/x/f.toml:1:") && e.contains("endpoint_id"),
        "{e}"
    );
}

#[test]
fn alias_validity() {
    assert!(valid_alias("home-server"));
    assert!(!valid_alias("") && !valid_alias("a:b") && !valid_alias("a/b"));
    assert!(!valid_alias(&id(1).to_string()));
    assert!(!valid_alias("all") && !valid_alias("*"));
}

#[test]
fn hosts_old_layout_names_the_new_location() {
    let e = Hosts::parse("\n[home]\nendpoint_id = \"x\"\n", p())
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("unknown table \"home\"; hosts live under [hosts.home]") && e.contains(":2:"),
        "{e}"
    );
    let e = Hosts::parse("x = 1\n", p()).unwrap_err().to_string();
    assert!(e.contains("unknown key \"x\""), "{e}");
    assert!(Hosts::parse("[hosts]\n", p()).unwrap().entries.is_empty());
    // The first offender in the file is reported, not the first by name.
    let e = Hosts::parse(
        "[zzz]\nendpoint_id = \"x\"\n[aaa]\nendpoint_id = \"x\"\n",
        p(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        e.contains("unknown table \"zzz\"") && e.contains(":1:"),
        "{e}"
    );
}

#[test]
fn exports_parse_all_table_and_exports_tables() {
    let t = "[all]\nrw = [\"admin\"]\n[exports.photos]\npath = \"/srv/photos\"\nrw = [\"laptop\"]\nro = [\"b2e1\"]\n[exports.docs]\npath = \"/srv/docs\"\nread_only = true\nwarm_index = true\n";
    let x = Exports::parse(t, p()).unwrap();
    assert_eq!(x.rw, vec!["admin"]);
    assert_eq!(x.exports["photos"].rw, vec!["laptop"]);
    assert_eq!(x.exports["photos"].ro, vec!["b2e1"]);
    assert!(x.exports["docs"].read_only && x.exports["docs"].warm_index);
    assert_eq!(x.exports["docs"].path, Path::new("/srv/docs"));
    assert!(Exports::parse("[all]\n", p()).unwrap().rw.is_empty());
}

#[test]
fn exports_rw_and_ro_are_valid_export_names() {
    for name in ["rw", "ro"] {
        assert!(Exports::parse(&format!("[exports.{name}]\npath = \"/x\"\n"), p()).is_ok());
    }
}

#[test]
fn exports_all_is_the_only_reserved_name() {
    let e = Exports::parse("\n[exports.all]\npath = \"/x\"\n", p()).unwrap_err();
    assert!(matches!(e, ConfigError::File { .. }), "{e:?}");
    let e = e.to_string();
    assert!(
        e.contains("\"all\" is reserved: it is the word `grant add all WHO` uses for every export"),
        "{e}"
    );
    assert!(e.contains(":2:"), "{e}");
}

#[test]
fn exports_old_layout_names_the_new_location() {
    let e = Exports::parse("\n[photos]\npath = \"/x\"\n", p())
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("unknown table \"photos\"; exports live under [exports.photos]")
            && e.contains(":2:"),
        "{e}"
    );
    let e = Exports::parse("rw = [\"a\"]\n", p())
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("unknown key \"rw\"; grants for every export live under [all]")
            && e.contains(":1:"),
        "{e}"
    );
    let e = Exports::parse("[all]\npath = \"/x\"\n", p())
        .unwrap_err()
        .to_string();
    assert!(e.contains("[all]") && e.contains("path"), "{e}");
    let e = Exports::parse("x = 1\n", p()).unwrap_err().to_string();
    assert!(e.contains("unknown key \"x\""), "{e}");
    // The first offender in the file is reported, not the first by name.
    let e = Exports::parse("[zzz]\npath = \"/x\"\n[aaa]\npath = \"/x\"\n", p())
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("unknown table \"zzz\"") && e.contains(":1:"),
        "{e}"
    );
}

#[test]
fn exports_relative_path_is_error_with_file_and_line() {
    let e = Exports::parse(
        "[exports.a]\npath = \"/a\"\n[exports.b]\npath = \"srv/b\"\n",
        p(),
    )
    .unwrap_err();
    assert!(matches!(e, ConfigError::File { .. }), "{e:?}");
    let e = e.to_string();
    assert!(
        e.starts_with("/x/f.toml:3:") && e.contains("srv/b") && e.contains("absolute"),
        "{e}"
    );
    // `~` expands to an absolute path and is accepted.
    assert!(Exports::parse("[exports.c]\npath = \"~/c\"\n", p())
        .unwrap()
        .exports["c"]
        .path
        .is_absolute());
}

#[test]
fn exports_missing_path_and_unknown_key_are_errors_with_file() {
    assert!(Exports::parse("[exports.a]\nread_only = true\n", p())
        .unwrap_err()
        .to_string()
        .contains("/x/f.toml"));
    assert!(
        Exports::parse("[exports.a]\npath = \"/x\"\nbogus = 1\n", p())
            .unwrap_err()
            .to_string()
            .contains("bogus")
    );
}

#[test]
fn exports_missing_file_is_empty() {
    let d = tempfile::tempdir().unwrap();
    let paths = Paths::resolve(Some(&d.path().join("none.toml")));
    let x = Config::load(&paths).unwrap().exports;
    assert!(x.exports.is_empty() && x.rw.is_empty());
}

#[test]
fn syntax_error_carries_its_line() {
    let e = Hosts::parse("[hosts.a]\nendpoint_id = \n", p())
        .unwrap_err()
        .to_string();
    assert!(e.starts_with("/x/f.toml:2:"), "{e}");
}

#[test]
fn hosts_invalid_alias_in_file_is_error_with_line() {
    let t = format!(
        "[hosts.ok]\nendpoint_id = \"{}\"\n[hosts.\"a:b\"]\nendpoint_id = \"{}\"\n",
        id(1),
        id(2)
    );
    let e = Hosts::parse(&t, p()).unwrap_err().to_string();
    assert!(e.contains("/x/f.toml:3:") && e.contains("a:b"), "{e}");
    let t = format!("[hosts.{}]\nendpoint_id = \"{}\"\n", id(1), id(1));
    assert!(Hosts::parse(&t, p())
        .unwrap_err()
        .to_string()
        .contains("invalid host alias"));
}

#[test]
fn hosts_bad_endpoint_id_has_line() {
    let e = Hosts::parse("[hosts.a]\nendpoint_id = \"zzz\"\n", p())
        .unwrap_err()
        .to_string();
    assert!(e.contains("/x/f.toml:2:"), "{e}");
}

#[test]
fn exports_schema_errors_have_lines() {
    let e = Exports::parse(
        "[exports.a]\npath = \"/x\"\n[exports.b]\npath = \"/y\"\nbogus = 1\n",
        p(),
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("/x/f.toml:") && e.contains("bogus"), "{e}");
    let e = Exports::parse("[all]\nrw = 5\n", p())
        .unwrap_err()
        .to_string();
    assert!(e.contains("/x/f.toml:2:"), "{e}");
    let e = Exports::parse("x = 1\n", p()).unwrap_err().to_string();
    assert!(
        e.contains("/x/f.toml:1:") && e.contains("unknown key \"x\""),
        "{e}"
    );
}

fn merged(dir: &Path, text: &str) -> Result<Config, ConfigError> {
    std::fs::write(dir.join("irsync.toml"), text).unwrap();
    Config::load(&Paths::resolve(Some(&dir.join("irsync.toml"))))
}

#[test]
fn merged_file_holds_every_table() {
    let d = tempfile::tempdir().unwrap();
    let c = merged(
        d.path(),
        &format!(
            "state_dir = \"/var/lib/irsync\"\n[client]\nparallel = 4\n[daemon]\nlisten = [\"127.0.0.1:0\"]\n\
             [hosts.home]\nendpoint_id = \"{}\"\n[all]\nro = [\"home\"]\n[exports.photos]\npath = \"/srv/photos\"\nrw = [\"home\"]\n",
            id(1)
        ),
    )
    .unwrap();
    assert_eq!(c.settings.state_dir, PathBuf::from("/var/lib/irsync"));
    assert_eq!(c.settings.client.parallel, Some(4));
    assert_eq!(c.hosts.get("home").unwrap().endpoint_id, id(1));
    assert_eq!(c.exports.ro, ["home"]);
    assert_eq!(c.exports.exports["photos"].rw, ["home"]);
    assert_eq!(
        (c.hosts.path.clone(), c.exports.path.clone()),
        (d.path().join("irsync.toml"), d.path().join("irsync.toml"))
    );
    assert!(c.split_hosts.is_none() && c.split_exports.is_none());
}

#[test]
fn merged_shape_errors_keep_the_old_layout_hints() {
    let d = tempfile::tempdir().unwrap();
    for (text, needle) in [
        (
            "[home]\nendpoint_id = \"x\"\n",
            "unknown table \"home\"; hosts live under [hosts.home]",
        ),
        (
            "[photos]\npath = \"/x\"\n",
            "unknown table \"photos\"; exports live under [exports.photos]",
        ),
        (
            "rw = [\"a\"]\n",
            "unknown key \"rw\"; grants for every export live under [all]",
        ),
        (
            "[nothing]\nx = 1\n",
            "hosts live under [hosts.nothing], exports under [exports.nothing]",
        ),
        ("[daemon]\nnope = 1\n", "nope"),
        ("[exports.all]\npath = \"/x\"\n", "\"all\" is reserved"),
        (
            "[hosts.all]\nendpoint_id = \"x\"\n",
            "invalid host alias \"all\"",
        ),
    ] {
        let e = merged(d.path(), text).unwrap_err().to_string();
        assert!(
            e.contains(needle) && e.contains("irsync.toml"),
            "{text:?}: {e}"
        );
    }
}

#[test]
fn split_files_beside_the_config_are_read() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("hosts.toml"),
        format!("[hosts.home]\nendpoint_id = \"{}\"\n", id(1)),
    )
    .unwrap();
    std::fs::write(
        d.path().join("exports.toml"),
        "[exports.p]\npath = \"/p\"\n",
    )
    .unwrap();
    let c = merged(d.path(), "[client]\nparallel = 2\n").unwrap();
    assert_eq!(c.hosts.get("home").unwrap().endpoint_id, id(1));
    assert!(c.exports.exports.contains_key("p"));
    assert_eq!(c.hosts.path, d.path().join("hosts.toml"));
    assert_eq!(c.exports.path, d.path().join("exports.toml"));
    assert_eq!(c.split_hosts, Some(d.path().join("hosts.toml")));
    assert_eq!(c.split_exports, Some(d.path().join("exports.toml")));
    // A missing config file with split files present still works.
    std::fs::remove_file(d.path().join("irsync.toml")).unwrap();
    let c = Config::load(&Paths::resolve(Some(&d.path().join("irsync.toml")))).unwrap();
    assert!(c.hosts.get("home").is_some());
}

#[test]
fn one_home_rule_names_both_files() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("hosts.toml"), "").unwrap();
    let e = merged(d.path(), "[hosts]\n").unwrap_err().to_string();
    assert!(
        e.contains("hosts are in")
            && e.contains("hosts.toml")
            && e.contains("remove [hosts] from")
            && e.contains("irsync.toml"),
        "{e}"
    );
    std::fs::remove_file(d.path().join("hosts.toml")).unwrap();
    std::fs::write(d.path().join("exports.toml"), "").unwrap();
    for text in ["[all]\nrw = []\n", "[exports.p]\npath = \"/p\"\n"] {
        let e = merged(d.path(), text).unwrap_err().to_string();
        assert!(
            e.contains("exports.toml") && e.contains("irsync.toml"),
            "{text:?}: {e}"
        );
    }
}

#[test]
fn a_split_file_accepts_only_its_own_tables() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("hosts.toml"), "[client]\nparallel = 1\n").unwrap();
    let e = merged(d.path(), "").unwrap_err().to_string();
    assert!(
        e.contains("hosts.toml") && e.contains("unknown table \"client\""),
        "{e}"
    );
}

#[test]
fn missing_everything_is_the_defaults() {
    let d = tempfile::tempdir().unwrap();
    let c = Config::load(&Paths::resolve(Some(&d.path().join("irsync.toml")))).unwrap();
    assert_eq!(c.settings, Settings::default());
    assert!(c.hosts.entries.is_empty() && c.exports.exports.is_empty());
}

#[test]
fn export_names_are_one_path_segment() {
    // An export is reached as the first segment of HOST:PATH, so its name
    // can hold neither a slash nor nothing at all.
    for (name, bad) in [("a/b", "/"), ("", "empty")] {
        let e = Exports::parse(&format!("[exports.{name:?}]\npath = \"/x\"\n"), p())
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("is not a valid export name") && e.contains(bad),
            "{name:?}: {e}"
        );
    }
    assert!(Exports::parse("[exports.\"a:b\"]\npath = \"/x\"\n", p()).is_ok());
}
