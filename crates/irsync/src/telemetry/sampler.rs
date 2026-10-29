//! Samples process usage and the connection's QUIC path once a second while a session runs.

use super::meter::{Meter, NetUsage, ProcessUsage};
use super::process::{self, ProcessSample};
use iroh::endpoint::Connection;
use noq_proto::PathId;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinHandle;

const INTERVAL: Duration = Duration::from_secs(1);

/// What one reading of the connection's best path saw.
struct PathSample {
    rtt: Duration,
    cwnd: u64,
}

#[derive(Default)]
struct Acc {
    process: ProcessUsage,
    net: NetUsage,
    rtt_sum: Duration,
    rtt_samples: u32,
    last_lost: u64,
}

struct Inner {
    conn: Connection,
    base: ProcessSample,
    acc: Mutex<Acc>,
}

/// The selected path, else the one with the lowest RTT; with no path, the connection's own RTT.
fn best_path(conn: &Connection) -> Option<PathSample> {
    let paths = conn.paths();
    let best = paths
        .iter()
        .find(|p| p.is_selected())
        .or_else(|| paths.iter().min_by_key(|p| p.rtt()));
    if let Some(p) = best {
        let s = p.stats();
        return Some(PathSample {
            rtt: s.rtt,
            cwnd: s.cwnd,
        });
    }
    conn.rtt(PathId::ZERO)
        .map(|rtt| PathSample { rtt, cwnd: 0 })
}

impl Inner {
    fn sample(&self) {
        let p = process::sample();
        let path = best_path(&self.conn);
        let stats = self.conn.stats();
        let mut a = self.acc.lock().unwrap();
        a.process = ProcessUsage {
            cpu_user: p.cpu_user.saturating_sub(self.base.cpu_user),
            cpu_sys: p.cpu_sys.saturating_sub(self.base.cpu_sys),
            peak_rss: p.max_rss_bytes,
            peak_fds: a.process.peak_fds.max(p.open_fds.unwrap_or(0)),
        };
        if let Some(PathSample { rtt, cwnd }) = path.filter(|s| !s.rtt.is_zero()) {
            let n = &mut a.net;
            n.rtt_min = if n.rtt_min.is_zero() {
                rtt
            } else {
                n.rtt_min.min(rtt)
            };
            n.rtt_max = n.rtt_max.max(rtt);
            n.cwnd = cwnd;
            a.rtt_sum += rtt;
            a.rtt_samples += 1;
            a.net.rtt_avg = a.rtt_sum / a.rtt_samples;
            metrics::histogram!("irsync_quic_rtt_seconds").record(rtt.as_secs_f64());
        }
        let lost = stats.lost_packets;
        metrics::counter!("irsync_quic_lost_packets_total")
            .increment(lost.saturating_sub(a.last_lost));
        a.last_lost = lost;
        a.net.lost_packets = lost;
        a.net.sent_packets = stats.udp_tx.datagrams;
    }
}

/// Handle to a running sampler; [`SamplerHandle::stop`] takes the final sample.
pub struct SamplerHandle {
    inner: Arc<Inner>,
    task: JoinHandle<()>,
}

/// Starts sampling for a session over `conn`; its process CPU counts from now.
pub fn spawn(meter: Arc<Meter>, conn: Connection) -> SamplerHandle {
    let inner = Arc::new(Inner {
        conn,
        base: process::sample(),
        acc: Mutex::default(),
    });
    let i = inner.clone();
    let task = tokio::spawn(async move {
        let mut tick = tokio::time::interval(INTERVAL);
        while meter.is_enabled() {
            tick.tick().await;
            i.sample();
        }
    });
    SamplerHandle { inner, task }
}

impl SamplerHandle {
    /// Stops the periodic task, samples once more and returns the session's totals.
    pub fn stop(self) -> (ProcessUsage, NetUsage) {
        self.task.abort();
        self.inner.sample();
        let a = self.inner.acc.lock().unwrap();
        (a.process.clone(), a.net.clone())
    }
}
