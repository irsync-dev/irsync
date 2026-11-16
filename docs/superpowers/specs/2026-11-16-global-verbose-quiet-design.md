# `-v` and `-q` work on every command

Date: 2026-11-16

Issue: #31

## Intent

A user typed `irsync -v id show` and got "neither source nor destination is
remote". `irsync -v gc` asks for `<SRC>... <DEST>`, and `irsync id show -v`
says "unexpected argument '-v'". The flags only work in some places, and the
errors do not say why.

`-v` and `-q` should be accepted before or after any command, and mean the
same kind of thing everywhere.

## Goals

- `-v`, `--verbose`, `-q` and `--quiet` are accepted before the command,
  after it, and after a nested command: `irsync -v gc`, `irsync gc -v`,
  `irsync id -v show`, `irsync id show -vv`.
- One rule for what they mean, the one a sync already follows (decided):
  - `-v` turns on the command's own extra output. A command with nothing
    extra to say shows nothing more. That is fine.
  - `-vv` and up raise the log level: `-vv` info, `-vvv` debug, `-vvvv`
    trace, `-vvvvv` trace plus every protocol message.
  - `-q` logs errors only and drops summaries.
- `RUST_LOG` still overrides the level, as today.

## Non-goals

- New `-v` output for `gc`, `index`, `export update` or the management
  commands. They get the flag; what it could show is for later issues.
- Making other sync flags work before a command (`irsync -n gc`).
- Any change to what a sync prints.

## Anti-goals

- A flag before a command must never be dropped without a word.
  `irsync -a gc` must fail, not run `gc`.
- A sync whose path happens to be named like a command must keep working
  where it works today: `irsync -a myserver:photos/ config` stays a pull
  into `config`.

## Approaches

1. **Drop `args_conflicts_with_subcommands` and make `-v`/`-q` global
   flags on the top-level command.** Chosen. The parser does the work, the
   flags show up in every `--help`, and there is one definition. Checked
   with a throwaway test against clap 4.6.7: all the forms in the goals
   parse, and a path named like a command is still a path once another
   path or a sync has started (`irsync src gc`,
   `irsync -a host:photos/ config`). The cost: the parser no longer
   rejects sync flags given with a command, so we check that ourselves.
2. **Keep the setting and move leading `-v`/`-q` behind the command before
   parsing.** Rejected: hand-written argument handling that the parser can
   do itself, and errors would describe a command line the user did not
   type.
3. **Two parsers**, one for a sync and one for the commands, chosen by
   looking at the arguments first. Rejected: it touches help, version
   output and many tests, which is too much for this issue.

Marking the flags global while keeping the setting does not work: clap
stops looking for a command once it has seen any flag, global or not.

## Design

### Parsing

In `crates/irsync/src/cli/mod.rs`:

- Remove `args_conflicts_with_subcommands` from `Args`.
- Define `verbose` (a count) and `quiet` once, with `global = true`, on
  the top level. Remove them from `SyncArgs`, and remove `LogArgs` from
  `daemon`, `gc`, `index` and `export update`. Every command reads the
  global pair.
- `-v` together with `-q` is accepted everywhere and `-q` wins, as in a
  sync today. (My call. Today the four logging commands reject the pair.)
- After parsing, when a command was given, reject any sync flag or path
  that was set on the command line. The error names the flag and the
  command, for example: `-a is a sync option and cannot be used with
  'gc'`. It exits 2 like other usage errors.

### What each command does with the flags

| Command | `-v` | `-vv` and up | `-q` |
| --- | --- | --- | --- |
| sync, listing | as today | as today | as today |
| `daemon` | nothing extra (as today) | log level (as today) | errors only (as today) |
| `gc`, `index`, `export update` | nothing extra (**changes**: today info logs) | log level, `-vv` is info | errors only, no summary (as today) |
| `id`, `host`, `export`, `grant`, `config`, `show`, `ssh-setup` | nothing extra | logging is turned on at that level | accepted, nothing to quiet |

So every command passes the count minus one to `log_default`, as the sync
and the daemon already do.

The management commands do not set up logging today. They start to, so
that `-vv` has an effect. With no flag the level is warnings, so they may
now print a warning they used to drop. The build-info line logged at start
is info level and stays hidden without `-vv`.

### Behaviour that changes

- `irsync gc -v`, `irsync index -v` and `irsync export update -v` no longer
  show info logs. `-vv` does.
- `irsync -v gc host:path` used to sync a local directory called `gc`.
  It now runs `gc` and fails on the extra argument, as `irsync gc
  host:path` already does. Write `./gc` for the directory.
- `-v -q` together is no longer an error on the four logging commands.

### Help and docs

- The `daemon` help text says "info level (-v: debug)". That is wrong
  today; fix it to match the rule.
- The usage line of `irsync --help` changes once the setting is gone.
  Check that it still reads well.
- Update `book/src/reference/command-line.md` (the `-v`/`-q` rows of
  `daemon`, `index`, `gc`, and a note that both flags work on every
  command, before or after it) and `book/src/reference/management.md`.

### Tests

Parser tests next to the existing ones in `cli/mod.rs`:

- each form in the goals parses to the right command, count and quiet;
- a sync flag before or after a command is rejected, with the flag named;
- `irsync -a host:photos/ config` and `irsync src gc` are still syncs;
- `irsync -v host:` is still a listing.

One end-to-end check that `irsync -v id show` prints the identity.

## Left to the builder

- The wording of the global flags' help text. It is now shared by every
  command, so it has to describe the rule, not only the sync.
- How to find "a sync flag was set on the command line" (for example the
  value source in clap's matches).
- Whether `LogArgs` survives as the type that holds the global pair.
