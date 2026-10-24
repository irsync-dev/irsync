//! Where the reporter draws: straight to a writer when stdout is not a
//! terminal, or a live block of indicatif bars (one per rendered line) with
//! scrolling lines printed above it.

use std::fmt;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle, TermLike};

use super::render::Geometry;

/// Block repaints per second, at most (the reporter redraws every 100 ms).
const REFRESH_HZ: u8 = 10;

/// Where the reporter draws.
pub enum Screen {
    /// Not a terminal: lines go straight to the writer; no block.
    Plain(Box<dyn Write + Send>),
    /// A terminal: lines print above the block, the block is a set of
    /// indicatif bars with a `{msg}` template, one per rendered line.
    Tty {
        mp: MultiProgress,
        bars: Vec<ProgressBar>,
        geometry: fn() -> Geometry,
        /// The terminal the block draws on, shared with `mp`: scrolling
        /// lines are written here with the block suspended, so they end in
        /// a newline instead of indicatif's width padding.
        term: Arc<dyn TermLike>,
    },
}

impl Screen {
    pub fn plain(out: Box<dyn Write + Send>) -> Screen {
        Screen::Plain(out)
    }

    /// A block drawn through `term` (stdout in production, a buffer in tests).
    pub fn tty(term: Box<dyn TermLike>, geometry: fn() -> Geometry) -> Screen {
        let term: Arc<dyn TermLike> = Arc::from(term);
        let target =
            ProgressDrawTarget::term_like_with_hz(Box::new(Shared(term.clone())), REFRESH_HZ);
        Screen::Tty {
            mp: MultiProgress::with_draw_target(target),
            bars: Vec::new(),
            geometry,
            term,
        }
    }

    pub fn is_tty(&self) -> bool {
        matches!(self, Screen::Tty { .. })
    }

    pub fn geometry(&self) -> Geometry {
        match self {
            Screen::Plain(_) => Geometry::fixed(80, 24),
            Screen::Tty { geometry, .. } => geometry(),
        }
    }

    /// A scrolling line (ends with a newline). Output errors (a closed pipe)
    /// are ignored.
    pub fn println(&mut self, s: &str) {
        match self {
            Screen::Plain(out) => {
                let _ = writeln!(out, "{s}");
                let _ = out.flush();
            }
            Screen::Tty { mp, term, .. } => mp.suspend(|| {
                let _ = term.write_line(s);
                let _ = term.flush();
            }),
        }
    }

    /// Replaces the block with `lines` (adds/removes bars to match; empty
    /// clears it).
    pub fn set_block(&mut self, lines: &[String]) {
        let Screen::Tty { mp, bars, .. } = self else {
            return;
        };
        while bars.len() > lines.len() {
            if let Some(bar) = bars.pop() {
                bar.finish_and_clear();
                mp.remove(&bar);
            }
        }
        while bars.len() < lines.len() {
            let style = ProgressStyle::with_template("{msg}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner());
            // Born with its line, so no repaint shows it empty.
            let bar = ProgressBar::no_length()
                .with_style(style)
                .with_message(lines[bars.len()].clone());
            bars.push(mp.add(bar));
        }
        for (bar, line) in bars.iter().zip(lines) {
            // Every `set_message` asks for a repaint (rate-limited to
            // REFRESH_HZ; the closing `tick` catches up): skip unchanged lines.
            if bar.message() != *line {
                bar.set_message(line.clone());
            }
        }
        if let Some(last) = bars.last() {
            last.tick();
        }
    }

    /// Removes the block and leaves the cursor on a fresh line.
    pub fn clear(&mut self) {
        self.set_block(&[]);
    }

    /// Runs `f` with the block hidden (for stderr writers).
    pub fn suspend<R>(&self, f: impl FnOnce() -> R) -> R {
        match self {
            Screen::Plain(_) => f(),
            Screen::Tty { mp, .. } => mp.suspend(f),
        }
    }

    /// The MultiProgress, for `LogWriter`. None for Plain.
    pub fn multi(&self) -> Option<MultiProgress> {
        match self {
            Screen::Plain(_) => None,
            Screen::Tty { mp, .. } => Some(mp.clone()),
        }
    }
}

/// Hands indicatif a terminal the screen also writes to.
#[derive(Debug)]
struct Shared(Arc<dyn TermLike>);

impl TermLike for Shared {
    fn width(&self) -> u16 {
        self.0.width()
    }

    fn height(&self) -> u16 {
        self.0.height()
    }

    fn move_cursor_up(&self, n: usize) -> io::Result<()> {
        self.0.move_cursor_up(n)
    }

    fn move_cursor_down(&self, n: usize) -> io::Result<()> {
        self.0.move_cursor_down(n)
    }

    fn move_cursor_right(&self, n: usize) -> io::Result<()> {
        self.0.move_cursor_right(n)
    }

    fn move_cursor_left(&self, n: usize) -> io::Result<()> {
        self.0.move_cursor_left(n)
    }

    fn write_line(&self, s: &str) -> io::Result<()> {
        self.0.write_line(s)
    }

    fn write_str(&self, s: &str) -> io::Result<()> {
        self.0.write_str(s)
    }

    fn clear_line(&self) -> io::Result<()> {
        self.0.clear_line()
    }

    fn flush(&self) -> io::Result<()> {
        self.0.flush()
    }
}

/// A `TermLike` over any writer with a size source. Writes ANSI cursor
/// sequences (`\x1b[{n}A` etc.) like `console::Term` does.
pub struct WriterTerm {
    out: Mutex<Box<dyn Write + Send>>,
    /// (rows, cols)
    size: fn() -> (u16, u16),
}

impl WriterTerm {
    pub fn new(out: Box<dyn Write + Send>, size: fn() -> (u16, u16)) -> WriterTerm {
        WriterTerm {
            out: Mutex::new(out),
            size,
        }
    }

    fn write(&self, s: &str) -> io::Result<()> {
        let mut out = self.out.lock().unwrap_or_else(|e| e.into_inner());
        out.write_all(s.as_bytes())
    }

    /// `\x1b[{n}{code}`; nothing for `n == 0`, as `console::Term`.
    fn cursor(&self, n: usize, code: char) -> io::Result<()> {
        if n == 0 {
            return Ok(());
        }
        self.write(&format!("\x1b[{n}{code}"))
    }
}

impl fmt::Debug for WriterTerm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WriterTerm")
            .field("size", &(self.size)())
            .finish_non_exhaustive()
    }
}

impl TermLike for WriterTerm {
    fn width(&self) -> u16 {
        (self.size)().1
    }

    fn height(&self) -> u16 {
        (self.size)().0
    }

    fn move_cursor_up(&self, n: usize) -> io::Result<()> {
        self.cursor(n, 'A')
    }

    fn move_cursor_down(&self, n: usize) -> io::Result<()> {
        self.cursor(n, 'B')
    }

    fn move_cursor_right(&self, n: usize) -> io::Result<()> {
        self.cursor(n, 'C')
    }

    fn move_cursor_left(&self, n: usize) -> io::Result<()> {
        self.cursor(n, 'D')
    }

    fn write_line(&self, s: &str) -> io::Result<()> {
        self.write(s)?;
        self.write("\n")
    }

    fn write_str(&self, s: &str) -> io::Result<()> {
        self.write(s)
    }

    fn clear_line(&self) -> io::Result<()> {
        self.write("\r\x1b[2K")
    }

    fn flush(&self) -> io::Result<()> {
        let mut out = self.out.lock().unwrap_or_else(|e| e.into_inner());
        out.flush()
    }
}
