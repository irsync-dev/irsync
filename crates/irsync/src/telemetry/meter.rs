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
    /// Waiting for the index lock another task holds, or for queued entries
    /// to be written; also counted in the activity that waits.
    IndexWait,
}

/// The number of activities.
pub const ACTIVITIES: usize = Activity::ALL.len();

/// The number of counters.
pub const COUNTERS: usize = Counter::ALL.len();

// The meters index their arrays by discriminant and iterate `ALL`: each
// `ALL` must list every variant at its own discriminant.
const _: () = {
    let mut i = 0;
    while i < ACTIVITIES {
        assert!(Activity::ALL[i] as usize == i);
        i += 1;
    }
    let mut i = 0;
    while i < COUNTERS {
        assert!(Counter::ALL[i] as usize == i);
        i += 1;
    }
};

impl Activity {
    pub const ALL: [Activity; 10] = [
        Activity::Walk,
        Activity::Plan,
        Activity::Hash,
        Activity::Read,
        Activity::Send,
        Activity::Recv,
        Activity::Write,
        Activity::Place,
        Activity::Delete,
        Activity::IndexWait,
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
            Activity::IndexWait => "index_wait",
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

/// Process-level usage over a session. These are figures of the whole
/// process, not of the session: in a daemon serving several sessions at
/// once, each session's figures include the others' work, so they must not
/// be summed across concurrent sessions. (A client runs its sources one
/// after another, so its totals over them hold.)
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcessUsage {
    /// CPU time the whole process spent since the session started.
    pub cpu_user: Duration,
    pub cpu_sys: Duration,
    /// Peak resident set size, in bytes.
    pub peak_rss: u64,
    pub peak_fds: u64,
}

/// QUIC path usage over a session; a zero RTT means no sample was taken.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NetUsage {
    pub rtt_min: Duration,
    pub rtt_max: Duration,
    pub rtt_avg: Duration,
    /// How many RTT samples `rtt_avg` is the mean of, so averages merge
    /// weighted by them.
    pub rtt_samples: u32,
    pub lost_packets: u64,
    pub sent_packets: u64,
    /// Congestion window of the best path at the last sample, in bytes.
    pub cwnd: u64,
}

/// A snapshot of a meter.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Usage {
    pub wall: Duration,
    /// Busy time per activity, indexed by `Activity as usize`: summed over
    /// the tasks doing it, so it can exceed the wall time.
    pub busy: [Duration; ACTIVITIES],
    /// Wall time with at least one task doing each activity, indexed by
    /// `Activity as usize`.
    pub active: [Duration; ACTIVITIES],
    /// Wall time with no activity in progress.
    pub peer_wait: Duration,
    /// Counter values, indexed by `Counter as usize`.
    pub counters: [u64; COUNTERS],
    pub process: ProcessUsage,
    pub net: NetUsage,
}

impl Usage {
    pub fn busy(&self, a: Activity) -> Duration {
        self.busy[a as usize]
    }

    pub fn active(&self, a: Activity) -> Duration {
        self.active[a as usize]
    }

    pub fn count(&self, c: Counter) -> u64 {
        self.counters[c as usize]
    }

    /// Sums `o` into `self`; peaks take the max, the RTT average is
    /// weighted by each side's samples, and the RTT minimum ignores
    /// unsampled (zero) sides.
    pub fn add(&mut self, o: &Usage) {
        self.wall += o.wall;
        for (a, b) in self.busy.iter_mut().zip(&o.busy) {
            *a += *b;
        }
        for (a, b) in self.active.iter_mut().zip(&o.active) {
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
        // An average without a sample count (built by hand) weighs one.
        let weight = |u: &NetUsage| match u.rtt_avg.is_zero() {
            true => 0,
            false => u.rtt_samples.max(1),
        };
        let (wn, wm) = (weight(n), weight(m));
        if wn + wm > 0 {
            n.rtt_avg = (n.rtt_avg * wn + m.rtt_avg * wm) / (wn + wm);
        }
        n.rtt_samples = wn + wm;
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

/// A stopwatch that runs while its condition holds.
#[derive(Clone, Copy, Default)]
struct Clock {
    since: Option<Instant>,
    total: Duration,
}

impl Clock {
    /// Starts or stops the clock to match `running`.
    fn set(&mut self, running: bool, now: Instant) {
        match (running, self.since) {
            (true, None) => self.since = Some(now),
            (false, Some(t)) => {
                self.total += now.duration_since(t);
                self.since = None;
            }
            _ => {}
        }
    }

    fn read(&self, end: Instant) -> Duration {
        self.total + self.since.map_or(Duration::ZERO, |t| end.duration_since(t))
    }
}

struct Clocks {
    /// Runs while no guard is active.
    idle: Clock,
    /// Runs while a guard of that activity is active.
    active: [Clock; ACTIVITIES],
    /// Set by `finish`; the clocks stay stopped afterwards.
    ended: Option<Instant>,
}

/// Measures one session, and mirrors its busy time and counters into a process-wide meter.
pub struct Meter {
    side: Side,
    role: Role,
    /// The daemon export the session serves, for the `export` label.
    export: Option<String>,
    enabled: bool,
    started: Instant,
    busy: [AtomicU64; ACTIVITIES],
    counters: [AtomicU64; COUNTERS],
    /// Guards in progress, in all and per activity.
    guards: AtomicUsize,
    per_activity: [AtomicUsize; ACTIVITIES],
    clocks: Mutex<Clocks>,
    /// A global meter's wall time per activity, summed over finished sessions.
    active_total: [AtomicU64; ACTIVITIES],
    global: Option<&'static Meter>,
}

/// Guard returned by [`Meter::start`]; adds the elapsed time to its activity on drop.
pub struct Timed<'a> {
    live: Option<(&'a Meter, Activity, Instant)>,
}

impl Meter {
    fn build(
        side: Side,
        role: Role,
        export: Option<String>,
        enabled: bool,
        global: Option<&'static Meter>,
    ) -> Meter {
        let now = Instant::now();
        Meter {
            side,
            role,
            export,
            enabled,
            started: now,
            busy: Default::default(),
            counters: Default::default(),
            guards: AtomicUsize::new(0),
            per_activity: Default::default(),
            clocks: Mutex::new(Clocks {
                // A global meter has no clocks of its own: it only sums finished sessions.
                idle: Clock {
                    since: global.map(|_| now),
                    total: Duration::ZERO,
                },
                active: [Clock::default(); ACTIVITIES],
                ended: None,
            }),
            active_total: Default::default(),
            global,
        }
    }

    /// A session meter, of daemon export `export` when there is one; when
    /// `enabled` is false every method is a no-op.
    pub fn new(side: Side, role: Role, export: Option<&str>, enabled: bool) -> Arc<Meter> {
        Arc::new(Meter::build(
            side,
            role,
            export.map(str::to_owned),
            enabled,
            Some(global(side, role)),
        ))
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

    pub fn export(&self) -> Option<&str> {
        self.export.as_deref()
    }

    /// This session's metric labels (see [`super::labels`]).
    pub fn labels(&self) -> Vec<metrics::Label> {
        super::labels(self.side, self.role, self.export())
    }

    /// [`Meter::labels`] plus `key=value`.
    pub fn labels_with(&self, key: &'static str, value: &'static str) -> Vec<metrics::Label> {
        let mut v = self.labels();
        v.push(metrics::Label::from_static_parts(key, value));
        v
    }

    /// Raises gauge `name` for this session until the guard drops.
    pub fn hold(&self, name: &'static str) -> super::Held {
        if !self.enabled {
            return super::Held(None);
        }
        super::hold(name, || self.labels())
    }

    /// Starts timing `a` until the returned guard drops.
    pub fn start(&self, a: Activity) -> Timed<'_> {
        if !self.enabled {
            return Timed { live: None };
        }
        let first = self.guards.fetch_add(1, Relaxed) == 0;
        if self.per_activity[a as usize].fetch_add(1, Relaxed) == 0 || first {
            self.reconcile(a);
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

    /// Re-decides the idle clock and `a`'s clock from the current guard
    /// counts, under the lock.
    fn reconcile(&self, a: Activity) {
        if self.global.is_none() {
            return;
        }
        let mut clocks = self.clocks.lock().unwrap();
        if clocks.ended.is_some() {
            return;
        }
        let now = Instant::now();
        let idle = self.guards.load(Relaxed) == 0;
        clocks.idle.set(idle, now);
        let running = self.per_activity[a as usize].load(Relaxed) > 0;
        clocks.active[a as usize].set(running, now);
    }

    pub fn usage(&self) -> Usage {
        if !self.enabled {
            return Usage::default();
        }
        let clocks = self.clocks.lock().unwrap();
        let end = clocks.ended.unwrap_or_else(Instant::now);
        let mut u = Usage {
            wall: end.duration_since(self.started),
            peer_wait: clocks.idle.read(end),
            ..Usage::default()
        };
        if self.global.is_some() {
            for (d, c) in u.active.iter_mut().zip(&clocks.active) {
                *d = c.read(end);
            }
        }
        drop(clocks);
        if self.global.is_none() {
            for (d, t) in u.active.iter_mut().zip(&self.active_total) {
                *d = Duration::from_nanos(t.load(Relaxed));
            }
        }
        for (d, b) in u.busy.iter_mut().zip(&self.busy) {
            *d = Duration::from_nanos(b.load(Relaxed));
        }
        for (c, v) in u.counters.iter_mut().zip(&self.counters) {
            *c = v.load(Relaxed);
        }
        u
    }

    /// Closes the clocks and adds this session's idle and per-activity wall
    /// time to the global meter once.
    pub fn finish(&self) {
        if !self.enabled {
            return;
        }
        let (idle, active) = {
            let mut clocks = self.clocks.lock().unwrap();
            if clocks.ended.is_some() {
                return;
            }
            let now = Instant::now();
            clocks.idle.set(false, now);
            for c in &mut clocks.active {
                c.set(false, now);
            }
            clocks.ended = Some(now);
            (clocks.idle.total, clocks.active.map(|c| c.total))
        };
        if let Some(g) = self.global {
            g.clocks.lock().unwrap().idle.total += idle;
            for (t, d) in g.active_total.iter().zip(active) {
                t.fetch_add(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX), Relaxed);
            }
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
        let last = m.guards.fetch_sub(1, Relaxed) == 1;
        if m.per_activity[a as usize].fetch_sub(1, Relaxed) == 1 || last {
            m.reconcile(a);
        }
    }
}

/// The process-wide meter for one side and role; never finished.
pub fn global(side: Side, role: Role) -> &'static Meter {
    const SLOTS: [(Side, Role); 4] = [
        (Side::Client, Role::Sender),
        (Side::Client, Role::Receiver),
        (Side::Daemon, Role::Sender),
        (Side::Daemon, Role::Receiver),
    ];
    static METERS: OnceLock<[Meter; 4]> = OnceLock::new();
    let all = METERS.get_or_init(|| SLOTS.map(|(s, r)| Meter::build(s, r, None, true, None)));
    let slot = SLOTS.iter().position(|&k| k == (side, role));
    &all[slot.expect("every side and role has a slot")]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::thread::sleep;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    /// A session meter summing into a parent of its own (`parent`), so a
    /// test never shares the process-wide meters other tests also add to.
    fn under_own_parent(side: Side, role: Role) -> (Meter, &'static Meter) {
        let parent: &'static Meter =
            Box::leak(Box::new(Meter::build(side, role, None, true, None)));
        (Meter::build(side, role, None, true, Some(parent)), parent)
    }

    #[test]
    fn session_meters_sum_into_the_process_wide_ones() {
        let m = Meter::new(Side::Client, Role::Sender, None, true);
        let g = global(Side::Client, Role::Sender);
        assert!(m.global.is_some_and(|p| std::ptr::eq(p, g)));
    }

    #[test]
    fn disabled_meter_records_nothing() {
        let m = Meter::new(Side::Client, Role::Receiver, None, false);
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
        let m = Meter::new(Side::Client, Role::Receiver, None, true);
        {
            let _g = m.start(Activity::Hash);
            sleep(MS(20));
        }
        let b = m.usage().busy(Activity::Hash);
        assert!(b >= MS(20) && b < MS(200), "{b:?}");
    }

    #[test]
    fn idle_is_time_with_no_guard_active() {
        let m = Meter::new(Side::Daemon, Role::Receiver, None, true);
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
        let m = Meter::new(Side::Daemon, Role::Sender, None, true);
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
    fn active_is_wall_time_with_the_activity_in_progress() {
        let m = Meter::new(Side::Client, Role::Receiver, None, true);
        let barrier = Barrier::new(2);
        std::thread::scope(|s| {
            for _ in 0..2 {
                s.spawn(|| {
                    let _g = m.start(Activity::Place);
                    barrier.wait();
                    sleep(MS(30));
                });
            }
        });
        {
            let _g = m.start(Activity::Write);
            sleep(MS(10));
        }
        m.finish();
        let u = m.usage();
        let (busy, active) = (u.busy(Activity::Place), u.active(Activity::Place));
        assert!(busy >= MS(60), "{busy:?}");
        assert!(active >= MS(30) && active < MS(55), "{active:?}");
        let w = u.active(Activity::Write);
        assert!(w >= MS(10) && w < MS(30), "{w:?}");
        assert_eq!(u.active(Activity::Hash), Duration::ZERO);
    }

    #[test]
    fn global_active_is_the_sum_of_finished_sessions() {
        let (m, g) = under_own_parent(Side::Daemon, Role::Receiver);
        {
            let _g = m.start(Activity::Delete);
            sleep(MS(10));
        }
        assert_eq!(g.usage().active(Activity::Delete), Duration::ZERO);
        m.finish();
        m.finish();
        let after = g.usage().active(Activity::Delete);
        assert!(after > Duration::ZERO);
        assert_eq!(after, m.usage().active(Activity::Delete));
    }

    #[test]
    fn usage_add_merges_process_and_net() {
        let mk = |rss, min, max, lost, samples| Usage {
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
                rtt_samples: samples,
                lost_packets: lost,
                cwnd: rss,
                ..NetUsage::default()
            },
            ..Usage::default()
        };
        let mut a = mk(100, 4, 8, 1, 3);
        a.add(&mk(200, 2, 20, 2, 1));
        a.add(&Usage::default());
        assert_eq!(a.process.cpu_user, MS(20));
        assert_eq!((a.process.peak_rss, a.process.peak_fds), (200, 20));
        assert_eq!((a.net.rtt_min, a.net.rtt_max), (MS(2), MS(20)));
        // Weighted by samples: (6 ms * 3 + 11 ms * 1) / 4, not their
        // pairwise mean of 8.5 ms.
        assert_eq!(a.net.rtt_avg, Duration::from_micros(7250));
        assert_eq!(a.net.rtt_samples, 4);
        assert_eq!((a.net.lost_packets, a.net.cwnd), (3, 200));
    }

    #[test]
    fn counters_reach_the_global_meter() {
        let (m, g) = under_own_parent(Side::Client, Role::Sender);
        m.add(Counter::BytesLiteral, 7);
        assert_eq!(g.usage().count(Counter::BytesLiteral), 7);
    }

    #[test]
    fn global_peer_wait_is_the_sum_of_finished_sessions() {
        let (m, g) = under_own_parent(Side::Client, Role::Receiver);
        sleep(MS(10));
        m.finish();
        m.finish();
        assert!(g.usage().peer_wait >= MS(10));
        assert_eq!(g.usage().peer_wait, m.usage().peer_wait);
    }
}
