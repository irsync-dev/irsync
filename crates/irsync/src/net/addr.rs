//! Direct socket addresses given by hand: `ip:port`, `[ipv6]:port` or
//! `host:port`, resolved with the system resolver.

use std::fmt;
use std::net::SocketAddr;
use std::str::FromStr;

/// A socket address as written in a config file or on the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostPort {
    /// A numeric address, IPv6 in brackets.
    Ip(SocketAddr),
    /// A hostname, resolved when used.
    Name { host: String, port: u16 },
}

#[derive(Debug, thiserror::Error)]
#[error("invalid address {0:?}: expected ip:port, [ipv6]:port or host:port")]
pub struct AddrError(pub String);

impl FromStr for HostPort {
    type Err = AddrError;

    fn from_str(s: &str) -> Result<Self, AddrError> {
        if let Ok(addr) = s.parse::<SocketAddr>() {
            return Ok(HostPort::Ip(addr));
        }
        let bad = || AddrError(s.to_string());
        let (host, port) = s.rsplit_once(':').ok_or_else(bad)?;
        if host.is_empty() || host.contains([':', '[', ']']) {
            return Err(bad());
        }
        // `u16::from_str` accepts a leading `+`.
        if port.starts_with('+') {
            return Err(bad());
        }
        let port = port.parse::<u16>().map_err(|_| bad())?;
        Ok(HostPort::Name {
            host: host.to_string(),
            port,
        })
    }
}

impl fmt::Display for HostPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HostPort::Ip(addr) => write!(f, "{addr}"),
            HostPort::Name { host, port } => write!(f, "{host}:{port}"),
        }
    }
}

impl<'de> serde::Deserialize<'de> for HostPort {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl HostPort {
    pub fn port(&self) -> u16 {
        match self {
            HostPort::Ip(addr) => addr.port(),
            HostPort::Name { port, .. } => *port,
        }
    }

    /// Every address the system resolver yields, in resolver order; an
    /// `Ip` yields itself.
    pub async fn resolve(&self) -> std::io::Result<Vec<SocketAddr>> {
        match self {
            HostPort::Ip(addr) => Ok(vec![*addr]),
            HostPort::Name { host, port } => Ok(tokio::net::lookup_host((host.as_str(), *port))
                .await?
                .collect()),
        }
    }
}

/// The sockets a client in isolated mode binds: for each IP version among
/// `dial`, the local address the system sends from to reach the first
/// address of that version, with port 0. Bound to one address, the client
/// offers the daemon only that one for extra paths, instead of every
/// interface it has. An IP version with no route is left out; the dial to
/// it would fail anyway.
pub fn source_binds(dial: &[SocketAddr]) -> Vec<SocketAddr> {
    let mut binds = Vec::new();
    for v6 in [false, true] {
        let Some(first) = dial.iter().find(|a| a.is_ipv6() == v6) else {
            continue;
        };
        match source_ip(*first) {
            Ok(ip) => binds.push(SocketAddr::new(ip, 0)),
            Err(e) => tracing::debug!(%first, "no source address: {e}"),
        }
    }
    binds
}

/// The local IP the system would send from to reach `to`. Connecting a UDP
/// socket only looks up the route; nothing is sent.
fn source_ip(to: SocketAddr) -> std::io::Result<std::net::IpAddr> {
    let any: SocketAddr = if to.is_ipv6() {
        (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
    } else {
        (std::net::Ipv4Addr::UNSPECIFIED, 0).into()
    };
    let sock = std::net::UdpSocket::bind(any)?;
    sock.connect(to)?;
    Ok(sock.local_addr()?.ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_binds_take_the_loopback_route() {
        let v4: SocketAddr = "127.0.0.1:4433".parse().unwrap();
        assert_eq!(source_binds(&[v4]), ["127.0.0.1:0".parse().unwrap()]);
    }

    #[test]
    fn source_binds_one_per_ip_version_from_the_first_of_each() {
        let dial: Vec<SocketAddr> = ["127.0.0.1:1", "127.0.0.2:2", "[::1]:3"]
            .iter()
            .map(|s| s.parse().unwrap())
            .collect();
        let binds = source_binds(&dial);
        assert_eq!(binds[0], "127.0.0.1:0".parse().unwrap());
        // A host with IPv6 turned off has no route to ::1.
        if let Some(v6) = binds.get(1) {
            assert_eq!(*v6, "[::1]:0".parse().unwrap());
        }
        assert!(binds.len() <= 2);
    }

    #[test]
    fn source_binds_of_nothing_is_nothing() {
        assert!(source_binds(&[]).is_empty());
    }
}
