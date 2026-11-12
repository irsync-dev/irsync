//! How fast a run takes its steps: flat out, at a steady rate, or in
//! bursts. Pace only decides when a step starts, never which one.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

/// The timing of a run's steps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pace {
    /// No waiting between steps.
    #[default]
    Fast,
    /// `per_sec` steps a second: step n starts no earlier than n / per_sec
    /// after the run began.
    Steady { per_sec: u32 },
    /// Steps run freely for `on_ms`, then wait out `off_ms`, repeating,
    /// counted from the run's start.
    Bursts { on_ms: u32, off_ms: u32 },
}

impl Pace {
    /// How long step `n` (counting from 0) must still wait, given the time
    /// elapsed since the run began.
    pub fn delay(&self, n: u64, elapsed: Duration) -> Duration {
        match *self {
            Pace::Fast => Duration::ZERO,
            Pace::Steady { per_sec } => {
                Duration::from_nanos(n.saturating_mul(1_000_000_000) / u64::from(per_sec))
                    .saturating_sub(elapsed)
            }
            Pace::Bursts { on_ms, off_ms } => {
                let on = u128::from(on_ms);
                let cycle = on + u128::from(off_ms);
                let into = (elapsed.as_millis() % cycle) as u64;
                if u128::from(into) < on {
                    Duration::ZERO
                } else {
                    // Round up so the step lands inside the next window.
                    Duration::from_millis(cycle as u64 - into).saturating_sub(Duration::from_nanos(
                        u64::from(elapsed.subsec_nanos() % 1_000_000),
                    ))
                }
            }
        }
    }
}

impl FromStr for Pace {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let bad =
            || format!("invalid pace {s:?}: expected fast, steady:<N> or bursts:<ON_MS>:<OFF_MS>");
        let num = |t: &str| -> Result<u32, String> {
            match t.parse::<u32>() {
                Ok(n) if n > 0 => Ok(n),
                _ => Err(bad()),
            }
        };
        let parts: Vec<&str> = s.split(':').collect();
        match parts.as_slice() {
            ["fast"] => Ok(Pace::Fast),
            ["steady", n] => Ok(Pace::Steady { per_sec: num(n)? }),
            ["bursts", on, off] => Ok(Pace::Bursts {
                on_ms: num(on)?,
                off_ms: num(off)?,
            }),
            _ => Err(bad()),
        }
    }
}

impl fmt::Display for Pace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Pace::Fast => write!(f, "fast"),
            Pace::Steady { per_sec } => write!(f, "steady:{per_sec}"),
            Pace::Bursts { on_ms, off_ms } => write!(f, "bursts:{on_ms}:{off_ms}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pace_parses_and_prints() {
        for (text, pace) in [
            ("fast", Pace::Fast),
            ("steady:50", Pace::Steady { per_sec: 50 }),
            (
                "bursts:200:800",
                Pace::Bursts {
                    on_ms: 200,
                    off_ms: 800,
                },
            ),
        ] {
            assert_eq!(text.parse::<Pace>().unwrap(), pace);
            assert_eq!(pace.to_string(), text);
        }
        for bad in [
            "steady:0",
            "bursts:0:10",
            "bursts:10:0",
            "slow",
            "steady:x",
            "steady",
            "bursts:1",
        ] {
            assert!(bad.parse::<Pace>().is_err(), "{bad}");
        }
    }

    #[test]
    fn delay_follows_the_schedule() {
        let ms = Duration::from_millis;
        assert_eq!(Pace::Fast.delay(9, ms(0)), ms(0));
        let steady = Pace::Steady { per_sec: 100 };
        assert_eq!(steady.delay(0, ms(0)), ms(0));
        assert_eq!(steady.delay(9, ms(30)), ms(60));
        assert_eq!(steady.delay(9, ms(95)), ms(0));
        let bursts = Pace::Bursts {
            on_ms: 200,
            off_ms: 800,
        };
        assert_eq!(bursts.delay(5, ms(150)), ms(0));
        assert_eq!(bursts.delay(5, ms(300)), ms(700));
        assert_eq!(bursts.delay(5, ms(1000)), ms(0));
        assert_eq!(bursts.delay(5, ms(1199)), ms(0));
        assert_eq!(bursts.delay(5, ms(1999)), ms(1));
    }
}
