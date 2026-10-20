//! The Markov chain over operation kinds and the size-cap pressure curve.

use rand::distributions::WeightedIndex;
use rand::Rng;
use rand_chacha::ChaCha8Rng;

use crate::config::{ConfigError, Weights};
use crate::kind::OpKind;

/// A 22x22 transition matrix indexed by [`OpKind::index`]: row = previous
/// kind, column = next kind.
#[derive(Clone, Debug)]
pub struct Chain {
    rows: Vec<[f64; 22]>,
}

impl Chain {
    /// Build the matrix from a weight table.
    pub fn from_weights(w: &Weights) -> Result<Chain, ConfigError> {
        let mut rows = vec![[0.0; 22]; 22];
        for from in OpKind::ALL {
            let row = w
                .get(&from)
                .ok_or_else(|| ConfigError::Invalid(format!("weights: missing row {from:?}")))?;
            for to in OpKind::ALL {
                let v = *row.get(&to).ok_or_else(|| {
                    ConfigError::Invalid(format!("weights: missing {from:?}->{to:?}"))
                })?;
                if !v.is_finite() || v < 0.0 {
                    return Err(ConfigError::Invalid(format!(
                        "weights: {from:?}->{to:?} must be finite and >= 0"
                    )));
                }
                rows[from.index()][to.index()] = v;
            }
            if rows[from.index()].iter().sum::<f64>() <= 0.0 {
                return Err(ConfigError::Invalid(format!(
                    "weights: row {from:?} has no non-zero weight"
                )));
            }
        }
        Ok(Chain { rows })
    }

    /// Draw the next kind after `prev`. `pressure` in [0, 1] damps growing
    /// kinds and boosts shrinking ones.
    pub fn next(&self, rng: &mut ChaCha8Rng, prev: OpKind, pressure: f64) -> OpKind {
        let row = &self.rows[prev.index()];
        let adjusted: Vec<f64> = OpKind::ALL
            .iter()
            .map(|&k| {
                let f = if k.grows() {
                    1.0 - pressure
                } else if k.shrinks() {
                    1.0 + 4.0 * pressure
                } else {
                    1.0
                };
                row[k.index()] * f
            })
            .collect();
        let dist = WeightedIndex::new(&adjusted)
            .or_else(|_| WeightedIndex::new(row.iter()))
            .expect("rows are validated to have a positive sum");
        OpKind::ALL[rng.sample(dist)]
    }

    /// Whether `k` can ever be drawn (non-zero in some row).
    pub fn enabled(&self, k: OpKind) -> bool {
        self.rows.iter().any(|r| r[k.index()] > 0.0)
    }
}

/// 0 below 80% of the cap, rising linearly to 1 at the cap.
pub fn cap_pressure(total: u64, max: u64) -> f64 {
    if max == 0 {
        return 1.0;
    }
    // (total/max - 0.8) / 0.2, rearranged to stay exact at the endpoints.
    ((5.0 * total as f64 - 4.0 * max as f64) / max as f64).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_weights;
    use rand::SeedableRng;

    #[test]
    fn pressure_curve() {
        assert_eq!(cap_pressure(0, 100), 0.0);
        assert_eq!(cap_pressure(80, 100), 0.0);
        assert!((cap_pressure(90, 100) - 0.5).abs() < 1e-9);
        assert_eq!(cap_pressure(100, 100), 1.0);
        assert_eq!(cap_pressure(150, 100), 1.0);
        assert_eq!(cap_pressure(5, 0), 1.0);
    }

    #[test]
    fn default_table_shape() {
        let w = default_weights();
        for k in OpKind::ALL {
            let row = &w[&k];
            assert_eq!(row.len(), 22, "{k:?}");
            for (n, v) in row {
                if n.default_enabled() {
                    assert!(*v > 0.0, "{k:?}->{n:?}")
                } else {
                    assert_eq!(*v, 0.0)
                }
            }
        }
        let after_mkdir = &w[&OpKind::Mkdir];
        let total: f64 = after_mkdir.values().sum();
        assert!((after_mkdir[&OpKind::CreateFile] + after_mkdir[&OpKind::Mkdir]) / total >= 0.6);
        let after_create = &w[&OpKind::CreateFile];
        let t2: f64 = after_create.values().sum();
        assert!(
            (after_create[&OpKind::Append]
                + after_create[&OpKind::OverwriteRange]
                + after_create[&OpKind::CreateFile])
                / t2
                >= 0.5
        );
    }

    #[test]
    fn next_is_deterministic_and_pressure_biases() {
        let c = Chain::from_weights(&default_weights()).unwrap();
        let mut a = ChaCha8Rng::seed_from_u64(7);
        let mut b = ChaCha8Rng::seed_from_u64(7);
        let sa: Vec<_> = (0..200)
            .map(|_| c.next(&mut a, OpKind::CreateFile, 0.0))
            .collect();
        let sb: Vec<_> = (0..200)
            .map(|_| c.next(&mut b, OpKind::CreateFile, 0.0))
            .collect();
        assert_eq!(sa, sb);
        let mut r = ChaCha8Rng::seed_from_u64(1);
        let grow = (0..5000)
            .filter(|_| c.next(&mut r, OpKind::Append, 1.0).grows())
            .count();
        assert_eq!(grow, 0);
        let shrink = (0..5000)
            .filter(|_| c.next(&mut r, OpKind::Append, 1.0).shrinks())
            .count();
        assert!(shrink > 1500);
    }

    #[test]
    fn every_enabled_kind_reachable_in_default_table() {
        let c = Chain::from_weights(&default_weights()).unwrap();
        let mut r = ChaCha8Rng::seed_from_u64(3);
        let mut seen = std::collections::BTreeSet::new();
        let mut k = OpKind::Mkdir;
        for _ in 0..20_000 {
            k = c.next(&mut r, k, 0.0);
            seen.insert(k);
        }
        for k in OpKind::ALL {
            assert_eq!(seen.contains(&k), k.default_enabled(), "{k:?}");
            assert_eq!(c.enabled(k), k.default_enabled(), "{k:?}");
        }
    }

    #[test]
    fn all_grow_row_under_full_pressure_falls_back() {
        let mut w = default_weights();
        let row = w.get_mut(&OpKind::Append).unwrap();
        for k in OpKind::ALL {
            row.insert(k, if k == OpKind::CreateFile { 1.0 } else { 0.0 });
        }
        let c = Chain::from_weights(&w).unwrap();
        let mut r = ChaCha8Rng::seed_from_u64(1);
        assert_eq!(c.next(&mut r, OpKind::Append, 1.0), OpKind::CreateFile);
    }

    #[test]
    fn from_weights_rejects_bad_table() {
        let mut w = default_weights();
        w.get_mut(&OpKind::Mkdir)
            .unwrap()
            .insert(OpKind::Rmdir, -1.0);
        assert!(Chain::from_weights(&w).is_err());
        let mut w = default_weights();
        w.remove(&OpKind::Mkdir);
        assert!(Chain::from_weights(&w).is_err());
    }
}
