# `--delete-during`: delete per directory, safely

Date: 2026-10-29. Status: implemented 2026-10-29 (no separate plan; the
change is in one commit series on `main`).

Reverses decision R21 of `../../IRSYNC_DECISIONS.md`, which ran
`--delete-during` as `--delete-after`.

## Intent

rsync's `--delete-during` removes a directory's extraneous entries while
the transfer is in progress, so space is freed as the run goes instead of
at the end (`--delete-after`) or only after both sides have been listed in
full and buffered (`--delete-before`). irsync accepted the flag but ran it
as `--delete-after`, because whether the sender's listing was complete
was only known at its `ListingEnd`, and irsync never deletes on an
incomplete listing: a path missing from the source may only have been
unreadable, not gone.

The fix keeps that guarantee and makes the mode real: each side reports
the gaps in its listing *inline*, at the point in path order where the
missing entries would have been, so the receiver always knows whether the
part of the listing it is about to act on is complete.

## Goals

- `--delete-during` deletes a directory's extraneous entries once the
  merged listing has left that directory, before the rest of the tree is
  planned, in both directions (push and pull).
- Nothing is ever deleted that might merely have been unreadable: a gap
  reported by either listing stops further deletions for the rest of the
  run (unless `--ignore-errors`); deletions made before the gap stand, as
  with rsync.
- `--delete-before` and `--delete-after` keep their behaviour; the
  deletion guards (`--max-delete`, `--dry-run`, protected excluded paths,
  symlink safety, deepest-first order) apply to every pass.

## Non-goals

- Undoing deletions made before a gap was reported (rsync does not either).
- Deleting a directory's entries *before* that directory's own files are
  requested (rsync's exact timing); irsync acts when the merge leaves the
  directory, which is the first moment its contents are known in full.

## Design

### Inline listing gaps

`scan::walk` streams `ScanEvent`s instead of bare items: `Entry(ScanItem)`
as before, and `Gap { path, message }` whenever a directory could not be
read or an entry could not be stat'ed, emitted at that path's position in
the sorted stream (a directory's gap right after the directory's own
entry). The summary at the end still says `incomplete` and lists the
errors, so every other consumer of the walk is unchanged.

The listing sends each gap to the peer as a new control message,
`ListingError(PathError)`, in order with the `ListingEntry` frames, and
hands it to its own planner the same way. The sender ignores the
receiver's gaps (it only needs the receiver's `ListingEnd`); the receiver
routes the sender's gaps into the planner's input stream.

### The planner

The planner's two input streams carry entries and gaps. A gap sets a
shared "a listing has a gap" flag and is otherwise skipped; because the
merge pulls each stream in order and looks at most one entry ahead, the
flag is set before the merge yields any path that comes after the gap.
A candidate for deletion is always at or after the gap that could explain
its absence (an unstatable entry is its own gap; an unreadable directory's
gap precedes everything below it), so the flag is reliable at the moment
a candidate could be acted on.

With `--delete-during`, whenever the merge crosses a directory boundary
(the parent of the current path changes), the planner takes the
candidates that are ready, those that are neither the current path nor an
ancestor of it, and hands them to the receiver's main loop as one pass,
waiting for the reply before going on. The pass is refused, and its
candidates dropped, when the gap flag is set and `--ignore-errors` is
off; the refusal is reported once per session. Candidates that are not
ready yet (ancestors of the current path, so root-level entries whose
subtree is still being merged) stay with the planner and go into the
final pass, which runs after the transfers exactly as `--delete-after`.

Protected paths (excluded destination entries without
`--delete-excluded`) keep their ancestors out of every pass: the set of
kept directories is shared by the passes of one run.

### The receiver

A new `Done::DeleteDuring` carries the pass and the planner's gap flag;
the main loop runs it at once (it does not wait for this side's walk to
end, which the planner's own progress depends on) and replies with the
deleted paths so the planner marks their parents for the directory pass.
`--max-delete` counts across passes through the persisted deletion
count, so a limit reached in one pass stops the next. Progress events
report the session's cumulative deletions, with a total only for the
final pass.

### Documentation

README: the limitation line is removed and the deletions paragraph names
the three timings. CLI help for `--delete-during` describes it. Decision
R21 is rewritten. `docs/protocol.md` mentions `ListingError`.

## Tests

- `scan`: a walk reports a gap for an unreadable directory right after the
  directory's entry, and for an unstatable entry at its position.
- `delete`: `DeletePass::split_ready` takes exactly the candidates that
  are not ancestors of the current path.
- End to end, both directions: with `--delete-during`, a directory's
  extraneous entries are deleted before the next directory's files are
  planned; with an unreadable source directory, entries in directories
  before it are deleted, nothing after it is, and the run reports the
  refusal; with `--ignore-errors` everything extraneous goes.
