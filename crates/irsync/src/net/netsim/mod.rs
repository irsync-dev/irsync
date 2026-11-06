//! Network-fault test rig: settings from `IRSYNC_NETSIM` and the custom
//! transport's addresses. Only built with the `netsim` feature.

use std::net::{Ipv4Addr, SocketAddrV4};
use std::str::FromStr;
use std::sync::OnceLock;

use anyhow::{anyhow, bail, Context};
use iroh_base::CustomAddr;

pub mod link;
pub mod profile;

/// The environment variable that switches the rig on.
pub const ENV: &str = "IRSYNC_NETSIM";

/// The custom transport's id: "netsim" in ASCII.
pub const TRANSPORT_ID: u64 = 0x6e65_7473_696d;

/// Which end of the connection this process is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Client,
    Daemon,
}

/// A named fault profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileName {
    FourGVehicle,
    DslVideo,
    TestBlackout,
}

impl FromStr for ProfileName {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "4g-vehicle" => Ok(Self::FourGVehicle),
            "dsl-video" => Ok(Self::DslVideo),
            "test-blackout" => Ok(Self::TestBlackout),
            _ => bail!("unknown profile {s:?}"),
        }
    }
}

impl std::fmt::Display for ProfileName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::FourGVehicle => "4g-vehicle",
            Self::DslVideo => "dsl-video",
            Self::TestBlackout => "test-blackout",
        })
    }
}

/// The parsed `IRSYNC_NETSIM` value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    pub profile: ProfileName,
    pub side: Side,
    pub seed: u64,
    pub stream: u64,
}

impl Spec {
    /// Parses `profile=<name>,side=<client|daemon>,seed=<u64>,stream=<u64>`;
    /// keys in any order, all four required.
    pub fn parse(s: &str) -> anyhow::Result<Spec> {
        let (mut profile, mut side, mut seed, mut stream) = (None, None, None, None);
        for part in s.split(',') {
            let (key, value) = part
                .split_once('=')
                .ok_or_else(|| anyhow!("expected key=value, got {part:?}"))?;
            let dup = || anyhow!("{key} given twice");
            match key {
                "profile" if profile.is_none() => profile = Some(value.parse()?),
                "side" if side.is_none() => {
                    side = Some(match value {
                        "client" => Side::Client,
                        "daemon" => Side::Daemon,
                        _ => bail!("side must be client or daemon, got {value:?}"),
                    })
                }
                "seed" if seed.is_none() => seed = Some(value.parse::<u64>().context("bad seed")?),
                "stream" if stream.is_none() => {
                    stream = Some(value.parse::<u64>().context("bad stream")?)
                }
                "profile" | "side" | "seed" | "stream" => return Err(dup()),
                _ => bail!("unknown key {key:?}"),
            }
        }
        Ok(Spec {
            profile: profile.ok_or_else(|| anyhow!("profile missing"))?,
            side: side.ok_or_else(|| anyhow!("side missing"))?,
            seed: seed.ok_or_else(|| anyhow!("seed missing"))?,
            stream: stream.ok_or_else(|| anyhow!("stream missing"))?,
        })
    }
}

/// The settings from `IRSYNC_NETSIM`, read once. `None` when it is unset; a
/// bad value is an error on every call.
pub fn active() -> anyhow::Result<Option<&'static Spec>> {
    static ACTIVE: OnceLock<Result<Option<Spec>, String>> = OnceLock::new();
    let cached = ACTIVE.get_or_init(|| match std::env::var(ENV) {
        Ok(v) => Spec::parse(&v).map(Some).map_err(|e| format!("{e:#}")),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(e) => Err(e.to_string()),
    });
    match cached {
        Ok(spec) => Ok(spec.as_ref()),
        Err(msg) => Err(anyhow!("bad {ENV}: {msg}")),
    }
}

/// The custom address for an IPv4 socket address: its 4 bytes, then the port's
/// 2 bytes, big-endian.
pub fn custom_addr(a: SocketAddrV4) -> CustomAddr {
    let mut data = [0u8; 6];
    data[..4].copy_from_slice(&a.ip().octets());
    data[4..].copy_from_slice(&a.port().to_be_bytes());
    CustomAddr::from_parts(TRANSPORT_ID, &data)
}

/// The socket address in a custom address; `None` for another transport id or
/// a wrong length.
pub fn socket_addr(c: &CustomAddr) -> Option<SocketAddrV4> {
    if c.id() != TRANSPORT_ID {
        return None;
    }
    let d: [u8; 6] = c.data().try_into().ok()?;
    Some(SocketAddrV4::new(
        Ipv4Addr::new(d[0], d[1], d[2], d[3]),
        u16::from_be_bytes([d[4], d[5]]),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_parses_in_any_order() {
        let s = Spec::parse("stream=3,seed=7,side=client,profile=4g-vehicle").unwrap();
        assert_eq!(
            s,
            Spec {
                profile: ProfileName::FourGVehicle,
                side: Side::Client,
                seed: 7,
                stream: 3
            }
        );
    }

    #[test]
    fn spec_rejects_missing_unknown_and_bad_values() {
        for bad in [
            "profile=4g-vehicle,side=client,seed=7",   // stream missing
            "profile=lte,side=client,seed=7,stream=0", // unknown profile
            "profile=dsl-video,side=both,seed=7,stream=0", // bad side
            "profile=dsl-video,side=daemon,seed=x,stream=0", // bad number
            "profile=dsl-video,side=daemon,seed=1,stream=0,x=1", // unknown key
        ] {
            assert!(Spec::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn custom_addr_round_trips() {
        let a: SocketAddrV4 = "127.0.0.1:43210".parse().unwrap();
        let c = custom_addr(a);
        assert_eq!(c.id(), TRANSPORT_ID);
        assert_eq!(socket_addr(&c), Some(a));
        assert_eq!(socket_addr(&CustomAddr::from_parts(0x20, &[1, 2])), None);
    }

    #[test]
    fn version_marks_a_netsim_build() {
        assert!(crate::build::LONG_VERSION.ends_with(" [netsim]"));
    }
}
