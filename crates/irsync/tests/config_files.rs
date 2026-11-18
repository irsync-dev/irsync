use irsync::config::exports::Exports;
use irsync::config::file::{first_sighting, ignored_split_files, Config};
use irsync::config::hosts::{short_host_name, valid_alias, Hosts};
use irsync::config::paths::Paths;
use irsync::config::settings::{RelayMode, Settings};
use irsync::config::ConfigError;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn id(n: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
}
fn parse_hosts(text: &str, path: &Path) -> Result<Hosts, ConfigError> {
    Config::parse(text, path).map(|c| c.hosts)
}
fn parse_exports(text: &str, path: &Path) -> Result<Exports, ConfigError> {
    Config::parse(text, path).map(|c| c.exports)
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
    let t = "identity = \"~/k\"\n[client]\nparallel = 8\nreconnect = \"1h\"\ndial_retries = 2\nrelay_mode = \"disabled\"\n[daemon]\nidentity = \"~/d\"\nrelay_mode = \"disabled\"\nlisten = [\"127.0.0.1:0\"]\n[daemon.discovery]\ndht = false\n";
    let s = Settings::parse(t, p()).unwrap();
    assert_eq!(s.identity, dirs::home_dir().unwrap().join("k"));
    assert_eq!(s.daemon.identity, dirs::home_dir().unwrap().join("d"));
    assert_eq!(s.client.parallel, Some(8));
    assert_eq!(s.client.relay_mode, Some(RelayMode::Disabled));
    assert_eq!(Settings::default().client.relay_mode, None);
    assert_eq!(s.client.reconnect, Some(Duration::from_secs(3600)));
    assert_eq!(s.client.dial_retries, Some(2));
    assert_eq!(Settings::default().client.dial_retries, None);
    assert_eq!(s.daemon.relay_mode, RelayMode::Disabled);
    assert_eq!(s.daemon.listen.len(), 1);
    assert!(!s.daemon.discovery.dht && s.daemon.discovery.dns);
}

#[test]
fn congestion_is_bbr_unless_set_to_cubic() {
    use irsync::config::settings::Congestion;
    let d = Settings::default();
    assert_eq!(
        (d.client.congestion, d.daemon.congestion),
        (None, Congestion::Bbr)
    );
    let s = Settings::parse(
        "[client]\ncongestion = \"cubic\"\n[daemon]\ncongestion = \"cubic\"\n",
        p(),
    )
    .unwrap();
    assert_eq!(s.client.congestion, Some(Congestion::Cubic));
    assert_eq!(s.daemon.congestion, Congestion::Cubic);
    let e = Settings::parse("[client]\ncongestion = \"reno\"\n", p()).unwrap_err();
    assert!(e.to_string().contains("reno"), "{e}");
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
    let h = parse_hosts(&t, p()).unwrap();
    assert_eq!(h.get("home").unwrap().endpoint_id, Some(id(1)));
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
    let e = parse_hosts("[hosts.home]\nparallel = 4\n", p())
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
fn short_host_names() {
    assert_eq!(
        short_host_name("myserver.example.com\n").as_deref(),
        Some("myserver")
    );
    assert_eq!(short_host_name("my server").as_deref(), Some("my server"));
    let endpoint = id(1).to_string();
    for bad in [
        "",
        "  ",
        ".local",
        "localhost",
        "localhost.localdomain",
        "LocalHost",
        "all",
        "*",
        "a/b",
        "a:b",
        "192.168.1.5",
        "10",
        &endpoint,
    ] {
        assert_eq!(short_host_name(bad), None, "{bad:?}");
    }
}

#[test]
fn host_by_ssh_destination() {
    let host = |alias: &str, n: u8, ssh: &str| {
        format!(
            "[hosts.\"{alias}\"]\nendpoint_id = \"{}\"\nssh = \"{ssh}\"\n",
            id(n)
        )
    };
    let text = [
        host("myserver", 1, "192.168.1.5"),
        host("u@other", 2, "u@other"),
        host("other", 2, "u@other"),
        host("a", 3, "10.0.0.1"),
        host("b", 4, "10.0.0.1"),
        format!("[hosts.plain]\nendpoint_id = \"{}\"\n", id(5)),
    ]
    .concat();
    let hosts = parse_hosts(&text, p()).unwrap();
    assert_eq!(hosts.by_ssh("192.168.1.5"), Ok(Some("myserver")));
    assert_eq!(hosts.by_ssh("192.168.1.50"), Ok(None));
    assert_eq!(hosts.by_ssh("plain"), Ok(None));
    // One daemon under two names: the first in order.
    assert_eq!(hosts.by_ssh("u@other"), Ok(Some("other")));
    // Two daemons: no way to tell which one is meant.
    assert_eq!(
        hosts.by_ssh("10.0.0.1"),
        Err(vec!["a".to_string(), "b".to_string()])
    );
}

/// Hosts saved as clients only have no daemon id to compare: two of them
/// with one `ssh` value are two machines, not one daemon under two names.
#[test]
fn client_only_hosts_with_one_ssh_destination_are_told_apart() {
    let text = format!(
        "[hosts.a]\nclient_id = \"{}\"\nssh = \"box\"\n\
         [hosts.b]\nclient_id = \"{}\"\nssh = \"box\"\n\
         [hosts.c]\nclient_id = \"{}\"\nssh = \"alone\"\n",
        id(1),
        id(2),
        id(3)
    );
    let hosts = parse_hosts(&text, p()).unwrap();
    assert_eq!(
        hosts.by_ssh("box"),
        Err(vec!["a".to_string(), "b".to_string()])
    );
    assert_eq!(hosts.by_ssh("alone"), Ok(Some("c")));
}

/// `ssh_aliases` lists other destinations typed for a host: they find it the
/// way its `ssh` value does.
#[test]
fn host_by_another_ssh_destination() {
    let text = format!(
        "[hosts.myserver]\nendpoint_id = \"{}\"\nssh = \"mike@myserver\"\n\
         ssh_aliases = [\"192.168.1.5\", \"u@192.168.1.5\"]\n\
         [hosts.other]\nendpoint_id = \"{}\"\nssh = \"u@192.168.1.5\"\n",
        id(1),
        id(2)
    );
    let hosts = parse_hosts(&text, p()).unwrap();
    assert_eq!(
        hosts.get("myserver").unwrap().ssh_aliases,
        ["192.168.1.5", "u@192.168.1.5"]
    );
    assert!(hosts.get("other").unwrap().ssh_aliases.is_empty());
    assert_eq!(hosts.by_ssh("192.168.1.5"), Ok(Some("myserver")));
    assert_eq!(hosts.by_ssh("mike@myserver"), Ok(Some("myserver")));
    assert_eq!(
        hosts.by_ssh("u@192.168.1.5"),
        Err(vec!["myserver".to_string(), "other".to_string()])
    );
}

/// One entry stands for one machine: `endpoint_id` is its daemon, the id
/// to dial; `client_id` is the machine as a client, the id grants are for.
#[test]
fn host_entry_with_a_client_id() {
    let text = format!(
        "[hosts.laptop]\nendpoint_id = \"{d}\"\nclient_id = \"{c}\"\n\
         [hosts.phone]\nclient_id = \"{p}\"\n\
         [hosts.nas]\nendpoint_id = \"{n}\"\n",
        d = id(1),
        c = id(2),
        p = id(3),
        n = id(4)
    );
    let hosts = parse_hosts(&text, p()).unwrap();
    let laptop = hosts.get("laptop").unwrap();
    assert_eq!(laptop.endpoint_id, Some(id(1)));
    assert_eq!(laptop.client_id, Some(id(2)));
    assert_eq!(laptop.peer_id(), id(2));
    let phone = hosts.get("phone").unwrap();
    assert_eq!((phone.endpoint_id, phone.peer_id()), (None, id(3)));
    // Without a client id, the one id is the peer, as it always was.
    assert_eq!(hosts.get("nas").unwrap().peer_id(), id(4));
    // Grants go by the peer id: the daemon of a machine is not its client.
    assert_eq!(hosts.resolve_peer("laptop").unwrap(), id(2));
    assert_eq!(hosts.alias_of(&id(2)), Some("laptop"));
    assert_eq!(hosts.alias_of(&id(1)), None);
    assert_eq!(hosts.alias_of(&id(3)), Some("phone"));
    assert_eq!(hosts.alias_of(&id(4)), Some("nas"));
    // An entry needs one of the two; a bad client id names its line.
    let e = parse_hosts("[hosts.x]\nssh = \"x\"\n", p())
        .unwrap_err()
        .to_string();
    assert!(e.contains("endpoint_id") && e.contains("client_id"), "{e}");
    let e = parse_hosts("[hosts.x]\nclient_id = \"nope\"\n", p())
        .unwrap_err()
        .to_string();
    assert!(e.contains(":2:") && e.contains("\"x\""), "{e}");
}

#[test]
fn hosts_old_layout_names_the_new_location() {
    let e = parse_hosts("\n[home]\nendpoint_id = \"x\"\n", p())
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("unknown table \"home\"; hosts live under [hosts.home]") && e.contains(":2:"),
        "{e}"
    );
    let e = parse_hosts("x = 1\n", p()).unwrap_err().to_string();
    assert!(e.contains("unknown key \"x\""), "{e}");
    assert!(parse_hosts("[hosts]\n", p()).unwrap().entries.is_empty());
    // The first offender in the file is reported, not the first by name.
    let e = parse_hosts(
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
    let t = "[all]\nrw = [\"admin\"]\n[exports.photos]\npath = \"/srv/photos\"\nrw = [\"laptop\"]\nro = [\"b2e1\"]\n[exports.docs]\npath = \"/srv/docs\"\nread_only = true\nwarm_index = true\nfsync = true\n";
    let x = parse_exports(t, p()).unwrap();
    assert_eq!(x.rw, vec!["admin"]);
    assert_eq!(x.exports["photos"].rw, vec!["laptop"]);
    assert_eq!(x.exports["photos"].ro, vec!["b2e1"]);
    assert!(x.exports["docs"].read_only && x.exports["docs"].warm_index);
    assert!(x.exports["docs"].fsync && !x.exports["photos"].fsync);
    assert_eq!(x.exports["docs"].path, Path::new("/srv/docs"));
    assert!(parse_exports("[all]\n", p()).unwrap().rw.is_empty());
}

#[test]
fn exports_rw_and_ro_are_valid_export_names() {
    for name in ["rw", "ro"] {
        assert!(parse_exports(&format!("[exports.{name}]\npath = \"/x\"\n"), p()).is_ok());
    }
}

#[test]
fn exports_all_is_the_only_reserved_name() {
    let e = parse_exports("\n[exports.all]\npath = \"/x\"\n", p()).unwrap_err();
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
    let e = parse_exports("\n[photos]\npath = \"/x\"\n", p())
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("unknown table \"photos\"; exports live under [exports.photos]")
            && e.contains(":2:"),
        "{e}"
    );
    let e = parse_exports("rw = [\"a\"]\n", p())
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("unknown key \"rw\"; grants for every export live under [all]")
            && e.contains(":1:"),
        "{e}"
    );
    let e = parse_exports("[all]\npath = \"/x\"\n", p())
        .unwrap_err()
        .to_string();
    assert!(e.contains(":2:") && e.contains("path"), "{e}");
    let e = parse_exports("x = 1\n", p()).unwrap_err().to_string();
    assert!(e.contains("unknown key \"x\""), "{e}");
    // The first offender in the file is reported, not the first by name.
    let e = parse_exports("[zzz]\npath = \"/x\"\n[aaa]\npath = \"/x\"\n", p())
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("unknown table \"zzz\"") && e.contains(":1:"),
        "{e}"
    );
}

#[test]
fn exports_relative_path_is_error_with_file_and_line() {
    let e = parse_exports(
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
    assert!(parse_exports("[exports.c]\npath = \"~/c\"\n", p())
        .unwrap()
        .exports["c"]
        .path
        .is_absolute());
}

#[test]
fn exports_missing_path_and_unknown_key_are_errors_with_file() {
    assert!(parse_exports("[exports.a]\nread_only = true\n", p())
        .unwrap_err()
        .to_string()
        .contains("/x/f.toml"));
    assert!(
        parse_exports("[exports.a]\npath = \"/x\"\nbogus = 1\n", p())
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
    let e = parse_hosts("[hosts.a]\nendpoint_id = \n", p())
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
    let e = parse_hosts(&t, p()).unwrap_err().to_string();
    assert!(e.contains("/x/f.toml:3:") && e.contains("a:b"), "{e}");
    let t = format!("[hosts.{}]\nendpoint_id = \"{}\"\n", id(1), id(1));
    assert!(parse_hosts(&t, p())
        .unwrap_err()
        .to_string()
        .contains("invalid host alias"));
}

#[test]
fn hosts_bad_endpoint_id_has_line() {
    let e = parse_hosts("[hosts.a]\nendpoint_id = \"zzz\"\n", p())
        .unwrap_err()
        .to_string();
    assert!(e.contains("/x/f.toml:2:"), "{e}");
}

#[test]
fn exports_schema_errors_have_lines() {
    let e = parse_exports(
        "[exports.a]\npath = \"/x\"\n[exports.b]\npath = \"/y\"\nbogus = 1\n",
        p(),
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("/x/f.toml:") && e.contains("bogus"), "{e}");
    let e = parse_exports("[all]\nrw = 5\n", p())
        .unwrap_err()
        .to_string();
    assert!(e.contains("/x/f.toml:2:"), "{e}");
    let e = parse_exports("x = 1\n", p()).unwrap_err().to_string();
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
    assert_eq!(c.hosts.get("home").unwrap().endpoint_id, Some(id(1)));
    assert_eq!(c.exports.ro, ["home"]);
    assert_eq!(c.exports.exports["photos"].rw, ["home"]);
    assert_eq!(
        (c.hosts.path.clone(), c.exports.path.clone()),
        (d.path().join("irsync.toml"), d.path().join("irsync.toml"))
    );
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
fn split_files_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("irsync.toml");
    std::fs::write(&config, "").unwrap();
    std::fs::write(
        dir.path().join("hosts.toml"),
        format!("[hosts.box]\nendpoint_id = \"{}\"\n", id(1)),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("exports.toml"),
        "[exports.p]\npath = \"/p\"\n",
    )
    .unwrap();
    let c = Config::load(&Paths::resolve(Some(&config))).unwrap();
    assert!(c.hosts.entries.is_empty());
    assert!(c.exports.exports.is_empty());
    assert_eq!(
        ignored_split_files(&config),
        vec![
            dir.path().join("hosts.toml"),
            dir.path().join("exports.toml")
        ]
    );
    // A split file that would not parse is ignored all the same.
    std::fs::write(dir.path().join("hosts.toml"), "not toml [").unwrap();
    assert!(Config::load(&Paths::resolve(Some(&config))).is_ok());
}

#[test]
fn no_split_files_nothing_ignored() {
    let dir = tempfile::tempdir().unwrap();
    assert!(ignored_split_files(&dir.path().join("irsync.toml")).is_empty());
}

#[test]
fn a_split_file_is_reported_once_per_process() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("hosts.toml");
    let b = dir.path().join("exports.toml");
    assert!(first_sighting(&a));
    assert!(!first_sighting(&a));
    assert!(first_sighting(&b));
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
        let e = parse_exports(&format!("[exports.{name:?}]\npath = \"/x\"\n"), p())
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("is not a valid export name") && e.contains(bad),
            "{name:?}: {e}"
        );
    }
    assert!(parse_exports("[exports.\"a:b\"]\npath = \"/x\"\n", p()).is_ok());
}

#[test]
fn telemetry_table_parses() {
    use irsync::config::settings::OtlpProtocol;
    let s = Settings::parse(
        "[telemetry]\notlp_endpoint = \"http://c:4318\"\notlp_protocol = \"http\"\n",
        p(),
    )
    .unwrap();
    assert_eq!(
        s.telemetry.otlp_endpoint.as_ref().map(|u| u.as_str()),
        Some("http://c:4318/")
    );
    assert_eq!(s.telemetry.otlp_protocol, Some(OtlpProtocol::Http));
    assert_eq!(Settings::default().telemetry.otlp_endpoint, None);
    let e = Settings::parse("[telemetry]\nbogus = 1\n", p())
        .unwrap_err()
        .to_string();
    assert!(e.contains("/x/f.toml") && e.contains("bogus"), "{e}");
}

#[test]
fn daemon_debug_addr_bare_port() {
    let s = Settings::parse("[daemon]\ndebug_addr = \"9464\"\n", p()).unwrap();
    assert_eq!(s.daemon.debug_addr, Some("127.0.0.1:9464".parse().unwrap()));
    let s = Settings::parse("[daemon]\ndebug_addr = \"[::1]:9\"\n", p()).unwrap();
    assert_eq!(s.daemon.debug_addr, Some("[::1]:9".parse().unwrap()));
    assert_eq!(Settings::default().daemon.debug_addr, None);
    let e = Settings::parse("[daemon]\ndebug_addr = \"nope\"\n", p())
        .unwrap_err()
        .to_string();
    assert!(e.contains("nope"), "{e}");
}

#[test]
fn immediate_table_parses() {
    let a = iroh::SecretKey::from_bytes(&[1; 32]).public();
    let x = parse_exports(&format!("[immediate]\nrw = [\"{a}\"]\n"), Path::new("e")).unwrap();
    assert_eq!(x.immediate_rw, vec![a.to_string()]);
    let e = parse_exports(&format!("[immediate]\nro = [\"{a}\"]\n"), Path::new("e"))
        .unwrap_err()
        .to_string();
    assert!(e.contains("immediate"), "{e}");
    let e = Config::parse(&format!("[immediate]\nro = [\"{a}\"]\n"), Path::new("c"))
        .unwrap_err()
        .to_string();
    assert!(e.contains("immediate"), "{e}");
}

#[test]
fn daemon_immediate_defaults_on() {
    let c = Config::parse("", Path::new("c")).unwrap();
    assert!(c.settings.daemon.immediate);
    let c = Config::parse("[daemon]\nimmediate = false\n", Path::new("c")).unwrap();
    assert!(!c.settings.daemon.immediate);
}

#[test]
fn daemon_allow_root_defaults_off() {
    let c = Config::parse("", Path::new("c")).unwrap();
    assert!(!c.settings.daemon.allow_root);
    let c = Config::parse("[daemon]\nallow_root = true\n", Path::new("c")).unwrap();
    assert!(c.settings.daemon.allow_root);
    let e = Config::parse("[daemon]\nallow_root = \"yes\"\n", Path::new("c"))
        .unwrap_err()
        .to_string();
    // Not a silent `false`: the error points at the line, as for every key.
    assert!(e.contains("c:2") && e.contains("expected a boolean"), "{e}");
}

#[test]
fn export_named_immediate_is_reserved() {
    assert!(parse_exports("[exports.immediate]\npath = \"/p\"\n", Path::new("e")).is_err());
}
