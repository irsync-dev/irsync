//! A machine-readable list of the files a sync retried, gave up on, or saw
//! vanish, for the stress harness. Off unless `IRSYNC_FILE_REPORT` names a
//! file; then one JSON object per line is appended to it:
//! `{"event":..,"path_hex":..,"path":..(lossy),"reason":..}`. Paths are
//! relative to the transfer root, as held in a [`RelPath`].

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use crate::fs::paths::RelPath;
use crate::session::Outcome;

/// The variable that names the report file.
pub const ENV: &str = "IRSYNC_FILE_REPORT";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportEvent {
    Retried,
    GaveUp,
    Vanished,
}

impl ReportEvent {
    fn name(self) -> &'static str {
        match self {
            ReportEvent::Retried => "retried",
            ReportEvent::GaveUp => "gave_up",
            ReportEvent::Vanished => "vanished",
        }
    }
}

/// The report file named by the variable, opened once; `None` when unset or
/// not openable.
fn env_sink() -> Option<&'static Mutex<File>> {
    static SINK: OnceLock<Option<Mutex<File>>> = OnceLock::new();
    SINK.get_or_init(|| {
        let p = std::env::var_os(ENV)?;
        match open(Path::new(&p)) {
            Ok(f) => Some(Mutex::new(f)),
            Err(e) => {
                eprintln!(
                    "irsync: cannot open the file report {}: {e}",
                    Path::new(&p).display()
                );
                None
            }
        }
    })
    .as_ref()
}

/// Says on stderr, once per process, that a report line could not be
/// written. The sync itself carries on.
fn warn_write(e: &std::io::Error) {
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!("irsync: cannot write the file report: {e}");
    }
}

fn open(p: &Path) -> std::io::Result<File> {
    OpenOptions::new().create(true).append(true).open(p)
}

fn escape(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
}

fn line(event: ReportEvent, path: &RelPath, reason: &str) -> String {
    let bytes = path.as_bytes();
    let mut s = format!("{{\"event\":\"{}\",\"path_hex\":\"", event.name());
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s.push_str("\",\"path\":\"");
    escape(&String::from_utf8_lossy(bytes), &mut s);
    s.push_str("\",\"reason\":\"");
    escape(reason, &mut s);
    s.push_str("\"}\n");
    s
}

/// Appends one line to the file `IRSYNC_FILE_REPORT` names; does nothing
/// when it is unset. A failure to open or write it is reported once on
/// stderr; the report must not change a sync.
pub fn record(event: ReportEvent, path: &RelPath, reason: &str) {
    if let Some(sink) = env_sink() {
        let l = line(event, path, reason);
        let mut f = sink.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = f.write_all(l.as_bytes()) {
            warn_write(&e);
        }
    }
}

/// Like [`record`], to the file at `sink` (opened for each line); for tests
/// that give each side its own file.
pub fn record_to(sink: &Path, event: ReportEvent, path: &RelPath, reason: &str) {
    match open(sink) {
        Ok(mut f) => {
            if let Err(e) = f.write_all(line(event, path, reason).as_bytes()) {
                warn_write(&e);
            }
        }
        Err(e) => warn_write(&e),
    }
}

/// Writes a `gave_up` line per error and a `vanished` line per vanished
/// file of the client's final outcome.
pub fn record_outcome(o: &Outcome) {
    for (p, why) in &o.errors {
        record(ReportEvent::GaveUp, p, why);
    }
    for p in &o.vanished {
        record(ReportEvent::Vanished, p, "vanished during the sync");
    }
}

/// Like [`record_outcome`], to the file at `sink`.
pub fn record_outcome_to(sink: &Path, o: &Outcome) {
    for (p, why) in &o.errors {
        record_to(sink, ReportEvent::GaveUp, p, why);
    }
    for p in &o.vanished {
        record_to(sink, ReportEvent::Vanished, p, "vanished during the sync");
    }
}
