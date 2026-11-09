//! Network-fault test rig: settings from `IRSYNC_NETSIM` and the custom
//! transport's addresses. Only built with the `netsim` feature.

use std::net::{Ipv4Addr, SocketAddrV4};
use std::str::FromStr;
use std::sync::{Arc, OnceLock};

use anyhow::{anyhow, bail, Context};
use iroh::endpoint::transports::CustomTransport;
use iroh_base::CustomAddr;

pub mod link;
pub mod outage;
pub mod profile;
pub mod transport;

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
    TestSlow,
    LongRtt,
}

impl FromStr for ProfileName {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "4g-vehicle" => Ok(Self::FourGVehicle),
            "dsl-video" => Ok(Self::DslVideo),
            "test-blackout" => Ok(Self::TestBlackout),
            "test-slow" => Ok(Self::TestSlow),
            "long-rtt" => Ok(Self::LongRtt),
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
            Self::TestSlow => "test-slow",
            Self::LongRtt => "long-rtt",
        })
    }
}

/// The ACKs an endpoint asks of its peer, for measuring them against each
/// other; only the fault build can change them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Acks {
    /// irsync's setting: every packet, within 1 ms.
    Immediate,
    /// Every second packet, within 1 ms.
    Gentle,
    /// Nothing asked: the peer's own delayed ACKs (every second packet or
    /// 25 ms).
    PeerDefault,
}

impl FromStr for Acks {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "immediate" => Ok(Self::Immediate),
            "gentle" => Ok(Self::Gentle),
            "peer-default" => Ok(Self::PeerDefault),
            _ => bail!("acks must be immediate, gentle or peer-default, got {s:?}"),
        }
    }
}

impl std::fmt::Display for Acks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Immediate => "immediate",
            Self::Gentle => "gentle",
            Self::PeerDefault => "peer-default",
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
    /// `None`: irsync's own setting.
    pub acks: Option<Acks>,
}

impl Spec {
    /// Parses `profile=<name>,side=<client|daemon>,seed=<u64>,stream=<u64>`
    /// and an optional `acks=<immediate|gentle|peer-default>`; keys in any
    /// order, the first four required.
    pub fn parse(s: &str) -> anyhow::Result<Spec> {
        let (mut profile, mut side, mut seed, mut stream) = (None, None, None, None);
        let mut acks = None;
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
                "acks" if acks.is_none() => acks = Some(value.parse()?),
                "profile" | "side" | "seed" | "stream" | "acks" => return Err(dup()),
                _ => bail!("unknown key {key:?}"),
            }
        }
        Ok(Spec {
            profile: profile.ok_or_else(|| anyhow!("profile missing"))?,
            side: side.ok_or_else(|| anyhow!("side missing"))?,
            seed: seed.ok_or_else(|| anyhow!("seed missing"))?,
            stream: stream.ok_or_else(|| anyhow!("stream missing"))?,
            acks,
        })
    }
}

/// What a fault random stream drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// The side's outgoing link.
    Link,
    /// The client's handovers and dead zone.
    Outages,
}

/// The seed of one random stream, from the run's seed, the side, the stream
/// number and what it drives: each part is folded in through splitmix64, so
/// streams that differ in any part are unrelated.
pub fn fault_seed(seed: u64, side: Side, stream: u64, purpose: Purpose) -> u64 {
    let side = match side {
        Side::Client => 1,
        Side::Daemon => 2,
    };
    let purpose = match purpose {
        Purpose::Link => 1,
        Purpose::Outages => 2,
    };
    [side, stream, purpose]
        .into_iter()
        .fold(splitmix64(seed), |h, part| splitmix64(h ^ part))
}

/// One step of splitmix64: a bijective mix of all 64 bits.
fn splitmix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
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

/// The fault-injecting transport for `spec`, its socket bound on `bind`.
pub fn transport(spec: &Spec, bind: SocketAddrV4) -> Arc<dyn CustomTransport> {
    Arc::new(transport::NetsimTransport {
        spec: spec.clone(),
        bind,
    })
}

/// Where the transport's socket listens, once one is bound in this process.
pub fn bound_addr() -> Option<SocketAddrV4> {
    *transport::BOUND.lock().expect("bound poisoned")
}

/// Says, at `warn` so any log level shows it, that the rig is on.
pub fn log_active(spec: &Spec) {
    tracing::warn!(
        profile = %spec.profile,
        side = ?spec.side,
        seed = spec.seed,
        stream = spec.stream,
        acks = %spec.acks.map_or("irsync's".to_string(), |a| a.to_string()),
        "netsim active"
    );
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
                stream: 3,
                acks: None,
            }
        );
    }

    #[test]
    fn spec_takes_an_optional_ack_setting() {
        let base = "profile=long-rtt,side=daemon,seed=1,stream=0";
        let s = Spec::parse(base).unwrap();
        assert_eq!(s.profile, ProfileName::LongRtt);
        assert_eq!(s.acks, None);
        for (v, want) in [
            ("immediate", Acks::Immediate),
            ("gentle", Acks::Gentle),
            ("peer-default", Acks::PeerDefault),
        ] {
            let s = Spec::parse(&format!("{base},acks={v}")).unwrap();
            assert_eq!(s.acks, Some(want), "{v}");
            assert_eq!(want.to_string(), v);
        }
        assert!(Spec::parse(&format!("{base},acks=often")).is_err());
        assert!(Spec::parse(&format!("{base},acks=gentle,acks=gentle")).is_err());
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
    fn fault_seeds_do_not_collide() {
        // client iteration 3's uplink against the daemon's downlink
        assert_ne!(
            fault_seed(1, Side::Client, 3, Purpose::Link),
            fault_seed(1, Side::Daemon, 0, Purpose::Link)
        );
        for purpose in [Purpose::Link, Purpose::Outages] {
            let seeds: std::collections::HashSet<u64> = (1..=10)
                .flat_map(|seed| (1..=10).map(move |stream| (seed, stream)))
                .map(|(seed, stream)| fault_seed(seed, Side::Client, stream, purpose))
                .collect();
            assert_eq!(seeds.len(), 100, "{purpose:?}");
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
