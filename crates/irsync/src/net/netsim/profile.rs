//! Link parameters for each fault profile and direction.

use super::ProfileName;

/// A direction on the link. `Up` is client to daemon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
}

/// One direction of a simulated link.
#[derive(Clone, Debug)]
pub struct LinkParams {
    /// Bits per second, drawn uniformly in `[min, max]`; `min == max` is a
    /// fixed rate, `u64::MAX` is unlimited.
    pub rate_bps: (u64, u64),
    /// How often the rate is redrawn, drawn in `[min, max]`; ignored when the
    /// rate is fixed.
    pub rate_change_ms: (u64, u64),
    /// Queue limit in bytes; `u64::MAX` means no limit and no queueing delay.
    pub queue_bytes: u64,
    /// One-way delay.
    pub base_delay_ms: u64,
    /// Amplitude of the slowly varying delay offset.
    pub jitter_ms: u64,
    pub loss: GilbertElliott,
    /// Per packet probabilities.
    pub reorder: f64,
    pub duplicate: f64,
    pub cross: Option<CrossTraffic>,
}

/// Bursty loss: a good and a bad state, each with its own loss probability,
/// and per packet chances of switching between them.
#[derive(Clone, Copy, Debug)]
pub struct GilbertElliott {
    pub to_bad: f64,
    pub to_good: f64,
    pub loss_good: f64,
    pub loss_bad: f64,
}

/// Other traffic that shares the link's queue in bursts. During a burst it
/// fills the queue up to `hold_bytes` and keeps it there; above that our
/// packets drain at the rate's remaining share, and only they can fill the
/// rest of the queue, so they compete rather than always losing.
#[derive(Clone, Copy, Debug)]
pub struct CrossTraffic {
    /// Time from one burst's start to the next one's.
    pub every_ms: (u64, u64),
    pub burst_ms: (u64, u64),
    /// Arrival rate as a multiple of the link rate while filling the queue.
    pub load: f64,
    /// The standing queue it keeps during a burst; below `queue_bytes`.
    pub hold_bytes: u64,
    /// Its share of the link rate while it holds the queue.
    pub share: f64,
}

const KIB: u64 = 1024;

/// The parameters of profile `p` in direction `d`.
pub fn link(p: ProfileName, d: Dir) -> LinkParams {
    match p {
        ProfileName::FourGVehicle => LinkParams {
            rate_bps: match d {
                Dir::Down => (2_000_000, 30_000_000),
                Dir::Up => (700_000, 10_000_000),
            },
            rate_change_ms: (2000, 6000),
            queue_bytes: match d {
                Dir::Down => 512 * KIB,
                Dir::Up => 256 * KIB,
            },
            base_delay_ms: 30,
            jitter_ms: 20,
            loss: GilbertElliott {
                to_bad: 0.004,
                to_good: 0.1,
                loss_good: 0.002,
                loss_bad: 0.2,
            },
            reorder: 0.005,
            duplicate: 0.001,
            cross: None,
        },
        ProfileName::DslVideo => LinkParams {
            rate_bps: match d {
                Dir::Down => (16_000_000, 16_000_000),
                Dir::Up => (1_000_000, 1_000_000),
            },
            rate_change_ms: (0, 0),
            queue_bytes: match d {
                Dir::Down => 1536 * KIB,
                Dir::Up => 64 * KIB,
            },
            base_delay_ms: 10,
            jitter_ms: 2,
            loss: GilbertElliott {
                to_bad: 0.0,
                to_good: 1.0,
                loss_good: 0.0001,
                loss_bad: 0.0,
            },
            reorder: 0.0,
            duplicate: 0.0,
            cross: match d {
                Dir::Down => Some(CrossTraffic {
                    every_ms: (2000, 4000),
                    burst_ms: (1000, 2000),
                    load: 2.0,
                    hold_bytes: 1280 * KIB,
                    share: 0.625,
                }),
                Dir::Up => None,
            },
        },
        ProfileName::TestBlackout => LinkParams {
            rate_bps: (u64::MAX, u64::MAX),
            rate_change_ms: (0, 0),
            queue_bytes: u64::MAX,
            base_delay_ms: 0,
            jitter_ms: 0,
            loss: GilbertElliott {
                to_bad: 0.0,
                to_good: 1.0,
                loss_good: 0.0,
                loss_bad: 0.0,
            },
            reorder: 0.0,
            duplicate: 0.0,
            cross: None,
        },
        // For tests only: a slow, clean uplink and an unlimited downlink.
        ProfileName::TestSlow => LinkParams {
            rate_bps: match d {
                Dir::Down => (u64::MAX, u64::MAX),
                Dir::Up => (256_000, 256_000),
            },
            rate_change_ms: (0, 0),
            queue_bytes: match d {
                Dir::Down => u64::MAX,
                Dir::Up => 64 * KIB,
            },
            base_delay_ms: 0,
            jitter_ms: 0,
            loss: GilbertElliott {
                to_bad: 0.0,
                to_good: 1.0,
                loss_good: 0.0,
                loss_bad: 0.0,
            },
            reorder: 0.0,
            duplicate: 0.0,
            cross: None,
        },
    }
}

/// Handovers and the dead zone of a profile.
#[derive(Clone, Debug)]
pub struct OutageParams {
    /// Time from one handover's start to the next one's; `(0, 0)`: none.
    pub handover_every_ms: (u64, u64),
    pub handover_ms: (u64, u64),
    /// How long the dead zone lasts; `(0, 0)`: none.
    pub dead_zone_ms: (u64, u64),
    /// Traffic seen before the dead zone opens.
    pub dead_zone_after_bytes: (u64, u64),
}

/// The outages of profile `p`; `None` when it has none.
pub fn outages(p: ProfileName) -> Option<OutageParams> {
    match p {
        ProfileName::FourGVehicle => Some(OutageParams {
            handover_every_ms: (30_000, 90_000),
            handover_ms: (200, 2000),
            dead_zone_ms: (20_000, 60_000),
            dead_zone_after_bytes: (256 * KIB, 4 * KIB * KIB),
        }),
        ProfileName::DslVideo | ProfileName::TestSlow => None,
        ProfileName::TestBlackout => Some(OutageParams {
            handover_every_ms: (0, 0),
            handover_ms: (0, 0),
            dead_zone_ms: (6000, 6000),
            dead_zone_after_bytes: (256 * KIB, 256 * KIB),
        }),
    }
}
