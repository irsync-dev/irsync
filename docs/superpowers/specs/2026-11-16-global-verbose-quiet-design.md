# `-v`, `-q`, `--config` and `--passphrase-file` work on every command

Date: 2026-11-16

Issue: #31

## Intent

A user typed `irsync -v id show` and got "neither source nor destination is
remote". `irsync -v gc` asks for `<SRC>... <DEST>`, and `irsync id show -v`
says "unexpected argument '-v'". The flags only work in some places, and the
errors do not say why.

`-v` and `-q` should be accepted before or after any command, and mean the
same kind of thing everywhere.

`--config PATH` gets the same treatment (decided later in the same
discussion). It names the same `irsync.toml` for every command, but it is
defined three times today: once for a sync, once for the daemon, and once
for the other commands. Only the last works on both sides of a verb, and
none works before a command.

`--passphrase-file PATH` becomes global as well (decided). It is defined
five times today: for a sync, `daemon`, `show`, `id list` and `id show`.
`id generate` starts to use it: the new private key is encrypted with the
passphrase in that file (decided). Today `id generate` can only write an
unencrypted key.

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
- `--config PATH` is accepted before the command, after it, and after a
  nested command, and means the same file everywhere:
  `irsync --config f host list`, `irsync daemon --config f`,
  `irsync host list --config f`.
- `--passphrase-file PATH` is accepted in the same places. Commands that
  load a key use it as today. Commands that load no key accept and ignore
  it.
- `irsync id generate --passphrase-file PATH` writes the private key
  encrypted with the passphrase in that file, so that the same flag and
  file unlock it afterwards.
- A command that would generate a key on the side fails instead when
  `--passphrase-file` is given (decided). Someone who passes a passphrase
  expects a protected key; the way to get one is `id generate`.

## Non-goals

- New `-v` output for `gc`, `index`, `export update` or the management
  commands. They get the flag; what it could show is for later issues.
- Making other sync flags work before a command (`irsync -n gc`).
- Encrypting keys that are generated on the side: by a first sync, by
  `id show` or `id list` on a machine without a client key, or by the
  daemon's first start. Only `id generate` encrypts. With
  `--passphrase-file` those commands refuse to generate a key at all (see
  below).
- Encrypting with `IRSYNC_PASSPHRASE` or a prompt. `id generate` only
  encrypts when the file is given.
- Changing the passphrase of an existing key, or encrypting one.
- Any change to what a sync prints.

## Anti-goals

- A flag before a command must never be dropped without a word.
  `irsync -a gc` must fail, not run `gc`.
- No command given `--passphrase-file` may write an unencrypted key.
- `id generate --passphrase-file` must never write an unencrypted key.
  If the file cannot be read or holds no passphrase, it fails and writes
  nothing.
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
- Define `--config` once, with `global = true`, on the top level (the
  existing `FilesArgs`). Remove the sync's own `--config`, the daemon's
  own, and the `files` field of every command. Every command reads the
  global one.
- Define `--passphrase-file` once, with `global = true`, on the top
  level, next to `--config`. Remove the five separate definitions.
- `-v` together with `-q` is accepted everywhere and `-q` wins, as in a
  sync today. (My call. Today the four logging commands reject the pair.)
- After parsing, when a command was given, reject any sync flag or path
  that was set on the command line (`-v`, `-q`, `--config` and
  `--passphrase-file` are no longer sync flags, so they pass). The error names the flag and the
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

### `id generate` with `--passphrase-file`

- The passphrase is the first line of the file only (decided). Its line
  ending is stripped, Unix (LF) or Windows (CR+LF), so a file copied from
  the other kind of system gives the same passphrase. Nothing else is
  trimmed: spaces are part of the passphrase. One function reads the file
  for unlocking and for generating, so the same file works for both.
- The private key is written in OpenSSH's encrypted format (what
  `ssh-keygen` writes with a passphrase; the `ssh-key` crate's `encrypt`).
  The `.pub` file, the two lines on stdout and the "wrote ..." line on
  stderr are the same as without the flag.
- It works with `--daemon`, `--out` and `--force` as generation does
  today.
- A file that cannot be read, or whose first line is empty, is an error
  (exit 1): `passphrase file PATH is empty` for the second case. No key
  file is written, and with `--force` the old key is not removed. (My
  call: an empty passphrase would look like protection and give none.)
- Without the flag the key is unencrypted, as today, even when
  `IRSYNC_PASSPHRASE` is set. (My call: you asked for the file; the
  variable is often set to unlock another key.)

### No key generated on the side with `--passphrase-file`

Decided. Today a missing key is generated, unencrypted, by a sync or a
listing, by `id show`, `id list` and `show` (the client's key), and by
the daemon's first start. With `--passphrase-file` given, these fail instead
and write nothing (exit 1). The error names the missing key file and says
what to run: `irsync id generate --passphrase-file PATH`, with `--daemon`
for the daemon's key.

- It applies only when no key serves. An existing key is loaded as today,
  and so is `~/.ssh/id_ed25519` when the client has no irsync key.
- Only the flag triggers it. `IRSYNC_PASSPHRASE` alone does not: those
  commands generate as today. (My call, to match your wording; the
  variable is often set for another key.)
- All of these go through one function (`load_or_generate` in
  `identity.rs`), which already receives the passphrase file. The check
  goes there.
- ssh setup on the server is not affected: it never passes the flag on.

### Behaviour that changes

- `irsync gc -v`, `irsync index -v` and `irsync export update -v` no longer
  show info logs. `-vv` does.
- `irsync -v gc host:path` used to sync a local directory called `gc`.
  It now runs `gc` and fails on the extra argument, as `irsync gc
  host:path` already does. Write `./gc` for the directory.
- `-v -q` together is no longer an error on the four logging commands.
- `--config` before a command used to be an error and now works. So does
  `--passphrase-file`.
- `--passphrase-file` is accepted, and ignored, by commands that load no
  key (`gc`, `host list`, ...). It used to be an error there.
- `id generate --passphrase-file` used to be an error and now writes an
  encrypted key.
- A sync, `id show`, `id list`, `show` or `daemon` run with `--passphrase-file`
  on a machine without the key used to generate an unencrypted key. Now
  it fails and points at `id generate`.

### Help and docs

- The `daemon` help text says "info level (-v: debug)". That is wrong
  today; fix it to match the rule.
- The usage line of `irsync --help` changes once the setting is gone.
  Check that it still reads well.
- Update `book/src/reference/command-line.md` (the `-v`/`-q` rows of
  `daemon`, `index`, `gc`, and a note that the three flags work on every
  command, before or after it) and `book/src/reference/management.md`
  (its paragraph on `--config` says the flag may stand before or after the
  verb; it may now stand before the noun too). Document
  `--passphrase-file` as a flag of every command, and on `id generate` as
  what encrypts the new key. `book/src/reference/configuration.md` has a
  paragraph on encrypted keys that should say how to make one.

### Tests

Parser tests next to the existing ones in `cli/mod.rs`:

- each form in the goals parses to the right command, count and quiet;
- a sync flag before or after a command is rejected, with the flag named;
- `irsync -a host:photos/ config` and `irsync src gc` are still syncs;
- `irsync -v host:` is still a listing;
- `--config f` before a command, after it and after a nested command
  gives the same path, for a sync, `daemon` and a management command;
- the same for `--passphrase-file`.

On the real binary: `id generate --passphrase-file` writes an encrypted
key that `id show --passphrase-file` unlocks to the same id and that does
not load without a passphrase; the empty-file and missing-file errors
write nothing; without the flag the key is unencrypted. `id show`,
`id list`, `daemon` and a sync with `--passphrase-file` and no key fail,
write no key, and name `id generate`; without the flag they generate as
today.

One end-to-end check that `irsync -v id show` prints the identity.

## Left to the builder

- The wording of the global flags' help text. It is now shared by every
  command, so it has to describe the rule, not only the sync. The
  `--config` text loses its per-command notes ("for `state_dir`").
- How to find "a sync flag was set on the command line" (for example the
  value source in clap's matches).
- Whether `LogArgs` survives as the type that holds the global pair.
