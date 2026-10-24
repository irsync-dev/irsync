//! User-facing output: the live progress block on a terminal, `-v` per-file
//! lines, `-i` itemize codes and the `--stats` block, driven by the session's
//! [`Event`]s.
//!
//! The reporter only ever reads from a broadcast channel, so it cannot slow
//! the engine down; if it lags it loses events (never the final totals,
//! which come from the [`Outcome`]).

pub mod render;
pub mod screen;
pub mod state;

use std::io::Write;
use std::time::{Duration, Instant};

use tokio::sync::broadcast;

use crate::cli::DeleteMode;
use crate::fs::paths::RelPath;
use crate::plan::DELETING;
use crate::session::reconnect::ConnState;
use crate::session::{Event, Outcome, Party};
use render::{render, Geometry};
use screen::{Screen, WriterTerm};
use state::Live;

/// The 10-character prefixes of the phase lines in a rendered block.
const PHASES: [&str; 3] = ["list      ", "transfer  ", "delete    "];

/// Redraw the progress block at most this often.
const REDRAW: Duration = Duration::from_millis(100);

pub struct Reporter {
    screen: Screen,
    live: Live,
    verbosity: u8,
    stats: bool,
    itemize: bool,
    dry_run: bool,
    /// Events lost because this reporter lagged.
    lagged: u64,
    last_draw: Option<Instant>,
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
pub(super) fn commas(n: u64) -> String {
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
pub(super) fn shown(p: &RelPath) -> String {
    if p.as_bytes().is_empty() {
        "./".to_string()
    } else {
        escape_path(p.as_bytes())
    }
}

pub(super) fn conn_label(c: ConnState) -> String {
    match c {
        ConnState::Connecting => "connecting".into(),
        ConnState::Direct => "direct".into(),
        ConnState::Relay => "relay".into(),
        ConnState::Reconnecting { attempt } => format!("reconnecting #{attempt}"),
    }
}

impl Reporter {
    /// `tty`: draw the live block, through a fixed 24x80 terminal over `out`
    /// (production uses [`Reporter::with_screen`]). `verbosity`: `-v` count.
    /// Output errors (a closed pipe) are ignored.
    pub fn new(
        out: Box<dyn Write + Send>,
        tty: bool,
        verbosity: u8,
        stats: bool,
        itemize: bool,
        dry_run: bool,
    ) -> Reporter {
        let screen = if tty {
            Screen::tty(Box::new(WriterTerm::new(out, || (24, 80))), || {
                Geometry::fixed(80, 24)
            })
        } else {
            Screen::plain(out)
        };
        Reporter::with_screen(screen, verbosity, stats, itemize, dry_run)
    }

    /// Production: draw on this screen (built in main from stdout).
    pub fn with_screen(
        screen: Screen,
        verbosity: u8,
        stats: bool,
        itemize: bool,
        dry_run: bool,
    ) -> Reporter {
        Reporter {
            screen,
            live: Live::new(Party::Local, DeleteMode::Off, Instant::now()),
            verbosity,
            stats,
            itemize,
            dry_run,
            lagged: 0,
            last_draw: None,
        }
    }

    /// The deletion pass to show (`Off`: no `delete` line).
    pub fn with_delete_mode(mut self, mode: DeleteMode) -> Reporter {
        self.live = Live::new(self.live.source, mode, self.live.start);
        self
    }

    /// Which party is the source (its listing total is the transfer
    /// denominator). Default [`Party::Local`].
    pub fn with_source(mut self, source: Party) -> Reporter {
        self.live.source = source;
        self
    }

    /// The live state (for tests).
    pub fn live(&self) -> &Live {
        &self.live
    }

    /// The block as it would be drawn now (for tests).
    pub fn frame(&self, now: Instant) -> Vec<String> {
        render(&self.live, &self.screen.geometry(), now)
    }

    /// Consumes events until the channel closes, then hands the reporter
    /// back for [`Reporter::finish`]. `total_hint` is (files, bytes) of the
    /// source when known up front, for the percentage.
    pub async fn run(
        mut self,
        mut rx: broadcast::Receiver<Event>,
        total_hint: Option<(u64, u64)>,
    ) -> Reporter {
        let start = Instant::now();
        self.live.start = start;
        if let Some((files, bytes)) = total_hint {
            let party = self.live.source;
            self.live.apply(
                &Event::Total {
                    party,
                    files,
                    bytes,
                },
                start,
            );
        }
        let tty = self.screen.is_tty();
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
                _ = tick.tick(), if tty => self.draw(false),
            }
        }
        self.live.transfer_done = true;
        if let Some(d) = &mut self.live.delete {
            d.finished = true;
        }
        self.draw(true);
        self
    }

    /// Applies one event (the unit the tests drive directly).
    pub fn handle(&mut self, ev: Event) {
        if matches!(ev, Event::SessionStart { index, .. } if index > 0) {
            self.print_finished_session();
        }
        self.live.apply(&ev, Instant::now());
        match ev {
            Event::FileDone(p, _) => {
                if self.verbosity >= 1 && !self.itemize {
                    self.line(&shown(&p));
                }
            }
            Event::Conn(_) => self.draw(true),
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
            Event::Bytes(_)
            | Event::FileStart(..)
            | Event::Scanned { .. }
            | Event::Total { .. }
            | Event::FileHashing(..)
            | Event::FileHashed(_)
            | Event::FileBytes(..)
            | Event::FileMatched(..)
            | Event::Skipped(..)
            | Event::PeerSkipped(_)
            | Event::Delete { .. }
            | Event::SessionStart { .. } => {}
        }
    }

    /// The finished session's `list`/`transfer`/`delete` lines, into the
    /// scrollback (a terminal only), before the block restarts.
    fn print_finished_session(&mut self) {
        if !self.screen.is_tty() {
            return;
        }
        let mut done = self.live.clone();
        done.transfer_done = true;
        if let Some(d) = &mut done.delete {
            d.finished = true;
        }
        let now = Instant::now();
        for l in render(&done, &self.screen.geometry(), now) {
            if PHASES.iter().any(|p| l.starts_with(p)) {
                self.screen.println(&l);
            }
        }
    }

    /// One output line, above any progress block.
    fn line(&mut self, s: &str) {
        self.screen.println(s);
    }

    /// Repaints the block, at most every [`REDRAW`] unless `force`; only on
    /// a terminal.
    fn draw(&mut self, force: bool) {
        if !self.screen.is_tty() {
            return;
        }
        let now = Instant::now();
        if !force && self.last_draw.is_some_and(|t| now - t < REDRAW) {
            return;
        }
        self.last_draw = Some(now);
        self.live.tick(now);
        let lines = render(&self.live, &self.screen.geometry(), now);
        self.screen.set_block(&lines);
    }

    /// Prints the closing lines: the `--stats` block and rsync's
    /// `sent .. received ..` / `total size is ..` trailer.
    pub fn finish(&mut self, outcome: &Outcome, elapsed: Duration) {
        self.screen.clear();
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
                self.line(&r);
            }
            self.line("");
        }
        if self.verbosity >= 1 || self.stats {
            let (sent, recv) = (o.bytes_sent, o.bytes_received);
            let rate = (sent + recv) as f64 / elapsed.as_secs_f64().max(0.001);
            let speedup = if sent + recv > 0 {
                o.listed.total_size as f64 / (sent + recv) as f64
            } else {
                0.0
            };
            self.line(&format!(
                "sent {} bytes  received {} bytes  {:.2} bytes/sec",
                commas(sent),
                commas(recv),
                rate
            ));
            self.line(&format!(
                "total size is {}  speedup is {:.2}{}",
                commas(o.listed.total_size),
                speedup,
                if self.dry_run { " (DRY RUN)" } else { "" }
            ));
        }
    }
}
