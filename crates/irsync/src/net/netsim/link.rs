//! One direction of a simulated link: decides each datagram's fate from the
//! link parameters, a seeded random generator and the times handed in. There
//! is no real clock here, so the same seed and the same offers give the same
//! fates.

use std::time::Duration;

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

use super::profile::{CrossTraffic, LinkParams};

/// How often the jitter offset takes a step.
const JITTER_STEP: Duration = Duration::from_millis(100);

pub struct Link {
    params: LinkParams,
    rng: ChaCha8Rng,
    /// Bytes waiting in the queue as of `clock`.
    occupancy: f64,
    /// The time the queue state was last brought up to date.
    clock: Duration,
    /// Current rate in bits per second.
    rate: u64,
    /// When the rate is redrawn next; `None` for a fixed rate.
    next_rate_change: Option<Duration>,
    cross: Option<Cross>,
    /// Gilbert-Elliott state.
    bad: bool,
    /// Signed offset added to the delay, in nanoseconds.
    jitter_ns: i64,
    next_jitter_step: Duration,
    /// The latest delivery time of a packet that was not reordered.
    last_delivery: Duration,
}

struct Cross {
    params: CrossTraffic,
    bursting: bool,
    burst_start: Duration,
    /// When the current burst ends, or the next one starts.
    next_edge: Duration,
}

impl Link {
    pub fn new(params: LinkParams, seed: u64) -> Link {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let (lo, hi) = params.rate_bps;
        let rate = rng.gen_range(lo..=hi);
        let next_rate_change = (lo != hi).then(|| draw_ms(&mut rng, params.rate_change_ms));
        let cross = params.cross.map(|c| Cross {
            params: c,
            bursting: false,
            burst_start: Duration::ZERO,
            next_edge: draw_ms(&mut rng, c.every_ms),
        });
        let j = jitter_amplitude_ns(&params);
        let jitter_ns = if j > 0 { rng.gen_range(-j..=j) } else { 0 };
        Link {
            params,
            rng,
            occupancy: 0.0,
            clock: Duration::ZERO,
            rate,
            next_rate_change,
            cross,
            bad: false,
            jitter_ns,
            next_jitter_step: JITTER_STEP,
            last_delivery: Duration::ZERO,
        }
    }

    /// What happens to one datagram of `len` bytes offered at `now` (time since the link was made):
    /// empty = dropped; otherwise the delivery times (two when duplicated).
    pub fn offer(&mut self, now: Duration, len: usize) -> Vec<Duration> {
        let Some(queueing) = self.enqueue(now, len) else {
            return Vec::new();
        };
        self.step_jitter(now);

        let ge = self.params.loss;
        self.bad = if self.bad {
            !self.rng.gen_bool(ge.to_good)
        } else {
            self.rng.gen_bool(ge.to_bad)
        };
        let lost = self
            .rng
            .gen_bool(if self.bad { ge.loss_bad } else { ge.loss_good });
        let reordered = self.rng.gen_bool(self.params.reorder);
        let extra = self.rng.gen_range(5..=30);
        let duplicated = self.rng.gen_bool(self.params.duplicate);
        if lost {
            return Vec::new();
        }

        let base = now + queueing + Duration::from_millis(self.params.base_delay_ms);
        let offset = Duration::from_nanos(self.jitter_ns.unsigned_abs());
        let mut at = if self.jitter_ns >= 0 {
            base + offset
        } else {
            base.saturating_sub(offset).max(now)
        };
        if reordered {
            at += Duration::from_millis(extra);
        } else {
            at = at.max(self.last_delivery);
            self.last_delivery = at;
        }
        if duplicated {
            vec![at, at + Duration::from_millis(1)]
        } else {
            vec![at]
        }
    }

    /// Puts `len` bytes in the queue at `now`; their queueing delay, or `None`
    /// when the queue has no room.
    fn enqueue(&mut self, now: Duration, len: usize) -> Option<Duration> {
        if self.rate == u64::MAX || self.params.queue_bytes == u64::MAX {
            return Some(Duration::ZERO);
        }
        self.advance(now);
        let after = self.occupancy + len as f64;
        if after > self.params.queue_bytes as f64 {
            return None;
        }
        self.occupancy = after;
        Some(Duration::from_secs_f64(after * 8.0 / self.rate as f64))
    }

    /// Drains the queue up to `now`, in pieces between rate changes and the
    /// edges of cross traffic bursts.
    fn advance(&mut self, now: Duration) {
        while self.clock < now {
            let mut until = now;
            if let Some(t) = self.next_rate_change {
                until = until.min(t);
            }
            if let Some(c) = &self.cross {
                until = until.min(c.next_edge);
            }
            let drained = self.rate as f64 / 8.0 * (until - self.clock).as_secs_f64();
            let arrived = match &self.cross {
                Some(c) if c.bursting => c.params.load * drained,
                _ => 0.0,
            };
            self.occupancy =
                (self.occupancy + arrived - drained).clamp(0.0, self.params.queue_bytes as f64);
            self.clock = until;

            if self.next_rate_change == Some(until) {
                let (lo, hi) = self.params.rate_bps;
                self.rate = self.rng.gen_range(lo..=hi);
                self.next_rate_change =
                    Some(until + draw_ms(&mut self.rng, self.params.rate_change_ms));
            }
            if let Some(c) = &mut self.cross {
                if c.next_edge == until {
                    if c.bursting {
                        c.bursting = false;
                        c.next_edge =
                            (c.burst_start + draw_ms(&mut self.rng, c.params.every_ms)).max(until);
                    } else {
                        c.bursting = true;
                        c.burst_start = until;
                        c.next_edge = until + draw_ms(&mut self.rng, c.params.burst_ms);
                    }
                }
            }
        }
    }

    /// Moves the jitter offset in small random steps, one per `JITTER_STEP`
    /// up to `now`, keeping it within the amplitude.
    fn step_jitter(&mut self, now: Duration) {
        let j = jitter_amplitude_ns(&self.params);
        while self.next_jitter_step <= now {
            if j > 0 {
                let step = j / 4;
                self.jitter_ns = (self.jitter_ns + self.rng.gen_range(-step..=step)).clamp(-j, j);
            }
            self.next_jitter_step += JITTER_STEP;
        }
    }
}

fn draw_ms(rng: &mut ChaCha8Rng, (lo, hi): (u64, u64)) -> Duration {
    Duration::from_millis(rng.gen_range(lo..=hi))
}

fn jitter_amplitude_ns(p: &LinkParams) -> i64 {
    i64::try_from(p.jitter_ms.saturating_mul(1_000_000)).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::netsim::profile::{link, Dir};
    use crate::net::netsim::ProfileName;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn same_seed_same_fates() {
        let run = || {
            let mut l = Link::new(link(ProfileName::FourGVehicle, Dir::Down), 7);
            (0..5000).map(|i| l.offer(ms(i), 1200)).collect::<Vec<_>>()
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn rate_limits_throughput() {
        // dsl-video Up: 1 Mbit/s, 64 KiB queue
        let mut l = Link::new(link(ProfileName::DslVideo, Dir::Up), 1);
        // offered: 10 Mbit/s for 10 s
        let delivered = (0..10_000)
            .filter(|i| !l.offer(ms(*i), 1250).is_empty())
            .count();
        // ≈ 1 Mbit/s × 10 s / 10 kbit
        assert!((900..=1_100).contains(&delivered), "{delivered}");
    }

    #[test]
    fn full_queue_drops_and_delays() {
        // the last accepted packet waits ≈ 64 KiB at 1 Mbit/s ≈ 524 ms
        let mut l = Link::new(link(ProfileName::DslVideo, Dir::Up), 1);
        let fates: Vec<_> = (0..200).map(|_| l.offer(ms(0), 1250)).collect();
        assert!(fates.iter().any(|f| f.is_empty()));
        let last = fates.iter().rev().find(|f| !f.is_empty()).unwrap()[0];
        assert!((ms(480)..=ms(560)).contains(&last), "{last:?}");
    }

    #[test]
    fn cross_traffic_causes_bufferbloat() {
        // dsl-video Down: some packet within 10 s waits ≥ 300 ms
        let mut l = Link::new(link(ProfileName::DslVideo, Dir::Down), 3);
        let worst = (0..10_000)
            .filter_map(|i| l.offer(ms(i), 300).first().map(|d| *d - ms(i)))
            .max()
            .unwrap();
        assert!(worst >= ms(300), "{worst:?}");
    }

    #[test]
    fn jitter_alone_keeps_order() {
        // 4g with reorder and duplicate set to 0
        let mut p = link(ProfileName::FourGVehicle, Dir::Down);
        p.reorder = 0.0;
        p.duplicate = 0.0;
        let mut l = Link::new(p, 5);
        let times: Vec<_> = (0..20_000).flat_map(|i| l.offer(ms(i / 4), 200)).collect();
        assert!(times.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn four_g_loses_about_one_percent_in_bursts() {
        let mut l = Link::new(link(ProfileName::FourGVehicle, Dir::Down), 11);
        let lost: Vec<bool> = (0..200_000)
            .map(|i| l.offer(ms(i), 100).is_empty())
            .collect();
        let rate = lost.iter().filter(|x| **x).count() as f64 / lost.len() as f64;
        assert!((0.004..=0.02).contains(&rate), "{rate}");
        let runs = lost.windows(2).filter(|w| w[0] && w[1]).count();
        assert!(runs > 0, "losses never come in bursts");
    }

    #[test]
    fn test_blackout_passes_everything_at_once() {
        let mut l = Link::new(link(ProfileName::TestBlackout, Dir::Up), 1);
        assert!((0..1000).all(|i| l.offer(ms(i), 1500) == vec![ms(i)]));
    }
}
