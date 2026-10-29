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

/// Process-level usage over a session.
#[derive(Clone, Debug, Default)]
pub struct ProcessUsage {
    /// CPU time the whole process spent since the session started.
    pub cpu_user: Duration,
    pub cpu_sys: Duration,
    /// Peak resident set size, in bytes.
    pub peak_rss: u64,
    pub peak_fds: u64,
}

/// QUIC path usage over a session; a zero RTT means no sample was taken.
#[derive(Clone, Debug, Default)]
pub struct NetUsage {
    pub rtt_min: Duration,
    pub rtt_max: Duration,
    pub rtt_avg: Duration,
    pub lost_packets: u64,
    pub sent_packets: u64,
    /// Congestion window of the best path at the last sample, in bytes.
    pub cwnd: u64,
}

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

    /// Sums `o` into `self`; peaks take the max, and the RTT minimum ignores unsampled (zero) sides.
    pub fn add(&mut self, o: &Usage) {
        self.wall += o.wall;
        for (a, b) in self.busy.iter_mut().zip(&o.busy) {
            *a += *b;
        }
        self.peer_wait += o.peer_wait;
        for (a, b) in self.counters.iter_mut().zip(&o.counters) {
            *a += *b;
        }
        let (p, q) = (&mut self.process, &o.process);
        p.cpu_user += q.cpu_user;
        p.cpu_sys += q.cpu_sys;
        p.peak_rss = p.peak_rss.max(q.peak_rss);
        p.peak_fds = p.peak_fds.max(q.peak_fds);
        let (n, m) = (&mut self.net, &o.net);
        n.rtt_avg = match (n.rtt_avg.is_zero(), m.rtt_avg.is_zero()) {
            (false, false) => (n.rtt_avg + m.rtt_avg) / 2,
            (true, _) => m.rtt_avg,
            _ => n.rtt_avg,
        };
        n.rtt_min = match (n.rtt_min.is_zero(), m.rtt_min.is_zero()) {
            (false, false) => n.rtt_min.min(m.rtt_min),
            (true, _) => m.rtt_min,
            _ => n.rtt_min,
        };
        n.rtt_max = n.rtt_max.max(m.rtt_max);
        n.lost_packets += m.lost_packets;
        n.sent_packets += m.sent_packets;
        n.cwnd = n.cwnd.max(m.cwnd);
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
    side: Side,
    role: Role,
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
    fn build(side: Side, role: Role, enabled: bool, global: Option<&'static Meter>) -> Meter {
        let now = Instant::now();
        Meter {
            side,
            role,
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
        Arc::new(Meter::build(side, role, enabled, Some(global(side, role))))
    }

    /// Whether this meter records anything.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn side(&self) -> Side {
        self.side
    }

    pub fn role(&self) -> Role {
        self.role
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
    let all = METERS.get_or_init(|| {
        std::array::from_fn(|i| {
            let side = if i / 2 == Side::Client as usize {
                Side::Client
            } else {
                Side::Daemon
            };
            let role = if i % 2 == Role::Sender as usize {
                Role::Sender
            } else {
                Role::Receiver
            };
            Meter::build(side, role, true, None)
        })
    });
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
    fn usage_add_merges_process_and_net() {
        let mk = |rss, min, max, lost| Usage {
            process: ProcessUsage {
                cpu_user: MS(10),
                peak_rss: rss,
                peak_fds: rss / 10,
                ..ProcessUsage::default()
            },
            net: NetUsage {
                rtt_min: MS(min),
                rtt_max: MS(max),
                rtt_avg: MS((min + max) / 2),
                lost_packets: lost,
                cwnd: rss,
                ..NetUsage::default()
            },
            ..Usage::default()
        };
        let mut a = mk(100, 4, 8, 1);
        a.add(&mk(200, 2, 20, 2));
        a.add(&Usage::default());
        assert_eq!(a.process.cpu_user, MS(20));
        assert_eq!((a.process.peak_rss, a.process.peak_fds), (200, 20));
        assert_eq!((a.net.rtt_min, a.net.rtt_max), (MS(2), MS(20)));
        assert_eq!(a.net.rtt_avg, Duration::from_micros(8500));
        assert_eq!((a.net.lost_packets, a.net.cwnd), (3, 200));
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
