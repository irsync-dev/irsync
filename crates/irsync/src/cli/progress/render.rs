//! Pure rendering of the live progress block: [`Live`] and a terminal
//! geometry in, lines out. Nothing here touches the terminal.
//!
//! Widths are display cells ([`console::measure_text_width`]): paths reach
//! this module escaped by `escape_path` (control and invalid bytes become
//! `\ooo`), but CJK and emoji pass through and take two cells each.

use std::time::{Duration, Instant};

use console::Style;

use super::state::{DeletePhase, Live, Row, RowState, Walk};
use super::{commas, conn_label, human, shown};
use crate::fs::paths::RelPath;
use crate::session::reconnect::ConnState;
use crate::session::Party;
use crate::telemetry::timing::clock;

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
pub(crate) fn locale_is_utf8() -> bool {
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

/// Display width of `s` in terminal cells (ANSI styling not counted).
fn cells(s: &str) -> usize {
    console::measure_text_width(s)
}

/// The longest prefix of `s` at most `max` cells wide.
fn head_cells(s: &str, max: usize) -> String {
    let mut out = String::new();
    for c in s.chars() {
        out.push(c);
        if cells(&out) > max {
            out.pop();
            break;
        }
    }
    out
}

/// The longest suffix of `s` at most `max` cells wide.
fn tail_cells(s: &str, max: usize) -> String {
    let mut start = s.len();
    for (i, _) in s.char_indices().rev() {
        if cells(&s[i..]) > max {
            break;
        }
        start = i;
    }
    s[start..].to_string()
}

/// `s` shortened to `max` cells with `…` in the middle (unicode) or `...`
/// (ascii).
pub fn truncate_middle(s: &str, max: usize, g: &Geometry) -> String {
    if cells(s) <= max {
        return s.to_string();
    }
    let dots = g.ellipsis();
    let keep = max.saturating_sub(cells(dots));
    let head = head_cells(s, keep / 2);
    // A wide character that did not fit in the head leaves its cell to the tail.
    let tail = tail_cells(&s[head.len()..], keep - cells(&head));
    head_cells(&format!("{head}{dots}{tail}"), max)
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
        cells(&self.plain)
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
fn red() -> Style {
    Style::new().red()
}

/// `text` cut to `width` cells (with an ellipsis) when wider.
fn fit(text: &str, width: usize, g: &Geometry) -> String {
    if cells(text) <= width {
        return text.to_string();
    }
    let dots = g.ellipsis();
    let mut out = head_cells(text, width.saturating_sub(cells(dots)));
    out.push_str(dots);
    head_cells(&out, width)
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

/// The block: a rule line, the phase lines, the in-flight rows, the
/// "… and N more" line and, once a file has failed, the `errors` section.
/// Every line fits in `g.width` cells. Empty when there is nothing to show
/// yet (no events).
pub fn render(live: &Live, g: &Geometry, now: Instant) -> Vec<String> {
    if !has_activity(live) {
        return Vec::new();
    }
    let mut lines = vec![rule(g)];
    let heading = session_line(live, g);
    lines.extend(heading.clone());
    lines.push(list_line(live, g));
    lines.push(transfer_line(live, g, now));
    let ended_failed = live.transfer_done && live.session_failed;
    let delete = live
        .delete
        .as_ref()
        .map(|d| delete_line(d, ended_failed, g));
    let errors = error_lines(live, g);

    let fixed =
        1 + usize::from(heading.is_some()) + 2 + usize::from(delete.is_some()) + 1 + errors.len();
    let cap = (g.height / 2).saturating_sub(fixed).max(3);
    let mut rows: Vec<(&RelPath, &Row)> = live.rows.iter().filter(|(_, r)| r.shown()).collect();
    order_rows(&mut rows, cap, now);
    let hidden = rows.split_off(cap.min(rows.len()));
    for (path, row) in rows {
        lines.push(row_line(path, row, g, now));
    }
    if !hidden.is_empty() {
        let mut text = format!("  {} and {} more", g.ellipsis(), hidden.len());
        // Stalled rows sort last, so they are the first to go unseen.
        let stalled = hidden
            .iter()
            .filter(|(_, r)| r.stalled_for(now).is_some())
            .count();
        if stalled > 0 {
            text.push_str(&format!(" ({stalled} stalled)"));
        }
        lines.push(plain_line(text, g));
    }
    lines.extend(delete);
    lines.extend(errors);
    lines
}

/// At most this many large files hold a place at the top.
const PINNED_ROWS: usize = 2;
/// A file is large when it is at least this many times the median file in
/// flight. Among files of about one size none is.
const PINNED_FACTOR: u64 = 4;

/// How much a row has to show, best first: moving, by a coarse speed;
/// hashing; open and idle; stalled.
fn activity(row: &Row, now: Instant) -> (u8, std::cmp::Reverse<u32>) {
    let (tier, speed) = match row.state {
        _ if row.stalled_for(now).is_some() => (3, 0),
        RowState::Hashing => (1, 0),
        // The power of two below the rate: two rows of about one speed
        // do not swap places each time their rates are recomputed.
        RowState::Moving { .. } if row.rate >= 1.0 => (0, (row.rate as u64).ilog2()),
        RowState::Moving { .. } => (2, 0),
    };
    (tier, std::cmp::Reverse(speed))
}

/// Puts the rows in the order they are drawn, of which `cap` fit: the
/// files that are making progress first, so that those are what is seen.
/// A large file that is not stalled keeps a place at the top whatever its
/// speed, to be followed through its whole transfer; the rest go by
/// [`activity`], then largest first, then by path. The order only changes
/// when a rate is recomputed or a row stalls, not on every redraw.
fn order_rows(rows: &mut [(&RelPath, &Row)], cap: usize, now: Instant) {
    let mut sizes: Vec<u64> = rows.iter().map(|(_, r)| r.size).collect();
    sizes.sort_unstable();
    // The lower median: of two files, the larger can stand out.
    let median = sizes.get(sizes.len().saturating_sub(1) / 2).copied();
    let floor = median.unwrap_or(0).saturating_mul(PINNED_FACTOR).max(1);
    let large = |r: &Row| r.stalled_for(now).is_none() && r.size >= floor;
    let by_size = |a: &(&RelPath, &Row), b: &(&RelPath, &Row)| {
        b.1.size.cmp(&a.1.size).then_with(|| a.0.cmp(b.0))
    };
    rows.sort_by(|a, b| large(b.1).cmp(&large(a.1)).then_with(|| by_size(a, b)));
    let pinned = rows
        .iter()
        .take(PINNED_ROWS.min(cap / 2))
        .take_while(|(_, r)| large(r))
        .count();
    rows[pinned..].sort_by(|a, b| {
        activity(a.1, now)
            .cmp(&activity(b.1, now))
            .then_with(|| by_size(a, b))
    });
}

/// `session 2/3  ./photos -> nas:/backup`: which source the block is for,
/// in the words of the `-vv` summary. Only in a run of several sources
/// (one session each), where the blocks could not be told apart.
fn session_line(live: &Live, g: &Geometry) -> Option<String> {
    let (index, of) = live.session;
    let (from, to) = live.operands.as_ref().filter(|_| of > 1)?;
    let text = format!("session {}/{of}  {from} -> {to}", index + 1);
    Some(plain_line(text, g))
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
        || live.failed > 0
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

/// Goodput, synced bytes per wire byte: a whole percent up to 2, a
/// ratio with one decimal above, `>999×` past `999.9×`.
pub fn goodput_text(ratio: f64, g: &Geometry) -> String {
    let times = if g.unicode { '×' } else { 'x' };
    if ratio <= 2.0 {
        format!("goodput {}%", (ratio * 100.0).round() as u64)
    } else if ratio >= 999.95 {
        format!("goodput >999{times}")
    } else {
        format!("goodput {ratio:.1}{times}")
    }
}

fn transfer_line(live: &Live, g: &Geometry, now: Instant) -> String {
    let elapsed = now
        .saturating_duration_since(live.start)
        .max(Duration::from_millis(1));
    let reconnecting = matches!(live.conn, ConnState::Reconnecting { .. });
    let goodput = live.goodput().map(|r| goodput_text(r, g));
    let mut line = Line::default();
    line.push(&phase("transfer"), None, g);
    if live.transfer_done {
        let (sent, synced) = if reconnecting {
            (0.0, 0.0)
        } else {
            let secs = elapsed.as_secs_f64();
            (
                live.literal as f64 / secs,
                live.literal.saturating_add(live.matched) as f64 / secs,
            )
        };
        let mut text = format!(
            "  {} sent  {} files  avg {} sent  {} synced",
            human(live.literal),
            commas(live.xfr),
            rate_text(sent),
            rate_text(synced),
        );
        if let Some(goodput) = &goodput {
            text.push_str(&format!("  {goodput}"));
        }
        text.push_str(&format!("  {}", clock(elapsed)));
        if live.session_failed {
            line.push("failed", Some(&red()), g);
        } else {
            line.push("done", Some(&green()), g);
        }
        line.push(&text, None, g);
        return cut(line, g);
    }
    let (num, den) = live.transfer_fraction();
    if g.width >= PHASE_BAR_MIN {
        line.push(&bar(num, den, PHASE_BAR, g), Some(&blue()), g);
        line.push("  ", None, g);
    }
    line.push(&pct_cell(live.transfer_percent()), None, g);

    let rates = if reconnecting {
        Some((0.0, 0.0))
    } else {
        live.current_rates()
    };
    let conn = format!("[{}]", conn_label(live.conn));
    // (keep priority, 0 = keep first; text; style), in display order.
    let mut optional: Vec<(usize, String, Option<Style>)> =
        vec![(3, format!("{}/{}", human(num), human(den)), None)];
    if let Some((sent, synced)) = rates {
        optional.push((1, format!("{} sent", rate_text(sent)), None));
        optional.push((5, format!("{} synced", rate_text(synced)), None));
    }
    if let Some(goodput) = goodput {
        optional.push((6, goodput, None));
    }
    optional.extend([
        (2, clock(elapsed), None),
        (4, format!("{} in flight", live.in_flight()), None),
        (7, format!("xfr#{}", live.xfr), None),
        (0, conn, reconnecting.then(yellow)),
    ]);
    let mut room = g.width.saturating_sub(line.width());
    let mut keep = vec![false; optional.len()];
    for prio in 0..=7 {
        let Some(i) = optional.iter().position(|(p, ..)| *p == prio) else {
            continue;
        };
        let need = 2 + cells(&optional[i].1);
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

/// `not_run`: the session failed, so a pass that has not started never will.
fn delete_line(d: &DeletePhase, not_run: bool, g: &Geometry) -> String {
    let mut line = Line::default();
    line.push(&phase("delete"), None, g);
    if d.finished {
        line.push("done", Some(&green()), g);
        line.push(&format!("  {} deleted", commas(d.done)), None, g);
    } else if !d.started && not_run {
        line.push("not run", None, g);
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

/// The `errors` section: the count, then the newest failures, oldest
/// first, one line each (`  path: message`). The path is cut in the
/// middle so the message stays readable. Empty while nothing has failed.
fn error_lines(live: &Live, g: &Geometry) -> Vec<String> {
    if live.failed == 0 {
        return Vec::new();
    }
    let mut head = Line::default();
    head.push(&phase("errors"), None, g);
    head.push(&commas(live.failed), Some(&red()), g);
    let mut lines = vec![cut(head, g)];
    for (path, msg) in &live.recent_errors {
        let tail = format!(": {}", super::escape_text(msg));
        let room = g.width.saturating_sub(2 + cells(&tail));
        let path = truncate_middle(&shown(path), room.max(4), g);
        lines.push(plain_line(format!("  {path}{tail}"), g));
    }
    lines
}
