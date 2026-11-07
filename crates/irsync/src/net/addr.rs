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
