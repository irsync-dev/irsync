//! CPU facts for the progress line: how many cores the machine has of each
//! kind. The calling thread's CPU time is `platform::thread_cpu_time`.

use crate::platform;

/// The cores this process may run on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cores {
    /// Logical CPUs available to the process.
    pub logical: usize,
    /// Performance and efficiency cores (logical CPUs of each), on machines
    /// that have both kinds and say so.
    pub split: Option<(usize, usize)>,
}

impl Cores {
    pub fn detect() -> Cores {
        Cores {
            logical: std::thread::available_parallelism().map_or(1, |n| n.get()),
            split: platform::core_split(),
        }
    }
}

/// Number of CPUs in a kernel CPU list such as `0-11,16,18-19`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn cpu_list_len(s: &str) -> Option<usize> {
    let s = s.trim();
    if s.is_empty() {
        return Some(0);
    }
    s.split(',').try_fold(0, |n, part| {
        let len = match part.split_once('-') {
            Some((a, b)) => {
                let (a, b): (usize, usize) = (a.parse().ok()?, b.parse().ok()?);
                b.checked_sub(a)? + 1
            }
            None => {
                part.parse::<usize>().ok()?;
                1
            }
        };
        Some(n + len)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_list_len_counts_ranges_and_singles() {
        assert_eq!(cpu_list_len("0-11\n"), Some(12));
        assert_eq!(cpu_list_len("0-3,8,10-11"), Some(7));
        assert_eq!(cpu_list_len("5"), Some(1));
        assert_eq!(cpu_list_len(""), Some(0));
        assert_eq!(cpu_list_len("3-1"), None);
        assert_eq!(cpu_list_len("x"), None);
    }

    #[test]
    fn thread_cpu_time_grows_with_work() {
        let Some(before) = platform::thread_cpu_time() else {
            return;
        };
        let mut x = 0u64;
        for i in 0..20_000_000u64 {
            x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(i));
        }
        assert!(platform::thread_cpu_time().unwrap() > before);
    }

    #[test]
    fn detect_is_consistent() {
        let c = Cores::detect();
        assert!(c.logical >= 1);
        if let Some((p, e)) = c.split {
            assert!(p > 0 && e > 0);
        }
    }
}
