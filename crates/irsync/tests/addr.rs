use irsync::net::addr::HostPort;
use std::net::SocketAddr;

#[test]
fn parses_ipv4_ipv6_and_names() {
    assert_eq!(
        "127.0.0.1:9".parse::<HostPort>().unwrap(),
        HostPort::Ip("127.0.0.1:9".parse().unwrap())
    );
    assert_eq!(
        "[::1]:9".parse::<HostPort>().unwrap(),
        HostPort::Ip("[::1]:9".parse().unwrap())
    );
    assert_eq!(
        "nas.local:9".parse::<HostPort>().unwrap(),
        HostPort::Name {
            host: "nas.local".into(),
            port: 9
        }
    );
    assert_eq!("127.0.0.1:0".parse::<HostPort>().unwrap().port(), 0);
}

#[test]
fn rejects_missing_port_bad_port_and_bare_ipv6() {
    for bad in [
        "127.0.0.1",
        "host",
        "host:",
        ":9",
        "host:99999",
        "host:+80",
        "::1:9",
        "[::1]",
        "",
    ] {
        assert!(bad.parse::<HostPort>().is_err(), "{bad:?} parsed");
    }
}

#[test]
fn display_round_trips() {
    for s in ["127.0.0.1:9", "[::1]:9", "nas.local:9"] {
        assert_eq!(s.parse::<HostPort>().unwrap().to_string(), s);
    }
}

#[test]
fn deserializes_from_toml_string() {
    #[derive(serde::Deserialize)]
    struct T {
        a: Vec<HostPort>,
    }
    let t: T = toml::from_str("a = [\"127.0.0.1:1\", \"[::1]:2\"]").unwrap();
    assert_eq!(t.a.len(), 2);
    assert!(toml::from_str::<T>("a = [\"nope\"]").is_err());
}

#[tokio::test]
async fn resolves_ip_to_itself_and_localhost_to_loopback() {
    let ip: HostPort = "127.0.0.1:7".parse().unwrap();
    assert_eq!(
        ip.resolve().await.unwrap(),
        vec!["127.0.0.1:7".parse::<SocketAddr>().unwrap()]
    );
    let lo: HostPort = "localhost:7".parse().unwrap();
    let v = lo.resolve().await.unwrap();
    assert!(
        !v.is_empty() && v.iter().all(|a| a.ip().is_loopback() && a.port() == 7),
        "{v:?}"
    );
    assert!("no-such-host.invalid:7"
        .parse::<HostPort>()
        .unwrap()
        .resolve()
        .await
        .is_err());
}
