//! The `-v` session summary: where one session's time and bytes went.
//!
//! ```text
//! session 1/2  push  ./photos -> nas:/backup  wall 3:12  direct, 1 reconnect
//!   busy      walk 21.0s  hash 37.1s  read 1:02  send 1:48  delete 1.3s
//!   waiting   peer 44.0s
//!   transfer  1,882 files  3.41G literal  12.00G matched  17.77M/s
//!   hashing   2,107 files  15.30G  412.00M/s
//!   disk      read 15.40G  write 0  fsync 0.0s
//!   network   sent 3.52G  recv 41.00M  rtt 12ms (max 40ms)  lost 0.02%
//!   process   cpu 1:12 user, 0:31 sys  peak rss 312.00M  peak fds 214
//! ```

use std::time::Duration;

use super::{clock, commas, human};
use crate::session::reconnect::ConnState;
use crate::telemetry::meter::{Activity, Counter, Usage};

/// One finished (or failed) session, for the `-v` summary.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    /// 0-based position among the run's sources, and their count.
    pub index: usize,
    pub of: usize,
    /// `"push"` or `"pull"`.
    pub verb: &'static str,
    /// The operands as typed.
    pub from: String,
    pub to: String,
    pub reconnects: u32,
    /// How the last connection was made; `None` if it never was.
    pub conn: Option<ConnState>,
    pub usage: Usage,
    pub files_transferred: u64,
    pub files_hashed: u64,
    /// The session's QUIC bytes, as in the `sent`/`received` trailer.
    pub bytes_sent: u64,
    pub bytes_received: u64,
}

/// `0.4s`, `21.0s`, `2:47`, `1:02:03`.
pub fn short_dur(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if (secs * 10.0).round() < 600.0 {
        return format!("{:.1}s", secs);
    }
    let whole = if secs < 60.0 { 60 } else { d.as_secs() };
    if whole >= 3600 {
        clock(Duration::from_secs(whole))
    } else {
        format!("{}:{:02}", whole / 60, whole % 60)
    }
}

/// Like [`short_dur`] but `m:ss` below a minute too (CPU time).
fn cpu_dur(d: Duration) -> String {
    if d.as_secs() < 60 {
        format!("0:{:02}", d.as_secs())
    } else {
        short_dur(d)
    }
}

fn ms(d: Duration) -> String {
    let v = d.as_secs_f64() * 1000.0;
    if v < 10.0 {
        format!("{v:.1}ms")
    } else {
        format!("{v:.0}ms")
    }
}

/// `label` in a 10-column field, then `items` joined by two spaces,
/// wrapped to `width` with continuation lines under the first item. An item
/// longer than a line is cut with `…`.
fn rows(label: &str, items: &[String], width: usize, out: &mut Vec<String>) {
    let indent = if label.is_empty() { 2 } else { 12 };
    let first = if label.is_empty() {
        String::new()
    } else {
        format!("  {label:<10}")
    };
    let room = width.saturating_sub(indent).max(1);
    let mut base = first.chars().count();
    let mut line = first;
    let mut used = 0;
    for item in items {
        let item: String = if item.chars().count() > room {
            item.chars().take(room - 1).chain(Some('…')).collect()
        } else {
            item.clone()
        };
        let n = item.chars().count();
        // `-> to` sits one space after its `from`.
        let sep = if item.starts_with("-> ") { " " } else { "  " };
        if used > 0 && base + used + sep.len() + n > width {
            out.push(std::mem::replace(&mut line, " ".repeat(indent)));
            base = indent;
            used = 0;
        }
        if used > 0 {
            line.push_str(sep);
            used += sep.len();
        }
        line.push_str(&item);
        used += n;
    }
    out.push(line);
}

/// The summary block, every line at most `width` characters.
pub fn render(s: &SessionSummary, width: usize) -> Vec<String> {
    let u = &s.usage;
    let mut out = Vec::new();

    let mut conn = match s.conn {
        Some(ConnState::Direct) => "direct".to_string(),
        Some(ConnState::Relay) => "relay".to_string(),
        _ => String::new(),
    };
    if s.reconnects > 0 {
        let n = s.reconnects;
        let plural = if n == 1 { "" } else { "s" };
        let r = format!("{n} reconnect{plural}");
        conn = if conn.is_empty() {
            r
        } else {
            format!("{conn}, {r}")
        };
    }
    let mut head = vec![
        format!("session {}/{}", s.index + 1, s.of),
        s.verb.to_string(),
        s.from.clone(),
        format!("-> {}", s.to),
        format!("wall {}", short_dur(u.wall)),
    ];
    if !conn.is_empty() {
        head.push(conn);
    }
    rows("", &head, width, &mut out);
    let busy: Vec<String> = Activity::ALL
        .iter()
        .filter(|a| !u.busy(**a).is_zero())
        .map(|a| format!("{} {}", a.label(), short_dur(u.busy(*a))))
        .collect();
    if !busy.is_empty() {
        rows("busy", &busy, width, &mut out);
    }
    if !u.peer_wait.is_zero() {
        rows(
            "waiting",
            &[format!("peer {}", short_dur(u.peer_wait))],
            width,
            &mut out,
        );
    }

    let (literal, matched) = (
        u.count(Counter::BytesLiteral),
        u.count(Counter::BytesMatched),
    );
    if s.files_transferred > 0 || literal > 0 || matched > 0 {
        let rate = literal as f64 / u.wall.as_secs_f64().max(0.001);
        rows(
            "transfer",
            &[
                format!("{} files", commas(s.files_transferred)),
                format!("{} literal", human(literal)),
                format!("{} matched", human(matched)),
                format!("{}/s", human(rate as u64)),
            ],
            width,
            &mut out,
        );
    }

    let hashed = u.count(Counter::BytesHashed);
    if s.files_hashed > 0 || hashed > 0 {
        let mut items = vec![format!("{} files", commas(s.files_hashed)), human(hashed)];
        let busy = u.busy(Activity::Hash).as_secs_f64();
        if busy > 0.0 {
            items.push(format!("{}/s", human((hashed as f64 / busy) as u64)));
        }
        rows("hashing", &items, width, &mut out);
    }

    let (read, write, fsync) = (
        u.count(Counter::DiskRead),
        u.count(Counter::DiskWrite),
        u.count(Counter::FsyncNanos),
    );
    if read > 0 || write > 0 || fsync > 0 {
        rows(
            "disk",
            &[
                format!("read {}", human(read)),
                format!("write {}", human(write)),
                format!("fsync {}", short_dur(Duration::from_nanos(fsync))),
            ],
            width,
            &mut out,
        );
    }

    let n = &u.net;
    if !n.rtt_avg.is_zero() {
        let lost = if n.sent_packets > 0 {
            n.lost_packets as f64 * 100.0 / n.sent_packets as f64
        } else {
            0.0
        };
        rows(
            "network",
            &[
                format!("sent {}", human(s.bytes_sent)),
                format!("recv {}", human(s.bytes_received)),
                format!("rtt {} (max {})", ms(n.rtt_avg), ms(n.rtt_max)),
                format!("lost {lost:.2}%"),
            ],
            width,
            &mut out,
        );
    }

    let p = &u.process;
    if !p.cpu_user.is_zero() || !p.cpu_sys.is_zero() || p.peak_rss > 0 || p.peak_fds > 0 {
        rows(
            "process",
            &[
                format!(
                    "cpu {} user, {} sys",
                    cpu_dur(p.cpu_user),
                    cpu_dur(p.cpu_sys)
                ),
                format!("peak rss {}", human(p.peak_rss)),
                format!("peak fds {}", commas(p.peak_fds)),
            ],
            width,
            &mut out,
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    fn fixture() -> SessionSummary {
        let mut u = Usage {
            wall: secs(192.0),
            peer_wait: secs(44.0),
            ..Usage::default()
        };
        for (a, t) in [
            (Activity::Walk, 21.0),
            (Activity::Hash, 37.1),
            (Activity::Read, 62.0),
            (Activity::Send, 108.0),
            (Activity::Delete, 1.3),
        ] {
            u.busy[a as usize] = secs(t);
        }
        let gib = 1u64 << 30;
        let mib = 1u64 << 20;
        for (c, v) in [
            (Counter::BytesLiteral, 3 * gib + gib * 41 / 100),
            (Counter::BytesMatched, 12 * gib),
            (Counter::BytesHashed, 15 * gib + gib * 30 / 100),
            (Counter::DiskRead, 15 * gib + gib * 40 / 100),
        ] {
            u.counters[c as usize] = v;
        }
        u.net.rtt_avg = Duration::from_millis(12);
        u.net.rtt_max = Duration::from_millis(40);
        u.net.sent_packets = 10_000;
        u.net.lost_packets = 2;
        u.process.cpu_user = secs(72.0);
        u.process.cpu_sys = secs(31.0);
        u.process.peak_rss = 312 * mib;
        u.process.peak_fds = 214;
        SessionSummary {
            index: 0,
            of: 2,
            verb: "push",
            from: "./photos".into(),
            to: "nas:/backup".into(),
            reconnects: 1,
            conn: Some(ConnState::Direct),
            usage: u,
            files_transferred: 1882,
            files_hashed: 2107,
            bytes_sent: 3 * gib + gib * 52 / 100,
            bytes_received: 41 * mib,
        }
    }

    #[test]
    fn renders_the_documented_layout() {
        let mut s = fixture();
        // The documented rates: literal over wall, hashed over hash busy.
        s.usage.wall = secs(192.0);
        let got = render(&s, 80);
        let lit = human(s.usage.count(Counter::BytesLiteral));
        let rate = human((s.usage.count(Counter::BytesLiteral) as f64 / 192.0) as u64);
        let hrate = human((s.usage.count(Counter::BytesHashed) as f64 / 37.1) as u64);
        let want = vec![
            "session 1/2  push  ./photos -> nas:/backup  wall 3:12  direct, 1 reconnect"
                .to_string(),
            "  busy      walk 21.0s  hash 37.1s  read 1:02  send 1:48  delete 1.3s".to_string(),
            "  waiting   peer 44.0s".to_string(),
            format!("  transfer  1,882 files  {lit} literal  12.00G matched  {rate}/s"),
            format!("  hashing   2,107 files  15.30G  {hrate}/s"),
            "  disk      read 15.40G  write 0  fsync 0.0s".to_string(),
            "  network   sent 3.52G  recv 41.00M  rtt 12ms (max 40ms)  lost 0.02%".to_string(),
            "  process   cpu 1:12 user, 0:31 sys  peak rss 312.00M  peak fds 214".to_string(),
        ];
        assert_eq!(got, want);
        assert_eq!(lit, "3.41G");
    }

    #[test]
    fn lines_fit_width() {
        let mut s = fixture();
        s.from = "/very/long/source/directory/name/that/goes/on".into();
        s.to = "a-host-with-a-long-name:/an/equally/long/destination/path".into();
        s.reconnects = 12;
        let u = &mut s.usage;
        u.wall = Duration::from_secs(90_000);
        for a in Activity::ALL {
            u.busy[a as usize] = Duration::from_secs(80_000);
        }
        u.peer_wait = Duration::from_secs(80_000);
        for c in Counter::ALL {
            u.counters[c as usize] = u64::MAX / 3;
        }
        u.process.cpu_user = Duration::from_secs(900_000);
        u.process.cpu_sys = Duration::from_secs(900_000);
        u.process.peak_rss = u64::MAX;
        u.process.peak_fds = u64::MAX;
        s.files_transferred = u64::MAX;
        s.files_hashed = u64::MAX;
        s.bytes_sent = u64::MAX;
        s.bytes_received = u64::MAX;
        for width in [60, 80] {
            for l in render(&s, width) {
                assert!(l.chars().count() <= width, "{width}: {l:?}");
            }
        }
        // Wrapping keeps every activity.
        assert!(render(&s, 60).join("\n").contains("delete 22:13:20"));
    }

    #[test]
    fn short_dur_forms() {
        assert_eq!(short_dur(secs(0.4)), "0.4s");
        assert_eq!(short_dur(secs(59.94)), "59.9s");
        assert_eq!(short_dur(secs(59.97)), "1:00");
        assert_eq!(short_dur(secs(167.0)), "2:47");
        assert_eq!(short_dur(secs(3723.0)), "1:02:03");
    }
}
