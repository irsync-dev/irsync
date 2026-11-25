use irsync::config::clients::Clients;
use irsync::config::exports::Exports;
use irsync::config::file::Config;
use irsync::config::names::{short_host_name, valid_alias};
use irsync::config::paths::Paths;
use irsync::config::servers::Servers;
use irsync::config::settings::{Relay, Settings};
use irsync::config::ConfigError;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn id(n: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
}
fn parse_servers(text: &str, path: &Path) -> Result<Servers, ConfigError> {
    Config::parse(text, path).map(|c| c.servers)
}
fn parse_clients(text: &str, path: &Path) -> Result<Clients, ConfigError> {
    Config::parse(text, path).map(|c| c.clients)
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
    assert_eq!(s.defaults.parallel, None);
}

#[test]
fn settings_tables_parse_and_expand_tilde() {
    let t = "[defaults]\nidentity = \"~/k\"\nparallel = 8\nreconnect = \"1h\"\ndial_retries = 2\nrelay = \"disabled\"\n[daemon]\nidentity = \"~/d\"\nrelay = \"disabled\"\nlisten = [\"127.0.0.1:0\"]\ndht = false\n";
    let s = Settings::parse(t, p()).unwrap();
    assert_eq!(s.identity, dirs::home_dir().unwrap().join("k"));
    assert_eq!(s.daemon.identity, dirs::home_dir().unwrap().join("d"));
    assert_eq!(s.defaults.parallel, Some(8));
    assert_eq!(s.defaults.relay, Some(Relay::Disabled));
    assert_eq!(Settings::default().defaults.relay, None);
    assert_eq!(s.defaults.reconnect, Some(Duration::from_secs(3600)));
    assert_eq!(s.defaults.dial_retries, Some(2));
    assert_eq!(Settings::default().defaults.dial_retries, None);
    assert_eq!(s.daemon.relay, Relay::Disabled);
    assert_eq!(Settings::default().daemon.relay, Relay::Default);
    assert_eq!(s.daemon.listen.len(), 1);
    assert!(!s.daemon.discovery.dht && s.daemon.discovery.dns);
}

#[test]
fn congestion_is_bbr_unless_set_to_cubic() {
    use irsync::config::settings::Congestion;
    let d = Settings::default();
    assert_eq!(
        (d.defaults.congestion, d.daemon.congestion),
        (None, Congestion::Bbr)
    );
    let s = Settings::parse(
        "[defaults]\ncongestion = \"cubic\"\n[daemon]\ncongestion = \"cubic\"\n",
        p(),
    )
    .unwrap();
    assert_eq!(s.defaults.congestion, Some(Congestion::Cubic));
    assert_eq!(s.daemon.congestion, Congestion::Cubic);
    let e = Settings::parse("[defaults]\ncongestion = \"reno\"\n", p()).unwrap_err();
    assert!(e.to_string().contains("reno"), "{e}");
}

#[test]
fn pq_key_exchange_is_prefer_unless_set_to_require() {
    use irsync::config::settings::PqKeyExchange;
    let d = Settings::default();
    assert_eq!(
        (d.defaults.pq_key_exchange, d.daemon.pq_key_exchange),
        (None, PqKeyExchange::Prefer)
    );
    let s = Settings::parse(
        "[defaults]\npq_key_exchange = \"require\"\n[daemon]\npq_key_exchange = \"require\"\n",
        p(),
    )
    .unwrap();
    assert_eq!(s.defaults.pq_key_exchange, Some(PqKeyExchange::Require));
    assert_eq!(s.daemon.pq_key_exchange, PqKeyExchange::Require);
    let s = Settings::parse("[defaults]\npq_key_exchange = \"prefer\"\n", p()).unwrap();
    assert_eq!(s.defaults.pq_key_exchange, Some(PqKeyExchange::Prefer));
    let e = Settings::parse("[daemon]\npq_key_exchange = \"only\"\n", p()).unwrap_err();
    assert!(e.to_string().contains("only"), "{e}");
}

#[test]
fn pq_key_exchange_of_a_server() {
    use irsync::config::settings::PqKeyExchange;
    let t = format!(
        "[defaults]\npq_key_exchange = \"require\"\n[servers.nas]\nendpoint_id = \"{}\"\npq_key_exchange = \"prefer\"\n[servers.home]\nendpoint_id = \"{}\"\n",
        id(1),
        id(2)
    );
    let c = Config::parse(&t, p()).unwrap();
    assert_eq!(
        c.settings.defaults.pq_key_exchange,
        Some(PqKeyExchange::Require)
    );
    assert_eq!(
        c.servers.get("nas").unwrap().pq_key_exchange,
        Some(PqKeyExchange::Prefer)
    );
    assert_eq!(c.servers.get("home").unwrap().pq_key_exchange, None);
    let e = parse_servers(
        &format!(
            "[servers.nas]\nendpoint_id = \"{}\"\npq_key_exchange = \"only\"\n",
            id(1)
        ),
        p(),
    )
    .unwrap_err();
    assert!(e.to_string().contains("only"), "{e}");
}

#[test]
fn settings_unknown_key_names_file_and_key() {
    let e = Settings::parse("[defaults]\nbogus = 1\n", p()).unwrap_err();
    let msg = e.to_string();
    assert!(msg.contains("/x/f.toml") && msg.contains("bogus"), "{msg}");
}

#[test]
fn settings_missing_file_is_default() {
    let d = tempfile::tempdir().unwrap();
    let paths = Paths::resolve(Some(&d.path().join("none.toml")));
    let s = Config::load(&paths).unwrap().settings;
    assert_eq!(s.defaults.parallel, None);
}

#[test]
fn servers_parse_and_clients_resolve_a_name_before_an_id() {
    let t = format!("[servers.home]\nendpoint_id = \"{}\"\nparallel = 4\naddress = [\"192.168.1.10:4000\"]\n[clients.laptop]\nclient_id = \"{}\"\n", id(1), id(2));
    let c = Config::parse(&t, p()).unwrap();
    let h = &c.servers;
    assert_eq!(h.get("home").unwrap().endpoint_id, id(1));
    assert_eq!(h.get("home").unwrap().parallel, Some(4));
    assert_eq!(h.get("home").unwrap().address.len(), 1);
    assert_eq!(c.clients.resolve_peer("laptop").unwrap(), id(2));
    assert_eq!(c.clients.resolve_peer(&id(3).to_string()).unwrap(), id(3));
    // Grants name clients: a server's name is nobody.
    for nobody in ["nope", "home"] {
        match c.clients.resolve_peer(nobody) {
            Err(ConfigError::UnknownClient { name, path }) => {
                assert_eq!(name, nobody);
                assert_eq!(path, p());
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn server_without_endpoint_id_is_error_with_line() {
    let e = parse_servers("[servers.home]\nparallel = 4\n", p())
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
fn server_by_ssh_destination() {
    let host = |alias: &str, n: u8, ssh: &str| {
        format!(
            "[servers.\"{alias}\"]\nendpoint_id = \"{}\"\nssh = \"{ssh}\"\n",
            id(n)
        )
    };
    let text = [
        host("myserver", 1, "192.168.1.5"),
        host("u@other", 2, "u@other"),
        host("other", 2, "u@other"),
        host("a", 3, "10.0.0.1"),
        host("b", 4, "10.0.0.1"),
        format!("[servers.plain]\nendpoint_id = \"{}\"\n", id(5)),
    ]
    .concat();
    let hosts = parse_servers(&text, p()).unwrap();
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

/// `ssh_aliases` lists other destinations typed for a host: they find it the
/// way its `ssh` value does.
#[test]
fn server_by_another_ssh_destination() {
    let text = format!(
        "[servers.myserver]\nendpoint_id = \"{}\"\nssh = \"mike@myserver\"\n\
         ssh_aliases = [\"192.168.1.5\", \"u@192.168.1.5\"]\n\
         [servers.other]\nendpoint_id = \"{}\"\nssh = \"u@192.168.1.5\"\n",
        id(1),
        id(2)
    );
    let hosts = parse_servers(&text, p()).unwrap();
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

/// A machine can be both: a server, by the id of its daemon, and a
/// client, by the id grants naming it are for. Each table keeps its own.
#[test]
fn a_machine_as_a_server_and_a_client() {
    let text = format!(
        "[servers.laptop]\nendpoint_id = \"{d}\"\n\
         [servers.nas]\nendpoint_id = \"{n}\"\n\
         [clients.laptop]\nclient_id = \"{c}\"\n\
         [clients.phone]\nclient_id = \"{p}\"\n",
        d = id(1),
        c = id(2),
        p = id(3),
        n = id(4)
    );
    let Config {
        servers, clients, ..
    } = Config::parse(&text, p()).unwrap();
    assert_eq!(servers.get("laptop").unwrap().endpoint_id, id(1));
    assert_eq!(clients.get("laptop").unwrap().client_id, id(2));
    // Grants go by the client id: the daemon of a machine is not its client.
    assert_eq!(clients.resolve_peer("laptop").unwrap(), id(2));
    assert_eq!(clients.alias_of(&id(2)), Some("laptop"));
    assert_eq!(clients.alias_of(&id(1)), None);
    assert_eq!(clients.alias_of(&id(3)), Some("phone"));
    assert_eq!(clients.alias_of(&id(4)), None);
    assert!(servers.get("phone").is_none());
    // A bad client id names its line.
    let e = parse_clients("[clients.x]\nclient_id = \"nope\"\n", p())
        .unwrap_err()
        .to_string();
    assert!(e.contains(":2:") && e.contains("\"x\""), "{e}");
}

#[test]
fn unknown_top_level_keys_and_tables_are_refused_with_their_line() {
    for (text, line, name) in [
        ("\n[home]\nendpoint_id = \"x\"\n", 2, "home"),
        ("x = 1\n", 1, "x"),
        ("[hosts]\n", 1, "hosts"),
        (
            "[exports.p]\npath = \"/p\"\n[all]\nrw = [\"a\"]\n",
            3,
            "all",
        ),
        // The first offender in the file is reported, not the first by name.
        ("[zzz]\npath = \"/x\"\n[aaa]\npath = \"/x\"\n", 1, "zzz"),
    ] {
        let e = Config::parse(text, p()).unwrap_err().to_string();
        assert_eq!(
            e,
            format!("/x/f.toml:{line}: unknown key {name:?}"),
            "{text:?}"
        );
    }
}

/// Only the current `config_version` is read; a file without it is read
/// as that version.
#[test]
fn only_the_current_config_version_is_read() {
    assert!(Config::parse("config_version = 2\n", p()).is_ok());
    assert!(Config::parse("", p()).is_ok());
    for (v, want) in [
        ("1", "config_version: expected 2, found 1"),
        ("0", "config_version: expected 2, found 0"),
        ("\"2\"", "config_version: expected 2, found string"),
        (
            "3",
            "config_version 3: this file was written by a newer irsync, which reads config_version 2; upgrade irsync",
        ),
    ] {
        let e = Config::parse(&format!("\nconfig_version = {v}\n"), p())
            .unwrap_err()
            .to_string();
        assert_eq!(e, format!("/x/f.toml:2: {want}"), "{v}");
    }
}

#[test]
fn exports_parse_exports_tables() {
    let t = "[exports.photos]\npath = \"/srv/photos\"\nrw = [\"laptop\"]\nro = [\"b2e1\"]\n[exports.docs]\npath = \"/srv/docs\"\nread_only = true\nindex_at_start = true\nfsync = true\n";
    let x = parse_exports(t, p()).unwrap();
    assert_eq!(x.exports["photos"].rw, vec!["laptop"]);
    assert_eq!(x.exports["photos"].ro, vec!["b2e1"]);
    assert!(x.exports["docs"].read_only && x.exports["docs"].index_at_start);
    assert!(x.exports["docs"].fsync && !x.exports["photos"].fsync);
    assert_eq!(x.exports["docs"].path, Path::new("/srv/docs"));
}

#[test]
fn exports_rw_and_ro_are_valid_export_names() {
    for name in ["rw", "ro"] {
        assert!(parse_exports(&format!("[exports.{name}]\npath = \"/x\"\n"), p()).is_ok());
    }
}

#[test]
fn exports_all_is_reserved() {
    let e = parse_exports("\n[exports.all]\npath = \"/x\"\n", p()).unwrap_err();
    assert!(matches!(e, ConfigError::File { .. }), "{e:?}");
    let e = e.to_string();
    assert!(
        e.contains("\"all\" is reserved: it is the word `grant remove all WHO` and `index --export all` use for every export"),
        "{e}"
    );
    assert!(e.contains(":2:"), "{e}");
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
    assert!(x.exports.is_empty());
}

#[test]
fn syntax_error_carries_its_line() {
    let e = parse_servers("[servers.a]\nendpoint_id = \n", p())
        .unwrap_err()
        .to_string();
    assert!(e.starts_with("/x/f.toml:2:"), "{e}");
}

#[test]
fn servers_invalid_name_in_file_is_error_with_line() {
    let t = format!(
        "[servers.ok]\nendpoint_id = \"{}\"\n[servers.\"a:b\"]\nendpoint_id = \"{}\"\n",
        id(1),
        id(2)
    );
    let e = parse_servers(&t, p()).unwrap_err().to_string();
    assert!(e.contains("/x/f.toml:3:") && e.contains("a:b"), "{e}");
    let t = format!("[servers.{}]\nendpoint_id = \"{}\"\n", id(1), id(1));
    assert!(parse_servers(&t, p())
        .unwrap_err()
        .to_string()
        .contains("invalid name"));
}

#[test]
fn servers_bad_endpoint_id_has_line() {
    let e = parse_servers("[servers.a]\nendpoint_id = \"zzz\"\n", p())
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
    let e = parse_exports("[exports.a]\npath = \"/x\"\nrw = 5\n", p())
        .unwrap_err()
        .to_string();
    assert!(e.contains("/x/f.toml:1:"), "{e}");
    let e = parse_exports("x = 1\n", p()).unwrap_err().to_string();
    assert!(
        e.contains("/x/f.toml:1:") && e.contains("unknown key \"x\""),
        "{e}"
    );
}

fn merged(dir: &Path, text: &str) -> Result<Config, ConfigError> {
    irsync::fs::private::write(dir.join("irsync.toml"), text).unwrap();
    Config::load(&Paths::resolve(Some(&dir.join("irsync.toml"))))
}

#[test]
fn merged_file_holds_every_table() {
    let d = tempfile::tempdir().unwrap();
    let c = merged(
        d.path(),
        &format!(
            "state_dir = \"/var/lib/irsync\"\n[defaults]\nparallel = 4\n[daemon]\nlisten = [\"127.0.0.1:0\"]\n\
             [servers.home]\nendpoint_id = \"{0}\"\n[clients.home]\nclient_id = \"{0}\"\nall_exports = \"ro\"\n[exports.photos]\npath = \"/srv/photos\"\nrw = [\"home\"]\n",
            id(1)
        ),
    )
    .unwrap();
    assert_eq!(c.settings.state_dir, PathBuf::from("/var/lib/irsync"));
    assert_eq!(c.settings.defaults.parallel, Some(4));
    assert_eq!(c.servers.get("home").unwrap().endpoint_id, id(1));
    assert_eq!(c.clients.get("home").unwrap().client_id, id(1));
    assert_eq!(
        c.clients.get("home").unwrap().all_exports,
        Some(irsync::authz::Access::ReadOnly)
    );
    assert_eq!(c.exports.exports["photos"].rw, ["home"]);
    assert_eq!(
        (c.servers.path.clone(), c.exports.path.clone()),
        (d.path().join("irsync.toml"), d.path().join("irsync.toml"))
    );
}

#[test]
fn missing_everything_is_the_defaults() {
    let d = tempfile::tempdir().unwrap();
    let c = Config::load(&Paths::resolve(Some(&d.path().join("irsync.toml")))).unwrap();
    assert_eq!(c.settings, Settings::default());
    assert!(c.servers.entries.is_empty() && c.clients.entries.is_empty());
    assert!(c.exports.exports.is_empty());
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
fn client_rights_are_read_and_written_back() {
    use irsync::authz::Access;
    use irsync::config::reader::read;
    use irsync::config::writer::write;
    let text = format!(
        "config_version = 2\n\n[clients.laptop]\nclient_id = \"{}\"\nall_exports = \"ro\"\nall_paths = \"rw\" # for the backup\n",
        id(1)
    );
    let c = Config::parse(&text, Path::new("c")).unwrap();
    let laptop = c.clients.get("laptop").unwrap();
    assert_eq!(laptop.all_exports, Some(Access::ReadOnly));
    assert_eq!(laptop.all_paths, Some(Access::ReadWrite));
    let msg = read(&text, Path::new("c")).unwrap();
    assert_eq!(write(&msg), text);
}

#[test]
fn a_client_right_other_than_rw_or_ro_is_refused() {
    let text = format!(
        "[clients.laptop]\nclient_id = \"{}\"\nall_paths = \"yes\"\n",
        id(1)
    );
    let e = Config::parse(&text, Path::new("c"))
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("client \"laptop\": all_paths must be \"rw\" or \"ro\""),
        "{e}"
    );
    assert!(e.contains(":3:") || e.contains("line 3"), "{e}");
}

#[test]
fn allow_all_paths_defaults_to_on() {
    let c = Config::parse("", Path::new("c")).unwrap();
    assert!(c.settings.daemon.allow_all_paths);
    let c = Config::parse("[daemon]\nallow_all_paths = false\n", Path::new("c")).unwrap();
    assert!(!c.settings.daemon.allow_all_paths);
}

#[test]
fn daemon_dormant_session_limit_defaults_to_25() {
    let c = Config::parse("", Path::new("c")).unwrap();
    assert_eq!(c.settings.daemon.max_dormant_sessions_per_key, 25);
    let set = |v: &str| {
        let text = format!("[daemon]\nmax_dormant_sessions_per_key = {v}\n");
        Config::parse(&text, Path::new("c")).map(|c| c.settings.daemon.max_dormant_sessions_per_key)
    };
    assert_eq!(set("3").unwrap(), 3);
    assert_eq!(set("0").unwrap(), 0);
    for bad in ["-1", "\"3\"", "1.5"] {
        let e = set(bad).unwrap_err().to_string();
        assert!(e.contains("c:2"), "{bad}: {e}");
    }
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
fn export_names_of_rights_are_reserved() {
    for (name, why) in [
        (
            "all_exports",
            "`irsync grant list all_exports` lists the clients with that right",
        ),
        (
            "all_paths",
            "`irsync grant list all_paths` lists the clients with that right",
        ),
    ] {
        let e = parse_exports(
            &format!("[exports.{name}]\npath = \"/p\"\n"),
            Path::new("e"),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(e, format!("e:1: {name:?} is reserved: {why}"));
    }
}

/// The config file is for its owner only. One that other users can read
/// or change is refused before anything in it counts, unless the file
/// itself sets `unsafe_config_allowed = true`.
#[test]
#[cfg(unix)]
fn a_config_open_to_other_users_is_refused_unless_it_says_otherwise() {
    let d = tempfile::tempdir().unwrap();
    let paths = Paths::resolve(Some(&d.path().join("irsync.toml")));
    let set = |text: &str, mode: u32| {
        let _ = std::fs::remove_file(&paths.config);
        std::fs::write(&paths.config, text).unwrap();
        irsync::platform::testutil::set_mode(&paths.config, mode).unwrap();
        Config::load(&paths)
    };
    // No file at all is still the defaults.
    assert!(!Config::load(&paths).unwrap().settings.unsafe_config_allowed);
    for mode in [0o600, 0o400] {
        let c = set("[defaults]\nparallel = 4\n", mode).unwrap();
        assert!(!c.settings.unsafe_config_allowed, "{mode:o}");
    }
    for mode in [0o640, 0o604, 0o620, 0o602, 0o610, 0o644, 0o666] {
        let e = set("[defaults]\nparallel = 4\n", mode).unwrap_err();
        assert!(matches!(e, ConfigError::TooOpen { .. }), "{mode:o}: {e}");
        let text = e.to_string();
        assert!(text.contains(&format!("has mode {mode:04o}")), "{text}");
        assert!(text.contains("chmod 600"), "{text}");
        assert!(text.contains("unsafe_config_allowed = true"), "{text}");
        // Switched off by the file itself.
        let c = set(
            "unsafe_config_allowed = true\n[defaults]\nparallel = 4\n",
            mode,
        )
        .unwrap();
        assert!(c.settings.unsafe_config_allowed, "{mode:o}");
    }
    // The permissions are judged first: a file that does not parse, or
    // sets the key to anything but true, is refused for being open.
    for text in [
        "not toml [",
        "unsafe_config_allowed = false\n",
        "unsafe_config_allowed = \"true\"\n",
        "[defaults]\nunsafe_config_allowed = true\n",
        "bogus_key = 1\n",
    ] {
        let e = set(text, 0o644).unwrap_err();
        assert!(matches!(e, ConfigError::TooOpen { .. }), "{text:?}: {e}");
    }
    // In a private file the key is an ordinary boolean.
    assert!(
        set("unsafe_config_allowed = true\n", 0o600)
            .unwrap()
            .settings
            .unsafe_config_allowed
    );
    let e = set("unsafe_config_allowed = \"true\"\n", 0o600).unwrap_err();
    assert!(e.to_string().contains("expected a boolean"), "{e}");
    // Editing reads the file under the same rule.
    set("[defaults]\nparallel = 4\n", 0o644).unwrap_err();
    let e = irsync::manage::load_message(&paths.config).unwrap_err();
    assert!(e.to_string().contains("has mode 0644"), "{e}");
    let mut lock = irsync::manage::lock_config(&paths.config).unwrap();
    assert!(lock.load().is_err());
}

/// `Config::parse` reads the values only, and gives the typed view the
/// full read gives, and the same errors.
#[test]
fn parse_reads_values_only_with_the_same_view_and_errors() {
    use irsync::config::reader::{read, read_values};
    let text = format!(
        "config_version = 2\n# top\n[servers.nas] # the nas\nendpoint_id = \"{}\"\n",
        id(1)
    );
    for text in [text.as_str(), "# only a comment\n"] {
        let full = read(text, p()).unwrap();
        let values = read_values(text, p()).unwrap();
        assert_ne!(values, full, "{text}");
        assert!(values.header.is_empty(), "{text}");
        assert_eq!(
            Config::from_message(&values, p()).unwrap(),
            Config::from_message(&full, p()).unwrap(),
            "{text}"
        );
        assert_eq!(
            Config::parse(text, p()).unwrap(),
            Config::from_message(&full, p()).unwrap()
        );
    }
    for bad in [
        "[servers.a]\nendpoint_id = \"x\"\n",
        "config_version = 3\n",
        "[hosts.a]\n",
        "x = [\n",
    ] {
        assert_eq!(
            read_values(bad, p()).unwrap_err().to_string(),
            read(bad, p()).unwrap_err().to_string(),
            "{bad}"
        );
    }
}

#[test]
fn admin_is_read_and_written_back() {
    use irsync::authz::Access;
    use irsync::config::reader::read;
    use irsync::config::writer::write;
    let text = format!(
        "config_version = 2\n\n[clients.laptop]\nclient_id = \"{}\"\nall_paths = \"rw\"\nadmin = \"ro\"\n",
        id(1)
    );
    let c = Config::parse(&text, Path::new("c")).unwrap();
    assert_eq!(
        c.clients.get("laptop").unwrap().admin,
        Some(Access::ReadOnly)
    );
    let msg = read(&text, Path::new("c")).unwrap();
    assert_eq!(write(&msg), text);
    let bad = format!(
        "[clients.laptop]\nclient_id = \"{}\"\nadmin = \"yes\"\n",
        id(1)
    );
    let e = Config::parse(&bad, Path::new("c")).unwrap_err().to_string();
    assert!(
        e.contains("client \"laptop\": admin must be \"rw\" or \"ro\""),
        "{e}"
    );
}
