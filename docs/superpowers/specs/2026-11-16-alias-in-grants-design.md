# The config names peers by alias, not by EndpointId

Date: 2026-11-16

Issue: #38 (and the `host remove` part of #37)

## Intent

A grant is written the way it was given. An EndpointId stays an id even
when `[hosts]` has a name for it, and ssh setup and the daemon's automatic
exports only ever write ids. The result is a config full of long ids that
the reader has to match up by eye, in a file that often knows a name for
them.

Every save should leave the config in one clean form: a peer that has an
alias is named by it.

## Goals

- Every time irsync saves the config, each EndpointId in a grant list that
  `[hosts]` has an alias for is written as that alias. This covers new
  entries and ids already in the file.
- `host remove` no longer leaves grants that point at nothing.

## Non-goals

- `irsync host rename`, and dropping `hosts.toml` and `exports.toml`. Both
  stay in #37.
- Rewriting `exports.toml`. irsync never edits it.
- Rewriting a config that irsync only reads. Nothing changes until the
  next save.
- Changing an entry that is already an alias, even when another alias of
  the same id sorts earlier. The admin chose that name, and a grant by
  alias follows that alias.

## Anti-goals

- A save that changes who has access. The rewrite changes how a peer is
  spelled, never what the peer may do at the time of the save.
- A save site that skips the rewrite. A file that is clean after some
  commands and not after others is worse than today.

## Decisions made in the issues

- Ids already in the file are rewritten too, not only new entries (#38).
- A grant rewritten to an alias follows the alias from then on: if the
  host entry later points at another id, the grant moves with it. This is
  wanted. It lets the daemon's admin move a client to a new id (#38).
- When two aliases have the same id, the first in lexicographic order is
  written (#38).
- `host remove` refuses while grants name the host. `--force` / `-f`
  removes those grants, then the host (#37, built here).

## Approaches

1. **Rewrite inside the save (chosen).** `ConfigLock::save` runs the
   rewrite on the document before writing it. All save sites get it:
   `grant`, `host`, `export`, `config set` and `unset`, ssh setup, ssh
   onboarding, the daemon's automatic exports. A new save site cannot
   forget it.
2. **A function each caller runs before saving.** The same rewrite, called
   by hand at each of the 11 sites. A site that forgets the call breaks
   the rule without any test noticing.
3. **Fix each place that writes a peer.** `PeerSpec::parse`, ssh setup and
   `add_auto_export` pick the alias themselves. Ids already in the file
   still need the pass from 1 or 2, so this is extra code for nothing.

Michael picked 1, with the rewrite as a function of its own that other
code can call.

## Design

### The rewrite

A public function in `crates/irsync/src/manage/grants.rs`, pure on a
parsed document like the other editors there. It takes the document and
the known hosts, and returns whether it changed anything.

It visits every grant list: `[all].rw` and `.ro`, `[immediate].rw`, and
`rw` and `ro` of every table under `[exports]`, inline tables included.
For each string entry:

- If it is a valid EndpointId and at least one alias has that id, the
  text becomes the first such alias in lexicographic order.
- Anything else is left alone: an alias, an id with no alias, text that
  matches nobody.

The entry keeps its place in the list and its whitespace and comments. A
comment written beside an id (the key comment that `grant add` writes for
a public key) stays beside the alias.

Then duplicates go, per table:

- The same text twice in one list: the first stays.
- The same text in both `rw` and `ro`: the `ro` entry goes. This is what
  the daemon already does when it reads the file (`rw` wins), so access
  is unchanged.

Two different aliases of the same id are not duplicates and both stay.

The function is idempotent: a second run changes nothing.

### Where it runs

`ConfigLock::save` runs it before the "would this load" check and the
write. `save` therefore takes the document mutably. `save_config` follows.

The hosts it uses are those of the document being saved, so an alias
added by `host add` in the same edit already counts, and the grants of
that id are renamed in the same save. While `hosts.toml` is still
supported, a `hosts.toml` beside the config is the source instead, as in
`Config::load`. If the hosts cannot be read, the save fails as it does
today for a file that would not load; nothing is written.

### `host remove`

`host remove ALIAS` first runs the rewrite, so a hand-written id of that
host counts as a grant naming it wherever the alias is the one the
rewrite would pick. It then looks for the alias in every grant list.

- None: the host is removed, as today.
- Some, without `--force`: nothing is changed. The error names the lists
  (for example `all`, `immediate`, `exports.photos`) and says to remove
  the grants or pass `--force`.
- Some, with `--force` / `-f`: those entries are removed, then the host.
  The message says how many grants went.

Grants kept in `exports.toml` cannot be edited. While that file is still
supported, a grant there that names the alias makes `host remove` refuse,
with or without `--force`, and the error names the file.

### Messages

Commands that print the peer after a grant (`<peer>: immediate access`,
`<peer>: rw on photos`) print what was written: the alias when the id has
one.

### Tests

Unit tests beside the function for: id to alias, two aliases for one id,
id without alias, an existing alias left alone, duplicates within a list
and across `rw` and `ro`, kept comments and layout, inline tables, a
second run changing nothing.

Command-level tests for: `grant add` by id writing the alias, `host add`
renaming existing grants, an unrelated `config set` cleaning the file,
ssh setup and the automatic export writing the alias, and `host remove`
refused, forced, and with no grants.

### Book

`book/src/reference/management.md` for `host remove` and `--force`, and
the configuration reference for the rule that a save writes aliases.

## Left to the builder

- The function's name and exact signature, and how `save` gets at the
  hosts of a split `hosts.toml`.
- The wording of the messages and errors.
- Whether `host remove` reuses `grant_remove` for `--force` or has its
  own small helper.
