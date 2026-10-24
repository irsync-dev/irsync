# Live progress block on a TTY

Date: 2026-10-24. Status: draft, awaiting approval.

## Intent

When `irsync` runs a sync with stdout on a terminal, the user should see
what is going on: which stage the sync is in, how far along it is, which
files are moving right now and how fast each one is, and whether the link
is healthy. Today the client shows one rsync-style `--info=progress2`
line, and only when `--progress` is passed. With 32 files in flight and a
scan running alongside the transfer, that line hides what is slow and
why the percentage moves the way it does.

The user's words: "I would like to see what operations are going on so I
get an understanding of progress."

## Goals

- On a TTY, always draw a live block at the bottom of the screen showing
  the phases of the sync and the files in flight. Only `-q` / `--quiet`
  turns it off.
- Make the far side's activity visible where it can be learned without
  new protocol messages.
- Keep every existing line of output (`-v`, `-i`, `--stats`, the rsync
  trailer, errors on stderr) intact and scrolling above the block.
- Keep non-TTY output exactly as it is today.

## Non-goals

- A progress display for the daemon process or for the management
  commands (`add`, `allow`, `index`, `gc`, ...). This is the sync client
  only.
- Byte-exact compatibility with rsync's `--progress` or `--info=progress2`
  line format on a TTY. The flags remain accepted (see below).
- A full-screen TUI. The block is a few lines at the bottom; the rest of
  the terminal behaves as usual.
- New protocol message types. Fields may be added to existing messages.

## Anti-goals

- The display must never slow the engine down or change the outcome of a
  sync. The reporter stays a lossy subscriber of a broadcast channel.
- No flicker and no runaway scrolling: redraw is rate limited and the
  block is never taller than half the terminal.
- No garbage when output is a pipe, a file, or a dumb terminal.

## Constraints

- Rust, in `crates/irsync`. The existing reporter lives in
  `crates/irsync/src/cli/progress.rs` and is driven by
  `session::Event` over a `tokio::sync::broadcast` channel. Keep that
  shape.
- The protocol is protobuf in `proto/irsync/v1/*.proto`. No backward
  compatibility is required (no users yet), but the rule from the user
  is: no new message types. Extra fields on existing messages are fine.
- Both sides list their tree; the client only sees the far side's listing
  as `ListingEntry` frames arriving. The far side sends a `Heartbeat`
  every 500 ms while it is busy and nothing else moves (see
  `HEARTBEAT_INTERVAL` in `session/engine.rs`).

## What the user sees

### When

| stdout is | `-q` | result |
| --- | --- | --- |
| a terminal | no | live block, plus all normal lines scrolling above it |
| a terminal | yes | nothing but errors on stderr (as today) |
| not a terminal | no | today's output; no block, no progress line |
| not a terminal | yes | nothing but errors on stderr (as today) |

`--progress`, `--info=progress1`, `--info=progress2` are still accepted and
no longer change anything: on a TTY the block is already on, off a TTY
nothing is drawn. The README flag table says so.

`TERM=dumb` counts as not a terminal.

### Layout

The block is pinned at the bottom. Above a thin rule, normal output
scrolls (per-file `-v` lines, `-i` codes, `deleting ...`). Below it:

```
──────────────────────────────────────────────────────────────────────────
list      18,204 entries  walking 2023/        peer 9,120 entries  walking 2022/
transfer  [██████████████░░░░░░░░░░░░░░░░]  47%  3.42GB/7.21GB  41.2MB/s  0:01:23  xfr#1,882  32 in flight  [direct]
  [███████░░░░░░░░░░░░░]  36%  2024/trip/clip_0007.mov            1.9GB  8.1MB/s
  [████████████████░░░░]  81%  2024/trip/clip_0003.mov            640MB  12.0MB/s
  [██░░░░░░░░░░░░░░░░░░]  11%  2023/xmas/panorama.tif             212MB  9.7MB/s
  hashing                      2024/trip/clip_0009.mov            2.4GB
  … and 28 more
```

Phase lines, in order, each present only when it applies to this run:

- `list`: local entries counted so far and the directory the walker is
  in, then the same for the far side (`peer N entries  walking D`). The
  client already receives every far-side `ListingEntry`, so the count is
  local bookkeeping; the directory comes from the heartbeat (below). No
  bar: the total is unknown until listing ends. When both sides are
  done: `done  24,118 entries  7.21GB`.
- `transfer`: bar, percent, bytes done / bytes total, rate, elapsed,
  `xfr#N` files transferred, files in flight, connection state in
  brackets. The percent has rsync's `--info=progress2` meaning: the
  denominator is the size of every regular file in the source listing
  (growing until listing ends), and the numerator counts a file's whole
  size once it is done, whether it was transferred, matched by delta or
  skipped as unchanged, plus the position of each file in flight. So an
  incremental sync over a large unchanged tree still runs to 100%, and
  the percent can drop while the list grows; the `list` line explains
  why. The rate is literal bytes over the wire per second, as today.
  Connection state is `connecting`, `direct`, `relay`, or `reconnecting
  #n` in a warning colour; while reconnecting the rate shows `0B/s`.
  When done: `done  3.42GB sent  4,010 files  41.9MB/s  0:02:56`.
- `delete`: bar and `done/total` once the pass knows its list, a running
  count before that. Present only with a `--delete*` flag
  (`--delete-before`, `--delete-during`, `--delete-after`,
  `--delete-excluded`). Before it starts: `waiting (--delete-after)`.
  On a push the far side deletes; its count and total ride on the
  heartbeat (below), so the line behaves the same in both directions.
- `--dry-run` shows the same phases; `transfer` counts files that would
  move and `delete` counts would-deletes.

Finished phases collapse to `done` and a one-line summary; they do not
keep a bar.

In-flight rows, indented under `transfer`:

- "In flight" means the local side is actively working on the file: it
  has an open data stream, or it is reading the file for a whole-file
  hash or a delta basis. Files the planner has queued but not started
  are not rows and are not counted in `N in flight`; with the default
  settings that count tops out at `--parallel` streams plus the hasher's
  workers.
- Rows are sorted by file size, largest first.
- A file that is being hashed or delta-matched before any byte moves
  shows the word `hashing` in place of the bar.
- A moving file shows a 20-cell bar, percent, path, size and its own
  rate. The percent is the position in the file: literal bytes received
  plus the bytes of basis chunks already matched, over the file's size.
  A delta transfer of a 2 GB file with 100 MB changed therefore walks
  from 0% to 100%, not to 5%. The rate is literal bytes per second.
- A file that has received no bytes for 2 s or more shows `stalled Ns`
  in place of the rate.
- The list is capped: never more than half the terminal height minus the
  phase lines, and at least 3 rows when there is room. The remainder is
  one line: `… and N smaller files`.
- Paths are shown with the same escaping as `-v` lines and truncated in
  the middle with `…` to fit the terminal width.

When a session ends the block is erased and the normal tail prints
(`--stats` block, `sent ... received ...`, `total size is ...`), so the
scrollback looks the way it does today.

A push with several sources runs one session per source on the same
reporter. The block is per session: when one session finishes, its
`done` lines for each phase are printed into the scrollback and the
block starts fresh for the next source. Totals in the trailer stay
combined, as today.

A reconnect does not reset anything. Listing restarts on the new
connection, but counters never go backwards: the `list` line shows the
larger of the old and new counts until the new walk passes it, and a
`FileStart` for a file that already has a row refreshes the row rather
than adding one.

Lines that bypass the reporter must not land inside the block. Tracing
output (`-v` info lines, walk warnings) and `eprintln!` errors go to
stderr; when stdout is a TTY the reporter's draw target is shared with
the logging setup in `main.rs` so those lines are printed above the
block too (indicatif's `suspend`/`println`, via a tracing `MakeWriter`
that wraps the `MultiProgress`). Off a TTY nothing changes.

### Behaviour on resize and narrow terminals

The block re-reads the terminal size on every redraw. Below 60 columns
the in-flight rows drop the size and rate; below 40 columns the bars are
dropped and only percent and path remain.

### Redraw

At most every 100 ms, plus an immediate redraw on a connection state
change and on the final frame.

## Protocol: piggy-backed far-side progress

Existing frames carry a little more. `Heartbeat` gains:

- `bytes walking = 1;` the directory the peer's listing walk is in,
  empty when it is not listing. (The peer's entry count is not sent: the
  client already receives every `ListingEntry` and counts them.)
- `uint64 deleted = 2;` and `uint64 delete_total = 3;` the peer's
  deletion pass so far and its known total (0 before the list is known).
  Zero in both when the peer is not deleting.

The heartbeat already goes out only while the peer is busy, so this
costs nothing when idle. The heartbeat task today only sees the
activity counter; it needs read access to the listing walker's current
directory and the delete pass's counters (a couple of shared atomics or
a small `Mutex<State>` on the session context).

Nothing else changes on the wire. Per-file far-side progress is already
observable: a file's `ChunkListPart`, `NeedChunks` and `FileAck` arrive
as the far side works through it. The far side does not hash during its
listing walk (it sends cached hashes), so there is no far-side
`hashing` hint.

The receiving side turns the heartbeat fields into `Event`s (below).

## Engine: new events

`session::Event` changes as follows. Existing variants keep their
meaning unless named here.

- `FileStart(RelPath, u64)` gains the file's size and now means "a data
  stream for this file is open", on both roles. On the sender it moves
  from plan arrival (`sender_role.rs`) to stream open (`send.rs`); on
  the receiver it already fires at writer open. The size is what the
  side knows at that point (the listing entry on the receiver, the stat
  on the sender).
- `FileHashing(RelPath, u64)`: the local side started reading this file
  for a whole-file hash or a delta basis; path and size. Cleared by
  `FileStart` or `FileDone` for the same path.
- `FileBytes(RelPath, u64)`: literal bytes moved for this file. Emitted
  where `Bytes` is emitted today: the receiver's data loop in
  `recv_io.rs`, and the sender's progress closure in `send.rs` (which
  must therefore learn the file it is sending). The aggregate `Bytes`
  stays.
- `FileMatched(RelPath, u64)`: bytes of this file covered by matched
  basis chunks, so the per-file bar can advance through a delta. Emitted
  by whichever side resolves the chunk list (`NeedChunks` on the
  sender; the receiver when it learns which chunks it keeps).
- `FileDone(RelPath, u64)` is unchanged and also fires for files the
  planner skips as unchanged (it already does, from `planner.rs`), which
  is what lets the total percent reach 100%.
- `Scanned { side: Side, entries: u64, bytes: u64, at: RelPath }`
  replaces `Scanned(u64)`. `Side` is `Local` or `Peer`. Local scans
  emit it from the walk; peer scans emit it from the control reader
  that counts `ListingEntry` frames, with `at` filled from the latest
  heartbeat. Emitted every 1000 entries and at least every 250 ms while
  walking, so the `walking` hint moves on a tree of few large
  directories.
- `Total { side: Side, files: u64, bytes: u64 }` gains the side. The
  transfer denominator is always the source side's total: local on a
  push, peer on a pull. The local listing on a pull currently runs with
  no bus (`receiver.rs`); it gets one so the `list` line can show it,
  and the reporter uses the side tag to keep the two totals apart.
- `Delete { side: Side, done: u64, total: Option<u64> }`: deletion pass
  progress. Locally it needs a per-path hook inside the delete pass
  (today `Deleted` events are emitted in a batch after the pass); on a
  push it comes from the heartbeat fields. `Deleted(RelPath)` stays for
  the `-v` lines.

Events stay best effort. If the reporter lags and loses a `FileDone`, a
row would stick: the reporter drops any in-flight row that has seen no
event for 30 s and shows the count from the `Outcome` at the end as it
does today.

Events stay best effort. If the reporter lags and loses a `FileDone`, a
row would stick: the reporter drops any in-flight row that has seen no
event for 30 s and shows the count from the `Outcome` at the end as it
does today.

## Rendering: decided for the builder

- Use `indicatif` 0.18, already a workspace dependency (used by
  `fileset` and `filegen`): `MultiProgress` for the pinned block,
  `MultiProgress::println` for the scrolling lines, so cursor handling,
  width and the TTY check are not hand-rolled. The existing ANSI
  `\r\x1b[K` drawing in `progress.rs` goes away.
- `Reporter` keeps its public shape (`new`, `run`, `handle`, `finish`)
  and gains a way to render one frame to a string for a given terminal
  size, for tests. The non-TTY path must produce byte-identical output
  to today; the existing tests pin it. The tests in `tests/progress.rs`
  that construct a TTY reporter over a byte buffer and assert `\r` and
  `progress_line` text are rewritten against the frame renderer; the
  exact-match on `Event::Scanned(1000)` in `tests/progress_e2e.rs`
  follows the new variant shape.
- Colours: bars in blue, `done` in green, `reconnecting` and `stalled`
  in yellow. No colour when `NO_COLOR` is set or the terminal does not
  support it (indicatif/console handle this).
- Bar glyphs: `█` and `░`; fall back to `#` and `-` when the locale is
  not UTF-8.

## Left to the builder

- Exact column widths and padding, as long as the layout above holds and
  nothing wraps.
- How `Scanned` learns the current directory from `jwalk` cheaply.
- How the rows are sorted when sizes tie, and how often the in-flight
  list is re-sorted (at most once per redraw is fine).
- Whether `human()`, `clock()` and the path escaping move to a shared
  module or stay where they are.
- Test strategy for the TTY path. A reasonable floor: unit tests on the
  reporter's rendered frame given a sequence of events and a fixed
  terminal size, and one end-to-end test that the block is absent when
  stdout is a pipe.

## Open calls I made (check these)

- Phase set is `list`, `transfer`, `delete`. Hashing is not a phase; it
  shows per file as `hashing` in the in-flight rows.
- The total percent has rsync's `progress2` meaning: size of every
  source file as the denominator, skipped and matched files counted as
  done. The per-file percent is position in the file, with matched
  delta chunks counted as done.
- "In flight" is open stream or active hash, not planned-and-queued.
- Far-side deletion progress rides on the heartbeat, so `delete` works
  the same for push and pull.
- Several sources in one push: the block is per session, finished
  sessions leave their `done` lines in the scrollback.
- stderr lines (tracing, errors) are routed above the block on a TTY.
- `--progress` and `--info=progress*` become no-ops rather than errors.
- Stalled threshold 2 s, stuck-row expiry 30 s, redraw 100 ms, cap at
  half the terminal.
