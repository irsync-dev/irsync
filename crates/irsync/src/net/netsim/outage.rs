//! When the simulated link is down: short handovers on a timetable and one
//! dead zone that opens after enough traffic. Pure: the times are handed in.

use std::time::Duration;

use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;

use super::profile::OutageParams;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Up,
    Handover,
    DeadZone,
}

/// What the transport logs.
pub enum Change {
    DeadZoneOpened { secs: u64 },
    DeadZoneClosed,
}

pub struct Outages {
    params: OutageParams,
    rng: ChaCha8Rng,
    bytes: u64,
    /// When the next handover starts; `None` if there are none.
    next_handover: Option<Duration>,
    handover_end: Duration,
    dead_zone: Option<DeadZone>,
}

struct DeadZone {
    after_bytes: u64,
    length: Duration,
    /// Start and end, once open.
    window: Option<(Duration, Duration)>,
    done: bool,
}

impl Outages {
    pub fn new(params: OutageParams, seed: u64) -> Outages {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let next_handover = (params.handover_every_ms != (0, 0))
            .then(|| draw_ms(&mut rng, params.handover_every_ms));
        let dead_zone = (params.dead_zone_ms != (0, 0)).then(|| DeadZone {
            after_bytes: draw(&mut rng, params.dead_zone_after_bytes),
            length: draw_ms(&mut rng, params.dead_zone_ms),
            window: None,
            done: false,
        });
        Outages {
            params,
            rng,
            bytes: 0,
            next_handover,
            handover_end: Duration::ZERO,
            dead_zone,
        }
    }

    /// Counts `len` bytes of traffic (either direction) at `now` and says whether the link is up.
    /// Returns the state and, when it changed into or out of the dead zone, the change to log.
    pub fn packet(&mut self, now: Duration, len: usize) -> (State, Option<Change>) {
        self.bytes += len as u64;
        let mut change = None;

        if let Some(dz) = &mut self.dead_zone {
            match dz.window {
                None if !dz.done && self.bytes >= dz.after_bytes => {
                    dz.window = Some((now, now + dz.length));
                    change = Some(Change::DeadZoneOpened {
                        secs: dz.length.as_secs(),
                    });
                }
                Some((_, end)) if now >= end => {
                    dz.window = None;
                    dz.done = true;
                    change = Some(Change::DeadZoneClosed);
                }
                _ => {}
            }
        }

        // Handovers that start inside the dead zone are skipped.
        while let Some(start) = self.next_handover.filter(|s| *s <= now) {
            let length = draw_ms(&mut self.rng, self.params.handover_ms);
            self.next_handover =
                Some(start + draw_ms(&mut self.rng, self.params.handover_every_ms));
            let in_dead_zone = matches!(
                self.dead_zone.as_ref().and_then(|d| d.window),
                Some((from, to)) if (from..to).contains(&start)
            );
            if !in_dead_zone {
                self.handover_end = self.handover_end.max(start + length);
            }
        }

        let in_dead_zone = self.dead_zone.as_ref().is_some_and(|d| d.window.is_some());
        let state = if in_dead_zone {
            State::DeadZone
        } else if now < self.handover_end {
            State::Handover
        } else {
            State::Up
        };
        (state, change)
    }
}

fn draw(rng: &mut ChaCha8Rng, (lo, hi): (u64, u64)) -> u64 {
    rng.random_range(lo..=hi)
}

fn draw_ms(rng: &mut ChaCha8Rng, range: (u64, u64)) -> Duration {
    Duration::from_millis(draw(rng, range))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::netsim::profile::outages;
    use crate::net::netsim::ProfileName;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Lengths of the consecutive runs of `which`.
    fn spans_of(states: &[State], which: State) -> Vec<u64> {
        let mut spans = vec![];
        let mut run = 0;
        for s in states {
            if *s == which {
                run += 1;
            } else if run > 0 {
                spans.push(run);
                run = 0;
            }
        }
        if run > 0 {
            spans.push(run);
        }
        spans
    }

    #[test]
    fn exactly_one_dead_zone_after_the_threshold() {
        let mut o = Outages::new(outages(ProfileName::TestBlackout).unwrap(), 1);
        let mut changes = vec![];
        for i in 0..20_000u64 {
            // 1 KiB per ms: threshold reached at ~256 ms
            let (s, c) = o.packet(ms(i), 1024);
            if let Some(c) = c {
                changes.push((i, c));
            }
            if i < 250 {
                assert_eq!(s, State::Up);
            }
        }
        assert!(
            matches!(changes[..], [(a, Change::DeadZoneOpened { secs: 6 }), (b, Change::DeadZoneClosed)] if b - a == 6000)
        );
    }

    #[test]
    fn no_dead_zone_before_enough_traffic() {
        let mut o = Outages::new(outages(ProfileName::TestBlackout).unwrap(), 1);
        // 100 KB < 256 KiB
        assert!((0..100_000u64).all(|i| o.packet(ms(i), 1).0 == State::Up));
    }

    #[test]
    fn four_g_handovers_are_short_and_spread() {
        let mut o = Outages::new(outages(ProfileName::FourGVehicle).unwrap(), 9);
        // 600 s at one small packet per ms, too little traffic for the dead zone's threshold
        let states: Vec<State> = (0..600_000u64).map(|i| o.packet(ms(i), 0).0).collect();
        let spans = spans_of(&states, State::Handover);
        // one per 30-90 s
        assert!((6..=21).contains(&spans.len()), "{}", spans.len());
        assert!(spans.iter().all(|n| (200..=2000).contains(n)));
    }

    #[test]
    fn dsl_video_has_no_outages() {
        assert!(outages(ProfileName::DslVideo).is_none());
    }
}
