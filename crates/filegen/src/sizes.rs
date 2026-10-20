//! File size, append, range and truncate draws.
//!
//! Every function consumes a fixed number of rng draws per call whatever
//! the outcome (band, caps, zero budget), so runs that differ only in what
//! the caps allow stay in step on the rng stream.

use rand::Rng;
use rand_chacha::ChaCha8Rng;

use crate::config::SizeDistribution;

/// Draw a new file's size: band by weight, log-uniform within the band,
/// then capped by `max_file` and `budget`. Two rng draws.
pub fn draw_file_size(
    rng: &mut ChaCha8Rng,
    dist: &SizeDistribution,
    max_file: u64,
    budget: u64,
) -> u64 {
    draw_banded(rng, dist, max_file).min(max_file).min(budget)
}

/// Draw an append amount for a file of `current` bytes, from the same
/// distribution, so that `current + delta <= max_file` and
/// `delta <= budget`. Two rng draws.
pub fn draw_delta(
    rng: &mut ChaCha8Rng,
    dist: &SizeDistribution,
    max_file: u64,
    current: u64,
    budget: u64,
) -> u64 {
    draw_banded(rng, dist, max_file)
        .min(max_file.saturating_sub(current))
        .min(budget)
}

/// Draw `(offset, length)` inside a file of `len` bytes: length
/// log-uniform in `[1, len]`, offset uniform in `[0, len - length]`.
/// `(0, 0)` for an empty file. Two rng draws.
pub fn draw_range(rng: &mut ChaCha8Rng, len: u64) -> (u64, u64) {
    let u_len: f64 = rng.gen();
    let u_off: f64 = rng.gen();
    if len == 0 {
        return (0, 0);
    }
    let length = log_uniform(1, len, u_len);
    let offset = scale(u_off, len - length + 1);
    (offset, length)
}

/// Draw a truncation target strictly below `len` (0 for `len == 0`):
/// 0 with probability 0.1, else uniform in `[0, len)`. Two rng draws.
pub fn draw_truncate(rng: &mut ChaCha8Rng, len: u64) -> u64 {
    let to_zero: f64 = rng.gen();
    let u: f64 = rng.gen();
    if len == 0 || to_zero < 0.1 {
        return 0;
    }
    scale(u, len)
}

/// Band by weight (one draw), then log-uniform within it (one draw).
/// `large_max` is the top of the large band. Uncapped.
fn draw_banded(rng: &mut ChaCha8Rng, dist: &SizeDistribution, large_max: u64) -> u64 {
    let weights = [
        dist.empty,
        dist.small.weight,
        dist.medium.weight,
        dist.large_weight,
    ];
    let band = pick_weighted(&weights, rng.gen());
    let u: f64 = rng.gen();
    let small_max = dist.small.max.0;
    let medium_max = dist.medium.max.0;
    match band {
        Some(1) => log_uniform(1, small_max, u),
        Some(2) => log_uniform(small_max.saturating_add(1), medium_max, u),
        Some(3) => log_uniform(medium_max.saturating_add(1), large_max, u),
        _ => 0,
    }
}

/// Index chosen by `u` in `[0, 1)` over non-negative weights; `None` if no
/// weight is positive and finite.
fn pick_weighted(weights: &[f64], u: f64) -> Option<usize> {
    let ok = |w: f64| w.is_finite() && w > 0.0;
    let total: f64 = weights.iter().copied().filter(|&w| ok(w)).sum();
    if !(total > 0.0 && total.is_finite()) {
        return None;
    }
    let mut target = u * total;
    let mut last = None;
    for (i, &w) in weights.iter().enumerate() {
        if !ok(w) {
            continue;
        }
        if target < w {
            return Some(i);
        }
        target -= w;
        last = Some(i);
    }
    last
}

/// Log-uniform integer in `[lo, hi]` from `u` in `[0, 1)`; `lo` if the
/// band is empty (`hi <= lo`). Requires `lo >= 1`.
fn log_uniform(lo: u64, hi: u64, u: f64) -> u64 {
    if hi <= lo {
        return lo;
    }
    let (a, b) = ((lo as f64).ln(), (hi as f64 + 1.0).ln());
    let v = (a + u * (b - a)).exp().floor();
    // `as` saturates; clamp absorbs float rounding at either end.
    (v as u64).clamp(lo, hi)
}

/// Uniform integer in `[0, n)` from `u` in `[0, 1)`; requires `n >= 1`.
fn scale(u: f64, n: u64) -> u64 {
    ((u * n as f64) as u64).min(n - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    #[test]
    fn sizes_within_caps_and_long_tailed() {
        let d = SizeDistribution::default();
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let v: Vec<u64> = (0..10_000)
            .map(|_| draw_file_size(&mut rng, &d, 256 << 20, u64::MAX))
            .collect();
        assert!(v.iter().all(|&s| s <= 256 << 20));
        assert!(v.iter().filter(|&&s| s == 0).count() > 200);
        assert!(v.iter().filter(|&&s| s > 8 << 20).count() > 100);
        assert!(v.iter().filter(|&&s| s < 64 << 10).count() > 5000);
        assert!(draw_file_size(&mut rng, &d, 256 << 20, 10) <= 10);
        assert_eq!(draw_file_size(&mut rng, &d, 256 << 20, 0), 0);
    }

    #[test]
    fn range_and_truncate_bounds() {
        let mut rng = ChaCha8Rng::seed_from_u64(2);
        for _ in 0..1000 {
            let (o, l) = draw_range(&mut rng, 1000);
            assert!(l >= 1 && o + l <= 1000);
            assert!(draw_truncate(&mut rng, 1000) < 1000);
        }
        assert_eq!(draw_truncate(&mut rng, 1), 0);
        assert_eq!(draw_range(&mut rng, 0), (0, 0));
        assert_eq!(draw_truncate(&mut rng, 0), 0);
        assert_eq!(draw_range(&mut rng, 1), (0, 1));
    }

    #[test]
    fn truncate_hits_zero_often() {
        let mut rng = ChaCha8Rng::seed_from_u64(3);
        let zeros = (0..10_000)
            .filter(|_| draw_truncate(&mut rng, 1 << 30) == 0)
            .count();
        assert!(zeros >= 900, "zeros = {zeros}");
    }

    #[test]
    fn delta_respects_caps() {
        let d = SizeDistribution::default();
        let mut rng = ChaCha8Rng::seed_from_u64(4);
        let max: u64 = 16 << 20;
        let mut grew = 0;
        for i in 0..5000u64 {
            let current = (i * 7919) % (max + 100);
            let budget = (i * 104_729) % (4 << 20);
            let delta = draw_delta(&mut rng, &d, max, current, budget);
            assert!(delta <= budget);
            if current >= max {
                assert_eq!(delta, 0);
            } else {
                assert!(current + delta <= max);
            }
            if delta > 0 {
                grew += 1;
            }
        }
        assert!(grew > 2500, "grew = {grew}");
    }

    #[test]
    fn draw_count_is_constant() {
        // Whatever the outcome, each call consumes the same rng draws, so the
        // stream position does not depend on caps or the band drawn.
        let d = SizeDistribution::default();
        let mut a = ChaCha8Rng::seed_from_u64(5);
        let mut b = ChaCha8Rng::seed_from_u64(5);
        for _ in 0..200 {
            draw_file_size(&mut a, &d, 256 << 20, u64::MAX);
            draw_file_size(&mut b, &d, 256 << 20, 0);
            draw_delta(&mut a, &d, 256 << 20, 0, u64::MAX);
            draw_delta(&mut b, &d, 256 << 20, 300 << 20, 0);
            draw_truncate(&mut a, 1000);
            draw_truncate(&mut b, 0);
        }
        assert_eq!(a.get_word_pos(), b.get_word_pos());
    }
}
