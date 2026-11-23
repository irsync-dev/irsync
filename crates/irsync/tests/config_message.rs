//! The config message as `config::reader` reads it from irsync.toml.

use irsync::config::reader::read_values;
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
fn errors_are_those_of_config_parse() {
    for text in [
        "[hosts.nas]\nendpoint_id = \"nope\"\n",
        "[exports.p]\npath = \"rel\"\n",
        "[nas]\nendpoint_id = \"x\"\n",
        "rw = [\"x\"]\n",
        "[daemon]\nlisten = [\"1.2.3.4:1\", \"1.2.3.5:1\"]\n",
        "[immediate]\nro = [\"x\"]\n",
        "[hosts.all]\nendpoint_id = \"x\"\n",
        "[client]\nbogus = 1\n",
        "[daemon]\nidentity = 1\n",
        "[daemon]\nidle_timeout = \"soon\"\n",
        "[daemon]\ndebug_addr = \"x\"\n",
        "[daemon.discovery]\nbogus = true\n",
        "[client]\nrelay = \"not a url\"\n",
        "[telemetry]\notlp_protocol = \"udp\"\n",
        "[hosts.nas]\naddress = [\"nope\"]\n",
        "[hosts.nas]\nssh = \"nas\"\n",
        "[hosts.b]\nendpoint_id = \"x\"\n[hosts.a]\nendpoint_id = \"y\"\n",
        "[exports.p]\nread_only = true\n",
        "[exports.p]\npath = \"/p\"\nbogus = 1\n",
        "[exports.b]\npath = \"b\"\n[exports.a]\npath = \"a\"\n",
        "[exports]\np = 1\n",
        "[all]\nrw = \"x\"\n",
        "identity = 1\n",
        "not toml",
    ] {
        let old = irsync::config::file::Config::parse(text, Path::new("f")).unwrap_err();
        let new = read_values(text, Path::new("f")).unwrap_err();
        assert_eq!(new.to_string(), old.to_string(), "{text}");
    }
}
