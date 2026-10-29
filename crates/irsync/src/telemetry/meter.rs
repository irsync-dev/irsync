//! Per-session meter: busy time per activity, idle time as peer wait, and counters.

use super::{Role, Side};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// A kind of work a session spends time on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Walk,
    Plan,
    Hash,
    Read,
    Send,
    Recv,
    Write,
    Place,
    Delete,
}

impl Activity {
    pub const ALL: [Activity; 9] = [
        Activity::Walk,
        Activity::Plan,
        Activity::Hash,
        Activity::Read,
        Activity::Send,
        Activity::Recv,
        Activity::Write,
        Activity::Place,
        Activity::Delete,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Activity::Walk => "walk",
            Activity::Plan => "plan",
            Activity::Hash => "hash",
            Activity::Read => "read",
            Activity::Send => "send",
            Activity::Recv => "recv",
            Activity::Write => "write",
            Activity::Place => "place",
            Activity::Delete => "delete",
        }
    }
}

/// A monotonic count a session accumulates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Counter {
    BytesLiteral,
    BytesMatched,
    BytesHashed,
    FilesHashed,
    DiskRead,
    DiskWrite,
    FsyncNanos,
}

impl Counter {
    pub const ALL: [Counter; 7] = [
        Counter::BytesLiteral,
        Counter::BytesMatched,
        Counter::BytesHashed,
        Counter::FilesHashed,
        Counter::DiskRead,
        Counter::DiskWrite,
        Counter::FsyncNanos,
    ];
}

/// Process-level usage (filled in by a later task).
#[derive(Clone, Debug, Default)]
pub struct ProcessUsage {}

/// Network usage (filled in by a later task).
#[derive(Clone, Debug, Default)]
pub struct NetUsage {}

/// A snapshot of a meter.
#[derive(Clone, Debug, Default)]
pub struct Usage {
    pub wall: Duration,
    /// Busy time per activity, indexed by `Activity as usize`.
    pub busy: [Duration; 9],
    /// Wall time with no activity in progress.
    pub peer_wait: Duration,
    /// Counter values, indexed by `Counter as usize`.
    pub counters: [u64; 7],
    pub process: ProcessUsage,
    pub net: NetUsage,
}

impl Usage {
    pub fn busy(&self, a: Activity) -> Duration {
        self.busy[a as usize]
    }

    pub fn count(&self, c: Counter) -> u64 {
        self.counters[c as usize]
    }

    /// Sums `o` into `self`; peaks (none yet) take the max.
    pub fn add(&mut self, o: &Usage) {
        self.wall += o.wall;
        for (a, b) in self.busy.iter_mut().zip(&o.busy) {
            *a += *b;
        }
        self.peer_wait += o.peer_wait;
        for (a, b) in self.counters.iter_mut().zip(&o.counters) {
            *a += *b;
        }
    }
}

struct IdleClock {
    /// Set while no guard is active.
    since: Option<Instant>,
    total: Duration,
    /// Set by `finish`; the clock stays stopped afterwards.
    ended: Option<Instant>,
}

/// Measures one session, and mirrors its busy time and counters into a process-wide meter.
pub struct Meter {
    enabled: bool,
    started: Instant,
    busy: [AtomicU64; 9],
    counters: [AtomicU64; 7],
    active: AtomicUsize,
    idle: Mutex<IdleClock>,
    global: Option<&'static Meter>,
}

/// Guard returned by [`Meter::start`]; adds the elapsed time to its activity on drop.
pub struct Timed<'a> {
    live: Option<(&'a Meter, Activity, Instant)>,
}

impl Meter {
    fn build(enabled: bool, global: Option<&'static Meter>) -> Meter {
        let now = Instant::now();
        Meter {
            enabled,
            started: now,
            busy: Default::default(),
            counters: Default::default(),
            active: AtomicUsize::new(0),
            idle: Mutex::new(IdleClock {
                // A global meter has no clock of its own: it only sums finished sessions.
                since: global.map(|_| now),
                total: Duration::ZERO,
                ended: None,
            }),
            global,
        }
    }

    /// A session meter; when `enabled` is false every method is a no-op.
    pub fn new(side: Side, role: Role, enabled: bool) -> Arc<Meter> {
        Arc::new(Meter::build(enabled, Some(global(side, role))))
    }

    /// Starts timing `a` until the returned guard drops.
    pub fn start(&self, a: Activity) -> Timed<'_> {
        if !self.enabled {
            return Timed { live: None };
        }
        if self.active.fetch_add(1, Relaxed) == 0 {
            self.reconcile_idle();
        }
        Timed {
            live: Some((self, a, Instant::now())),
        }
    }

    /// Adds `n` to counter `c`.
    pub fn add(&self, c: Counter, n: u64) {
        if !self.enabled {
            return;
        }
        self.counters[c as usize].fetch_add(n, Relaxed);
        if let Some(g) = self.global {
            g.counters[c as usize].fetch_add(n, Relaxed);
        }
    }

    /// Re-decides the idle clock from the current guard count, under the lock.
    fn reconcile_idle(&self) {
        if self.global.is_none() {
            return;
        }
        let mut clock = self.idle.lock().unwrap();
        if clock.ended.is_some() {
            return;
        }
        let active = self.active.load(Relaxed);
        match (active == 0, clock.since) {
            (true, None) => clock.since = Some(Instant::now()),
            (false, Some(t)) => {
                clock.total += t.elapsed();
                clock.since = None;
            }
            _ => {}
        }
    }

    pub fn usage(&self) -> Usage {
        if !self.enabled {
            return Usage::default();
        }
        let clock = self.idle.lock().unwrap();
        let end = clock.ended.unwrap_or_else(Instant::now);
        let open = clock
            .since
            .map_or(Duration::ZERO, |t| end.duration_since(t));
        let mut u = Usage {
            wall: end.duration_since(self.started),
            peer_wait: clock.total + open,
            ..Usage::default()
        };
        drop(clock);
        for (d, b) in u.busy.iter_mut().zip(&self.busy) {
            *d = Duration::from_nanos(b.load(Relaxed));
        }
        for (c, v) in u.counters.iter_mut().zip(&self.counters) {
            *c = v.load(Relaxed);
        }
        u
    }

    /// Closes the idle clock and adds this session's idle time to the global meter once.
    pub fn finish(&self) {
        if !self.enabled {
            return;
        }
        let idle = {
            let mut clock = self.idle.lock().unwrap();
            if clock.ended.is_some() {
                return;
            }
            let now = Instant::now();
            if let Some(t) = clock.since.take() {
                clock.total += now.duration_since(t);
            }
            clock.ended = Some(now);
            clock.total
        };
        if let Some(g) = self.global {
            let mut gc = g.idle.lock().unwrap();
            gc.total += idle;
        }
    }
}

impl Drop for Timed<'_> {
    fn drop(&mut self) {
        let Some((m, a, t)) = self.live.take() else {
            return;
        };
        let ns = u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX);
        m.busy[a as usize].fetch_add(ns, Relaxed);
        if let Some(g) = m.global {
            g.busy[a as usize].fetch_add(ns, Relaxed);
        }
        if m.active.fetch_sub(1, Relaxed) == 1 {
            m.reconcile_idle();
        }
    }
}

/// The process-wide meter for one side and role; never finished.
pub fn global(side: Side, role: Role) -> &'static Meter {
    static METERS: OnceLock<[Meter; 4]> = OnceLock::new();
    let all = METERS.get_or_init(|| std::array::from_fn(|_| Meter::build(true, None)));
    &all[(side as usize) * 2 + role as usize]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::thread::sleep;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn disabled_meter_records_nothing() {
        let m = Meter::new(Side::Client, Role::Receiver, false);
        {
            let _g = m.start(Activity::Hash);
            sleep(MS(5));
        }
        m.add(Counter::DiskRead, 10);
        let u = m.usage();
        assert_eq!(u.busy(Activity::Hash), Duration::ZERO);
        assert_eq!(u.count(Counter::DiskRead), 0);
        assert_eq!(u.wall, Duration::ZERO);
        assert_eq!(u.peer_wait, Duration::ZERO);
    }

    #[test]
    fn timed_guard_adds_busy_time() {
        let m = Meter::new(Side::Client, Role::Receiver, true);
        {
            let _g = m.start(Activity::Hash);
            sleep(MS(20));
        }
        let b = m.usage().busy(Activity::Hash);
        assert!(b >= MS(20) && b < MS(200), "{b:?}");
    }

    #[test]
    fn idle_is_time_with_no_guard_active() {
        let m = Meter::new(Side::Daemon, Role::Receiver, true);
        sleep(MS(30));
        {
            let _g = m.start(Activity::Recv);
            sleep(MS(30));
        }
        m.finish();
        let u = m.usage();
        assert!(
            u.peer_wait >= MS(25) && u.peer_wait < MS(100),
            "{:?}",
            u.peer_wait
        );
        assert!(u.wall >= MS(60), "{:?}", u.wall);
    }

    #[test]
    fn overlapping_guards_do_not_count_as_idle() {
        let m = Meter::new(Side::Daemon, Role::Sender, true);
        let barrier = Barrier::new(2);
        std::thread::scope(|s| {
            for _ in 0..2 {
                s.spawn(|| {
                    let _g = m.start(Activity::Read);
                    barrier.wait();
                    sleep(MS(30));
                });
            }
        });
        m.finish();
        let u = m.usage();
        assert!(
            u.busy(Activity::Read) >= MS(60),
            "{:?}",
            u.busy(Activity::Read)
        );
        assert!(u.peer_wait < MS(15), "{:?}", u.peer_wait);
    }

    #[test]
    fn counters_reach_the_global_meter() {
        let g = global(Side::Client, Role::Sender);
        let before = g.usage().count(Counter::BytesLiteral);
        let m = Meter::new(Side::Client, Role::Sender, true);
        m.add(Counter::BytesLiteral, 7);
        assert_eq!(g.usage().count(Counter::BytesLiteral), before + 7);
    }

    #[test]
    fn global_peer_wait_is_the_sum_of_finished_sessions() {
        let g = global(Side::Client, Role::Receiver);
        let before = g.usage().peer_wait;
        let m = Meter::new(Side::Client, Role::Receiver, true);
        sleep(MS(10));
        m.finish();
        m.finish();
        assert_eq!(g.usage().peer_wait, before + m.usage().peer_wait);
    }
}
