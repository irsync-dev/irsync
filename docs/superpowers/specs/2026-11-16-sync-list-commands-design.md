# `sync` and `list` commands (issue #62)

## Why

The main operation of irsync has no name: it is whatever is not one of the
nine named commands. Giving it one (`irsync sync`) gives scripts and docs a
spelling that a future command can never take over, and gives it its own
`--help`. `irsync list` does the same for listing, which today is "one remote
operand". The rsync-style forms stay, unchanged and not deprecated: anyone
replacing `rsync` with `irsync` keeps working.

## What the user sees

```
irsync sync -av ~/Photos/ myserver:/srv/photos/   # explicit
irsync -av ~/Photos/ myserver:/srv/photos/        # rsync-style, as today

irsync list myserver                              # exports on myserver
irsync list myserver:                             # same
irsync sync myserver:                             # same
irsync myserver:                                  # same, as today

irsync list myserver:photos/                      # files a pull would fetch
irsync myserver:photos/                           # same, as today
```

- `sync` takes exactly the options and operands of the rsync-style form.
  Output, exit codes and resume behaviour are identical.
- `list` takes one operand: `HOST`, `HOST:` or `HOST:PATH`. The colon is
  optional because the operand is always remote. It takes the same options
  as the one-operand form today (`-v`, `--ssh`, identity options, `-r` and
  so on).
- `irsync --help` stays as it is (every transfer option at the top level)
  and gains `sync` and `list` in its command list. `irsync sync --help` and
  `irsync list --help` work.
- A command name is recognised only as the first argument. A local path
  named like a command (`sync`, `list`, `grant`, ...) is written `./sync`.
  This is already true of the nine existing names; the docs state the rule
  once, for all of them.

## Goals, non-goals

- Goal: both spellings of each operation run the same code, by
  construction rather than by keeping two paths in step.
- Non-goal: shortening `irsync --help` or moving transfer options out of it.
- Non-goal: guessing whether `sync` is a path (issue option 2). Rejected:
  less predictable, and the other command names don't do it either.
- Anti-goal: any change in behaviour of an existing rsync-style command.

## Approaches

1. **Real subcommands, then one form at parse time (chosen).** `sync` and
   `list` are ordinary clap subcommands, so they appear in help. Right after
   parsing, `Args::try_parse_from` rewrites the short forms into the long
   ones: an rsync-style sync becomes `Command::Sync`, a one-remote-operand
   call (and `sync HOST:...` with one operand) becomes `Command::List`, and
   a bare `HOST` given to `list` becomes `HOST:`. `main.rs` only ever sees
   the long forms; there is no "no command" branch left.
2. Real subcommands, dispatched separately in `main.rs` next to the
   existing no-command branch. Rejected: two paths into `run_sync` /
   `run_list` that can drift.
3. Strip the word `sync` from argv before clap sees it. Rejected: the
   commands would never show in `--help`.

Your direction on 1: rewrite short into long (not long into short), so the
named commands are the real ones and the rsync-style forms are shorthand.

## How (decided)

- `Command` gains `Sync(SyncArgs)` and `List(..)`. After normalising,
  `run_sync`, `run_list`, `locations`, `direction_of`,
  `SyncOptions::from_args` and the other readers of `args.sync` take the
  `SyncArgs` from the command instead of from the top level.
- The include/exclude order (`ordered_filters`) is read from the
  `ArgMatches` of whichever level held the options: the top level for the
  short form, the `sync`/`list` subcommand for the long one.
- Usage errors stay as today: `irsync sync localdir` and `irsync list` with
  no operand or two operands are errors.

## Left to the builder

- How `list` shows its operand in help as `HOST[:PATH]` rather than
  `SRC... DEST` (its own args struct flattening the shared options, or
  another clap means). My call: whatever keeps one definition of the
  shared options.
- Whether normalised `Args` keeps `cmd: Option<Command>` or `try_parse_from`
  returns a type with a non-optional command.

## Tests

- Parse tests: each short form normalises to the same `Command` value as
  its long form (push, pull, several sources, export listing, file
  listing, bare `HOST` for `list`), filter order included.
- `irsync sync --help` and `irsync list --help` succeed; `irsync --help`
  lists both.
- `irsync ./sync host:` treats `./sync` as a path; `irsync sync host:` is
  the command.
- One end-to-end push via `sync` and one export listing via `list`, using
  the existing integration test harness. Existing tests stay unchanged.

## Docs

- `book/src/reference/command-line.md`: both forms, `list`, and the
  command-name rule.
- README and irsync.org (`site/`): the explicit form next to the
  rsync-style one, which is described as the rsync-compatible shorthand,
  not as legacy.
