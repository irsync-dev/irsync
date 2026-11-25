//! A dial's settings: the command line, then the server's table, then
//! `[defaults]`, then the built-in default, and the endpoint they make.

use iroh::SecretKey;
use irsync::config::dial::{Dial, DialFlags};
use irsync::config::servers::ServerEntry;
use irsync::config::settings::{Congestion, Defaults, PqKeyExchange, RelayMode};
use irsync::net::{client_endpoint_opts, RelayChoice};
use std::time::Duration;

/// A server entry that sets nothing but its id.
fn server() -> ServerEntry {
    ServerEntry {
        endpoint_id: SecretKey::from_bytes(&[1; 32]).public(),
        relay: None,
        relay_mode: None,
        congestion: None,
        pq_key_exchange: None,
        address: Vec::new(),
        parallel: None,
        reconnect: None,
        dial_retries: None,
        dht: None,
        dns: None,
        mdns: None,
        ssh: None,
        ssh_aliases: Vec::new(),
    }
}

fn resolve(flags: &DialFlags, s: Option<&ServerEntry>, d: &Defaults) -> Dial {
    Dial::resolve(flags, s, d).unwrap()
}

fn url(s: &str) -> url::Url {
    s.parse().unwrap()
}

#[test]
fn nothing_set_is_the_built_in_default() {
    let d = resolve(&DialFlags::default(), None, &Defaults::default());
    assert_eq!(d, Dial::default());
    assert_eq!(d.parallel, 128);
    assert_eq!(d.reconnect, Duration::from_secs(86400));
    assert_eq!(d.dial_retries, 4);
    assert!(d.ssh_install && d.dht && d.dns && d.mdns);
    assert_eq!(d.relay, RelayChoice::N0Default);
    assert_eq!(d.congestion, Congestion::Bbr);
    assert_eq!(d.pq_key_exchange, PqKeyExchange::Prefer);
}

/// Each key: the flag over the server over `[defaults]`.
#[test]
fn the_flag_then_the_server_then_the_defaults_decide() {
    let defaults = Defaults {
        parallel: Some(2),
        reconnect: Some(Duration::from_secs(2)),
        dial_retries: Some(2),
        dht: Some(false),
        dns: Some(false),
        mdns: Some(false),
        congestion: Some(Congestion::Cubic),
        pq_key_exchange: Some(PqKeyExchange::Require),
        ..Defaults::default()
    };
    let d = resolve(&DialFlags::default(), None, &defaults);
    assert_eq!(
        (d.parallel, d.reconnect.as_secs(), d.dial_retries),
        (2, 2, 2)
    );
    assert!(!d.dht && !d.dns && !d.mdns);
    assert_eq!(d.congestion, Congestion::Cubic);
    assert_eq!(d.pq_key_exchange, PqKeyExchange::Require);

    let s = ServerEntry {
        parallel: Some(3),
        reconnect: Some(Duration::from_secs(3)),
        dial_retries: Some(3),
        dht: Some(true),
        dns: Some(true),
        mdns: Some(true),
        congestion: Some(Congestion::Bbr),
        pq_key_exchange: Some(PqKeyExchange::Prefer),
        ..server()
    };
    let d = resolve(&DialFlags::default(), Some(&s), &defaults);
    assert_eq!(
        (d.parallel, d.reconnect.as_secs(), d.dial_retries),
        (3, 3, 3)
    );
    assert!(d.dht && d.dns && d.mdns);
    assert_eq!(d.congestion, Congestion::Bbr);
    assert_eq!(d.pq_key_exchange, PqKeyExchange::Prefer);

    let flags = DialFlags {
        parallel: Some(4),
        reconnect: Some(Duration::from_secs(4)),
        dial_retries: Some(4),
        no_dht: true,
        pq_key_exchange: Some(PqKeyExchange::Require),
        ..DialFlags::default()
    };
    let d = resolve(&flags, Some(&s), &defaults);
    assert_eq!(
        (d.parallel, d.reconnect.as_secs(), d.dial_retries),
        (4, 4, 4)
    );
    assert!(!d.dht && d.dns && d.mdns);
    assert_eq!(d.pq_key_exchange, PqKeyExchange::Require);
}

#[test]
fn ssh_install_comes_from_the_defaults() {
    let off = Defaults {
        ssh_install: Some(false),
        ..Defaults::default()
    };
    assert!(!resolve(&DialFlags::default(), None, &off).ssh_install);
}

#[test]
fn a_paired_server_is_dialled_once_unless_the_flag_says_otherwise() {
    let paired = ServerEntry {
        ssh: Some("me@nas".into()),
        ..server()
    };
    let d = resolve(&DialFlags::default(), Some(&paired), &Defaults::default());
    assert_eq!(d.dial_retries, 0);
    let flags = DialFlags {
        dial_retries: Some(5),
        ..DialFlags::default()
    };
    assert_eq!(
        resolve(&flags, Some(&paired), &Defaults::default()).dial_retries,
        5
    );
}

#[test]
fn the_relay_the_flags_and_the_file_choose() {
    let none = Defaults::default();
    let flags = |relay: Option<&str>, no_relay| DialFlags {
        relay: relay.map(url),
        no_relay,
        ..DialFlags::default()
    };
    // --no-relay: no relay, the lookups stay.
    let d = resolve(&flags(None, true), None, &none);
    assert_eq!(d.relay, RelayChoice::Disabled);
    assert!(d.dht && d.dns && d.mdns);
    // [defaults] relay_mode = "disabled", even with a relay URL beside it.
    let off = Defaults {
        relay: Some(url("https://c.example")),
        relay_mode: Some(RelayMode::Disabled),
        ..Defaults::default()
    };
    assert_eq!(
        resolve(&flags(None, false), None, &off).relay,
        RelayChoice::Disabled
    );
    // The server's mode decides over [defaults]'s, either way round.
    let mut s = ServerEntry {
        relay_mode: Some(RelayMode::Default),
        ..server()
    };
    assert_eq!(
        resolve(&flags(None, false), Some(&s), &off).relay,
        RelayChoice::Custom(url("https://c.example"))
    );
    s.relay_mode = Some(RelayMode::Disabled);
    assert_eq!(
        resolve(&flags(None, false), Some(&s), &none).relay,
        RelayChoice::Disabled
    );
    // --relay on the command line beats a file's "disabled".
    assert_eq!(
        resolve(&flags(Some("https://cli.example"), false), Some(&s), &none).relay,
        RelayChoice::Custom(url("https://cli.example"))
    );
}

#[test]
fn addresses_make_the_endpoint_isolated() {
    let s = ServerEntry {
        address: vec!["127.0.0.1:1".parse().unwrap()],
        dht: Some(true),
        ..server()
    };
    let cubic = Defaults {
        congestion: Some(Congestion::Cubic),
        ..Defaults::default()
    };
    let d = resolve(&DialFlags::default(), Some(&s), &cubic);
    assert_eq!(d.address, s.address);
    let o = client_endpoint_opts(SecretKey::generate(), &d);
    assert_eq!(o.relay, RelayChoice::Disabled);
    assert!(!o.dht && !o.dns && !o.mdns && !o.portmap && !o.publish);
    assert_eq!(o.congestion, Congestion::Cubic);
    // --address replaces the server's list.
    let flags = DialFlags {
        address: vec!["localhost:2".parse().unwrap()],
        ..DialFlags::default()
    };
    assert_eq!(resolve(&flags, Some(&s), &cubic).address, flags.address);
    // --relay with the server's addresses is refused.
    let flags = DialFlags {
        relay: Some(url("https://c.example")),
        ..DialFlags::default()
    };
    let e = Dial::resolve(&flags, Some(&s), &cubic).unwrap_err();
    assert!(e.contains("--relay"), "{e}");
}

#[test]
fn without_addresses_the_endpoint_follows_the_dial() {
    let d = Dial {
        dht: false,
        relay: RelayChoice::Custom(url("https://c.example")),
        ..Dial::default()
    };
    let o = client_endpoint_opts(SecretKey::generate(), &d);
    assert_eq!(o.relay, d.relay);
    assert!(!o.dht && o.dns && o.mdns && o.portmap && !o.publish);
    assert_eq!(o.congestion, Congestion::Bbr);
}
