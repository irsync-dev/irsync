//! User-facing output: rsync-style `--info=progress2` line, `-v` per-file
//! lines, `-i` itemize codes and the `--stats` block, driven by the session's
//! [`Event`]s.
//!
//! The reporter only ever reads from a broadcast channel, so it cannot slow
//! the engine down; if it lags it loses events (never the final totals,
//! which come from the [`Outcome`]).

use std::io::Write;
use std::time::{Duration, Instant};

use tokio::sync::broadcast;

use crate::fs::paths::RelPath;
use crate::plan::DELETING;
use crate::session::reconnect::ConnState;
use crate::session::{Event, Outcome};

/// Redraw the progress line at most this often.
const REDRAW: Duration = Duration::from_millis(100);

pub struct Reporter {
    out: Box<dyn Write + Send>,
    tty: bool,
    verbosity: u8,
    stats: bool,
    itemize: bool,
    dry_run: bool,
    // Running totals, from events.
    bytes: u64,
    xfr: u64,
    scanned: u64,
    conn: ConnState,
    hint: Option<(u64, u64)>,
    /// The hint came from the caller or a `Total` event: later ones are ignored.
    hint_fixed: bool,
    /// Events lost because this reporter lagged.
    lagged: u64,
    start: Instant,
    last_draw: Option<Instant>,
    /// A progress line is on screen (no newline yet).
    drawn: bool,
}

/// rsync's human-readable size: `123`, `456.78K`, `12.34M`, `1.23G`, `T`.
pub fn human(n: u64) -> String {
    const UNITS: [(u64, char); 4] = [
        (1 << 40, 'T'),
        (1 << 30, 'G'),
        (1 << 20, 'M'),
        (1 << 10, 'K'),
    ];
    for (size, unit) in UNITS {
        if n >= size {
            return format!("{:.2}{unit}", n as f64 / size as f64);
        }
    }
    n.to_string()
}

/// `h:mm:ss`.
pub fn clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// `1,234,567`, as rsync prints numbers in `--stats`.
fn commas(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A path as rsync prints it: bytes that are not valid UTF-8, control
/// characters and backslashes become `\ooo` (octal); never fails.
fn octal(out: &mut String, b: u8) {
    out.push_str(&format!("\\{b:03o}"));
}

pub fn escape_path(p: &[u8]) -> String {
    let mut out = String::new();
    let mut rest = p;
    while !rest.is_empty() {
        let (good, bad) = match std::str::from_utf8(rest) {
            Ok(s) => (s, 0),
            Err(e) => {
                let (g, _) = rest.split_at(e.valid_up_to());
                // `g` is valid by construction.
                (
                    std::str::from_utf8(g).unwrap_or(""),
                    e.error_len().unwrap_or(rest.len() - g.len()),
                )
            }
        };
        for c in good.chars() {
            if c.is_control() || c == '\\' {
                // Control characters and `\` are all ASCII or C1 (2 bytes).
                let mut buf = [0u8; 4];
                c.encode_utf8(&mut buf)
                    .bytes()
                    .for_each(|b| octal(&mut out, b));
            } else {
                out.push(c);
            }
        }
        rest = &rest[good.len()..];
        rest[..bad].iter().for_each(|&b| octal(&mut out, b));
        rest = &rest[bad..];
    }
    out
}

/// A path as shown in a line: `./` for the session root.
fn shown(p: &RelPath) -> String {
    if p.as_bytes().is_empty() {
        "./".to_string()
    } else {
        escape_path(p.as_bytes())
    }
}

fn conn_label(c: ConnState) -> String {
    match c {
        ConnState::Connecting => "connecting".into(),
        ConnState::Direct => "direct".into(),
        ConnState::Relay => "relay".into(),
        ConnState::Reconnecting { attempt } => format!("reconnecting #{attempt}"),
    }
}

impl Reporter {
    /// `tty`: draw the live progress line (the caller decides whether the
    /// user asked for it). `verbosity`: `-v` count. Output errors (a closed
    /// pipe) are ignored.
    pub fn new(
        out: Box<dyn Write + Send>,
        tty: bool,
        verbosity: u8,
        stats: bool,
        itemize: bool,
        dry_run: bool,
    ) -> Reporter {
        Reporter {
            out,
            tty,
            verbosity,
            stats,
            itemize,
            dry_run,
            bytes: 0,
            xfr: 0,
            scanned: 0,
            conn: ConnState::Connecting,
            hint: None,
            hint_fixed: false,
            lagged: 0,
            start: Instant::now(),
            last_draw: None,
            drawn: false,
        }
    }

    /// Consumes events until the channel closes, then hands the reporter
    /// back for [`Reporter::finish`]. `total_hint` is (files, bytes) when
    /// known up front, for the percentage and `to-chk`.
    pub async fn run(
        mut self,
        mut rx: broadcast::Receiver<Event>,
        total_hint: Option<(u64, u64)>,
    ) -> Reporter {
        if total_hint.is_some() {
            self.hint = total_hint;
            self.hint_fixed = true;
        }
        self.start = Instant::now();
        let mut tick = tokio::time::interval(REDRAW);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                ev = rx.recv() => match ev {
                    Ok(ev) => self.handle(ev),
                    // Lost some events: carry on; totals come from the Outcome.
                    Err(broadcast::error::RecvError::Lagged(n)) => self.lagged += n,
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = tick.tick(), if self.tty => self.draw(false),
            }
        }
        self.draw(true);
        self
    }

    /// Applies one event (the unit the tests drive directly).
    pub fn handle(&mut self, ev: Event) {
        match ev {
            Event::Bytes(n) => self.bytes += n,
            Event::FileStart(_) => {}
            Event::FileDone(p, _) => {
                self.xfr += 1;
                if self.verbosity >= 1 && !self.itemize {
                    self.line(&shown(&p));
                }
            }
            Event::Scanned(n) => self.scanned = n,
            Event::Total { files, bytes } => {
                if !self.hint_fixed {
                    self.hint = Some((files, bytes));
                    self.hint_fixed = true;
                }
            }
            Event::Conn(c) => {
                self.conn = c;
                self.draw(true);
            }
            Event::Deleted(p) => {
                if self.verbosity >= 1 && !self.itemize {
                    self.line(&format!("deleting {}", shown(&p)));
                }
            }
            Event::WouldDelete(p) => {
                let path = shown(&p);
                if self.itemize {
                    self.line(&format!("{DELETING} {path}"));
                } else {
                    self.line(&format!("would delete {path}"));
                }
            }
            Event::Itemized(p, code) => {
                if self.itemize {
                    self.line(&format!("{code} {}", shown(&p)));
                }
            }
            Event::Hardlinked(p, target) => {
                if self.verbosity >= 1 && !self.itemize {
                    self.line(&format!("{} => {}", shown(&p), shown(&target)));
                }
            }
        }
    }

    /// One output line, below any progress line.
    fn line(&mut self, s: &str) {
        if self.drawn {
            let _ = write!(self.out, "\r\x1b[K");
            self.drawn = false;
        }
        let _ = writeln!(self.out, "{s}");
        // Keep the progress line below the text.
        self.last_draw = None;
    }

    /// The progress line for the current totals.
    pub fn progress_line(&self, elapsed: Duration) -> String {
        let secs = elapsed.as_secs_f64().max(0.001);
        let rate = format!("{}B/s", human((self.bytes as f64 / secs) as u64));
        let pct = match self.hint {
            Some((_, total)) if total > 0 => {
                format!("{:>3}%", (self.bytes.saturating_mul(100) / total).min(100))
            }
            _ => "    ".to_string(),
        };
        let chk = match self.hint {
            Some((files, _)) => format!(
                "(xfr#{}, to-chk={}/{})",
                self.xfr,
                files.saturating_sub(self.xfr),
                files
            ),
            None => format!("(xfr#{})", self.xfr),
        };
        format!(
            "{:>12}  {pct}  {rate:>11}  {}  {chk}  [{}]",
            human(self.bytes),
            clock(elapsed),
            conn_label(self.conn)
        )
    }

    fn draw(&mut self, force: bool) {
        if !self.tty {
            return;
        }
        let now = Instant::now();
        if !force && self.last_draw.is_some_and(|t| now - t < REDRAW) {
            return;
        }
        self.last_draw = Some(now);
        let line = self.progress_line(now - self.start);
        let _ = write!(self.out, "\r\x1b[K{line}");
        let _ = self.out.flush();
        self.drawn = true;
    }

    /// Prints the closing lines: the `--stats` block and rsync's
    /// `sent .. received ..` / `total size is ..` trailer.
    pub fn finish(&mut self, outcome: &Outcome, elapsed: Duration) {
        if self.drawn {
            let _ = writeln!(self.out);
            self.drawn = false;
        }
        if self.lagged > 0 {
            eprintln!(
                "irsync: note: {} progress events were dropped (output may be incomplete)",
                self.lagged
            );
        }
        let o = outcome;
        if self.stats {
            let l = &o.listed;
            let deleted = if self.dry_run {
                o.would_delete.len()
            } else {
                o.deleted.len()
            } as u64;
            let rows = [
                format!(
                    "Number of files: {} (reg: {}, dir: {}, link: {}, special: {})",
                    commas(l.files()),
                    commas(l.reg),
                    commas(l.dir),
                    commas(l.link),
                    commas(l.special)
                ),
                format!("Number of created files: {}", commas(o.created)),
                format!("Number of deleted files: {}", commas(deleted)),
                format!(
                    "Number of regular files transferred: {}",
                    commas(o.files_transferred)
                ),
                format!("Total file size: {} bytes", commas(l.total_size)),
                format!(
                    "Total transferred file size: {} bytes",
                    commas(o.transferred_size)
                ),
                format!("Literal data: {} bytes", commas(o.bytes_transferred)),
                format!(
                    "Matched data: {} bytes",
                    commas(o.transferred_size.saturating_sub(o.bytes_transferred))
                ),
                "File list size: 0".to_string(),
                format!("Total bytes sent: {}", commas(o.bytes_sent)),
                format!("Total bytes received: {}", commas(o.bytes_received)),
            ];
            for r in rows {
                let _ = writeln!(self.out, "{r}");
            }
            let _ = writeln!(self.out);
        }
        if self.verbosity >= 1 || self.stats {
            let (sent, recv) = (o.bytes_sent, o.bytes_received);
            let rate = (sent + recv) as f64 / elapsed.as_secs_f64().max(0.001);
            let speedup = if sent + recv > 0 {
                o.listed.total_size as f64 / (sent + recv) as f64
            } else {
                0.0
            };
            let _ = writeln!(
                self.out,
                "sent {} bytes  received {} bytes  {:.2} bytes/sec",
                commas(sent),
                commas(recv),
                rate
            );
            let _ = writeln!(
                self.out,
                "total size is {}  speedup is {:.2}{}",
                commas(o.listed.total_size),
                speedup,
                if self.dry_run { " (DRY RUN)" } else { "" }
            );
        }
        let _ = self.out.flush();
    }
}
