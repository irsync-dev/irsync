//! User-facing output: the live progress block (`--progress`), `-v` per-file
//! lines, `-i` itemize codes, the `--stats` block and failed files on stderr,
//! driven by the session's [`Event`]s.
//!
//! As in rsync, a sync prints nothing on stdout by default; `-v` lists the
//! files and ends with the `sent .. received ..` trailer, `-vv` adds a
//! summary per session, and `-q` prints errors only. Without the block each
//! failed file goes to stderr as it fails, and main leaves those out of its
//! list at the end ([`Reporter::take_shown`]).
//!
//! With `--progress` (or `-P`, or `--info=progress1|2`), when stdout is a
//! terminal (and `TERM` is not `dumb`) and `-q` is not given, a block sits
//! under the scrolling output:
//!
//! ```text
//! ──────────────────────────────────────────────────────────────────────
//! list      18,204 entries  walking 2023/  peer 9,120 entries  walking 2022/
//! transfer  [██████████░░░░░░░░░░░░░░░░░░░░]   33%  41.20MB/s  0:01:23  xfr#1,882  [direct]
//!   [███████░░░░░░░░░░░░░]   36%  2024/trip/clip_0007.mov  1.90G  8.10MB/s
//!   hashing                       2024/trip/clip_0009.mov  2.40G
//!   … and 28 more
//! delete    waiting (--delete-after)
//! errors    3
//!   share/emacs/30.1/etc/sch…/xhtml-csismap.rnc: receiver: opening partial file: Too many open files (os error 24)
//! ```
//!
//! One line per phase (`list`, `transfer`, `delete` with a `--delete*` flag),
//! each collapsing to `done` and a summary when it ends, and one row per file
//! the local side is working on (an open stream or a hash), largest first,
//! capped at half the terminal. Once a file has failed an `errors` section
//! closes the block: the count and the newest three failures, one fitted
//! line each (the full list is printed on stderr after the block is
//! cleared). The roles log a failed file at `info`, so by default nothing
//! scrolls for it on the client while the daemon's log still records it. The transfer percent has rsync's
//! `--info=progress2` meaning: skipped and matched bytes count as done. Lines
//! are fitted to the terminal width, which is re-read on every redraw (at most
//! every 100 ms, at once on a connection state change). At the end of a
//! session the block is erased before the trailer; with several sources each
//! finished session leaves its `done` lines in the scrollback.
//!
//! The work is split three ways:
//! - [`state`](mod@state): [`Live`], what the block knows, folded from events
//!   ([`Live::apply`]); no I/O, no clock of its own.
//! - [`render`](mod@render): [`Live`] plus a [`Geometry`] (size, colour, unicode) in,
//!   lines out; pure, so tests assert on frames for a given size.
//! - [`screen`](mod@screen): where lines go. [`Screen::Plain`] writes straight to the
//!   writer; [`Screen::Tty`] draws the block through indicatif and prints
//!   scrolling lines (and, via [`screen::LogWriter`], tracing output on
//!   stderr) above it.
//!
//! Without the block the output is the same `-v`, `-i`, `--stats` and
//! trailer lines, byte for byte.
//!
//! The reporter only ever reads from a broadcast channel, so it cannot slow
//! the engine down; if it lags it loses events (never the final totals,
//! which come from the [`Outcome`]).

pub mod render;
pub mod screen;
pub mod state;
pub mod summary;

use std::collections::HashMap;
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
    /// `-q`: nothing on stdout; errors still go to `errors`.
    quiet: bool,
    /// Where a failed file is reported as it happens when there is no block
    /// (stderr in production).
    errors: Box<dyn Write + Send>,
    /// Failures already printed on `errors`, with how often, so that main
    /// does not list them again at the end ([`Reporter::take_shown`]).
    shown: HashMap<(RelPath, String), usize>,
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

pub use crate::fs::paths::escape_path;

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
            quiet: false,
            errors: Box::new(std::io::stderr()),
            shown: HashMap::new(),
            stats,
            itemize,
            dry_run,
            lagged: 0,
            last_draw: None,
        }
    }

    /// `-q`: print nothing on stdout, only the failures (on `errors`).
    pub fn with_quiet(mut self, quiet: bool) -> Reporter {
        self.quiet = quiet;
        self
    }

    /// Where failures go as they happen (default stderr).
    pub fn with_errors(mut self, errors: Box<dyn Write + Send>) -> Reporter {
        self.errors = errors;
        self
    }

    /// Whether this failure was already printed as it happened; each call
    /// accounts for one printing. Main lists only the others at the end.
    pub fn take_shown(&mut self, path: &RelPath, msg: &str) -> bool {
        let key = (path.clone(), msg.to_string());
        match self.shown.get_mut(&key) {
            Some(n) if *n > 1 => {
                *n -= 1;
                true
            }
            Some(_) => {
                self.shown.remove(&key);
                true
            }
            None => false,
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
            Event::FileDone(p, _) | Event::WouldTransfer(p) => {
                if self.verbosity >= 1 && !self.itemize {
                    self.line(&shown(&p));
                }
            }
            Event::Conn(_) => self.draw(true),
            Event::SessionEnd(s) => {
                if self.verbosity >= 2 {
                    let width = self.screen.geometry().width;
                    for l in summary::render(&s, width) {
                        self.line(&l);
                    }
                }
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
                } else if self.verbosity >= 1 {
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
            | Event::FileSent(_)
            | Event::Skipped(..)
            | Event::PeerSkipped(_)
            | Event::Delete { .. }
            | Event::SessionStart { .. } => {}
            // On a terminal with the block, the block's `errors` section
            // shows it and main lists every failure once the block is gone.
            Event::Failed(p, msg) => {
                if !self.screen.is_tty() {
                    let line = format!("irsync: {}: {msg}\n", p.to_os().to_string_lossy());
                    let _ = self.errors.write_all(line.as_bytes());
                    let _ = self.errors.flush();
                    *self.shown.entry((p, msg)).or_default() += 1;
                }
            }
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

    /// One output line, above any progress block; none under `-q`.
    fn line(&mut self, s: &str) {
        if self.quiet {
            return;
        }
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

    /// Erases the block, leaving the cursor at the start of a blank line, so
    /// that other writers to the terminal (stderr) can print. Idempotent;
    /// [`Reporter::finish`] calls it too.
    pub fn clear(&mut self) {
        self.screen.clear();
    }

    /// Prints the closing lines: the `--stats` block and rsync's
    /// `sent .. received ..` / `total size is ..` trailer.
    pub fn finish(&mut self, outcome: &Outcome, elapsed: Duration) {
        self.screen.clear();
        if self.lagged > 0 && !self.quiet {
            eprintln!(
                "irsync: note: {} progress events were dropped (output may be incomplete)",
                self.lagged
            );
        }
        let o = outcome;
        if self.stats {
            let l = &o.listed;
            let deleted = if self.dry_run {
                o.would_delete_count()
            } else {
                o.deleted_count()
            };
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
