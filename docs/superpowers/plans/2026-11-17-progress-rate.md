# Current Transfer Rate in the Progress Block Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The `-P` block's `transfer` line shows the current sent and synced rates (last 5 s) and goodput, all labelled; the `done` line shows the same three as run averages.

**Architecture:** `Live` (`state.rs`) gains a run counter of matched bytes and a 5-second list of samples taken in `tick`; it answers `current_rates()` and `goodput()`. `render.rs` formats them on the transfer line with new drop priorities. No engine or event change.

**Tech Stack:** Rust, `console` crate (already used), integration tests in `crates/irsync/tests/progress_live.rs`.

**Spec:** `docs/superpowers/specs/2026-11-17-progress-rate-design.md`

## Global Constraints

- Window 5 s; no current rate shown until the samples span at least 1 s.
- While `ConnState::Reconnecting`, both rates show 0 (`0B/s sent  0B/s synced`).
- Goodput = `(literal + matched) / literal`, cumulative over the session; hidden while literal is 0.
- Goodput ≤ 2: whole percent (`100%`..`200%`); > 2: one decimal and `×` (`x` when `!g.unicode`); a value that would print above `999.9` prints `>999×` (`>999x`).
- Labels: `41.20MB/s sent`, `380.00MB/s synced`, `goodput 9.2×`; done line `avg 17.80MB/s sent  80.30MB/s synced  goodput 4.5×`.
- Running line display order: bytes, sent, synced, goodput, clock, in flight, xfr, conn. Keep priority (0 kept first): conn 0, sent 1, clock 2, bytes 3, in flight 4, synced 5, goodput 6, xfr 7.
- Unchanged: per-file row rates, `--stats`, `-vv` summary, `sent … received …` trailer.
- Commits on `main`, message = summary line, blank line, short why. No attribution trailers.

## Review Focus

1. A second `FileMatched` for a path with a smaller value (row re-created after a reconnect): the matched counter must never go down.
2. Bytes stop (stall) while redraws continue: the current rates must fall to 0 within 5 s, not hold the last value.
3. Redraws arrive late (one 10 s gap between ticks): the rate must use the real time span, not assume 5 s.
4. Every byte matched, nothing on the wire (an unchanged tree re-synced): goodput hidden, synced rate still shown, no division by zero.
5. Any terminal width: the transfer line, with all new fields, never exceeds the width (20..200 columns).

Tests for 1–4 are in Task 1, for 5 in Task 2.

---

### Task 1: Matched counter, rate window and goodput in `Live`

**Files:**
- Modify: `crates/irsync/src/cli/progress/state.rs` (`Live` struct, `Live::new`, `apply` for `FileMatched`, `tick`)
- Test: `crates/irsync/tests/progress_live.rs`

**Interfaces:**
- Produces:
  - `pub const SPEED_WINDOW: Duration` = 5 s; `const SPEED_MIN_SPAN: Duration` = 1 s (private).
  - `Live.matched: u64` (pub): matched bytes this session.
  - `Live.samples: VecDeque<(Instant, u64, u64)>` (pub): `(time, literal, literal + matched)`.
  - `Live::current_rates(&self) -> Option<(f64, f64)>`: `(sent, synced)` bytes/s; `None` when the samples span < 1 s.
  - `Live::goodput(&self) -> Option<f64>`: `None` while `literal == 0`.

- [ ] **Step 1: Write the failing tests** in `progress_live.rs` (after `failed_events_…`):

```rust
#[test]
fn matched_bytes_are_counted_once_and_never_go_down() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(&Event::FileMatched(rp("b"), 300), t); // before FileStart
    l.apply(&Event::FileStart(rp("b"), 400), t);
    l.apply(&Event::FileMatched(rp("b"), 350), t);
    assert_eq!(l.matched, 350);
    l.apply(&Event::FileMatched(rp("b"), 100), t); // smaller: ignored
    assert_eq!(l.matched, 350);
    l.apply(&Event::FileDone(rp("b"), 400), t);
    assert_eq!(l.matched, 350);
}

#[test]
fn current_rates_cover_the_last_five_seconds() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(&Event::FileStart(rp("f"), 1 << 30), t);
    l.tick(t);
    assert_eq!(l.current_rates(), None);
    l.apply(&Event::FileBytes(rp("f"), 500), t);
    l.tick(t + Duration::from_millis(500));
    assert_eq!(l.current_rates(), None, "under 1 s of samples");
    l.apply(&Event::FileMatched(rp("f"), 1500), t);
    l.tick(t + Duration::from_secs(1));
    assert_eq!(l.current_rates(), Some((500.0, 2000.0)));
    // Bytes stop; ticks go on: the burst leaves the window.
    for s in 2..=7 {
        l.tick(t + Duration::from_secs(s));
    }
    assert_eq!(l.current_rates(), Some((0.0, 0.0)));
}

#[test]
fn a_late_tick_uses_the_real_time_span() {
    let (mut l, t) = (live(), Instant::now());
    l.tick(t);
    l.apply(&Event::FileBytes(rp("f"), 1000), t);
    l.tick(t + Duration::from_secs(10));
    assert_eq!(l.current_rates(), Some((100.0, 100.0)));
}

#[test]
fn goodput_is_hidden_without_wire_bytes() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(&Event::FileMatched(rp("f"), 1000), t);
    assert_eq!(l.goodput(), None);
    l.apply(&Event::FileBytes(rp("f"), 250), t);
    assert_eq!(l.goodput(), Some(5.0));
}

#[test]
fn a_new_session_clears_matched_and_samples() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(&Event::FileMatched(rp("f"), 1000), t);
    l.tick(t);
    l.apply(&Event::SessionStart { index: 1, of: 2 }, t);
    assert_eq!(l.matched, 0);
    assert!(l.samples.is_empty());
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -q -p irsync --test progress_live`
Expected: compile errors (`no field matched`, `no method current_rates`).

- [ ] **Step 3: Implement in `state.rs`**

- `FileMatched`: before setting the row's `matched = *n`, add `n.saturating_sub(old)` to `self.matched`, where `old` is the row's current `matched` (0 for a new or hashing row).
- `tick(now)`: push `(now, literal, literal + matched)`; then drop the front while the *second* sample's time is `<= now - SPEED_WINDOW` (so the oldest kept sample is at or just before the window start, and a lone late tick still has a partner).
- `current_rates`: oldest vs newest sample; span = newest.0 − oldest.0; `None` if span < `SPEED_MIN_SPAN`; else the two deltas divided by the span in seconds.
- `goodput`: `(literal + matched) as f64 / literal as f64`.
- `Live::new` initialises both; `reset` needs no change (it rebuilds from `new`).

- [ ] **Step 4: Run the tests**

Run: `cargo test -q -p irsync --test progress_live`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/irsync/src/cli/progress/state.rs crates/irsync/tests/progress_live.rs
git commit -m "Track matched bytes and a 5-second rate window in the progress state (#25)

The transfer line needs current sent and synced rates and goodput."
```

---

### Task 2: Show the rates and goodput on the transfer line

**Files:**
- Modify: `crates/irsync/src/cli/progress/render.rs` (`transfer_line`, new `goodput_text`)
- Test: `crates/irsync/tests/progress_live.rs` (render section, imports at line 239)

**Interfaces:**
- Consumes: `Live::current_rates`, `Live::goodput`, `Live.matched` (Task 1).
- Produces: `pub fn goodput_text(ratio: f64, g: &Geometry) -> String` → `"goodput 150%"`, `"goodput 9.2×"`, `"goodput >999×"`.

- [ ] **Step 1: Write the failing tests** (add `goodput_text` to the render import):

```rust
#[test]
fn goodput_is_a_percent_up_to_two_and_a_ratio_above() {
    let mut g = Geometry::fixed(80, 20);
    assert_eq!(goodput_text(1.0, &g), "goodput 100%");
    assert_eq!(goodput_text(2.0, &g), "goodput 200%");
    assert_eq!(goodput_text(2.1, &g), "goodput 2.1×");
    assert_eq!(goodput_text(999.9, &g), "goodput 999.9×");
    assert_eq!(goodput_text(5000.0, &g), "goodput >999×");
    g.unicode = false;
    assert_eq!(goodput_text(9.2, &g), "goodput 9.2x");
}

#[test]
fn transfer_line_shows_labelled_rates_and_goodput() {
    let (mut l, t) = mid_sync();
    l.tick(t);
    l.apply(&Event::FileMatched(rp("2024/trip/clip_0007.mov"), 5_480_000_000), t);
    l.apply(&Event::FileBytes(rp("2024/trip/clip_0007.mov"), 1_000_000), t);
    l.tick(t + Duration::from_secs(1));
    let lines = render(&l, &Geometry::fixed(200, 40), t + Duration::from_secs(1));
    let line = &lines[2];
    assert!(line.contains("976.56KB/s sent"), "{line}");
    assert!(line.contains("5.10GB/s synced"), "{line}");
    assert!(line.contains("goodput 9.0×"), "{line}");
    let order = ["B/s sent", "B/s synced", "goodput", "0:00:01", "in flight", "xfr#", "[connecting]"];
    let at: Vec<usize> = order.iter().map(|s| line.find(s).unwrap()).collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{line}");
}

#[test]
fn narrow_transfer_lines_drop_xfr_before_goodput_before_synced() {
    let (mut l, t) = mid_sync();
    l.tick(t);
    l.apply(&Event::FileMatched(rp("2024/trip/clip_0007.mov"), 1_000_000_000), t);
    l.tick(t + Duration::from_secs(1));
    let now = t + Duration::from_secs(1);
    let full = render(&l, &Geometry::fixed(200, 40), now)[2].clone();
    let w = full.chars().count();
    let at = |width| render(&l, &Geometry::fixed(width, 40), now)[2].clone();
    assert!(!at(w - 1).contains("xfr#") && at(w - 1).contains("goodput"));
    let no_xfr = at(w - 1).chars().count();
    assert!(!at(no_xfr - 1).contains("goodput") && at(no_xfr - 1).contains("synced"));
    for width in 20..=200 {
        for line in render(&l, &Geometry::fixed(width, 40), now) {
            assert!(line.chars().count() <= width, "{width}: {line:?}");
        }
    }
}

#[test]
fn done_line_shows_averages_and_goodput() {
    let t = Instant::now();
    let mut l = live();
    l.apply(&Event::FileMatched(rp("f"), 3_000_000), t);
    l.apply(&Event::FileBytes(rp("f"), 1_000_000), t);
    l.transfer_done = true;
    l.xfr = 1;
    let lines = render(&l, &Geometry::fixed(200, 30), t + Duration::from_secs(2));
    assert!(
        lines[2].contains("avg 488.28KB/s sent  1.91MB/s synced  goodput 4.0×  0:00:02"),
        "{}",
        lines[2]
    );
}
```

The existing `stalled_and_reconnecting_are_shown` (expects `0B/s`) and `connection_state_survives_at_80_columns` must keep passing; the `0B/s` comes from the reconnect rule, which applies even when `current_rates()` is `None`.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -q -p irsync --test progress_live`
Expected: compile error on `goodput_text`, then the new assertions fail.

- [ ] **Step 3: Implement in `render.rs`**

- `goodput_text`: ratio ≤ 2.0 → `format!("goodput {}%", (ratio * 100.0).round())`; otherwise one decimal with `×`/`x`, and `>999×` when the one-decimal value would exceed 999.9 (ratio ≥ 999.95).
- Running line: rates = `Some((0.0, 0.0))` when reconnecting, else `live.current_rates()`; when `None`, the two rate fields are left out. Goodput field present when `live.goodput()` is `Some`. Turn the fixed `[_; 6]` array into a `Vec` of `(priority, text, style)` in the display order and priorities from Global Constraints; keep the existing fit loop.
- Done line: `"  {sent} sent  {files} files  avg {r} sent  {s} synced  goodput …  {clock}"`, averages = bytes / elapsed (0 while reconnecting, as today); goodput part left out when `None`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -q -p irsync --test progress_live`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/irsync/src/cli/progress/render.rs crates/irsync/tests/progress_live.rs
git commit -m "Show current sent and synced rates and goodput on the transfer line (#25)

The old rate was an unlabelled run average of wire bytes, which starts
low and moves slowly."
```

---

### Task 3: Docs

**Files:**
- Modify: `crates/irsync/src/cli/progress/mod.rs:16-24` (example block) and the paragraph after it
- Modify: `book/src/reference/command-line.md:210-224` (Output section)

- [ ] **Step 1: Update the example** transfer line in `mod.rs` to
`transfer  [██████████░░░░░░░░░░░░░░░░░░░░]   33%  41.20MB/s sent  380.00MB/s synced  goodput 9.2×  0:01:23  xfr#1,882  [direct]`
and add one sentence: the rates are over the last 5 seconds; `synced` counts matched bytes too; goodput is synced over sent bytes for the session; the `done` line shows averages over the run.

- [ ] **Step 2: Add the same explanation to the book's Output section**, after the sentence about the transfer bar's `N in flight`: what `sent`, `synced` and `goodput` mean, the percent-up-to-2 / ratio-above format, and that `done` shows run averages.

- [ ] **Step 3: Run the man page tests**

Run: `python3 -m unittest discover -s tools/tests`
Expected: `OK`.

- [ ] **Step 4: Full regression**

Run: `tools/ci.sh check`
Expected: exits 0.

- [ ] **Step 5: Commit**

```bash
git add crates/irsync/src/cli/progress/mod.rs book/src/reference/command-line.md
git commit -m "Document the transfer rates and goodput in the progress block (#25)"
```
