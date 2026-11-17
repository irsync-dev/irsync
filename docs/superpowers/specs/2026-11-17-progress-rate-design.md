# Current transfer rate in the progress block (#25)

## Why

Feedback on `-P`: people want an overall MB/s they can read at a glance.
The `transfer` line has one rate today, but it is the average since the
run started (listing and hashing time count), it covers wire bytes only,
and it has no label. So it starts low, moves slowly, and is easy to
misread.

## Goals

- The `transfer` line shows how fast things are going now, over all files:
  - **sent**: wire (literal) bytes per second.
  - **synced**: literal plus matched bytes per second, the speed you feel
    when most of a file is already on the other side.
  - **goodput**: synced bytes over wire bytes: how much data each byte on
    the wire puts in place. Up to 2 it is a percentage (`100%` means
    nothing was matched, `150%`); above 2 it is a ratio (`9.2×`).
- Every number has a label.
- The `done` line keeps averages over the whole run, with the same three
  numbers.

## Non-goals

- The per-file row rates stay as they are.
- `--stats`, the `-vv` summary and the `sent … received …` trailer stay
  as they are (they are rsync's format or already labelled).
- No protocol or engine change: everything comes from events the block
  already gets.

## Approaches

- **A. Wire bytes only, labelled.** Honest network speed, but with good
  delta matches the bar races ahead while the rate stays low.
- **B. Sent plus synced rates.** Shows both the network and the effective
  speed; costs width, so synced drops early on narrow terminals.
- **C. Combined rate only.** Rejected: does not match the network.

Picked: **B**, plus goodput (a percentage up to 2, a ratio above).

## Design

**State (`state.rs`).**
- `Live` gets `matched: u64`, the run's matched bytes. `FileMatched`
  carries the file's running total, so the counter adds the increase over
  the row's previous `matched`.
- `Live` keeps a short list of samples `(Instant, literal, synced)`, one
  per `tick`, and drops those older than the window. The current rates
  are the change from the oldest kept sample to now. The window is 5
  seconds; until 1 second of samples exists, no rate is shown (the field
  is left out rather than showing a misleading number).
- `Live::reset` (a new session) starts the counters and samples afresh,
  as it does now.
- While reconnecting both rates show 0, as today.

**Goodput** is cumulative over the session (`(literal + matched) /
literal`), not windowed: a windowed ratio jumps between "one big match"
and "all new data" and tells you little. It is shown only once literal
bytes are above 0. Format:
- ratio ≤ 2: whole percent, `100%` to `200%`;
- ratio > 2: one decimal and `×` (`x` on ASCII terminals), e.g. `2.1×` to
  `999.9×`; above that `>999×`, so a nearly unchanged tree does not print
  a huge number.

**Transfer line (`render.rs`), while running:**

```text
transfer  [██████████░░░░░░░░░░░░░░░░░░░░]   33%  4.00G/12.00G  41.20MB/s sent  380.00MB/s synced  goodput 9.2×  0:01:23  4 in flight  xfr#1,882  [direct]
```

Fields are kept in this order of priority as the terminal narrows:
connection label, sent rate, clock, byte counts, in flight, synced rate,
goodput, xfr count. (Today: conn, rate, clock, bytes, in flight, xfr.)

**`done` line:**

```text
transfer  done  3.41G sent  1,882 files  avg 17.80MB/s sent  80.30MB/s synced  goodput 4.5×  0:03:20
```

Averages over the time since the run started, as today. It is cut to
the width like every other line.

**Docs.** Update the example block in `progress/mod.rs` and the Output
section of `book/src/reference/command-line.md` (what the three numbers
mean, that the rates are over the last few seconds and the `done` line
shows averages). That book chapter feeds the man pages, so run the man
page tests.

**Tests.** State: the matched counter (increments, `FileMatched` before
`FileStart`, reset on a new session), the window (rate after a burst,
falls back once bytes stop, nothing shown before 1 s). Render: frames at
wide and narrow widths showing which fields drop first, the `done` line,
goodput at 100%, at exactly 2 (`200%`), just above 2 (`2.1×`), the
`>999×` cap and the ASCII `x`.

## Left to the builder

- Exact sample bookkeeping (a `VecDeque` is the obvious fit).
- Whether `rate_text` grows a label argument or the labels are added at
  the call site.
