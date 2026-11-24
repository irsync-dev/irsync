use irsync::config::clients::Clients;
use irsync::config::exports::Exports;
use irsync::config::file::{first_sighting, ignored_split_files, Config};
use irsync::config::names::{short_host_name, valid_alias};
use irsync::config::paths::Paths;
use irsync::config::servers::Servers;
use irsync::config::settings::{RelayMode, Settings};
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
fn pq_key_exchange_is_prefer_unless_set_to_require() {
    use irsync::config::settings::PqKeyExchange;
    let d = Settings::default();
    assert_eq!(
        (d.client.pq_key_exchange, d.daemon.pq_key_exchange),
        (None, PqKeyExchange::Prefer)
    );
    let s = Settings::parse(
        "[client]\npq_key_exchange = \"require\"\n[daemon]\npq_key_exchange = \"require\"\n",
        p(),
    )
    .unwrap();
    assert_eq!(s.client.pq_key_exchange, Some(PqKeyExchange::Require));
    assert_eq!(s.daemon.pq_key_exchange, PqKeyExchange::Require);
    let s = Settings::parse("[client]\npq_key_exchange = \"prefer\"\n", p()).unwrap();
    assert_eq!(s.client.pq_key_exchange, Some(PqKeyExchange::Prefer));
    let e = Settings::parse("[daemon]\npq_key_exchange = \"only\"\n", p()).unwrap_err();
    assert!(e.to_string().contains("only"), "{e}");
}

#[test]
fn pq_key_exchange_of_a_server() {
    use irsync::config::settings::PqKeyExchange;
    let t = format!(
        "[client]\npq_key_exchange = \"require\"\n[servers.nas]\nendpoint_id = \"{}\"\npq_key_exchange = \"prefer\"\n[servers.home]\nendpoint_id = \"{}\"\n",
        id(1),
        id(2)
    );
    let c = Config::parse(&t, p()).unwrap();
    assert_eq!(
        c.settings.client.pq_key_exchange,
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
fn hosts_entry_without_endpoint_id_is_error_with_line() {
    let e = parse_servers("[hosts.home]\nparallel = 4\n", p())
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

/// Hosts saved as clients only had no daemon to reach over ssh: in an old
/// file their `ssh` value is dropped, and no destination finds them.
#[test]
fn client_only_hosts_are_no_ssh_destination() {
    let text = format!(
        "[hosts.a]\nclient_id = \"{}\"\nssh = \"box\"\n\
         [hosts.b]\nclient_id = \"{}\"\nssh = \"box\"\n\
         [hosts.c]\nclient_id = \"{}\"\nssh = \"alone\"\n",
        id(1),
        id(2),
        id(3)
    );
    let c = Config::parse(&text, p()).unwrap();
    assert_eq!(c.servers.by_ssh("box"), Ok(None));
    assert_eq!(c.servers.by_ssh("alone"), Ok(None));
    assert_eq!(c.clients.entries.len(), 3);
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
    // An old entry needs one of the two ids; a bad client id names its line.
    let e = parse_servers("[hosts.x]\nssh = \"x\"\n", p())
        .unwrap_err()
        .to_string();
    assert!(e.contains("endpoint_id") && e.contains("client_id"), "{e}");
    let e = parse_servers("[hosts.x]\nclient_id = \"nope\"\n", p())
        .unwrap_err()
        .to_string();
    assert!(e.contains(":2:") && e.contains("\"x\""), "{e}");
}

#[test]
fn hosts_old_layout_names_the_new_location() {
    let e = parse_servers("\n[home]\nendpoint_id = \"x\"\n", p())
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("unknown table \"home\"; servers live under [servers.home], clients under [clients.home]")
            && e.contains(":2:"),
        "{e}"
    );
    let e = parse_servers("x = 1\n", p()).unwrap_err().to_string();
    assert!(e.contains("unknown key \"x\""), "{e}");
    assert!(parse_servers("[hosts]\n", p()).unwrap().entries.is_empty());
    assert!(parse_clients("[hosts]\n", p()).unwrap().entries.is_empty());
    // The first offender in the file is reported, not the first by name.
    let e = parse_servers(
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
fn exports_parse_exports_tables() {
    let t = "[exports.photos]\npath = \"/srv/photos\"\nrw = [\"laptop\"]\nro = [\"b2e1\"]\n[exports.docs]\npath = \"/srv/docs\"\nread_only = true\nwarm_index = true\nfsync = true\n";
    let x = parse_exports(t, p()).unwrap();
    assert_eq!(x.exports["photos"].rw, vec!["laptop"]);
    assert_eq!(x.exports["photos"].ro, vec!["b2e1"]);
    assert!(x.exports["docs"].read_only && x.exports["docs"].warm_index);
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
        e.contains("\"all\" is reserved: it is the word `grant remove all WHO` and `export update all` use for every export"),
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
        e.contains("unknown key \"rw\"; a client's right over every export is all_exports under [clients.NAME]")
            && e.contains(":1:"),
        "{e}"
    );
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
    assert!(x.exports.is_empty());
}

#[test]
fn syntax_error_carries_its_line() {
    let e = parse_servers("[hosts.a]\nendpoint_id = \n", p())
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
    let e = parse_servers(&t, p()).unwrap_err().to_string();
    assert!(e.contains("/x/f.toml:3:") && e.contains("a:b"), "{e}");
    let t = format!("[hosts.{}]\nendpoint_id = \"{}\"\n", id(1), id(1));
    assert!(parse_servers(&t, p())
        .unwrap_err()
        .to_string()
        .contains("invalid host alias"));
}

#[test]
fn hosts_bad_endpoint_id_has_line() {
    let e = parse_servers("[hosts.a]\nendpoint_id = \"zzz\"\n", p())
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
            "state_dir = \"/var/lib/irsync\"\n[client]\nparallel = 4\n[daemon]\nlisten = [\"127.0.0.1:0\"]\n\
             [servers.home]\nendpoint_id = \"{0}\"\n[clients.home]\nclient_id = \"{0}\"\nall_exports = \"ro\"\n[exports.photos]\npath = \"/srv/photos\"\nrw = [\"home\"]\n",
            id(1)
        ),
    )
    .unwrap();
    assert_eq!(c.settings.state_dir, PathBuf::from("/var/lib/irsync"));
    assert_eq!(c.settings.client.parallel, Some(4));
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
fn merged_shape_errors_keep_the_old_layout_hints() {
    let d = tempfile::tempdir().unwrap();
    for (text, needle) in [
        (
            "[home]\nendpoint_id = \"x\"\n",
            "unknown table \"home\"; servers live under [servers.home], clients under [clients.home]",
        ),
        (
            "[photos]\npath = \"/x\"\n",
            "unknown table \"photos\"; exports live under [exports.photos]",
        ),
        (
            "rw = [\"a\"]\n",
            "unknown key \"rw\"; a client's right over every export is all_exports under [clients.NAME]",
        ),
        (
            "[nothing]\nx = 1\n",
            "servers live under [servers.nothing], clients under [clients.nothing], exports under [exports.nothing]",
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
    irsync::fs::private::write(&config, "").unwrap();
    irsync::fs::private::write(
        dir.path().join("hosts.toml"),
        format!("[hosts.box]\nendpoint_id = \"{}\"\n", id(1)),
    )
    .unwrap();
    irsync::fs::private::write(
        dir.path().join("exports.toml"),
        "[exports.p]\npath = \"/p\"\n",
    )
    .unwrap();
    let c = Config::load(&Paths::resolve(Some(&config))).unwrap();
    assert!(c.servers.entries.is_empty());
    assert!(c.exports.exports.is_empty());
    assert_eq!(
        ignored_split_files(&config),
        vec![
            dir.path().join("hosts.toml"),
            dir.path().join("exports.toml")
        ]
    );
    // A split file that would not parse is ignored all the same.
    irsync::fs::private::write(dir.path().join("hosts.toml"), "not toml [").unwrap();
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

/// The error of each text, with and without `config_version = 2` above it
/// (two lines more).
fn refusals(body: &str) -> [String; 2] {
    [body.to_string(), format!("config_version = 2\n\n{body}")]
        .map(|t| Config::parse(&t, p()).unwrap_err().to_string())
}

#[test]
fn old_all_table_is_refused_with_a_hint() {
    let hint = "[all] is not read any more: give a client all_exports = \"rw\" or \"ro\" under [clients.NAME] (irsync client set NAME all_exports=rw)";
    let [plain, versioned] = refusals("[exports.p]\npath = \"/p\"\n[all]\nrw = [\"laptop\"]\n");
    assert_eq!(plain, format!("/x/f.toml:3: {hint}"));
    assert_eq!(versioned, format!("/x/f.toml:5: {hint}"));
}

#[test]
fn old_immediate_table_is_refused_with_a_hint() {
    let hint = "[immediate] is not read any more: give a client all_paths = \"rw\" or \"ro\" under [clients.NAME] (irsync client set NAME all_paths=rw)";
    let [plain, versioned] = refusals("[immediate]\nrw = [\"laptop\"]\n");
    assert_eq!(plain, format!("/x/f.toml:1: {hint}"));
    assert_eq!(versioned, format!("/x/f.toml:3: {hint}"));
}

#[test]
fn old_daemon_immediate_is_refused_with_a_hint() {
    let hint = "[daemon] immediate is now allow_all_paths";
    let [plain, versioned] = refusals("[daemon]\nportmap = false\nimmediate = false\n");
    assert_eq!(plain, format!("/x/f.toml:3: {hint}"));
    assert_eq!(versioned, format!("/x/f.toml:5: {hint}"));
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
    let msg = read(&text, Path::new("c")).unwrap().config;
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
            "immediate",
            "it used to name grants to any path; use irsync client set NAME all_paths=rw",
        ),
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
        let c = set("[client]\nparallel = 4\n", mode).unwrap();
        assert!(!c.settings.unsafe_config_allowed, "{mode:o}");
    }
    for mode in [0o640, 0o604, 0o620, 0o602, 0o610, 0o644, 0o666] {
        let e = set("[client]\nparallel = 4\n", mode).unwrap_err();
        assert!(matches!(e, ConfigError::TooOpen { .. }), "{mode:o}: {e}");
        let text = e.to_string();
        assert!(text.contains(&format!("has mode {mode:04o}")), "{text}");
        assert!(text.contains("chmod 600"), "{text}");
        assert!(text.contains("unsafe_config_allowed = true"), "{text}");
        // Switched off by the file itself.
        let c = set(
            "unsafe_config_allowed = true\n[client]\nparallel = 4\n",
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
        "[client]\nunsafe_config_allowed = true\n",
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
    set("[client]\nparallel = 4\n", 0o644).unwrap_err();
    let e = irsync::manage::load_message(&paths.config).unwrap_err();
    assert!(e.to_string().contains("has mode 0644"), "{e}");
    let mut lock = irsync::manage::lock_config(&paths.config).unwrap();
    assert!(lock.load().is_err());
}

/// `Config::parse` reads the values only, of either layout, and gives the
/// typed view the full read gives, and the same errors.
#[test]
fn parse_reads_values_only_with_the_same_view_and_errors() {
    use irsync::config::reader::{read, read_values_any};
    let v1 = format!(
        "# top\n[hosts.nas] # the nas\nendpoint_id = \"{}\"\n[hosts.lap]\nclient_id = \"{}\"\nmdns = false\n",
        id(1),
        id(2)
    );
    let v2 = format!(
        "config_version = 2\n# top\n[servers.nas] # the nas\nendpoint_id = \"{}\"\n",
        id(1)
    );
    for text in [v1.as_str(), v2.as_str(), "# only a comment\n"] {
        let full = read(text, p()).unwrap().config;
        let values = read_values_any(text, p()).unwrap();
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
        "[hosts.a]\nendpoint_id = \"x\"\n",
        "config_version = 3\n",
        "[hosts.a]\n[servers.b]\n",
        "x = [\n",
    ] {
        assert_eq!(
            read_values_any(bad, p()).unwrap_err().to_string(),
            read(bad, p()).unwrap_err().to_string(),
            "{bad}"
        );
    }
}
