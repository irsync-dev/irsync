//! Publishes the global meters and the process's usage as `metrics` values, once a second.

use super::meter::{global, Activity, Counter};
use super::{process, Role, Side};
use metrics::{describe_counter, describe_gauge, describe_histogram, Label, Unit};
use std::time::Duration;
use tokio::task::JoinHandle;

const INTERVAL: Duration = Duration::from_secs(1);

/// The meter counters published as `metrics` counters: name, unit and help.
const COUNTERS: [(Counter, &str, Unit, &str); 6] = [
    (
        Counter::BytesLiteral,
        "irsync_bytes_literal_total",
        Unit::Bytes,
        "File data sent or received as literal chunk bytes",
    ),
    (
        Counter::BytesMatched,
        "irsync_bytes_matched_total",
        Unit::Bytes,
        "File data rebuilt from the destination's existing chunks",
    ),
    (
        Counter::BytesHashed,
        "irsync_bytes_hashed_total",
        Unit::Bytes,
        "Bytes the hashers read",
    ),
    (
        Counter::FilesHashed,
        "irsync_files_hashed_total",
        Unit::Count,
        "Files the hashers read",
    ),
    (
        Counter::DiskRead,
        "irsync_disk_read_bytes_total",
        Unit::Bytes,
        "File data irsync read from disk",
    ),
    (
        Counter::DiskWrite,
        "irsync_disk_write_bytes_total",
        Unit::Bytes,
        "File data irsync wrote to disk",
    ),
];

/// Describes every metric irsync emits, with its unit, to the installed recorder.
pub fn describe() {
    let per_side = "; global meters aggregate across exports, so no export label";
    describe_counter!(
        "irsync_busy_microseconds_total",
        Unit::Microseconds,
        format!("Time spent per activity, peer_wait being time with none in progress{per_side}")
    );
    for (_, name, unit, help) in COUNTERS {
        describe_counter!(name, unit, format!("{help}{per_side}"));
    }
    describe_counter!(
        "irsync_sessions_total",
        Unit::Count,
        "Sessions ended, by outcome (ok/partial/failed/max_delete)"
    );
    describe_counter!(
        "irsync_files_total",
        Unit::Count,
        "Files by result (transferred/skipped/failed/created)"
    );
    describe_counter!(
        "irsync_entries_listed_total",
        Unit::Count,
        "Source entries listed, by kind"
    );
    describe_counter!(
        "irsync_wire_bytes_total",
        Unit::Bytes,
        "Bytes on the QUIC connection, by direction (tx/rx)"
    );
    describe_counter!(
        "irsync_deleted_total",
        Unit::Count,
        "Paths the receiver deleted"
    );
    describe_counter!(
        "irsync_reconnects_total",
        Unit::Count,
        "Client reconnects, by reason (dial/lost)"
    );
    describe_counter!(
        "irsync_quic_lost_packets_total",
        Unit::Count,
        "QUIC packets lost"
    );
    describe_histogram!(
        "irsync_phase_duration_seconds",
        Unit::Seconds,
        "Wall time of each session phase; phases overlap"
    );
    describe_histogram!(
        "irsync_file_transfer_seconds",
        Unit::Seconds,
        "Time from a file's first request to its DONE"
    );
    describe_histogram!(
        "irsync_hash_wait_seconds",
        Unit::Seconds,
        "Time a role waited on a hash result"
    );
    describe_histogram!(
        "irsync_fsync_seconds",
        Unit::Seconds,
        "Time to fsync a received file"
    );
    describe_histogram!(
        "irsync_file_size_bytes",
        Unit::Bytes,
        "Size of each transferred file"
    );
    describe_histogram!(
        "irsync_quic_rtt_seconds",
        Unit::Seconds,
        "QUIC path RTT, sampled once a second per connection"
    );
    describe_gauge!(
        "irsync_sessions_active",
        Unit::Count,
        "Sessions the daemon is serving"
    );
    describe_gauge!("irsync_streams_open", Unit::Count, "Open data streams");
    describe_gauge!(
        "irsync_hash_queue_depth",
        Unit::Count,
        "Hash jobs waiting for a worker"
    );
    describe_gauge!(
        "process_cpu_seconds_total",
        Unit::Seconds,
        "CPU time of the process, by mode (user/sys)"
    );
    describe_gauge!(
        "process_resident_memory_bytes",
        Unit::Bytes,
        "Peak resident set size of the process"
    );
    describe_gauge!(
        "process_open_fds",
        Unit::Count,
        "Open file descriptors of the process"
    );
}

/// Publishes the global meters of `side` and the process usage once.
pub fn once(side: Side) {
    for role in [Role::Sender, Role::Receiver] {
        let u = global(side, role).usage();
        let labels = super::labels(side, role, None);
        let with = |k: &'static str, v: &'static str| {
            let mut l = labels.clone();
            l.push(Label::from_static_parts(k, v));
            l
        };
        let micros = |d: Duration| u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
        for a in Activity::ALL {
            metrics::counter!(
                "irsync_busy_microseconds_total",
                with("activity", a.label())
            )
            .absolute(micros(u.busy(a)));
        }
        metrics::counter!(
            "irsync_busy_microseconds_total",
            with("activity", "peer_wait")
        )
        .absolute(micros(u.peer_wait));
        for (c, name, ..) in COUNTERS {
            metrics::counter!(name, labels.clone()).absolute(u.count(c));
        }
    }
    let p = process::sample();
    let side_label = || vec![Label::from_static_parts("side", side.label())];
    for (mode, t) in [("user", p.cpu_user), ("sys", p.cpu_sys)] {
        let mut l = side_label();
        l.push(Label::from_static_parts("mode", mode));
        metrics::gauge!("process_cpu_seconds_total", l).set(t.as_secs_f64());
    }
    metrics::gauge!("process_resident_memory_bytes", side_label()).set(p.max_rss_bytes as f64);
    if let Some(n) = p.open_fds {
        metrics::gauge!("process_open_fds", side_label()).set(n as f64);
    }
}

/// Publishes [`once`] every second until aborted.
pub fn spawn(side: Side) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(INTERVAL);
        loop {
            tick.tick().await;
            once(side);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use metrics_util::debugging::DebuggingRecorder;
    use metrics_util::MetricKind;

    #[test]
    fn once_publishes_each_role_with_side_and_role_labels() {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        metrics::with_local_recorder(&recorder, || once(Side::Daemon));
        let keys: Vec<_> = snapshotter
            .snapshot()
            .into_vec()
            .into_iter()
            .map(|(k, ..)| k)
            .collect();
        let has = |kind: MetricKind, name: &str, labels: &[(&str, &str)]| {
            keys.iter().any(|k| {
                k.kind() == kind
                    && k.key().name() == name
                    && labels
                        .iter()
                        .all(|(n, v)| k.key().labels().any(|l| l.key() == *n && l.value() == *v))
            })
        };
        for role in ["sender", "receiver"] {
            let side_role = [("side", "daemon"), ("role", role)];
            assert!(has(
                MetricKind::Counter,
                "irsync_busy_microseconds_total",
                &[side_role[0], side_role[1], ("activity", "peer_wait")]
            ));
            for (_, name, ..) in COUNTERS {
                assert!(has(MetricKind::Counter, name, &side_role), "{name}");
            }
        }
        assert!(has(
            MetricKind::Gauge,
            "process_cpu_seconds_total",
            &[("side", "daemon"), ("mode", "user")]
        ));
        assert!(has(MetricKind::Gauge, "process_resident_memory_bytes", &[]));
    }
}
