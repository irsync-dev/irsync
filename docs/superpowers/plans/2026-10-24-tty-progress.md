# Live Progress Block on a TTY Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When the sync client's stdout is a terminal, draw a live block at the bottom of the screen showing the list, transfer and delete phases and one bar per file in flight, on by default and off with `-q`.

**Architecture:** The engine keeps emitting `session::Event`s on the lossy broadcast bus; the events grow per-file detail, a `Party` tag (local or peer) and a few new kinds. The far side's walk position, skipped bytes and deletion count ride on the existing `Heartbeat` frame. On the client, a pure `Live` state consumes events, a pure renderer turns it into lines for a given terminal size, and an `indicatif` `MultiProgress` paints those lines as a sticky block with normal output and tracing scrolling above it.

**Tech Stack:** Rust, tokio, prost protobuf, `indicatif` 0.18 and `console` 0.16 (both already in `Cargo.lock`), `tracing-subscriber` 0.3.

**Spec:** `docs/superpowers/specs/2026-10-24-tty-progress-design.md`

## Global Constraints

- No new protocol message types. New fields go on `Heartbeat` only.
- Non-TTY output stays byte-identical to today. The existing non-TTY tests in `crates/irsync/tests/progress.rs` must keep passing unchanged.
- The reporter never blocks the engine: it stays a `broadcast` subscriber; event emission stays `EventBus::emit` (drop on lag).
- Redraw at most every 100 ms (`REDRAW` in `cli/progress`), immediately on a connection state change and on the final frame.
- Block height: never more than half the terminal; at least 3 in-flight rows when there is room.
- Stalled after 2 s without `FileBytes`; a row with no event for 30 s is dropped.
- Bars: 30 cells on the `transfer` phase line, 20 cells per in-flight row; glyphs `█`/`░`, or `#`/`-` when the locale is not UTF-8.
- Colours: bars blue, `done` green, `reconnecting #n` and `stalled Ns` yellow; none when `NO_COLOR` is set or the terminal lacks colour (via `console`).
- Narrow terminals: below 60 columns in-flight rows drop size and rate; below 40 columns bars are dropped, percent and path remain.
- Only `-q` turns the block off. `--progress`, `--info=progress1`, `--info=progress2` stay accepted and change nothing.
- `TERM=dumb` counts as not a terminal.
- Commit messages follow the repo's `type(scope): summary` style and end with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.
- Run `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings` before every commit.

## Review Focus

1. A path longer than the terminal is wide must be truncated in the middle with `…`, never wrapped (a wrapped line breaks indicatif's cursor arithmetic and the block "walks" down the screen). Test in Task 3.
2. A resumed connection re-emits `FileStart` for a file that already has a row; the row must be refreshed, never duplicated, and the total percent must not jump back. Test in Task 2.
3. `Total` can arrive after bytes have moved (pull: the peer's listing ends late). The percent is capped at 100 and the bar must not panic on a zero denominator. Test in Task 2.
4. A lagging reporter can lose a `FileDone`; the row must disappear after 30 s and the `in flight` count must follow. Test in Task 2.
5. A locale without UTF-8 (`LC_ALL=C`) must get ASCII bar glyphs so the block does not print mojibake. Test in Task 3.

---

## File Structure

- Modify `crates/irsync/src/session/mod.rs`: `Event` gains variants and `Party`; `Busy` shared peer-progress state.
- Modify `crates/irsync/src/session/{engine,control,sender_role,receiver,recv_io,planner,recv_delete}.rs`, `crates/irsync/src/{send,hasher,delete}.rs`: emit the new events; fill and read the heartbeat fields.
- Modify `proto/irsync/v1/control.proto`: `Heartbeat` fields.
- Replace `crates/irsync/src/cli/progress.rs` with a directory module:
  - `cli/progress/mod.rs`: `Reporter` (event loop, scrolling lines, `finish`), `human`, `clock`, `escape_path` (moved verbatim).
  - `cli/progress/state.rs`: `Live`, the pure state the block renders from.
  - `cli/progress/render.rs`: `Geometry`, `render(&Live, &Geometry, Instant) -> Vec<String>`.
  - `cli/progress/screen.rs`: `Screen` (plain writer or indicatif block), `WriterTerm` (a `TermLike` over a `Write`, for tests and for stdout), `LogWriter` (tracing `MakeWriter` that prints above the block).
- Modify `crates/irsync/src/main.rs`: always-on TTY detection, `-q`, logging routed through the screen, `SessionStart` events.
- Tests: `crates/irsync/tests/progress.rs` (reporter), new `crates/irsync/tests/progress_live.rs` (state and renderer), `crates/irsync/tests/progress_e2e.rs` (engine events), `crates/irsync/tests/common/` is the existing harness (`Harness`, `Flow::{Push,Pull}`, `run_reconnect`, `local_side().hooks.bus`).

---

### Task 1: Event model

**Files:**
- Modify: `crates/irsync/src/session/mod.rs:433-460` (the `Event` enum)
- Modify: every emitter named below so the crate compiles with the new shapes
- Modify: `crates/irsync/src/cli/progress.rs:200-245` (`Reporter::handle`)
- Test: `crates/irsync/tests/progress_e2e.rs:176-211`, `crates/irsync/tests/progress.rs`

**Interfaces:**
- Produces, in `crates/irsync/src/session/mod.rs`:

```rust
/// Which side of the session an event describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Party { Local, Peer }

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A data stream for this file is open (both roles); its size.
    FileStart(RelPath, u64),
    /// A hasher started reading this file; its size.
    FileHashing(RelPath, u64),
    /// The hasher finished reading this file.
    FileHashed(RelPath),
    /// Literal bytes moved on a data stream for this file.
    FileBytes(RelPath, u64),
    /// Bytes of this file that need no transfer (matched basis chunks,
    /// local copies). Absolute, sent once per open.
    FileMatched(RelPath, u64),
    /// A file was placed (receiver) or acknowledged (sender); its size.
    FileDone(RelPath, u64),
    /// The planner found this regular file up to date; its size.
    Skipped(RelPath, u64),
    /// Bytes of source files the peer's planner skipped so far (cumulative).
    PeerSkipped(u64),
    /// File data bytes moved on a data stream (aggregate, as before).
    Bytes(u64),
    /// Listing progress for one side.
    Scanned { party: Party, entries: u64, bytes: u64, at: Option<RelPath> },
    /// One side's listing is complete: regular files and their total size.
    Total { party: Party, files: u64, bytes: u64 },
    /// Deletion pass progress for one side.
    Delete { party: Party, done: u64, total: Option<u64> },
    /// A new session (one source) starts: its 0-based index and the count.
    SessionStart { index: usize, of: usize },
    Conn(reconnect::ConnState),
    Deleted(RelPath),
    WouldDelete(RelPath),
    Itemized(RelPath, String),
    Hardlinked(RelPath, RelPath),
}
```

- [ ] **Step 1: Update the e2e assertions to the new shapes**

In `crates/irsync/tests/progress_e2e.rs`, `scanned_event_emitted_on_push` (line 176) and `pull_reports_total_from_peer_listing` (line 197):

```rust
// scanned_event_emitted_on_push
assert!(
    events.iter().any(|e| matches!(e, Event::Scanned { party: Party::Local, entries, .. } if *entries >= 1000)),
    "no local Scanned >= 1000: {events:?}"
);
let total = events.iter().find_map(|e| match e {
    Event::Total { party: Party::Local, files, bytes } => Some((*files, *bytes)),
    _ => None,
});
assert_eq!(total, Some((1100, 1100)));

// pull_reports_total_from_peer_listing
let total = events.iter().find_map(|e| match e {
    Event::Total { party: Party::Peer, files, bytes } => Some((*files, *bytes)),
    _ => None,
});
assert_eq!(total, Some((2, 3 + 5000)));
```

In `crates/irsync/tests/progress.rs`, every `Event::FileStart(rp("a/one"))` becomes `Event::FileStart(rp("a/one"), 10)` and every `Event::Total { files, bytes }` gains `party: Party::Local`. Import `Party` from `irsync::session`.

- [ ] **Step 2: Run the two test files to see them fail to compile**

Run: `cargo test -p irsync --test progress --test progress_e2e 2>&1 | head -30`
Expected: compile errors naming `Party` and the `Scanned` shape.

- [ ] **Step 3: Replace the `Event` enum and add `Party`**

In `crates/irsync/src/session/mod.rs:433-460`, as in the Interfaces block. Re-export nothing new from `lib.rs`; `Party` is reached as `irsync::session::Party`.

- [ ] **Step 4: Make every emitter compile, preserving today's meaning**

Minimal edits only; the new variants are emitted in later tasks.

- `crates/irsync/src/session/sender_role.rs:261`: `FileStart(path.clone(), 0)` (moved and given a size in Task 6).
- `crates/irsync/src/session/receiver.rs:608`: `FileStart(f.path.clone(), f.entry.size)`.
- `crates/irsync/src/session/control.rs:142` (peer listing end in `spawn_reader`): `Total { party: Party::Peer, files: seen.reg, bytes: seen.total_size }`.
- `crates/irsync/src/session/control.rs:295`: `Scanned { party: Party::Local, entries: count, bytes: stats.total_size, at: None }`.
- `crates/irsync/src/session/control.rs:305`: `Total { party: Party::Local, files: stats.reg, bytes: stats.total_size }`.

- [ ] **Step 5: Make the reporter compile**

In `crates/irsync/src/cli/progress.rs` `handle`: `FileStart(_, _) => {}`; `Scanned { entries, .. } => self.scanned = entries`; `Total { bytes, files, .. }` as before (ignoring `party` keeps today's pull behaviour, because the local pull listing still has no bus until Task 7); the other new variants `=> {}`.

- [ ] **Step 6: Build and run the whole suite**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo test -p irsync 2>&1 | tail -15`
Expected: all green (the e2e tests take a while).

- [ ] **Step 7: Commit**

```bash
git add -A crates/irsync
git commit -m "refactor(session): tag listing events with the party and give events a per-file shape"
```

---

### Task 2: Live state

**Files:**
- Create: `crates/irsync/src/cli/progress/state.rs`
- Modify: `crates/irsync/src/cli/progress.rs` → move to `crates/irsync/src/cli/progress/mod.rs` (`git mv`), add `pub mod state;`
- Test: `crates/irsync/tests/progress_live.rs` (new)

**Interfaces:**
- Produces, in `cli/progress/state.rs`:

```rust
use std::collections::BTreeMap;
use std::time::Instant;
use crate::cli::DeleteMode;
use crate::fs::paths::RelPath;
use crate::session::reconnect::ConnState;
use crate::session::Event;

pub const STALL_AFTER: Duration = Duration::from_secs(2);
pub const ROW_EXPIRY: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Walk { pub entries: u64, pub bytes: u64, pub at: Option<RelPath>, pub done: bool }

#[derive(Debug, Clone, PartialEq)]
pub enum RowState { Hashing, Moving { literal: u64, matched: u64 } }

#[derive(Debug, Clone)]
pub struct Row {
    pub size: u64,
    pub state: RowState,
    pub started: Instant,
    pub last_event: Instant,
    /// Literal bytes since `rate_from`, for the per-file rate.
    pub rate_from: Instant,
    pub rate_bytes: u64,
    pub rate: f64, // bytes per second, last computed
}

impl Row {
    /// Position in the file, capped at `size`.
    pub fn position(&self) -> u64;
    pub fn stalled_for(&self, now: Instant) -> Option<Duration>; // Some when Moving and no FileBytes for >= STALL_AFTER
}

#[derive(Debug, Clone)]
pub struct DeletePhase { pub mode: DeleteMode, pub done: u64, pub total: Option<u64>, pub started: bool, pub finished: bool }

#[derive(Debug, Clone)]
pub struct Live {
    pub local: Walk,
    pub peer: Walk,
    /// Which party is the source (its total is the transfer denominator).
    pub source: Party,
    pub literal: u64,          // wire bytes, for the rate
    pub done_bytes: u64,       // sum of FileDone sizes
    pub skipped_local: u64,    // sum of Skipped sizes
    pub skipped_peer: u64,     // latest PeerSkipped
    pub xfr: u64,
    pub rows: BTreeMap<RelPath, Row>,
    pub conn: ConnState,
    pub delete: Option<DeletePhase>,
    pub transfer_done: bool,
    pub session: (usize, usize), // (index, of)
    pub start: Instant,
}

impl Live {
    pub fn new(source: Party, delete: DeleteMode, start: Instant) -> Live;
    /// Applies one event at `now`.
    pub fn apply(&mut self, ev: &Event, now: Instant);
    /// Drops rows idle for ROW_EXPIRY; recomputes rates. Called each redraw.
    pub fn tick(&mut self, now: Instant);
    /// (numerator, denominator) for the transfer percent, per the spec.
    pub fn transfer_fraction(&self) -> (u64, u64);
    /// 0..=100, None when the denominator is 0.
    pub fn transfer_percent(&self) -> Option<u8>;
    pub fn in_flight(&self) -> usize;
    /// Clears per-session state for a new session; keeps `conn`.
    pub fn reset(&mut self, now: Instant);
}
```

Semantics to implement (from the spec):
- `transfer_fraction`: numerator = `done_bytes + skipped_local + skipped_peer + Σ rows.position()`; denominator = source party's `Walk.bytes` (`local` on push, `peer` on pull). Cap numerator at denominator.
- `FileHashing` inserts/updates a row as `Hashing`; `FileHashed` removes it only if still `Hashing`.
- `FileStart` inserts or refreshes a row as `Moving { literal: 0, matched: 0 }`, keeping `started` if the row exists.
- `FileBytes` adds to `literal` of that row (creating a `Moving` row of unknown size 0 if missing) and to `self.literal`, and updates `last_event`.
- `FileMatched` sets `matched` (absolute).
- `FileDone` removes the row, `xfr += 1`, `done_bytes += size`.
- `Skipped` adds to `skipped_local`; `PeerSkipped(n)` sets `skipped_peer = max(skipped_peer, n)`.
- `Scanned` updates that party's `Walk` with `entries = max(old, new)` and `bytes = max(old, new)`; `at` replaced when `Some`. `Total` sets `done = true` and the final `entries`/`bytes` (files count is not shown; store `bytes`).
- `Delete { party, done, total }` marks `started`, sets `done`/`total` (`total` only when `Some`); `finished` is set by `tick` when `total == Some(done)`.
- `SessionStart { index, of }` with `index > 0` calls `reset`; always sets `session`.
- `Conn` sets `conn`.
- `tick`: remove rows with `now - last_event >= ROW_EXPIRY`; for each `Moving` row recompute `rate` from `rate_bytes / (now - rate_from)` when that window is ≥ 1 s, then reset the window.

- [ ] **Step 1: Move the module into a directory**

```bash
mkdir -p crates/irsync/src/cli/progress
git mv crates/irsync/src/cli/progress.rs crates/irsync/src/cli/progress/mod.rs
```

Add `pub mod state;` at the top of `mod.rs`. Build: `cargo build -p irsync`.

- [ ] **Step 2: Write the failing tests**

Create `crates/irsync/tests/progress_live.rs`:

```rust
use std::time::{Duration, Instant};
use irsync::cli::progress::state::{Live, RowState, ROW_EXPIRY, STALL_AFTER};
use irsync::cli::DeleteMode;
use irsync::fs::paths::RelPath;
use irsync::session::{Event, Party};

fn rp(s: &str) -> RelPath { RelPath::new(s.as_bytes()).unwrap() }
fn live() -> Live { Live::new(Party::Local, DeleteMode::Off, Instant::now()) }

#[test]
fn percent_counts_skipped_matched_and_done_files() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(&Event::Total { party: Party::Local, files: 3, bytes: 1000 }, t);
    l.apply(&Event::Skipped(rp("a"), 500), t);
    l.apply(&Event::FileStart(rp("b"), 400), t);
    l.apply(&Event::FileMatched(rp("b"), 300), t);
    l.apply(&Event::FileBytes(rp("b"), 50), t);
    assert_eq!(l.transfer_fraction(), (850, 1000));
    assert_eq!(l.transfer_percent(), Some(85));
    l.apply(&Event::FileDone(rp("b"), 400), t);
    assert_eq!(l.transfer_fraction(), (900, 1000));
    assert_eq!(l.xfr, 1);
    assert_eq!(l.in_flight(), 0);
}

#[test]
fn percent_is_none_before_a_total_and_capped_after() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(&Event::FileStart(rp("b"), 100), t);
    l.apply(&Event::FileBytes(rp("b"), 100), t);
    assert_eq!(l.transfer_percent(), None);
    l.apply(&Event::Total { party: Party::Local, files: 1, bytes: 50 }, t);
    assert_eq!(l.transfer_percent(), Some(100));
}

#[test]
fn pull_uses_the_peer_total_as_denominator() {
    let mut l = Live::new(Party::Peer, DeleteMode::Off, Instant::now());
    let t = Instant::now();
    l.apply(&Event::Total { party: Party::Local, files: 9, bytes: 9 }, t);
    l.apply(&Event::Total { party: Party::Peer, files: 1, bytes: 200 }, t);
    l.apply(&Event::PeerSkipped(0), t);
    l.apply(&Event::Skipped(rp("x"), 100), t);
    assert_eq!(l.transfer_fraction(), (100, 200));
}

#[test]
fn restarted_file_refreshes_its_row() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(&Event::FileStart(rp("f"), 10), t);
    l.apply(&Event::FileBytes(rp("f"), 4), t);
    l.apply(&Event::FileStart(rp("f"), 10), t + Duration::from_secs(1));
    assert_eq!(l.in_flight(), 1);
    assert_eq!(l.rows[&rp("f")].state, RowState::Moving { literal: 0, matched: 0 });
    assert_eq!(l.rows[&rp("f")].started, t);
}

#[test]
fn hashing_row_clears_on_hashed_and_turns_into_moving_on_start() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(&Event::FileHashing(rp("h"), 5), t);
    assert_eq!(l.rows[&rp("h")].state, RowState::Hashing);
    l.apply(&Event::FileHashed(rp("h")), t);
    assert_eq!(l.in_flight(), 0);
    l.apply(&Event::FileHashing(rp("g"), 5), t);
    l.apply(&Event::FileStart(rp("g"), 5), t);
    assert!(matches!(l.rows[&rp("g")].state, RowState::Moving { .. }));
}

#[test]
fn stalled_and_expired_rows() {
    let (mut l, t) = (live(), Instant::now());
    l.apply(&Event::FileStart(rp("s"), 10), t);
    l.apply(&Event::FileBytes(rp("s"), 1), t);
    assert_eq!(l.rows[&rp("s")].stalled_for(t + Duration::from_secs(1)), None);
    assert_eq!(l.rows[&rp("s")].stalled_for(t + STALL_AFTER), Some(STALL_AFTER));
    l.tick(t + ROW_EXPIRY);
    assert_eq!(l.in_flight(), 0);
}

#[test]
fn walks_never_go_backwards_and_delete_finishes_at_total() {
    let (mut l, t) = (Live::new(Party::Local, DeleteMode::After, Instant::now()), Instant::now());
    l.apply(&Event::Scanned { party: Party::Local, entries: 2000, bytes: 10, at: Some(rp("a")) }, t);
    l.apply(&Event::Scanned { party: Party::Local, entries: 1000, bytes: 5, at: None }, t);
    assert_eq!((l.local.entries, l.local.bytes, l.local.at.clone()), (2000, 10, Some(rp("a"))));
    let d = l.delete.as_ref().unwrap();
    assert!(!d.started && d.mode == DeleteMode::After);
    l.apply(&Event::Delete { party: Party::Peer, done: 3, total: Some(3) }, t);
    l.tick(t);
    assert!(l.delete.as_ref().unwrap().finished);
}

#[test]
fn second_session_resets_counters_but_keeps_connection() {
    use irsync::session::reconnect::ConnState;
    let (mut l, t) = (live(), Instant::now());
    l.apply(&Event::Conn(ConnState::Relay), t);
    l.apply(&Event::FileDone(rp("a"), 7), t);
    l.apply(&Event::SessionStart { index: 1, of: 2 }, t);
    assert_eq!((l.xfr, l.done_bytes, l.session), (0, 0, (1, 2)));
    assert_eq!(l.conn, ConnState::Relay);
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p irsync --test progress_live 2>&1 | head`
Expected: compile error, `state` module not found.

- [ ] **Step 4: Implement `cli/progress/state.rs`** per the Interfaces block. `ConnState` needs `PartialEq` for the last test; it has `Debug, Clone, Copy` at `session/reconnect.rs:95`; add `PartialEq, Eq` there.

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p irsync --test progress_live`
Expected: 8 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/irsync/src/cli/progress crates/irsync/src/session/reconnect.rs crates/irsync/tests/progress_live.rs
git commit -m "feat(cli): live progress state: phases, in-flight rows and rsync-style percent"
```

---

### Task 3: Renderer

**Files:**
- Create: `crates/irsync/src/cli/progress/render.rs`
- Modify: `crates/irsync/src/cli/progress/mod.rs` (add `pub mod render;`)
- Test: `crates/irsync/tests/progress_live.rs`

**Interfaces:**
- Consumes: `Live`, `Row`, `Walk`, `DeletePhase` from Task 2; `human`, `clock`, `escape_path` from `cli/progress/mod.rs`.
- Produces, in `cli/progress/render.rs`:

```rust
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
    pub fn detect() -> Geometry;
    pub fn fixed(width: usize, height: usize) -> Geometry; // color false, unicode true
}

/// True when LC_ALL, else LC_CTYPE, else LANG contains "utf-8"/"utf8" (case-insensitive).
pub fn locale_is_utf8() -> bool;

/// The block: a rule line, the phase lines, the in-flight rows and the
/// "… and N more" line. Every line fits in `g.width` cells. Empty when
/// there is nothing to show yet (no events).
pub fn render(live: &Live, g: &Geometry, now: Instant) -> Vec<String>;

/// A bar of `cells` cells for `num/den`.
pub fn bar(num: u64, den: u64, cells: usize, g: &Geometry) -> String;

/// `s` shortened to `max` cells with `…` in the middle (unicode) or `...` (ascii).
pub fn truncate_middle(s: &str, max: usize, g: &Geometry) -> String;
```

Layout rules (from the spec; exact padding is the builder's):
- Line 1: a rule of `─` (or `-`) across `width`.
- `list` line: `list      {local.entries} entries  walking {at}` while not done, then `  peer {peer.entries} entries  walking {at}` when `peer.entries > 0`; when both walks are done: `list      done  {entries} entries  {human(bytes)}` using the source party's numbers.
- `transfer` line: `transfer  {bar30}  {pct:>3}%  {human(num)}/{human(den)}  {rate}B/s  {clock}  xfr#{xfr}  {n} in flight  [{conn}]`. Without a percent (den 0) the bar is all empty cells and the percent column is blank. While `conn` is `Reconnecting` the rate reads `0B/s`. When `transfer_done`: `transfer  done  {human(literal)} sent  {xfr} files  {rate}B/s  {clock}`.
- `walking {at}` prints `shown(at)` with a trailing `/` (`./` for the root), as `-v` lines print directories.
- In-flight rows: sorted by `size` descending, then path. Each `  {bar20}  {pct:>3}%  {path}  {human(size)}  {rate}B/s`; `Hashing` rows: `  hashing  {path}  {human(size)}` aligned to the same path column; stalled rows replace the rate with `stalled {secs}s`. Row cap = `max(3, height / 2 - fixed)` where `fixed` = the rule, the phase lines present and one line for the overflow; rows beyond the cap become one line `  … and {n} more` (absent when nothing overflows).
- `delete` line (present when `live.delete.is_some()`): `delete    waiting (--delete-{mode})` before `started`; `delete    {bar30}  {pct}%  {done}/{total}` with a total; `delete    {done} deleted` without; `delete    done  {done} deleted` when `finished`.
- Width < 60: rows drop size and rate. Width < 40: rows and the transfer line drop the bar.
- Colour via `console::Style` only when `g.color`.

- [ ] **Step 1: Write the failing tests** (append to `crates/irsync/tests/progress_live.rs`)

```rust
use irsync::cli::progress::render::{bar, render, truncate_middle, Geometry};

fn mid_sync() -> (Live, Instant) {
    let t = Instant::now();
    let mut l = Live::new(Party::Local, DeleteMode::After, t);
    l.apply(&Event::Scanned { party: Party::Local, entries: 18204, bytes: 7_000_000_000, at: Some(rp("2023")) }, t);
    l.apply(&Event::Scanned { party: Party::Peer, entries: 9120, bytes: 0, at: Some(rp("2022")) }, t);
    l.apply(&Event::FileStart(rp("2024/trip/clip_0007.mov"), 1_900_000_000), t);
    l.apply(&Event::FileBytes(rp("2024/trip/clip_0007.mov"), 684_000_000), t);
    l.apply(&Event::FileStart(rp("2024/trip/IMG_0419.heic"), 4_200_000), t);
    l.apply(&Event::FileHashing(rp("2024/trip/clip_0009.mov"), 2_400_000_000), t);
    (l, t)
}

#[test]
fn block_has_rule_phases_and_rows_largest_first() {
    let (l, t) = mid_sync();
    let lines = render(&l, &Geometry::fixed(120, 40), t);
    assert!(lines[0].chars().all(|c| c == '─'), "{lines:?}");
    assert!(lines[1].starts_with("list      18,204 entries  walking 2023/"), "{}", lines[1]);
    assert!(lines[1].contains("peer 9,120 entries  walking 2022/"), "{}", lines[1]);
    assert!(lines[2].starts_with("transfer  ["), "{}", lines[2]);
    assert!(lines[2].contains("3 in flight") && lines[2].contains("[connecting]"), "{}", lines[2]);
    assert!(lines[3].contains("clip_0009.mov") && lines[3].contains("hashing"), "{}", lines[3]);
    assert!(lines[4].contains("clip_0007.mov") && lines[4].contains(" 36%"), "{}", lines[4]);
    assert!(lines[5].contains("IMG_0419.heic"), "{}", lines[5]);
    assert!(lines.last().unwrap().starts_with("delete    waiting (--delete-after)"), "{lines:?}");
    for l in &lines { assert!(l.chars().count() <= 120, "{l:?}"); }
}

#[test]
fn done_phases_collapse() {
    let t = Instant::now();
    let mut l = Live::new(Party::Local, DeleteMode::Off, t);
    l.apply(&Event::Total { party: Party::Local, files: 4010, bytes: 7_210_000_000 }, t);
    l.apply(&Event::Total { party: Party::Peer, files: 0, bytes: 0 }, t);
    l.transfer_done = true;
    l.xfr = 4010;
    let lines = render(&l, &Geometry::fixed(100, 30), t + Duration::from_secs(176));
    assert!(lines[1].starts_with("list      done  4,010 entries"), "{}", lines[1]);
    assert!(lines[2].starts_with("transfer  done  ") && lines[2].contains("4,010 files") && lines[2].contains("0:02:56"), "{}", lines[2]);
    assert_eq!(lines.len(), 3);
}

#[test]
fn rows_are_capped_to_half_the_height() {
    let t = Instant::now();
    let mut l = live();
    for i in 0..40 { l.apply(&Event::FileStart(rp(&format!("f{i:02}")), 100 - i), t); }
    let lines = render(&l, &Geometry::fixed(100, 24), t);
    assert!(lines.len() <= 12, "{}", lines.len());
    assert!(lines.last().unwrap().contains("… and"), "{lines:?}");
    let lines = render(&l, &Geometry::fixed(100, 6), t);
    assert!(lines.iter().filter(|l| l.starts_with("  [")).count() >= 3, "{lines:?}");
}

#[test]
fn long_paths_are_truncated_in_the_middle_never_wrapped() {
    let t = Instant::now();
    let mut l = live();
    let long = "a/".repeat(80) + "end.bin";
    l.apply(&Event::FileStart(rp(&long), 10), t);
    let g = Geometry::fixed(60, 20);
    let lines = render(&l, &g, t);
    for line in &lines { assert!(line.chars().count() <= 60, "{line:?}"); }
    assert!(lines.iter().any(|l| l.contains('…') && l.contains("end.bin")), "{lines:?}");
    assert_eq!(truncate_middle("abcdefghij", 7, &g), "abc…hij");
}

#[test]
fn narrow_terminals_drop_columns() {
    let (l, t) = mid_sync();
    let lines = render(&l, &Geometry::fixed(59, 30), t);
    let row = lines.iter().find(|l| l.contains("clip_0007")).unwrap();
    assert!(row.contains('[') && !row.contains("B/s"), "{row}");
    let lines = render(&l, &Geometry::fixed(39, 30), t);
    let row = lines.iter().find(|l| l.contains("clip_0007")).unwrap();
    assert!(!row.contains('[') && row.contains("36%"), "{row}");
    for line in &lines { assert!(line.chars().count() <= 39, "{line:?}"); }
}

#[test]
fn ascii_glyphs_without_utf8_locale() {
    let mut g = Geometry::fixed(80, 20);
    assert_eq!(bar(1, 2, 4, &g), "[██░░]");
    g.unicode = false;
    assert_eq!(bar(1, 2, 4, &g), "[##--]");
    let (l, t) = mid_sync();
    let lines = render(&l, &g, t);
    assert!(lines.iter().all(|l| l.is_ascii()), "{lines:?}");
}

#[test]
fn stalled_and_reconnecting_are_shown() {
    use irsync::session::reconnect::ConnState;
    let (mut l, t) = mid_sync();
    l.apply(&Event::Conn(ConnState::Reconnecting { attempt: 3 }), t);
    let later = t + Duration::from_secs(8);
    let lines = render(&l, &Geometry::fixed(120, 40), later);
    assert!(lines[2].contains("0B/s") && lines[2].contains("[reconnecting #3]"), "{}", lines[2]);
    assert!(lines.iter().any(|l| l.contains("clip_0007") && l.contains("stalled 8s")), "{lines:?}");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p irsync --test progress_live 2>&1 | head`
Expected: compile error, `render` module not found.

- [ ] **Step 3: Add `console` to the workspace and crate**

`Cargo.toml` `[workspace.dependencies]`: `console = "0.16"`. `crates/irsync/Cargo.toml` `[dependencies]`: `indicatif = { workspace = true }` and `console = { workspace = true }`.

- [ ] **Step 4: Implement `cli/progress/render.rs`** per the Interfaces block and layout rules. Measure widths in `chars().count()` (paths are already escaped to printable ASCII-or-UTF-8 by `escape_path`, so no wide-char table is needed; note this in a comment).

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p irsync --test progress_live`
Expected: 15 passed.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/irsync/Cargo.toml crates/irsync/src/cli/progress crates/irsync/tests/progress_live.rs
git commit -m "feat(cli): render the progress block for a terminal size"
```

---

### Task 4: Screen and reporter integration

**Files:**
- Create: `crates/irsync/src/cli/progress/screen.rs`
- Modify: `crates/irsync/src/cli/progress/mod.rs` (`Reporter`)
- Modify: `crates/irsync/src/main.rs:84-104`
- Modify: `README.md:203` and the paragraph that explains `--progress`, if any (`grep -n progress README.md`)
- Test: `crates/irsync/tests/progress.rs`

**Interfaces:**
- Consumes: `Live` (Task 2), `render`, `Geometry` (Task 3).
- Produces, in `cli/progress/screen.rs`:

```rust
/// Where the reporter draws.
pub enum Screen {
    /// Not a terminal: lines go straight to the writer; no block.
    Plain(Box<dyn Write + Send>),
    /// A terminal: lines print above the block, the block is a set of
    /// indicatif bars with a `{msg}` template, one per rendered line.
    Tty { mp: indicatif::MultiProgress, bars: Vec<indicatif::ProgressBar>, geometry: fn() -> Geometry },
}

impl Screen {
    pub fn plain(out: Box<dyn Write + Send>) -> Screen;
    /// A block drawn through `term` (stdout in production, a buffer in tests).
    pub fn tty(term: Box<dyn indicatif::TermLike>, geometry: fn() -> Geometry) -> Screen;
    pub fn is_tty(&self) -> bool;
    pub fn geometry(&self) -> Geometry; // Plain: fixed(80, 24)
    /// A scrolling line (ends with a newline).
    pub fn println(&mut self, s: &str);
    /// Replaces the block with `lines` (adds/removes bars to match; empty clears it).
    pub fn set_block(&mut self, lines: &[String]);
    /// Removes the block and leaves the cursor on a fresh line.
    pub fn clear(&mut self);
    /// Runs `f` with the block hidden (for stderr writers).
    pub fn suspend<R>(&self, f: impl FnOnce() -> R) -> R;
    /// The MultiProgress, for `LogWriter` (Task 5). None for Plain.
    pub fn multi(&self) -> Option<indicatif::MultiProgress>;
}

/// A `TermLike` over any writer with a size source. Writes ANSI cursor
/// sequences (`\x1b[{n}A` etc.) like `console::Term` does.
pub struct WriterTerm { /* out: Mutex<Box<dyn Write + Send>>, size: fn() -> (u16, u16) */ }
impl WriterTerm {
    pub fn new(out: Box<dyn Write + Send>, size: fn() -> (u16, u16)) -> WriterTerm;
}
impl indicatif::TermLike for WriterTerm { /* width, height, move_cursor_*, write_line, write_str, clear_line, flush */ }
```

- `Reporter` in `cli/progress/mod.rs` keeps `new(out, tty, verbosity, stats, itemize, dry_run)`; `tty: true` builds `Screen::tty(Box::new(WriterTerm::new(out, || (24, 80))), || Geometry::fixed(80, 24))`. Adds:

```rust
impl Reporter {
    /// Production: draw on this screen (built in main from stdout).
    pub fn with_screen(screen: Screen, verbosity: u8, stats: bool, itemize: bool, dry_run: bool) -> Reporter;
    pub fn with_delete_mode(self, mode: DeleteMode) -> Reporter;
    pub fn with_source(self, source: Party) -> Reporter; // default Local
    /// The block as it would be drawn now (for tests).
    pub fn frame(&self, now: Instant) -> Vec<String>;
}
```

`Reporter` replaces its `bytes/xfr/scanned/conn/hint/hint_fixed/last_draw/drawn` fields with a `Live` and a `Screen`; `line()` becomes `screen.println`; `draw()` becomes `live.tick(now); screen.set_block(&render(&live, &geometry, now))` under the 100 ms rule; `finish` calls `screen.clear()` first, then prints the stats and trailer through `screen.println`. `progress_line` is deleted. `Live.transfer_done` is set in `finish`... no: set it when `run` sees the channel close, before the final draw, so the last frame shows `done`; then `finish` clears.

- [ ] **Step 1: Rewrite the TTY tests in `crates/irsync/tests/progress.rs`**

Delete `progress_line_shows_connection_state`, `progress_line_format_with_hint`, `progress_line_shows_percentage_after_total`. Keep the import of `human`. Add:

```rust
#[test]
fn frame_shows_connection_state_and_percent() {
    let buf = Buf::default();
    let mut r = reporter(&buf, true, 0, false, false);
    r.handle(Event::Conn(ConnState::Direct));
    r.handle(Event::Total { party: Party::Local, files: 10, bytes: 1000 });
    r.handle(Event::FileDone(rp("a"), 500));
    let f = r.frame(std::time::Instant::now());
    assert!(f[2].contains(" 50%") && f[2].contains("[direct]") && f[2].contains("xfr#1"), "{f:?}");
    r.handle(Event::Conn(ConnState::Reconnecting { attempt: 1 }));
    let f = r.frame(std::time::Instant::now());
    assert!(f[2].contains("[reconnecting #1]"), "{f:?}");
    assert_eq!(human(456 * 1024 + 800), "456.78K");
    assert_eq!(human(12), "12");
}

#[tokio::test]
async fn tty_reporter_draws_a_block_and_clears_it_before_the_trailer() {
    let buf = Buf::default();
    let mut r = feed(
        reporter(&buf, true, 1, false, false),
        vec![Event::FileStart(rp("a"), 10), Event::FileBytes(rp("a"), 10), Event::FileDone(rp("a"), 10)],
    )
    .await;
    r.finish(&Outcome::default(), Duration::from_secs(1));
    let t = buf.text();
    assert!(t.contains("transfer"), "block drawn: {t:?}");
    assert!(t.contains("\x1b["), "cursor movement used: {t:?}");
    let trailer = t.rfind("sent ").expect("trailer");
    assert!(!t[trailer..].contains("transfer"), "block gone before the trailer: {t:?}");
    assert!(t.contains("a\n"), "-v line printed above: {t:?}");
}

#[tokio::test]
async fn plain_reporter_output_is_unchanged_with_new_events() {
    let buf = Buf::default();
    feed(
        reporter(&buf, false, 1, false, false),
        vec![
            Event::FileHashing(rp("a"), 10), Event::FileHashed(rp("a")),
            Event::FileStart(rp("a"), 10), Event::FileMatched(rp("a"), 5), Event::FileBytes(rp("a"), 5),
            Event::Skipped(rp("s"), 1), Event::PeerSkipped(3),
            Event::Scanned { party: Party::Peer, entries: 1, bytes: 1, at: None },
            Event::Delete { party: Party::Local, done: 0, total: Some(1) },
            Event::SessionStart { index: 0, of: 1 },
            Event::FileDone(rp("a"), 10),
        ],
    )
    .await;
    assert_eq!(buf.text(), "a\n");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p irsync --test progress 2>&1 | head -20`
Expected: compile errors (`frame`, `FileBytes` unknown to the reporter is fine, `Party` import).

- [ ] **Step 3: Implement `screen.rs` and rework `Reporter`** per the Interfaces block. Bars use `ProgressStyle::with_template("{msg}")`, `ProgressBar::no_length()`, and `MultiProgress::with_draw_target(ProgressDrawTarget::term_like_with_hz(term, 10))`. `set_block` calls `set_message` on each bar then `tick()` on the last so indicatif repaints.

- [ ] **Step 4: Wire `main.rs`**

In `run_sync` (`crates/irsync/src/main.rs:84-104`): drop the `progress` computation. `let tty = std::io::stdout().is_terminal() && std::env::var("TERM").map_or(true, |t| t != "dumb");`. When `!opts.quiet`: `Screen::tty(Box::new(WriterTerm::new(Box::new(std::io::stdout()), || console::Term::stdout().size())), Geometry::detect)` if `tty`, else `Screen::plain(Box::new(std::io::stdout()))`; `Reporter::with_screen(screen, opts.verbose, opts.stats, opts.itemize, opts.dry_run).with_delete_mode(opts.delete).with_source(match direction { ClientSends => Party::Local, DaemonSends => Party::Peer })`. The direction is computed inside `sync()` today (`main.rs:296+`); hoist the `direction` computation into a helper `fn direction_of(args: &Args) -> anyhow::Result<Direction>` used by both, or pass the reporter's `Live.source` through a `SessionStart`-like event: choose the helper. `opts.progress` and `opts.info` stay parsed and unused (keep the fields; add `#[allow(dead_code)]` if clippy complains, with a comment "accepted for rsync compatibility; the block is always on").

- [ ] **Step 5: README**

In the flag table at `README.md:203` change the row to: ``| `--stats`, `-i` / `--itemize-changes` | reporting |`` and add a row ``| `--progress`, `--info=progress2` | accepted; the live progress block is always on when stdout is a terminal and off with `-q` |``. Add one paragraph under the flag table: "On a terminal the client draws a live block at the bottom of the screen: the listing walk on both sides, a transfer bar with rsync's percent semantics, one bar per file in flight, and the deletion pass. `-q` turns it off; piped output is unchanged."

- [ ] **Step 6: Run the suite**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo test -p irsync --test progress --test progress_live --test cli`
Expected: all pass. Then try it: `cargo run -p irsync -- --help | head -3` still works.

- [ ] **Step 7: Commit**

```bash
git add -A crates/irsync README.md
git commit -m "feat(cli): live progress block on a TTY, on by default and off with -q"
```

---

### Task 5: stderr above the block

**Files:**
- Modify: `crates/irsync/src/cli/progress/screen.rs` (add `LogWriter`)
- Modify: `crates/irsync/src/main.rs:55-71` (`init_logging`) and `run_sync`
- Test: `crates/irsync/tests/progress.rs`

**Interfaces:**
- Produces, in `screen.rs`:

```rust
/// A tracing `MakeWriter` whose lines print above the block (or straight
/// to stderr when there is no block).
#[derive(Clone)]
pub struct LogWriter(Arc<Mutex<Option<indicatif::MultiProgress>>>);
impl LogWriter {
    pub fn new() -> LogWriter;
    /// Called once the screen exists; before that, lines go to stderr.
    pub fn attach(&self, mp: Option<indicatif::MultiProgress>);
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogWriter { type Writer = LogLine; ... }
/// Buffers one line; on drop writes it via `mp.suspend(|| stderr.write_all(..))`.
pub struct LogLine { /* buf: Vec<u8>, mp: Option<MultiProgress> */ }
```

- `init_logging(verbose, quiet, writer: LogWriter)` in `main.rs` uses `.with_writer(writer)`; `run_sync` creates the `LogWriter` first, initialises logging, builds the screen, then `writer.attach(screen.multi())`.

- [ ] **Step 1: Write the failing test** (append to `crates/irsync/tests/progress.rs`)

```rust
#[test]
fn log_lines_are_written_through_suspend_not_into_the_block() {
    use irsync::cli::progress::screen::{LogWriter, Screen, WriterTerm};
    use tracing_subscriber::fmt::MakeWriter;
    let buf = Buf::default();
    let mut screen = Screen::tty(
        Box::new(WriterTerm::new(Box::new(buf.clone()), || (24, 80))),
        || irsync::cli::progress::render::Geometry::fixed(80, 24),
    );
    screen.set_block(&["─".repeat(80), "transfer  x".into()]);
    let w = LogWriter::new();
    w.attach(screen.multi());
    {
        let mut line = w.make_writer();
        line.write_all(b"irsync: warn: something\n").unwrap();
    }
    // The block is still the last thing drawn, and the writer did not panic
    // with the block up. (The log line itself went to the real stderr.)
    screen.set_block(&["─".repeat(80), "transfer  y".into()]);
    assert!(buf.text().contains("transfer  y"));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p irsync --test progress log_lines 2>&1 | head`
Expected: compile error, `LogWriter` not found.

- [ ] **Step 3: Implement `LogWriter`/`LogLine`** and wire `init_logging` and `run_sync` as above. The `eprintln!` in `session/reconnect.rs:271` becomes `tracing::warn!` (it already runs while the block is up).

- [ ] **Step 4: Run to verify pass**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo test -p irsync --test progress`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add -A crates/irsync
git commit -m "feat(cli): print tracing output above the progress block"
```

---

### Task 6: Per-file events from the engine

**Files:**
- Modify: `crates/irsync/src/send.rs:76-77,191-193,251-290` (`ProgressFn`, `set_progress`, the range scheduler)
- Modify: `crates/irsync/src/session/sender_role.rs:258-262,363-440,644-650` (`on_plan`, `on_need`, the progress closure)
- Modify: `crates/irsync/src/session/receiver.rs:599-612` (`on_opened`)
- Modify: `crates/irsync/src/session/recv_io.rs:892-894`
- Modify: `crates/irsync/src/hasher.rs:95-184` (progress callback)
- Modify: `crates/irsync/src/session/engine.rs:520-540,735-745` (hasher construction, both client and daemon)
- Test: `crates/irsync/tests/progress_e2e.rs`

**Interfaces:**
- Produces, in `send.rs`:

```rust
/// What the sender reports about a file as it goes.
pub enum SendProgress {
    /// The first data stream for this call opened; the file's size.
    Opened { size: u64 },
    /// Bytes put on the wire.
    Sent(u64),
}
pub type ProgressFn = Arc<dyn Fn(&RelPath, SendProgress) + Send + Sync>;
```

- Produces, in `hasher.rs`:

```rust
pub type HashProgress = Arc<dyn Fn(&RelPath, HashEvent) + Send + Sync>;
pub enum HashEvent { Start { size: u64 }, Done }
impl Hasher { pub fn with_progress(self, p: HashProgress) -> Self; }
```

Emission sites:
- `send.rs` `send_chunks_against`: after the first permit is acquired (inside the `'sched` loop, first iteration only), call `progress(path, Opened { size: digest.size })`. `write_range` calls `progress(path, Sent(n))` instead of `p(n)`; pass `path` (as `&RelPath`) into `send_range`/`write_range` alongside `name`.
- `sender_role.rs:258-262`: delete the `FileStart` emission from `on_plan`. The closure at `:646` becomes `move |path, p| { activity.fetch_add(1, Relaxed); match p { Opened { size } => bus.emit(FileStart(path.clone(), size)), Sent(n) => { bus.emit(FileBytes(path.clone(), n)); bus.emit(Bytes(n)) } } }`.
- `sender_role.rs` `on_need`: `SFile` gains `matched_reported: bool`; on the first `on_need` with a digest, emit `FileMatched(path, digest.size - Σ digest.chunks[i].len for i in idxs)` and set the flag.
- `receiver.rs:608`: after `FileStart(f.path.clone(), f.entry.size)` emit `FileMatched(f.path.clone(), digest.size.saturating_sub(Σ digest.chunks[i].len for i in &remote))` (chunks in `local` count as matched; `Fill.remote` holds receiver-side indices, `digest.chunks[i]` for `i in remote`).
- `recv_io.rs:894`: also emit `FileBytes(route.path.clone(), wire)` (`route.path` is the `Route`'s path, see `Route` at `receiver.rs:626`).
- `hasher.rs` `digest`: inside the blocking closure, right before `open_regular_nofollow`, call `progress(&item.path, Start { size: item.entry.size })`; after `digest_reader` returns (ok or err), `progress(&item.path, Done)`. Not called on a cache hit.
- `engine.rs`: both `hasher` and `forced` for the **client** (`:520-540`) get `.with_progress(Arc::new(move |p, e| match e { Start { size } => bus.emit(FileHashing(p.clone(), size)), Done => bus.emit(FileHashed(p.clone())) }))` with `bus = local.hooks.bus.clone()`. The daemon's hashers (`:735-745`) are left alone.

- [ ] **Step 1: Write the failing e2e test** (append to `crates/irsync/tests/progress_e2e.rs`)

```rust
fn big(p: &RelPath) -> bool { p.as_bytes() == b"big.bin" }

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn per_file_events_in_both_directions() {
    for flow in [Flow::Push, Flow::Pull] {
        within30(async {
            let h = Harness::new().await;
            fs::write(h.src(flow).join("big.bin"), vec![3u8; 3_000_000]).unwrap();
            let (out, events) = run(&h, &opts(&[]), flow).await;
            assert!(events.iter().any(|e| matches!(e, Event::FileStart(p, 3_000_000) if big(p))), "{flow:?}: no FileStart with size: {events:?}");
            let bytes: u64 = events.iter().filter_map(|e| match e { Event::FileBytes(p, n) if big(p) => Some(*n), _ => None }).sum();
            assert_eq!(bytes, out.bytes_transferred, "{flow:?}");
            assert!(events.iter().any(|e| matches!(e, Event::FileMatched(p, 0) if big(p))), "{flow:?}: a new file matches nothing: {events:?}");
            assert!(events.iter().any(|e| matches!(e, Event::FileDone(p, 3_000_000) if big(p))), "{flow:?}");
        })
        .await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hashing_events_when_a_file_is_read() {
    within30(async {
        // Push with -c: the sender reads the source file for its digest.
        let h = Harness::new().await;
        fs::write(h.src(Flow::Push).join("big.bin"), vec![3u8; 3_000_000]).unwrap();
        let (_, events) = run(&h, &opts(&["-c"]), Flow::Push).await;
        let start = events.iter().position(|e| matches!(e, Event::FileHashing(p, 3_000_000) if big(p)));
        let end = events.iter().position(|e| matches!(e, Event::FileHashed(p) if big(p)));
        let open = events.iter().position(|e| matches!(e, Event::FileStart(p, _) if big(p)));
        assert!(start < end && end <= open, "hashing, then hashed, then the stream: {events:?}");
        // Pull over a differing old file: the receiver reads it for the delta basis.
        let h = Harness::new().await;
        fs::write(h.src(Flow::Pull).join("big.bin"), vec![3u8; 3_000_000]).unwrap();
        fs::write(h.dst(Flow::Pull).join("big.bin"), vec![4u8; 2_000_000]).unwrap();
        let (_, events) = run(&h, &opts(&[]), Flow::Pull).await;
        assert!(events.iter().any(|e| matches!(e, Event::FileHashing(p, 2_000_000) if big(p))), "{events:?}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sender_file_start_means_a_stream_opened_not_a_plan() {
    within30(async {
        let h = Harness::new().await;
        for i in 0..50 { fs::write(h.src(Flow::Push).join(format!("f{i}")), b"x").unwrap(); }
        let (_, events) = run(&h, &opts(&["--parallel", "2"]), Flow::Push).await;
        // Every FileStart is followed by FileBytes for the same path before
        // more than `parallel` other files start: in-flight is bounded.
        let mut open = std::collections::BTreeSet::new();
        let mut max_open = 0;
        for e in &events {
            match e {
                Event::FileStart(p, _) => { open.insert(p.clone()); max_open = max_open.max(open.len()); }
                Event::FileDone(p, _) => { open.remove(p); }
                _ => {}
            }
        }
        // `--parallel 2` streams; acknowledgements lag a little behind the
        // streams closing, so allow some slack. Before this change every
        // planned file started at once and this peaked at 50.
        assert!(max_open <= 10, "in flight peaked at {max_open}: {events:?}");
    })
    .await;
}
```

Add `use irsync::fs::paths::RelPath;` to the test file.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p irsync --test progress_e2e per_file sender_file_start 2>&1 | tail -20`
Expected: assertion failures (no `FileHashing`, `FileStart` size 0, `FileBytes` sum 0).

- [ ] **Step 3: Implement** the emission sites listed above.

- [ ] **Step 4: Run the e2e and the rest**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo test -p irsync 2>&1 | tail -15`
Expected: all pass, including `tests/send.rs` and `tests/sub_chunk*.rs`, which call `set_progress` or inspect sent bytes (adapt their closures to the new `ProgressFn` shape).

- [ ] **Step 5: Commit**

```bash
git add -A crates/irsync
git commit -m "feat(session): per-file start, bytes, matched and hashing events; sender start means a stream opened"
```

---

### Task 7: Skipped files and listing detail

**Files:**
- Modify: `crates/irsync/src/session/planner.rs:485-512,630-640` (skip counting), add emission
- Modify: `crates/irsync/src/session/receiver.rs:570-576` (identical files), `:940-957` (local listing `bus`)
- Modify: `crates/irsync/src/session/control.rs:120-160` (reader counts peer entries in both roles), `:260-310` (`Scanned.at` and the 250 ms rule)
- Test: `crates/irsync/tests/progress_e2e.rs`

**Interfaces:**
- Consumes: `Event::{Skipped, Scanned, Total}` from Task 1.
- `ListingSpec` (control.rs:181) unchanged in shape; the receiver passes `bus: Some(ctx.hooks.bus.clone())`.

Emission rules:
- `planner.rs`: wherever `self.report.skipped += 1` runs for a regular source file (lines 492, 509, 637), also `self.ctx.hooks.bus.emit(Skipped(path.clone(), src.entry.size))` (at 637 the size is the resumed plan's entry size).
- `receiver.rs:572` (`Opened::Identical`): emit `Skipped(f.path.clone(), f.entry.size)`.
- `control.rs` `spawn_reader`: count `seen` for every `Msg::Listing` in both roles (move `seen.count(&e)` above the `match &listing`); every 1000 entries emit `Scanned { party: Peer, entries: seen.files(), bytes: seen.total_size, at: None }`; at `ListingEnd` emit `Total { party: Peer, .. }` in both roles (today only when `listing.is_some()`).
- `control.rs` `spawn_listing`: track `last_emit: Instant`; emit `Scanned { party: Local, entries: count, bytes: stats.total_size, at: Some(parent_dir(&path)) }` when `count % 1000 == 0` or `last_emit.elapsed() >= 250 ms`. `parent_dir` is the path with its last component removed (`RelPath::root()` for a top-level entry); add `pub fn parent(&self) -> RelPath` to `crates/irsync/src/fs/paths.rs`.
- `receiver.rs:950`: `bus: Some(ctx.hooks.bus.clone())`.

- [ ] **Step 1: Write the failing tests** (append to `progress_e2e.rs`)

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn second_run_reports_skipped_bytes_locally_on_pull() {
    within30(async {
        let h = Harness::new().await;
        tree(h.src(Flow::Pull));
        run(&h, &opts(&[]), Flow::Pull).await;
        let (_, events) = run(&h, &opts(&[]), Flow::Pull).await;
        let skipped: u64 = events.iter().filter_map(|e| match e { Event::Skipped(_, n) => Some(*n), _ => None }).sum();
        assert_eq!(skipped, 3 + 5000, "{events:?}");
        assert!(events.iter().any(|e| matches!(e, Event::Total { party: Party::Local, .. })), "local listing now reports: {events:?}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_listing_is_counted_on_push_and_local_walk_says_where_it_is() {
    within30(async {
        let h = Harness::new().await;
        fs::create_dir_all(h.src(Flow::Push).join("deep/dir")).unwrap();
        for i in 0..1100 { fs::write(h.src(Flow::Push).join(format!("deep/dir/f{i}")), b"x").unwrap(); }
        fs::write(h.dst(Flow::Push).join("extra"), b"x").unwrap();
        let (_, events) = run(&h, &opts(&[]), Flow::Push).await;
        assert!(events.iter().any(|e| matches!(e, Event::Total { party: Party::Peer, files: 1, .. })), "{events:?}");
        assert!(events.iter().any(|e| matches!(e, Event::Scanned { party: Party::Local, at: Some(p), .. } if p.as_bytes() == b"deep/dir")), "{events:?}");
    })
    .await;
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p irsync --test progress_e2e second_run peer_listing 2>&1 | tail`
Expected: both fail on their first assertion.

- [ ] **Step 3: Implement** per the emission rules. `RelPath::parent` gets a unit test in `fs/paths.rs`'s existing test module: `parent("a/b/c") == "a/b"`, `parent("a") == root`.

- [ ] **Step 4: Run the suite**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo test -p irsync 2>&1 | tail -15`
Expected: pass. `pull_reports_total_from_peer_listing` still passes (it filters on `Party::Peer`).

- [ ] **Step 5: Commit**

```bash
git add -A crates/irsync
git commit -m "feat(session): skipped-file events, peer listing counts in both roles, and where the local walk is"
```

---

### Task 8: Heartbeat piggy-back

**Files:**
- Modify: `proto/irsync/v1/control.proto:98-102`
- Modify: `crates/irsync/src/session/mod.rs` (add `Busy`)
- Modify: `crates/irsync/src/session/engine.rs:239-278` (`Ctx.busy`), `:528-560,746-770` (construction), `:828,893-905` (`heartbeat`)
- Modify: `crates/irsync/src/session/control.rs` (`ListingSpec.busy`/`activity`, reader handles `Heartbeat` fields)
- Modify: `crates/irsync/src/session/planner.rs` (skipped bytes into `Busy`), `crates/irsync/src/session/receiver.rs:572` (identical into `Busy`)
- Test: `crates/irsync/tests/progress_e2e.rs`

**Interfaces:**
- Proto:

```proto
// "I am busy": sent by either side while it makes local progress ... Carries
// what the peer cannot otherwise see about this side's progress.
message Heartbeat {
  bytes walking = 1;        // directory this side's listing walk is in; empty when not listing
  uint64 skipped_bytes = 2; // bytes of source files this side's planner found up to date (cumulative)
  uint64 deleted = 3;       // paths this side's deletion pass removed so far
  uint64 delete_total = 4;  // its candidate count once known, else 0
}
```

- `session/mod.rs`:

```rust
/// This side's progress the peer cannot see, carried on `Heartbeat`.
#[derive(Default)]
pub struct Busy {
    pub walking: std::sync::Mutex<Option<RelPath>>,
    pub skipped_bytes: AtomicU64,
    pub deleted: AtomicU64,
    pub delete_total: AtomicU64,
}
impl Busy { pub fn heartbeat(&self) -> irsync_proto::v1::Heartbeat; }
```

- `Ctx` gains `pub busy: Arc<Busy>` (default in both constructors). `heartbeat(activity, busy: Arc<Busy>, out)` sends `busy.heartbeat()`.
- `ListingSpec` gains `pub busy: Option<Arc<Busy>>` and `pub activity: Option<Arc<AtomicU64>>`; the listing loop sets `busy.walking` to the entry's parent whenever it changes, clears it at the end, and bumps `activity` per entry (so the heartbeat fires while only this side is walking).
- Planner skip sites and the receiver identical site also `ctx.busy.skipped_bytes.fetch_add(size)`.
- Two heartbeats are sent outside the 500 ms task so the peer always gets the news: the listing loop sends `Msg::Heartbeat(busy.heartbeat())` once before its first entry (walking = the root) and then whenever it emits a `Scanned` (the 1000-entry / 250 ms rule from Task 7); the receiver sends one right before `Msg::Done(SessionDone)` so the final skipped and deleted counts arrive even when the session is shorter than one interval.
- Reader (`control.rs` `Msg::Heartbeat(h)`): if `!h.walking.is_empty()` and `RelPath::new(&h.walking)` is ok, emit `Scanned { party: Peer, entries: seen.files(), bytes: seen.total_size, at: Some(path) }`; if `h.skipped_bytes > 0` emit `PeerSkipped(h.skipped_bytes)`; if `h.deleted > 0 || h.delete_total > 0` emit `Delete { party: Peer, done: h.deleted, total: (h.delete_total > 0).then_some(h.delete_total) }`.

- [ ] **Step 1: Write the failing tests** (append to `progress_e2e.rs`)

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_learns_peer_skipped_bytes_from_the_heartbeat() {
    within30(async {
        let h = Harness::new().await;
        tree(h.src(Flow::Push));
        run(&h, &opts(&[]), Flow::Push).await;
        let (_, events) = run(&h, &opts(&[]), Flow::Push).await;
        let max = events.iter().filter_map(|e| match e { Event::PeerSkipped(n) => Some(*n), _ => None }).max();
        assert_eq!(max, Some(3 + 5000), "{events:?}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_sees_the_peer_walk_position() {
    within30(async {
        let h = Harness::new().await;
        fs::create_dir_all(h.dst(Flow::Push).join("old/stuff")).unwrap();
        for i in 0..3000 { fs::write(h.dst(Flow::Push).join(format!("old/stuff/f{i}")), b"x").unwrap(); }
        let (_, events) = run(&h, &opts(&[]), Flow::Push).await;
        // The walk's first heartbeat names the root; a later one may name
        // `old/stuff` if the walk outlived a tick. Either proves the path.
        assert!(events.iter().any(|e| matches!(e, Event::Scanned { party: Party::Peer, at: Some(_), .. })), "{events:?}");
    })
    .await;
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p irsync --test progress_e2e heartbeat peer_walk 2>&1 | tail`
Expected: both fail (no such events).

- [ ] **Step 3: Implement** the proto fields (the build script regenerates), `Busy`, `Ctx.busy`, `heartbeat`, `ListingSpec` fields, the planner and receiver bumps, and the reader conversion. The `Msg::Heartbeat(_) => {}` arm at `control.rs:139` and the `"Heartbeat"` name at `:546` are the two places that mention it today.

- [ ] **Step 4: Run the suite**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo test -p irsync 2>&1 | tail -15`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add -A proto crates/irsync
git commit -m "feat(proto): heartbeat carries the walk position, skipped bytes and deletion progress"
```

---

### Task 9: Deletion pass progress

**Files:**
- Modify: `crates/irsync/src/delete.rs:59-120,174-220` (`DeletePass`)
- Modify: `crates/irsync/src/session/recv_delete.rs:13-20`
- Test: `crates/irsync/tests/progress_e2e.rs`, `crates/irsync/tests/delete.rs`

**Interfaces:**
- Produces, in `delete.rs`:

```rust
/// Told (done, total) per candidate handled; `total` is the candidate count.
pub type DeleteProgress = Arc<dyn Fn(u64, u64) + Send + Sync>;
impl DeletePass { pub fn set_progress(&mut self, p: DeleteProgress); }
```

`run` calls `progress(handled, candidates_len)` once before the loop (with `handled = 0`) and once after each candidate (deleted, would-delete, kept or failed all count as handled).

- `recv_delete.rs` `run_deletes`: before `execute`, `pass.set_progress(Arc::new(move |done, total| { busy.deleted.store(done); busy.delete_total.store(total); bus.emit(Delete { party: Local, done, total: Some(total) }) }))` with `busy = ctx.busy.clone()`, `bus = ctx.hooks.bus.clone()`.

- [ ] **Step 1: Write the failing tests**

Append to `progress_e2e.rs`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_progress_events_both_directions() {
    for flow in [Flow::Pull, Flow::Push] {
        within30(async {
            let h = Harness::new().await;
            fs::write(h.src(flow).join("keep"), b"k").unwrap();
            for i in 0..5 { fs::write(h.dst(flow).join(format!("extra{i}")), b"x").unwrap(); }
            let (_, events) = run(&h, &opts(&["--delete"]), flow).await;
            let party = if flow == Flow::Pull { Party::Local } else { Party::Peer };
            let last = events.iter().rev().find_map(|e| match e {
                Event::Delete { party: p, done, total } if *p == party => Some((*done, *total)),
                _ => None,
            });
            assert_eq!(last, Some((5, Some(5))), "{flow:?}: {events:?}");
        })
        .await;
    }
}
```

Append to `crates/irsync/tests/delete.rs` a unit test on the pass:

```rust
#[tokio::test]
async fn delete_pass_reports_progress_per_candidate() {
    use irsync::cli::DeleteMode;
    use irsync::delete::DeletePass;
    use irsync_proto::v1::file_entry::Kind;
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("a"), b"a").unwrap();
    fs::write(root.path().join("b"), b"b").unwrap();
    let mut pass = DeletePass::new(DeleteMode::After, None, false, false, false);
    pass.plan(RelPath::new(b"a").unwrap(), Kind::Regular);
    pass.plan(RelPath::new(b"b").unwrap(), Kind::Regular);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    pass.set_progress(Arc::new(move |d, t| s.lock().unwrap().push((d, t))));
    let rep = pass.execute(root.path().to_path_buf(), false).await.unwrap();
    assert_eq!(rep.deleted.len(), 2);
    assert_eq!(*seen.lock().unwrap(), vec![(0, 2), (1, 2), (2, 2)]);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p irsync --test progress_e2e delete_progress --test delete reports_progress 2>&1 | tail`
Expected: compile error on `set_progress`.

- [ ] **Step 3: Implement** per the Interfaces block. On the push side nothing else is needed: Task 8's heartbeat carries `busy.deleted`/`delete_total`, the daemon's `run_deletes` is the same code, and the receiver's heartbeat before `SessionDone` (Task 8) delivers the final count.

- [ ] **Step 4: Run the suite**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo test -p irsync 2>&1 | tail -15`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add -A crates/irsync
git commit -m "feat(delete): report deletion progress per candidate, locally and over the heartbeat"
```

---

### Task 10: Several sources in one push

**Files:**
- Modify: `crates/irsync/src/main.rs:376-445` (the per-source loop in `sync`)
- Modify: `crates/irsync/src/cli/progress/mod.rs` (`Reporter::handle` for `SessionStart`)
- Test: `crates/irsync/tests/progress.rs`

**Interfaces:**
- Consumes: `Event::SessionStart`, `Live::reset` (Task 2), `render` (Task 3).
- `sync()` emits `bus.emit(Event::SessionStart { index, of: pairs.len() })` at the top of each loop iteration, before `session(...)`.
- `Reporter::handle(SessionStart { index, .. })` with `index > 0` on a TTY: render the current frame with `transfer_done = true`, `screen.println` each phase line of it (lines 1.. without the rule and without in-flight rows), then `live.apply` (which resets). Off a TTY: nothing printed.

- [ ] **Step 1: Write the failing test** (append to `progress.rs`)

```rust
#[tokio::test]
async fn a_second_session_leaves_the_first_ones_done_lines_in_the_scrollback() {
    let buf = Buf::default();
    let mut r = feed(
        reporter(&buf, true, 0, false, false),
        vec![
            Event::SessionStart { index: 0, of: 2 },
            Event::Total { party: Party::Local, files: 1, bytes: 5 },
            Event::FileStart(rp("a"), 5), Event::FileBytes(rp("a"), 5), Event::FileDone(rp("a"), 5),
            Event::SessionStart { index: 1, of: 2 },
            Event::FileStart(rp("b"), 7),
        ],
    )
    .await;
    let f = r.frame(std::time::Instant::now());
    assert_eq!(r.live().xfr, 0, "second session starts fresh");
    assert!(f.iter().any(|l| l.contains("b")), "{f:?}");
    let t = buf.text();
    let first_done = t.find("transfer  done").expect("first session's done line printed");
    assert!(t[first_done..].contains("1 files"), "{t:?}");
    r.finish(&Outcome::default(), Duration::from_secs(1));
}
```

Add `pub fn live(&self) -> &Live` to `Reporter`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p irsync --test progress second_session 2>&1 | tail`
Expected: fails (`live` missing or no done line).

- [ ] **Step 3: Implement** the emission in `main.rs` and the handling in the reporter.

- [ ] **Step 4: Run the suite**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo test -p irsync --test progress --test progress_live --test cli`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add -A crates/irsync
git commit -m "feat(cli): one progress block per session when pushing several sources"
```

---

### Task 11: Try it for real and finish the docs

**Files:**
- Modify: `docs/IRSYNC_VS_RSYNC.md` (a short note on what `--progress` does here versus rsync; find the reporting section with `grep -n "progress\|--stats" docs/IRSYNC_VS_RSYNC.md`)
- Modify: `crates/irsync/src/cli/progress/mod.rs` module doc comment (describe the block, the state/render/screen split, and that non-TTY output is unchanged)

- [ ] **Step 1: Run a real push on a terminal**

With a daemon on localhost (see README quick start, `--address 127.0.0.1:PORT`), push a directory of a few hundred files including one of several hundred MB, in an 80x24 terminal and in a 40-column one; with `-v`; with `-q`; piped through `cat`. Check: the block stays at the bottom, `-v` lines scroll above, the 40-column view has no wrapped lines, `-q` prints nothing, `| cat` output has no escape sequences (`grep -c $'\x1b'` is 0).

- [ ] **Step 2: Fix anything found**, with a test where one is missing, then run `cargo test -p irsync`.

- [ ] **Step 3: Docs**, as listed in Files.

- [ ] **Step 4: Commit**

```bash
git add -A docs crates/irsync/src/cli/progress/mod.rs
git commit -m "docs: the live progress block; what --progress means in irsync"
```
