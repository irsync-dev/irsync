//! Operation log: one parseable text line per step, plus run header/footer
//! and per-kind statistics.

use crate::escape::escape;
use crate::kind::OpKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const TS_FMT: &str = "%Y-%m-%dT%H:%M:%S%.6fZ";

/// What one executed step did, as recorded in the log.
pub struct OpRecord {
    pub kind: OpKind,
    pub path: Vec<u8>,
    pub to: Option<Vec<u8>>,
    pub bytes: u64,
    pub details: Vec<(&'static str, String)>,
}

/// Run parameters written as the `# run` line.
pub struct Header {
    pub seed: u64,
    pub start_step: u64,
    pub steps: u64,
    pub log_path: PathBuf,
    pub state_path: PathBuf,
    pub config_file: Option<PathBuf>,
    pub dry_run: bool,
    pub started: jiff::Timestamp,
}

/// Run outcome written as the `# end` line.
pub struct Footer {
    pub steps_done: u64,
    pub outcome: String,
    pub elapsed: Duration,
    /// Steps of this invocation that found nothing to do; logged when > 0.
    pub skipped: u64,
}

/// Per-kind counters and raw durations.
#[derive(Default)]
pub struct KindStats {
    pub count: u64,
    pub bytes: u64,
    durs_us: Vec<u32>,
}

/// Statistics over all recorded steps, keyed by kind.
#[derive(Default)]
pub struct Stats(BTreeMap<OpKind, KindStats>);

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct KindSummary {
    pub kind: OpKind,
    pub count: u64,
    pub bytes: u64,
    pub p50_us: u32,
    pub p99_us: u32,
    pub max_us: u32,
}

impl Stats {
    pub fn record(&mut self, kind: OpKind, bytes: u64, dur: Duration) {
        let k = self.0.entry(kind).or_default();
        k.count += 1;
        k.bytes += bytes;
        k.durs_us
            .push(u32::try_from(dur.as_micros()).unwrap_or(u32::MAX));
    }

    /// One summary per kind seen, in `OpKind::ALL` order.
    pub fn summary(&self) -> Vec<KindSummary> {
        OpKind::ALL
            .iter()
            .filter_map(|kind| {
                let k = self.0.get(kind).filter(|k| k.count > 0)?;
                let mut d = k.durs_us.clone();
                d.sort_unstable();
                let n = d.len();
                Some(KindSummary {
                    kind: *kind,
                    count: k.count,
                    bytes: k.bytes,
                    p50_us: d[(n - 1) / 2],
                    p99_us: d[((n - 1) * 99).div_ceil(100)],
                    max_us: d[n - 1],
                })
            })
            .collect()
    }
}

/// Appending line writer for the operation log.
pub struct LogWriter {
    out: BufWriter<std::fs::File>,
    echo: bool,
}

fn fmt_ts(ts: jiff::Timestamp) -> String {
    ts.strftime(TS_FMT).to_string()
}

fn quoted(bytes: &[u8]) -> String {
    format!("\"{}\"", escape(bytes))
}

fn path_bytes(p: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    p.as_os_str().as_bytes().to_vec()
}

/// Bare when only `[A-Za-z0-9_.:+-/=]`, else quoted and escaped.
fn detail_value(v: &str) -> String {
    let bare = !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.:+-/=".contains(c));
    if bare {
        v.to_string()
    } else {
        quoted(v.as_bytes())
    }
}

impl LogWriter {
    pub fn open_append(path: &Path, echo: bool) -> io::Result<LogWriter> {
        let f = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(LogWriter {
            out: BufWriter::new(f),
            echo,
        })
    }

    fn line(&mut self, s: &str) -> io::Result<()> {
        if self.echo {
            eprintln!("{s}");
        }
        self.out.write_all(s.as_bytes())?;
        self.out.write_all(b"\n")
    }

    pub fn header(&mut self, h: &Header) -> io::Result<()> {
        let config = match &h.config_file {
            Some(c) => quoted(&path_bytes(c)),
            None => "\"-\"".to_string(),
        };
        let s = format!(
            "# run seed={} start_step={} steps={} log={} state={} config={} dry_run={} started={}",
            h.seed,
            h.start_step,
            h.steps,
            quoted(&path_bytes(&h.log_path)),
            quoted(&path_bytes(&h.state_path)),
            config,
            u8::from(h.dry_run),
            fmt_ts(h.started),
        );
        self.line(&s)
    }

    pub fn step(
        &mut self,
        step: u64,
        ts: jiff::Timestamp,
        rec: &OpRecord,
        dur: Duration,
        dry: bool,
        pressure: f64,
    ) -> io::Result<()> {
        let mut s = format!(
            "step={step} ts={} op={} path={}",
            fmt_ts(ts),
            rec.kind.name(),
            quoted(&rec.path)
        );
        if let Some(to) = &rec.to {
            s.push_str(&format!(" to={}", quoted(to)));
        }
        if dry {
            s.push_str(" dry=1");
        }
        if rec.bytes > 0 {
            s.push_str(&format!(" bytes={}", rec.bytes));
        }
        for (k, v) in &rec.details {
            s.push_str(&format!(" {k}={}", detail_value(v)));
        }
        if pressure > 0.0 {
            s.push_str(&format!(" pressure={pressure:.2}"));
        }
        s.push_str(&format!(" dur_us={}", dur.as_micros()));
        self.line(&s)
    }

    pub fn failure(
        &mut self,
        step: u64,
        kind: OpKind,
        path: &[u8],
        to: Option<&[u8]>,
        err: &io::Error,
    ) -> io::Result<()> {
        let mut s = format!(
            "step={step} ts={} op={} path={}",
            fmt_ts(jiff::Timestamp::now()),
            kind.name(),
            quoted(path),
        );
        if let Some(to) = to {
            s.push_str(&format!(" to={}", quoted(to)));
        }
        s.push_str(&format!(" error={}", quoted(err.to_string().as_bytes())));
        self.line(&s)
    }

    pub fn footer(&mut self, f: &Footer, summary: &[KindSummary]) -> io::Result<()> {
        let mut s = format!(
            "# end steps_done={} outcome={} elapsed_ms={}",
            f.steps_done,
            detail_value(&f.outcome),
            f.elapsed.as_millis()
        );
        if f.skipped > 0 {
            s.push_str(&format!(" skipped={}", f.skipped));
        }
        self.line(&s)?;
        for k in summary {
            let s = format!(
                "# stat op={} count={} bytes={} p50_us={} p99_us={} max_us={}",
                k.kind.name(),
                k.count,
                k.bytes,
                k.p50_us,
                k.p99_us,
                k.max_us
            );
            self.line(&s)?;
        }
        self.flush()
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind::OpKind;
    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;

    fn tmp_path(name: &str) -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join(name);
        (d, p)
    }

    fn ts() -> jiff::Timestamp {
        "2026-10-20T12:00:00.123456Z".parse().unwrap()
    }

    #[test]
    fn step_line_format() {
        let (_d, p) = tmp_path("x.log");
        let mut w = LogWriter::open_append(&p, false).unwrap();
        let rec = OpRecord {
            kind: OpKind::Rename,
            path: b"a b".to_vec(),
            to: Some(b"c\"d".to_vec()),
            bytes: 0,
            details: vec![("size", "12".into())],
        };
        w.step(7, ts(), &rec, Duration::from_micros(812), false, 0.0)
            .unwrap();
        w.flush().unwrap();
        assert_eq!(
            fs::read_to_string(&p).unwrap(),
            "step=7 ts=2026-10-20T12:00:00.123456Z op=rename path=\"a b\" to=\"c\\\"d\" size=12 dur_us=812\n"
        );
    }

    #[test]
    fn dry_and_pressure_and_bytes_appear() {
        let (_d, p) = tmp_path("x.log");
        let mut w = LogWriter::open_append(&p, false).unwrap();
        let rec = OpRecord {
            kind: OpKind::Append,
            path: b"f".to_vec(),
            to: None,
            bytes: 5,
            details: vec![("note", "has space".into())],
        };
        w.step(1, ts(), &rec, Duration::ZERO, true, 0.25).unwrap();
        w.flush().unwrap();
        assert_eq!(
            fs::read_to_string(&p).unwrap(),
            "step=1 ts=2026-10-20T12:00:00.123456Z op=append path=\"f\" dry=1 bytes=5 note=\"has space\" pressure=0.25 dur_us=0\n"
        );
    }

    #[test]
    fn failure_line() {
        let (_d, p) = tmp_path("x.log");
        let mut w = LogWriter::open_append(&p, false).unwrap();
        let err = io::Error::other("boom \"x\"");
        w.failure(3, OpKind::Mkdir, b"d", None, &err).unwrap();
        w.failure(4, OpKind::Rename, b"a", Some(b"b c"), &err)
            .unwrap();
        w.flush().unwrap();
        let s = fs::read_to_string(&p).unwrap();
        let lines: Vec<&str> = s.lines().collect();
        assert!(lines[0].starts_with("step=3 ts="));
        assert!(lines[0].ends_with(" op=mkdir path=\"d\" error=\"boom \\\"x\\\"\""));
        assert!(lines[1].ends_with(" op=rename path=\"a\" to=\"b c\" error=\"boom \\\"x\\\"\""));
        let t = s.split(' ').nth(1).unwrap().strip_prefix("ts=").unwrap();
        assert_eq!(t.len(), "2026-10-20T12:00:00.123456Z".len());
        assert!(t.parse::<jiff::Timestamp>().is_ok());
    }

    #[test]
    fn append_across_opens_and_header_footer() {
        let (_d, p) = tmp_path("x.log");
        let h = Header {
            seed: 42,
            start_step: 0,
            steps: 10,
            log_path: p.clone(),
            state_path: PathBuf::from("s.yaml"),
            config_file: None,
            dry_run: false,
            started: ts(),
        };
        {
            let mut w = LogWriter::open_append(&p, false).unwrap();
            w.header(&h).unwrap();
            w.flush().unwrap();
        }
        let mut w = LogWriter::open_append(&p, false).unwrap();
        let rec = OpRecord {
            kind: OpKind::Mkdir,
            path: b"d".to_vec(),
            to: None,
            bytes: 0,
            details: vec![],
        };
        w.step(0, ts(), &rec, Duration::from_micros(9), false, 0.0)
            .unwrap();
        let mut st = Stats::default();
        st.record(OpKind::Mkdir, 0, Duration::from_micros(9));
        w.footer(
            &Footer {
                steps_done: 1,
                outcome: "ok".into(),
                elapsed: Duration::from_millis(5),
                skipped: 0,
            },
            &st.summary(),
        )
        .unwrap();
        let text = fs::read_to_string(&p).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4);
        assert!(lines[0].starts_with("# run seed=42 start_step=0 steps=10 log=\""));
        assert!(lines[0].ends_with(
            "state=\"s.yaml\" config=\"-\" dry_run=0 started=2026-10-20T12:00:00.123456Z"
        ));
        assert!(lines[1].starts_with("step=0 "));
        assert_eq!(lines[2], "# end steps_done=1 outcome=ok elapsed_ms=5");
        assert_eq!(
            lines[3],
            "# stat op=mkdir count=1 bytes=0 p50_us=9 p99_us=9 max_us=9"
        );
    }

    #[test]
    fn stats_percentiles() {
        let mut s = Stats::default();
        for d in [5u64, 1, 3, 2, 4] {
            s.record(OpKind::Mkdir, 1, Duration::from_micros(d));
        }
        let k = &s.summary()[0];
        assert_eq!(
            (k.count, k.bytes, k.p50_us, k.p99_us, k.max_us),
            (5, 5, 3, 5, 5)
        );
    }

    #[test]
    fn summary_in_all_order_and_only_seen() {
        let mut s = Stats::default();
        s.record(OpKind::Rename, 0, Duration::from_micros(1));
        s.record(OpKind::CreateFile, 0, Duration::from_micros(1));
        let kinds: Vec<_> = s.summary().iter().map(|k| k.kind).collect();
        assert_eq!(kinds, vec![OpKind::CreateFile, OpKind::Rename]);
    }
}
