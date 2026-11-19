//! CPU facts for the progress line: the calling thread's CPU time, and how many
//! cores the machine has of each kind.

use std::time::Duration;

/// CPU time the calling thread has used so far (user and system), or `None`
/// if the clock cannot be read.
pub fn thread_cpu_time() -> Option<Duration> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `&mut ts` points to a local, initialised timespec that
    // outlives the call; clock_gettime writes only that one struct.
    let ret = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    if ret != 0 {
        return None;
    }
    Some(Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32))
}

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
            split: split(),
        }
    }
}

/// Apple silicon: `hw.perflevel0` is the performance cores, the levels after
/// it are efficiency cores.
#[cfg(target_os = "macos")]
fn split() -> Option<(usize, usize)> {
    fn sysctl_u32(name: &std::ffi::CStr) -> Option<u32> {
        let mut value: u32 = 0;
        let mut size = std::mem::size_of::<u32>();
        // SAFETY: `name` is a CStr, so NUL-terminated. `value` is a local
        // u32 and `size` holds its length, so sysctlbyname writes at most
        // 4 bytes there (and the size it wrote into `size`). No new value
        // is set: null and 0.
        let ret = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                (&mut value as *mut u32).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        (ret == 0 && size == std::mem::size_of::<u32>()).then_some(value)
    }
    if sysctl_u32(c"hw.nperflevels")? < 2 {
        return None;
    }
    let p = sysctl_u32(c"hw.perflevel0.logicalcpu")? as usize;
    let e = sysctl_u32(c"hw.perflevel1.logicalcpu")? as usize;
    (p > 0 && e > 0).then_some((p, e))
}

/// Intel hybrid CPUs list their performance and efficiency cores as two PMU
/// devices.
#[cfg(target_os = "linux")]
fn split() -> Option<(usize, usize)> {
    let count = |path: &str| {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| cpu_list_len(&s))
    };
    let p = count("/sys/devices/cpu_core/cpus")?;
    let e = count("/sys/devices/cpu_atom/cpus")?;
    (p > 0 && e > 0).then_some((p, e))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn split() -> Option<(usize, usize)> {
    None
}

/// Number of CPUs in a kernel CPU list such as `0-11,16,18-19`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn cpu_list_len(s: &str) -> Option<usize> {
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
        let Some(before) = thread_cpu_time() else {
            return;
        };
        let mut x = 0u64;
        for i in 0..20_000_000u64 {
            x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(i));
        }
        assert!(thread_cpu_time().unwrap() > before);
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
