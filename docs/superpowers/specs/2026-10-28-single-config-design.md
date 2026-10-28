# One config file, and `irsync config get|set|unset|list`

Date: 2026-10-28. Status: approved design, not yet planned.

Builds on `2026-10-23-zero-config-cli-design.md` (the three files),
`2026-10-28-config-cli-design.md` (the `host`, `export`, `grant` and `id`
nouns) and `2026-10-28-remote-path-design.md`. This document changes where
configuration lives and adds one noun; the sync, the daemon's behaviour
and the other nouns' verbs are unchanged.

## Intent

Today configuration is spread over three files in `~/.config/irsync/`:
`irsync.toml` (settings), `hosts.toml` (aliases) and `exports.toml`
(exports and grants). The split was chosen for tidiness and as the
hot-reload boundary, but it costs three path flags on every command, a
`$F` recipe in the daemon guide, a three-file `show`, and a standing
"which file" question. The user wants every piece of irsync configuration
editable by irsync itself, in the spirit of `git config`, without hand
edits, and a single dotted namespace for that only makes sense over a
single file.

So: one file, `irsync.toml`, holds everything. The two other files are
still *read* when present, for people who prefer to keep them apart by
hand, but irsync *edits* exactly one file, `irsync.toml` by default or
whatever `--config` names. A new `config` noun reads and writes any key in
it.

## Goals

- One file, `irsync.toml`, with the settings, the host aliases, the
  exports and the grants, in the tables they already use.
- `irsync config get KEY`, `set KEY VALUE...`, `unset KEY` and `list`,
  over a dotted key namespace that is the TOML path, with every write
  validated by the same parser that loads the file.
- Every management command (`host`, `export`, `grant`, `config`) edits the
  one file named by `--config` (default `~/.config/irsync/irsync.toml`),
  preserving comments and layout as today.
- `hosts.toml` and `exports.toml` beside the config file are still read
  when they exist; each table has exactly one home, so there is never a
  question of precedence.
- The daemon still picks up hosts, exports and grants on every connection
  without a restart.

## Non-goals

- Changing any setting's name, type, default or meaning, or any table's
  layout. `[hosts.*]`, `[exports.*]`, `[all]`, `[client]`, `[daemon]` and
  `[daemon.discovery]` stay as they are.
- Migrating files automatically. No users yet; the README says how to
  merge by hand (concatenate), and `show` says which files were read.
- Scopes (git's `--system`, `--global`, `--local`). One file, one scope.
- Editing a split-out `hosts.toml` or `exports.toml` with irsync. People
  who keep them apart edit them by hand; that is the deal.
- Printing defaults for keys that are not set (`config list` shows the
  file; `config get` of an unset key with a default prints the default,
  see below, but there is no "list everything with defaults").

## The file

`irsync.toml`, merged:

```toml
identity = "~/.config/irsync/id_ed25519"
state_dir = "~/.local/state/irsync"

[client]
parallel = 32

[daemon]
listen = ["0.0.0.0:0", "[::]:0"]

[daemon.discovery]
mdns = true

[hosts.home]
endpoint_id = "k51qzi5..."
address = ["192.168.1.10:4000"]

[all]                        # grants for every export
ro = ["laptop"]

[exports.photos]
path = "/srv/photos"
rw = ["laptop"]
```

Every table keeps its current schema and validation (alias rules, absolute
export paths, `all` reserved as an export name, unknown keys rejected
naming file, table and key). `[all]` keeps its name: it matches `grant add
all WHO`, and it is the same table whether it sits in `irsync.toml` or in
`exports.toml`. Top-level keys that are not one of `identity`,
`state_dir`, `client`, `daemon`, `hosts`, `exports`, `all` are errors as
today, with the old-layout hints (`unknown table "home"; hosts live under
[hosts.home]`).

### Split files, still read

When `hosts.toml` exists in the config file's directory, it holds the
`[hosts]` table and `irsync.toml` must not: a `[hosts]` table in both is
an error naming both files (`hosts are in hosts.toml; remove [hosts] from
irsync.toml, or delete hosts.toml`). Likewise `exports.toml` holds
`[exports]` and `[all]`. A split file is parsed with its current schema
and nothing else is allowed in it. "Beside the config file" means the
directory of the path `--config` names, or `~/.config/irsync/` by default;
there are no `--hosts` or `--exports` flags any more, anywhere.

An editing command whose target table lives in a split file refuses,
naming the file: `irsync host add ...` with `hosts.toml` present prints
`hosts are kept in hosts.toml here; irsync edits irsync.toml only: move
the [hosts] table into irsync.toml, or edit hosts.toml by hand` and
changes nothing. `config get` and `config list` read across the split
files; `config set` and `config unset` refuse the same way.

### Loading

One loader, `Config::load(config_path) -> Config { settings, hosts,
exports }`, parses `irsync.toml`, then the split files if present, and
applies the one-home rule. Everything that loads any of the three today
(`Settings::load`, `Hosts::load`, `Exports::load`, the daemon's
per-connection snapshot, `show`, the client's sync setup) goes through it.
The daemon re-reads the whole thing on every connection, as it re-reads
two files today, and takes hosts, exports and grants from the result;
settings still need a restart, documented as before. A file that does not
parse fails closed for the daemon, naming the file and line, as a broken
`exports.toml` does today. That widens the blast radius of a typo in
`[client]` to the daemon's accept path; accepted, and the log line says
where.

## `irsync config`

The fourth noun. `KEY` is a dotted TOML path: `identity`,
`client.parallel`, `daemon.listen`, `daemon.discovery.mdns`,
`hosts.home.relay`, `exports.photos.read_only`, `all.rw`.

- `irsync config get KEY`: prints the value. A scalar prints bare (no
  quotes); an array prints one element per line, as `git config
  --get-all` does. A key that names a table (`hosts.home`) prints its
  keys as `KEY = VALUE` lines in file order, one per line, with the
  dotted path relative to the table (`endpoint_id = "k51..."`, `address =
  ["192.168.1.10:4000"]`). A settings key that is not set but has a
  default prints the default (`config get daemon.idle_timeout` → `30m`);
  a key that is not set and has no default (`client.parallel`,
  `hosts.nope`) prints nothing and exits 1, like git. An unknown key
  (`daemon.nope`) is an error.
- `irsync config set KEY VALUE...`: sets the key. The value is read in the
  key's type: booleans as `true`/`false`, integers, durations as the file
  writes them (`30m`, `7d`), paths and URLs as strings, an `endpoint_id`
  as an id. For an array key every `VALUE` is one element, so `config set
  daemon.listen 0.0.0.0:4433 '[::]:4433'` writes a two-element array and
  one `VALUE` writes a one-element array; `config set all.rw` with no
  value writes an empty array (an error for a scalar key). A table key
  (`hosts.home`) cannot be set. The write is validated by loading the
  edited document with the full parser before anything touches the disk;
  a bad value is refused naming the key and the parser's message, and the
  file is unchanged. Tables are created on demand (`config set
  hosts.home.endpoint_id ID` creates `[hosts.home]`, validated like `host
  add`: a bad alias or id is refused). Comments and the rest of the file
  are preserved, as the other nouns do.
- `irsync config unset KEY`: removes the key, or the whole table when
  `KEY` names one (`config unset hosts.home` is `host remove home`;
  `config unset exports.photos` is `export remove photos`). Removing a key
  that is not there is an error (`nothing to remove`), like `grant
  remove`.
- `irsync config list`: every key set in the file(s), one `KEY = VALUE`
  line per key in file order, dotted from the root (`daemon.listen =
  ["0.0.0.0:0", "[::]:0"]`, `exports.photos.path = "/srv/photos"`), values
  in TOML spelling. Keys from a split file are listed too, after the main
  file's.

`config` takes `--config PATH` like the other nouns, global, before or
after the verb. `--hosts` and `--exports` go away from every command
(`FilesArgs` becomes `--config` alone; `daemon` loses the two flags).
Nothing in `config set` bypasses the file parser's validation: setting
`exports.photos.path` to a relative path, creating `hosts.all` or
`exports.all`, or a bad alias or id, fails with the parser's message.

### What `config set` validates

`config set` is the low-level tool, as `git config` is: it checks the
key's type and then what the file parser checks, and nothing more. The
nouns keep their extra conveniences: `export add` joins a relative path
onto the working directory, expands `~` and requires the directory to
exist; `grant add` accepts a pasted key line or a `.pub` file, keeps the
key's comment, and moves a peer between `rw` and `ro`. None of that
happens in `config set`: `config set exports.photos.path ./x` is refused
because the parser requires an absolute path, and `config set
exports.photos.rw ~/laptop.pub` writes that string as a grant entry that
matches nobody, which the daemon warns about at the next connection, as a
hand edit would. The README says so in one sentence: `config` writes what
you say, the nouns know what you mean. (My call; the reviewer asked.)

### Keys with dots in a name

An alias may contain a dot and an export named after a directory often
does (`www.example.com`). `KEY` is parsed with TOML's key grammar, so a
segment with a dot is quoted: `config get exports."www.example.com".path`.
`config list` quotes such segments the same way, so its lines round-trip
into `get` and `set`. The README's `config` row shows one quoted example.
(My call.)

### One strict loader

`Config::load` validates every table it finds, for every command. A pure
client with a broken `[exports]` table therefore cannot sync until the
table is fixed, just as a daemon with a broken `[client]` table refuses
peers. This is deliberate: a config file either parses or it does not,
and the error names the file, table and line. (My call; the alternative,
per-table laziness, would let a machine run for weeks with a table it
never reads silently wrong.)

The three existing nouns stay as the convenient spellings for the common
edits; `config` is the complete one. `grant list`, `export list` and
`host list` are unchanged.

## `show`

The first line names the config file; when a split file was read, a line
`hosts: PATH (also read)` or `exports: PATH (also read)` follows it. The
rest is as today.

## What changes where

- `config/`: a `Config` type and loader (`config/mod.rs` or a new
  `config/file.rs`), the merged `RawFile` with the shape pass extended to
  the seven top-level keys, the one-home rule; `Hosts::parse` and
  `Exports::parse` keep their schemas for the split files and become the
  table parsers the merged loader calls. `Paths` becomes the config path
  plus the two derived sibling paths.
- `manage/`: `load_doc` and `save_doc` for the one file; `save_hosts` and
  `save_exports` fold into one `save_config` that validates the whole
  document; the refusal when a split file owns the table; a new
  `manage/config.rs` with get, set, unset and list over the document plus
  a value parser driven by the key's type.
- `cli/`: `Command::Config(ConfigArgs)` with `ConfigCmd::{Get, Set, Unset,
  List}`; `FilesArgs { config }`; `DaemonArgs` loses `hosts` and
  `exports`; `SyncOptions` loses `hosts`.
- `main.rs`: `run_config`; every `file_paths` user reads `Config`.
- `daemon.rs`: `FileAuthorizer` holds the config path and re-reads
  `Config` per connection.
- Docs: README (files section, one TOML example, the `config` rows in the
  command table, flag table), `docs/daemon.md` (the system-wide recipe
  loses `$F`; the split-file option gets one paragraph), notes in the
  three earlier specs.
- Tests: the merged parser (every table, the shape errors, the one-home
  rule both ways), `config` get/set/unset/list over scalars, arrays,
  tables, durations, unknown keys, bad values leaving the file untouched,
  the split-file refusal, the nouns writing `irsync.toml`, the daemon
  re-reading the merged file per connection, `show`'s "(also read)" line.

## Left to the builder

- The exact `KEY = VALUE` spelling `list` uses for inline tables, if any
  appear in a file (the nouns never write them).
- Whether `config get` on a table prints nested tables (`daemon` →
  includes `daemon.discovery.*` as dotted keys) or only its direct keys.
  Dotted, in file order, is the suggestion.
- How the value parser learns a key's type: a small table of known keys
  with a kind (bool, int, duration, string, id, string array, address
  array) is the suggestion; the full validation is the parser's anyway.
- Error text for a `config set` value the key's type cannot read, beyond
  naming the key and the type.

## Decisions made in the conversation

- Merge everything into `irsync.toml`; keep reading `hosts.toml` and
  `exports.toml` when they exist, for people who keep them apart by hand.
- irsync edits one file only, `irsync.toml` by default, `--config PATH`
  otherwise; no `--hosts` or `--exports` flags.
- `config get|set` in the spirit of git, so that no configuration needs a
  hand edit.

Calls I made while writing (the user can overrule):

- Each table has exactly one home: a split file present means the table
  must not also be in `irsync.toml`, and the editing commands refuse to
  touch a table a split file owns.
- `[all]` keeps its name in the merged file.
- `config unset` exists, and `config list` prints `KEY = VALUE` in TOML
  spelling; `config get` prints bare scalars, one array element per line,
  and the default for an unset settings key.
- `config set` takes one `VALUE` per array element rather than a single
  TOML literal.
- Fail closed for the daemon on any error in the merged file.
- `config set` validates by type and parser only; the nouns keep their
  conveniences (path joining and existence, key lines and `.pub` files).
- Dotted names are addressed with TOML key quoting, and `list` prints
  them quoted.
- One strict loader for every command: a broken table blocks the client
  too.
