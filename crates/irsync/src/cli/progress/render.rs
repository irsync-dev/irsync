//! Pure rendering of the live progress block: [`Live`] and a terminal
//! geometry in, lines out. Nothing here touches the terminal.
//!
//! Widths are measured in `chars()`, not display cells: paths reach this
//! module already escaped by `escape_path` (control and invalid bytes become
//! `\ooo`), so no wide-character table is needed.

use std::time::{Duration, Instant};

use console::Style;

use super::state::{DeletePhase, Live, Row, RowState, Walk};
use super::{clock, commas, conn_label, human, shown};
use crate::fs::paths::RelPath;
use crate::session::reconnect::ConnState;
use crate::session::Party;

/// Cells in the bars on the `transfer` and `delete` lines.
const PHASE_BAR: usize = 30;
/// Cells in the bar on each in-flight row.
const ROW_BAR: usize = 20;
/// Below this width rows drop size and rate.
const NARROW: usize = 60;
/// Below this width rows and the transfer line drop the bar.
const TINY: usize = 40;
/// Below this width the phase lines drop the bar (prefix + bar + percent).
const PHASE_BAR_MIN: usize = PHASE_COL + PHASE_BAR + 2 + 2 + 4;
/// Width of the phase name column.
const PHASE_COL: usize = 10;

#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    pub width: usize,
    pub height: usize,
    pub color: bool,
    pub unicode: bool,
}

impl Geometry {
    /// Reads the terminal size via `console::Term::stdout().size()`, colour
    /// via `console::colors_enabled()`, unicode via `locale_is_utf8()`.
    pub fn detect() -> Geometry {
        let (rows, cols) = console::Term::stdout().size();
        Geometry {
            width: cols as usize,
            height: rows as usize,
            color: console::colors_enabled(),
            unicode: locale_is_utf8(),
        }
    }

    /// A fixed size without colour, with unicode glyphs.
    pub fn fixed(width: usize, height: usize) -> Geometry {
        Geometry {
            width,
            height,
            color: false,
            unicode: true,
        }
    }

    fn ellipsis(&self) -> &'static str {
        if self.unicode {
            "…"
        } else {
            "..."
        }
    }
}

/// True when LC_ALL, else LC_CTYPE, else LANG contains "utf-8"/"utf8"
/// (case-insensitive).
pub fn locale_is_utf8() -> bool {
    let value = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty())
        .unwrap_or_default()
        .to_lowercase();
    value.contains("utf-8") || value.contains("utf8")
}

/// A bar of `cells` cells for `num/den`, in brackets.
pub fn bar(num: u64, den: u64, cells: usize, g: &Geometry) -> String {
    let filled = if den == 0 {
        0
    } else {
        (num.min(den) as u128 * cells as u128 / den as u128) as usize
    };
    let (full, empty) = if g.unicode {
        ('█', '░')
    } else {
        ('#', '-')
    };
    let mut out = String::from("[");
    out.extend(std::iter::repeat_n(full, filled));
    out.extend(std::iter::repeat_n(empty, cells - filled));
    out.push(']');
    out
}

/// `s` shortened to `max` cells with `…` in the middle (unicode) or `...`
/// (ascii).
pub fn truncate_middle(s: &str, max: usize, g: &Geometry) -> String {
    let len = s.chars().count();
    if len <= max {
        return s.to_string();
    }
    let dots = g.ellipsis();
    let keep = max.saturating_sub(dots.len().min(dots.chars().count()));
    let keep = keep.min(len);
    let head = keep / 2;
    let tail = keep - head;
    let mut out: String = s.chars().take(head).collect();
    out.push_str(dots);
    out.extend(s.chars().skip(len - tail));
    out.chars().take(max).collect()
}

/// A line under construction: the plain text decides what fits, the styled
/// copy is what gets printed.
#[derive(Default)]
struct Line {
    plain: String,
    styled: String,
}

impl Line {
    fn push(&mut self, text: &str, style: Option<&Style>, g: &Geometry) {
        self.plain.push_str(text);
        match style {
            Some(style) if g.color => {
                self.styled
                    .push_str(&style.clone().force_styling(true).apply_to(text).to_string());
            }
            _ => self.styled.push_str(text),
        }
    }

    fn width(&self) -> usize {
        self.plain.chars().count()
    }

    fn finish(self) -> String {
        self.styled
    }
}

fn blue() -> Style {
    Style::new().blue()
}
fn green() -> Style {
    Style::new().green()
}
fn yellow() -> Style {
    Style::new().yellow()
}

/// `text` cut to `width` chars (with an ellipsis) when longer.
fn fit(text: &str, width: usize, g: &Geometry) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let dots = g.ellipsis();
    let keep = width.saturating_sub(dots.chars().count());
    let mut out: String = text.chars().take(keep).collect();
    out.push_str(dots);
    out.chars().take(width).collect()
}

fn plain_line(text: String, g: &Geometry) -> String {
    fit(&text, g.width, g)
}

fn pct_cell(pct: Option<u8>) -> String {
    match pct {
        Some(p) => format!("{p:>3}%"),
        None => "    ".to_string(),
    }
}

fn rate_text(bytes_per_sec: f64) -> String {
    format!("{}B/s", human(bytes_per_sec as u64))
}

/// The block: a rule line, the phase lines, the in-flight rows and the
/// "… and N more" line. Every line fits in `g.width` cells. Empty when
/// there is nothing to show yet (no events).
pub fn render(live: &Live, g: &Geometry, now: Instant) -> Vec<String> {
    if !has_activity(live) {
        return Vec::new();
    }
    let mut lines = vec![rule(g)];
    lines.push(list_line(live, g));
    lines.push(transfer_line(live, g, now));
    let delete = live.delete.as_ref().map(|d| delete_line(d, g));

    let fixed = 1 + 2 + usize::from(delete.is_some()) + 1;
    let cap = (g.height / 2).saturating_sub(fixed).max(3);
    let mut rows: Vec<(&RelPath, &Row)> = live.rows.iter().filter(|(_, r)| r.shown()).collect();
    rows.sort_by(|a, b| b.1.size.cmp(&a.1.size).then_with(|| a.0.cmp(b.0)));
    let overflow = rows.len().saturating_sub(cap);
    for (path, row) in rows.into_iter().take(cap) {
        lines.push(row_line(path, row, g, now));
    }
    if overflow > 0 {
        let text = format!("  {} and {overflow} more", g.ellipsis());
        lines.push(plain_line(text, g));
    }
    lines.extend(delete);
    lines
}

fn has_activity(live: &Live) -> bool {
    live.local.entries > 0
        || live.peer.entries > 0
        || live.local.done
        || live.peer.done
        || !live.rows.is_empty()
        || live.literal > 0
        || live.xfr > 0
        || live.delete.as_ref().is_some_and(|d| d.started)
}

fn rule(g: &Geometry) -> String {
    let c = if g.unicode { '─' } else { '-' };
    std::iter::repeat_n(c, g.width).collect()
}

fn phase(name: &str) -> String {
    format!("{name:<PHASE_COL$}")
}

fn walk_text(w: &Walk) -> String {
    let mut text = format!("{} entries", commas(w.entries));
    if !w.done {
        if let Some(at) = &w.at {
            let at = shown(at);
            let slash = if at.ends_with('/') { "" } else { "/" };
            text.push_str(&format!("  walking {at}{slash}"));
        }
    }
    text
}

fn list_line(live: &Live, g: &Geometry) -> String {
    let mut line = Line::default();
    line.push(&phase("list"), None, g);
    if live.local.done && live.peer.done {
        let src = match live.source {
            Party::Local => &live.local,
            Party::Peer => &live.peer,
        };
        line.push("done", Some(&green()), g);
        line.push(
            &format!("  {} entries  {}", commas(src.entries), human(src.bytes)),
            None,
            g,
        );
    } else {
        let mut text = walk_text(&live.local);
        if live.peer.entries > 0 {
            text.push_str(&format!("  peer {}", walk_text(&live.peer)));
        }
        line.push(&text, None, g);
    }
    cut(line, g)
}

/// A line cut to the width; styling is dropped for a cut line.
fn cut(line: Line, g: &Geometry) -> String {
    if line.width() <= g.width {
        line.finish()
    } else {
        fit(&line.plain, g.width, g)
    }
}

fn transfer_line(live: &Live, g: &Geometry, now: Instant) -> String {
    let elapsed = now
        .saturating_duration_since(live.start)
        .max(Duration::from_millis(1));
    let reconnecting = matches!(live.conn, ConnState::Reconnecting { .. });
    let rate = if reconnecting {
        0.0
    } else {
        live.literal as f64 / elapsed.as_secs_f64()
    };
    let mut line = Line::default();
    line.push(&phase("transfer"), None, g);
    if live.transfer_done {
        line.push("done", Some(&green()), g);
        line.push(
            &format!(
                "  {} sent  {} files  {}  {}",
                human(live.literal),
                commas(live.xfr),
                rate_text(rate),
                clock(elapsed)
            ),
            None,
            g,
        );
        return cut(line, g);
    }
    let (num, den) = live.transfer_fraction();
    if g.width >= PHASE_BAR_MIN {
        line.push(&bar(num, den, PHASE_BAR, g), Some(&blue()), g);
        line.push("  ", None, g);
    }
    line.push(&pct_cell(live.transfer_percent()), None, g);

    let conn = format!("[{}]", conn_label(live.conn));
    // (keep priority, 0 = keep first; text; style), in display order.
    let optional: [(usize, String, Option<Style>); 6] = [
        (3, format!("{}/{}", human(num), human(den)), None),
        (1, rate_text(rate), None),
        (2, clock(elapsed), None),
        (5, format!("xfr#{}", live.xfr), None),
        (4, format!("{} in flight", live.in_flight()), None),
        (0, conn, reconnecting.then(yellow)),
    ];
    let mut room = g.width.saturating_sub(line.width());
    let mut keep = [false; 6];
    for prio in 0..6 {
        let Some(i) = optional.iter().position(|(p, ..)| *p == prio) else {
            continue;
        };
        let need = 2 + optional[i].1.chars().count();
        // Skip what does not fit; a shorter, less important field may.
        if need > room {
            continue;
        }
        room -= need;
        keep[i] = true;
    }
    for (i, (_, text, style)) in optional.iter().enumerate() {
        if keep[i] {
            line.push("  ", None, g);
            line.push(text, style.as_ref(), g);
        }
    }
    cut(line, g)
}

fn row_line(path: &RelPath, row: &Row, g: &Geometry, now: Instant) -> String {
    let with_bar = g.width >= TINY;
    let with_cols = g.width >= NARROW;
    let mut line = Line::default();
    line.push("  ", None, g);

    // Bar and percent, or the word `hashing`, in one fixed-width column.
    let lead_width = if with_bar { ROW_BAR + 2 + 2 + 4 } else { 7 };
    let lead_start = line.width();
    match row.state {
        RowState::Hashing => line.push("hashing", None, g),
        RowState::Moving { .. } => {
            let pct = (row.size > 0)
                .then(|| (row.position() as u128 * 100 / row.size as u128).min(100) as u8);
            if with_bar {
                line.push(&bar(row.position(), row.size, ROW_BAR, g), Some(&blue()), g);
                line.push("  ", None, g);
            }
            line.push(&pct_cell(pct), None, g);
        }
    }
    let pad = (lead_start + lead_width).saturating_sub(line.width());
    line.push(&" ".repeat(pad), None, g);
    line.push("  ", None, g);

    // Trailing columns, built first so the path gets what is left.
    let mut tail = Line::default();
    if with_cols {
        tail.push(&format!("  {}", human(row.size)), None, g);
        match (row.stalled_for(now), &row.state) {
            (Some(idle), _) => {
                tail.push("  ", None, g);
                tail.push(&format!("stalled {}s", idle.as_secs()), Some(&yellow()), g);
            }
            (None, RowState::Moving { .. }) => {
                tail.push(&format!("  {}", rate_text(row.rate)), None, g);
            }
            (None, RowState::Hashing) => {}
        }
    }
    let room = g.width.saturating_sub(line.width() + tail.width()).max(1);
    line.push(&truncate_middle(&shown(path), room, g), None, g);
    line.styled.push_str(&tail.styled);
    line.plain.push_str(&tail.plain);
    cut(line, g)
}

fn delete_line(d: &DeletePhase, g: &Geometry) -> String {
    let mut line = Line::default();
    line.push(&phase("delete"), None, g);
    if d.finished {
        line.push("done", Some(&green()), g);
        line.push(&format!("  {} deleted", commas(d.done)), None, g);
    } else if !d.started {
        let mode = format!("{:?}", d.mode).to_lowercase();
        line.push(&format!("waiting (--delete-{mode})"), None, g);
    } else if let Some(total) = d.total {
        let pct = (total > 0).then(|| (d.done.min(total) as u128 * 100 / total as u128) as u8);
        if g.width >= PHASE_BAR_MIN {
            line.push(&bar(d.done, total, PHASE_BAR, g), Some(&blue()), g);
            line.push("  ", None, g);
        }
        line.push(&pct_cell(pct), None, g);
        line.push(&format!("  {}/{}", commas(d.done), commas(total)), None, g);
    } else {
        line.push(&format!("{} deleted", commas(d.done)), None, g);
    }
    cut(line, g)
}
