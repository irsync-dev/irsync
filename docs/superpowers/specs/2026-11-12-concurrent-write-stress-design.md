# Writing to the source while irsync syncs it: a stress mode

Date: 2026-11-12

## Why

irsync now detects source files that change during a sync (design:
`docs/superpowers/specs/2026-11-10-source-change-detection-design.md`). That
design asked for a long-run check, and none exists yet. The stress loop in
`crates/stress/src/run.rs` changes the source and syncs it one after the
other, never at the same time. So the `paranoid` row of `matrix.sh` runs
`--paranoid` on a tree nothing writes to during the sync, and no long-run test
exercises the change checks. `TODO.md` has the open item ("A stress mode that
runs filegen against the source while irsync syncs it").

Correctness comes first for irsync. The goal, in the user's words: **no
destination file is ever a state the source never held.**

## Goals

1. A stress mode, in both `irsync-stress` and `irsync-netstress`, in which
   filegen writes to the source tree while irsync syncs it.
2. After each sync, check every destination entry, **content and metadata**,
   against the states the source really held.
3. Runs are repeatable from their seed, including every random choice the new
   mode makes.
4. New `matrix.sh` rows, run by hand, about an hour each.

## Non-goals

- Replacing or changing any existing test, stress mode, sweep or matrix row.
  The new campaigns are **in addition** to today's coverage (the user's
  requirement). Today's modes keep their behaviour exactly, including how they
  treat exit 23 and 24.
- Running in CI. The campaigns are run by hand.
- Writers that change files through memory-mapped pages. filegen writes with
  ordinary `write` calls; that limit is already documented.
- A consistent snapshot of the whole tree. Each path is judged on its own.

## Anti-goals

- **A false alarm.** A check that rejects a state the source really held wastes
  an hour-long run and teaches people to ignore failures.
- **A check that cannot fail.** If irsync gives up on most files, a round can
  pass while checking almost nothing. And a checker that is too generous would
  pass torn files. Both must be guarded against (see "Proving it bites").
- Slowing filegen so much that the races stop happening.

## What "a state the source held" means

The user chose the **faithful** rule. For each path, the states the source
held during a round are:

- its state at the start of the round;
- its state after each complete filegen step;
- **the in-between states of a step while it was running.** filegen writes in
  place, in pieces of up to 1 MiB, without fsync. A reader can see a
  truncated file, a prefix of new content, or a mix of old and new pieces.
  Those are states the source held, so they pass.

*Claude's call, how to judge in-between states.* The rule is per operation:
a destination passes if it is a state that operation really passes through,
given how filegen writes it. Examples:

- `rewrite` empties the file (`File::create`) and then writes the new
  content: any prefix of the new content, including empty, passes.
- `append` and `create`: the old content followed by any prefix of the new
  bytes.
- `overwrite_range`: the old file with any front part of the range already
  written.
- `truncate` and `punch_hole`: the old or the new state, plus whatever
  middle states the system calls used can show.

If in doubt, the builder accepts what the operation really does: the
faithful principle wins over any simpler rule, because a false alarm is an
anti-goal. Bytes from two different steps never mix: no single moment of the
source looked like that, and it is exactly the torn copy the
change-detection work fixed.

**Also allowed (Claude's call):** if irsync gave up on a path (exit 23) or
reported it vanished (exit 24), the destination may still hold exactly what
it held at the start of the round. That state was checked in the round
before.

**Anything else fails the round**, including a destination path that holds a
file when the source path never held one during the round.

### Metadata

Checked too (the user's decision), for whatever the run asks irsync to keep:
file type, size, permissions, modification time, symlink target, and owner
when run as root.

- Modification times come from the clock, not the seed, so replaying cannot
  rebuild them. *Claude's call:* filegen records, after each step, the stat of
  every path the step touched (size, mode, mtime, type, symlink target).
- For a step in flight, permissions must be the old or the new value, and the
  modification time must lie between the values before and after the step.
- **Content and metadata must belong together.** If a destination's size and
  modification time equal some recorded source state, its content must be that
  state's content. A file whose times say "current" while its bytes are old is
  the dangerous case: the next normal sync skips it forever.

*Claude's call:* directories are judged on existence and permissions only;
their modification times change with every entry and are left to the quiet
sync below. Hard-link groups are also left to the quiet sync.

### Where the history comes from

The checker must be able to rebuild the exact bytes of each file before and
after every step of the round. filegen's content is fully determined by its
seed, and its log already records each step's path, offset and length.
*Claude's guess, left to the builder:* keep a cheap copy of the source tree
at the start of each round (a reflink copy where the filesystem allows it, a
plain copy where not) and replay the round's log onto it, step by step, with
filegen's own code. That needs no recording of file bytes while filegen runs,
which keeps it fast. If replay turns out awkward, recording per-step content
hashes plus whatever the in-between check needs is acceptable.

## How a run works

### Settings for the whole run (the user's decisions)

- **Direction:** push or pull, as today. On a pull, filegen writes to the
  server's tree while the daemon sends it.
- **`--paranoid` on or off.**

Each is fixed for the run, so a full campaign is four separate runs per tool.
The mode is switched on by a new flag on `irsync-stress` and
`irsync-netstress` (name left to the builder).

### One round

1. **Pick the pace** from the run's seed, and log the choice (the user's
   decision: the pace changes from round to round, and every choice is
   logged so the same seed replays the same run). The paces:
   - **fast:** filegen never pauses;
   - **steady:** a fixed rate, also drawn from the seed;
   - **bursts:** fast for a while, quiet for a while, also drawn from the seed.

   A flag fixes the pace for the whole run when someone wants to hammer one
   case.

   **What a seed reproduces (Claude's call).** How many steps filegen gets
   through before irsync exits depends on timing, not the seed. So the seed
   reproduces the random choices (pace, operations, content), and each round
   logs the number of steps filegen really ran. The rerun command printed on
   failure replays the run with those step counts, so every round starts
   from the same source tree. The race itself cannot be replayed, so a rerun
   may not fail again; the kept trees and logs are the evidence. Passing
   roots may still be deleted.
2. Take the start-of-round copy of the source (see "Where the history comes
   from").
3. **Start filegen in the background**, then start the irsync sync.
4. When irsync exits, **stop filegen cleanly between two steps** (SIGTERM;
   filegen already finishes the current step and saves its state).
5. **Check the destination** against the history, as above.
6. **Quiet sync (Claude's call).** Run irsync once more with nothing writing,
   then require an exact match, as today's loop does. This catches a file
   whose content and times disagree, because the normal quick check would
   skip it. It also shows the change checks leave nothing permanently wrong.
7. Bring the fileset manifest up to date, as today.

The next round starts from wherever the source ended up. filegen continues
from its saved state, as today.

### Exit codes of the concurrent sync

- 0 and 24 are fine. The content check still runs on every path.
- **23 is fine only when every file irsync gave up on is one filegen touched
  during that sync** (the user's decision). Under netstress's bad network
  irsync can also give up for reasons that have nothing to do with source
  changes, and those must stay failures, as they are today.
- To make that possible, **irsync gains a small machine-readable report**
  (the user's decision): the files it retried and the files it gave up on,
  each with the reason. It is written by whichever side sends (the daemon on
  a pull), and only adds output; nothing else in irsync changes. Its form
  and how the harness gets it (a file the client or daemon is asked to
  write, for example) are left to the builder. The same report gives the
  counts in "Proving it bites", so the harness never parses log text.
- Any other exit code, a crash, or a hang (the client's `--timeout`) fails
  the round.
- The quiet sync must exit 0.

Today's modes keep treating 23 as a failed iteration and 24 as a harness
error.

### Proving it bites

- **Counts.** Each round logs how many files filegen changed during the sync,
  how many irsync placed, retried and gave up on, and how many were judged.
  The run summary totals them.
- **A run that tested nothing fails.** *Claude's call:* if no file that
  changed during a sync was ever placed across the whole run, the run ends
  with its own exit code, like netstress's "faults did not bite" (exit 3).
- **Self-test (Claude's call).** The checker must catch the original bug.
  - Add a `test-hooks` switch that turns off all of irsync's change checks:
    the stat after hashing, the ask before placing, and `--paranoid`'s
    clone, second hash and final re-check.
  - A manual campaign step, written up in `bug-hunting.md`: with the switch
    on, a short run of the new mode must fail. It depends on timing, so it
    is not a `cargo test`.
  - Deterministic `cargo test`s for the checker: it rejects a hand-made torn
    file (bytes from two steps), and accepts every in-between state a real
    step can produce, frozen at many points mid-step.

### On failure

Stop the run (or carry on with `--keep-going`, as today) and keep everything:
both trees, the start-of-round copy, filegen's log and state, irsync's logs,
the seed and the round's pace. Print the path that failed, which rule it
broke, and the command that reruns the run. This follows today's harness.

## Campaigns (the user's decisions)

- New rows in `crates/stress/tools/matrix.sh`, run by hand, **about an hour
  each.**
- Rows: concurrent writes without `--paranoid`, and with it. `breadth.sh`
  already runs push and pull as separate runs, so each row covers both
  directions as separate runs.
- netstress rows too, in the same piece of work, without and with
  `--paranoid`. They need the fault build (`target/netsim`) and run through
  `crates/netstress/tools/sweep.sh`. *Claude's call:* `matrix.sh` gets these
  rows and calls `sweep.sh` for them, refusing clearly if the fault build is
  missing.
- *Claude's call:* "about an hour" is a wall-clock budget. `breadth.sh` and
  `sweep.sh` gain an optional time limit after which they start no new runs;
  the rows set it to one hour. Run sizes per seed stay small enough that the
  last run finishes soon after.
- The existing `paranoid` row stays as it is.
- *Claude's call:* the campaigns run on the development machine's ZFS, which
  has reflinks, so the `--paranoid` rows cover the clone path. A run logs
  whether clones worked; on a filesystem without them it says so plainly
  and still runs (the second-hash path then gets tested instead).

## Docs to update

- `book/src/reference/command-line.md`: the option that asks irsync for the
  retry report, if it is user-visible.
- `book/src/contributing/bug-hunting.md`: the new rows in the campaign
  matrix, and a short section on the concurrent mode and what a failure
  looks like.
- The `matrix.sh`, `breadth.sh` and `sweep.sh` header comments.
- `crates/netstress/README.md` if it lists the modes.
- `book/src/explanation/correctness.md`: mention the new long-run check
  where it lists how irsync's correctness is tested.
- `TODO.md`: close the stress-mode item and point at this design.

## Left to the builder

- How the history is kept (replay onto a start-of-round copy, or recording).
- How filegen runs in the background and is paced (a new filegen option, or
  the harness pausing it).
- The flag names, the exact pace ranges, and the log format of the pace
  choice.
- How the stress and netstress loops share the new code. netstress has its own
  copy of the loop today (`crates/netstress/src/run.rs`); sharing more is fine
  if existing behaviour stays the same.
- The exact length of each run inside the hour budget.
