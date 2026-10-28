# One grammar for hosts, exports and grants

Status: design, 2026-10-28. Supersedes the "Commands" section and the
`hosts.toml` and `exports.toml` layouts of `2026-10-23-zero-config-cli-design.md`.
There are no users yet; nothing here keeps compatibility with the current
commands or file layouts.

## Intent

The management commands grew in three grammars. Exports are bare verbs
(`irsync add`, `irsync remove`) that do not say what they add. Grants are a
second pair of bare verbs (`allow`, `revoke`). Hosts are a noun namespace
(`irsync host add`). The three file flags are declared on every command
separately. And `revoke` is the one command whose positionals change
meaning by count.

The fix is one grammar: three nouns, each with the same three verbs.

```sh
irsync host   add     ALIAS ID [--relay URL] [--address ADDR]... [--parallel N] [--reconnect DUR] [--dht yes|no] [--mdns yes|no]
irsync host   remove  ALIAS
irsync host   list

irsync export add     NAME:PATH | PATH [--ro] [--warm-index]
irsync export remove  NAME
irsync export list

irsync grant  add     EXPORT|all WHO [--ro]
irsync grant  remove  EXPORT|all WHO|all
irsync grant  list    [EXPORT | WHO]
```

The two files those commands edit change shape to match: every export
lives under `[exports.NAME]`, every alias under `[hosts.ALIAS]`, and the
grants that apply to every export live in a table called `[all]`, the same
word the command line uses.

The quick start becomes:

```sh
# laptop
irsync id                                  # public key line, then the client EndpointId

# home server
irsync export add photos:/srv/photos       # prints the daemon's EndpointId
irsync host add laptop <laptop-id>
irsync grant add photos laptop
irsync daemon

# laptop
irsync host add home <server-id>
irsync -av ~/Photos home:photos/
```

## Goals

- Every management command is `irsync NOUN VERB ...` with the nouns `host`,
  `export`, `grant` and the verbs `add`, `remove`, `list`.
- `grant` positionals are always two. Nothing changes meaning by count.
- `grant list WHO` answers "what may this peer reach?" without reading
  every export block of `show`.
- `--config`, `--hosts` and `--exports` are given once, anywhere on the
  line, for the management commands.
- Neither file has bare top-level keys, so a line appended by hand cannot
  land in the wrong table, and no export name needs reserving for the
  file's sake.
- Everything the old commands did, the new ones do: same validation, same
  atomic writes, same comment preservation through `toml_edit`, same
  per-connection read by the daemon.

## Non-goals

- Changing `show`, `id`, `daemon`, `index`, `gc` or `keygen`. Folding
  `index --daemon` into `export index`, `daemon --show-id` into `id
  --daemon`, and the fate of `keygen` are the next round, not this one.
- A command that edits `irsync.toml` (`config set`). The settings file is
  untouched.
- Letting `[all]` carry anything but grants (for example a default
  `warm_index`). The table is named for growth; nothing grows into it now.
- Aliases for the old spellings. `add`, `remove`, `allow`, `revoke` at the
  top level are gone, and top-level `[photos]` or `[home]` tables are
  errors, not accepted silently.
- Any change to the address syntax, the sync flags, the wire protocol or
  the authorizer's semantics.

## Anti-goals

- A name that exists in a file but no command can address. The one reserved
  export name, `all`, is refused by `export add` and by the parser with the
  same message.
- Writing anything the user did not ask for. `export add` and `host add`
  still write only the keys their flags set.
- An old-layout file that parses as an empty one. A top-level table in
  either file is an error that names the file, the line and the new
  location.

## Constraints

- clap, `toml`, `toml_edit`, `ssh-key`, `dirs` stay; no new dependency.
- The stress harness (`crates/stress/src/daemon.rs`) writes `exports.toml`
  by hand and must write the new layout.
- The reconnect hint in `crates/irsync/src/session/reconnect.rs` (the line
  a rejected client prints for the daemon's admin) names the command to
  run and must name the new one.
- Exit codes are unchanged: a management command exits 0 on success and 1
  on any error, as today.

## Commands

All management commands take `--config PATH`, `--hosts PATH` and
`--exports PATH`. They are accepted anywhere after the noun: between noun
and verb, or after the verb's arguments (`irsync host --hosts H list` and
`irsync host list --hosts H` are the same). They are not accepted before
the noun: the root command's flags are the sync's, and `daemon`, `index`
and a sync keep exactly the flags they have now. Defaults are
`irsync.toml`, `hosts.toml` and `exports.toml` in `~/.config/irsync/`, as
before. `show` and `id` take the same three flags as today. The natural
implementation is `global = true` arguments declared once on each noun
subcommand, but that is the builder's choice; the observable positions
are what is specified.

Writes are atomic (temporary file in the same directory, then rename), a
missing file is created with its directory, and a document that would not
load is never saved. All unchanged from the 2026-10-23 design.

### `irsync host add ALIAS ID [flags]`, `irsync host remove ALIAS`, `irsync host list`

Unchanged in arguments, flags, validation and messages. `host add` writes
`[hosts.ALIAS]` with `endpoint_id = ID` and one key per flag given. On an
existing alias it updates the id and the keys named by flags, prints old
and new id when the id changes, and leaves the rest of the table alone.
`host remove` deletes `[hosts.ALIAS]` and does not touch `exports.toml`.
`host list` prints each alias as `alias: id`, then one indented `key =
value` per other key in file order, the same lines as today.

### `irsync export add NAME:PATH | PATH [--ro] [--warm-index]`

Unchanged in arguments and validation: `NAME:PATH` names the export, a
bare `PATH` uses its last component, `PATH` is made absolute and must be an
existing directory, an existing export of that name is an error. `--ro`
writes `read_only = true`, `--warm-index` writes `warm_index = true`. The
table written is `[exports.NAME]`. The name `all` is refused:

```
"all" is reserved: it is the word `grant add all WHO` uses for every export
```

On success the first line below goes to stderr and the id to stdout, as
today:

```
added export photos (/srv/photos) to ~/.config/irsync/exports.toml
8d01073f44cd...
```

### `irsync export remove NAME`

Deletes `[exports.NAME]`, grants included. Unknown name: error.

### `irsync export list`

Prints the exports section of `show` (below): one block per export.

### `irsync grant add EXPORT|all WHO [--ro]`

`WHO` is an alias from `hosts.toml`, an EndpointId in either spelling, a
pasted `ssh-ed25519 ...` line, or the path of a `.pub` file, resolved and
stored exactly as `allow` did: an alias is stored as the alias, everything
else as the EndpointId, a key comment becomes a trailing TOML comment on
the entry. The entry goes into `rw`, or `ro` with `--ro`, of
`[exports.EXPORT]`, or of `[all]` when the first argument is `all`; it is
removed from the other array first, and an entry already present under
another spelling is replaced. Unknown export, or an alias not in
`hosts.toml`: error. `WHO` of `all` or `*`: error saying there is no open
access.

### `irsync grant remove EXPORT|all WHO|all`

Two positionals, always. The first names an export, or `all` for every
export and the `[all]` table together. The second names a peer (matched by
literal text or by the EndpointId it resolves to), or `all` for every peer.
So:

| Command | Effect |
| --- | --- |
| `grant remove photos laptop` | drop `laptop` from both arrays of `[exports.photos]` |
| `grant remove photos all` | empty both arrays of `[exports.photos]` |
| `grant remove all laptop` | drop `laptop` from every export and from `[all]` |
| `grant remove all all` | empty every array in the file |

Nothing to remove is an error naming what was looked for. The old
one-argument form (`revoke WHO`) is gone; `grant remove all WHO` is its
spelling.

### `irsync grant list [EXPORT | WHO]`

With no argument: an `all:` block with the `rw` and `ro` lines of `[all]`,
then one `NAME:` block per export with its `rw` and `ro` lines, in the
same line format `show` uses for grants (`    rw: a, b (id)`). Path,
`read_only` and `warm_index` lines are not repeated; `export list` has
them. Exports with no grants are listed with their header line and
nothing under it, so the output always names every export.

With an argument that names an export: that export's `rw` and `ro` lines.
`all` here means the `[all]` table.

With an argument that does not name an export but resolves to a peer (an
alias in `hosts.toml`, or an EndpointId): one line per export the peer may
reach, with the access level and where it comes from.

```
laptop (k51qzi5...):
    photos: rw
    docs: ro (read_only export)
    backups: rw (all)
```

`(all)` marks a grant inherited from the `[all]` table; a per-export entry
for the same peer wins over it, as in the authorizer. `(read_only export)`
marks an `rw` grant capped to `ro` by the export's `read_only`. A peer with
no grant at all prints the header line and nothing under it, exit 0.

When an export and an alias share a name, the export wins; the daemon's
machine has no reason to name a client after an export, and `grant list`
with no argument shows everything either way. An argument that is neither
an export, an alias nor an id is an error naming all three possibilities.

### `irsync show`

Unchanged in content and order: config path, client identity, hosts path
and aliases, exports path, daemon identity, one block per export (path,
flags, `rw`, `ro`), then the grants for every export. The last block is
now labelled `all:` instead of `all exports:`. `host list` and `export
list` print the hosts section and the export blocks verbatim; `grant list`
reuses the grant lines of those blocks, so every grant is spelled the same
way wherever it is printed.

## Files

### `hosts.toml`

```toml
[hosts.home]
endpoint_id = "k51qzi5uqu5dj..."      # hex or z-base-32
relay = "https://relay.example.net"   # the rest is optional
address = ["192.168.1.10:4000"]
parallel = 64
reconnect = "1h"
dht = false
mdns = false

[hosts.laptop]
endpoint_id = "7f3a0c..."
```

The only top-level key is `hosts`, a table of tables. Alias rules are
unchanged: non-empty, no `:` or `/`, not itself an EndpointId. A missing
file is empty.

### `exports.toml`

```toml
[all]                        # grants for every export, present and future
rw = ["9c4d..."]
ro = []

[exports.photos]
path = "/srv/photos"
rw = [
  "laptop",      # an alias from hosts.toml
  "k51qzi5...",  # or a raw id
]
ro = ["b2e1..."]
warm_index = false

[exports.docs]
path = "/srv/docs"
read_only = true             # caps everyone, even ids under rw
ro = ["7f3a0c..."]
```

The top-level keys are `all` (optional; `rw` and `ro` arrays only) and
`exports` (optional; a table of export tables). Each export table takes
`path` (required, absolute), `read_only`, `warm_index`, `rw`, `ro` as
today. `exports.all` is refused with the message `export add` uses.
Precedence is unchanged: a per-export entry for an id wins over `[all]`;
an id in both `rw` and `ro` of one table has `rw`; `read_only` caps
everyone.

### Errors for the old layouts

`deny_unknown_fields` already rejects any other top-level key. The message
for a top-level *table* says where it moved, so the one person with an
old file (the author) is told what to do:

```
~/.config/irsync/exports.toml:5: unknown table "photos"; exports live under [exports.photos]
~/.config/irsync/hosts.toml:1: unknown table "home"; hosts live under [hosts.home]
```

A top-level `rw` or `ro` key in `exports.toml` gets the same treatment:
`unknown key "rw"; grants for every export live under [all]`. Other unknown
keys keep today's message.

The daemon's per-connection read, the fail-closed rule for a file that
does not parse, and the fatal startup check are unchanged; they simply see
the new layout.

## Where the old names appear

Each of these changes to the new spelling:

- `crates/irsync/src/cli/mod.rs`: the `Command` enum (`Add`, `Remove`,
  `Allow`, `Revoke`, `Host`) becomes `Host`, `Export`, `Grant`, each with
  an `Add | Remove | List` subcommand enum. `FilesArgs` moves to wherever
  the builder puts the shared flags.
- `crates/irsync/src/main.rs`: the dispatch.
- `crates/irsync/src/manage/`: the writers address `doc["hosts"][alias]`
  and `doc["exports"][name]` / `doc["all"]`; `host_list` iterates the
  `hosts` table; a `grant_list` reporter is new.
- `crates/irsync/src/config/hosts.rs`, `config/exports.rs`: the parsers,
  with the "where it moved" messages.
- `crates/irsync/src/session/reconnect.rs`: the hint becomes
  `irsync grant add {export} {client}`.
- `crates/stress/src/daemon.rs`: writes `[exports.data]`.
- `README.md`: quick start, the "Managing exports and hosts" table, both
  file examples, the `Files` paragraph listing which commands take which
  flags.
- `docs/daemon.md`: every `irsync add`, `allow`, `revoke`, `host add`
  mention; the system-wide example; the `[hosts.local]` example near the
  end.
- `docs/superpowers/specs/2026-10-23-zero-config-cli-design.md`: a
  one-paragraph note at the top of "Commands" and the two file sections
  pointing here.

## Tests

Rewritten, not deleted, wherever they touch the old surface: `cli.rs`
(parsing of every noun and verb, the flags in each position, `grant
remove` with one argument is a usage error, `export add all:...` refused),
`config_files.rs` (both layouts parse, old layouts fail with the "where it
moved" message, `exports.all` refused), `commands_e2e.rs` and
`live_exports_e2e.rs` (the add, grant, connect flow against a running
daemon with the new commands), `daemon_auth.rs` and `authz_snapshot.rs`
(grants read from `[all]` and `[exports.*]`, precedence), the reconnect
hint test in `session/reconnect.rs`, and the stress harness's own tests
if any assert on the file it writes.

New: `grant list` in all three forms, including the `(all)` and
`(read_only export)` markers and the export-wins-over-alias rule; `host
list` and `export list` output equals the matching section of `show`
line for line, and every `rw:`/`ro:` line `grant list` prints appears
verbatim in `show`.

## Left to the builder

- How the three file flags are declared in clap so they are accepted in
  every position, and whether `show` and `id` share that declaration.
- Module layout under `manage/` (one file per noun is the obvious shape).
- The exact text of `show` and `list` blocks beyond what is shown above.
- Whether the parsers special-case the "where it moved" messages by
  inspecting the unknown key, or by a pre-pass over the raw table.
- The order of the `grant list WHO` lines (file order or alphabetical).

## Decisions made in the conversation

Theirs (Mike's):

- Noun first (`host`, `export`, `grant`), not verb first (`add host`),
  after weighing help discoverability, future noun-specific verbs
  (`export index`, `host ping`) and the plural problem of `list hosts`.
- `grant add` / `grant remove` as the words, not `allow` / `revoke`.
- Every-export grants move into an `[all]` table.
- Exports move under `[exports.NAME]`.
- Hosts move under `[hosts.ALIAS]`, for symmetry and so the two files
  could one day be sections of one.

Mine (the designer's calls, open to change):

- `grant remove` always takes two positionals; `grant remove all WHO`
  replaces the old bare `revoke WHO`.
- `grant list` exists, with the three forms and the markers above; export
  wins over alias when a name is both.
- `all` stays reserved as an export name, enforced in `export add` and the
  parser with one message, because the command keyword needs it even
  though the file no longer does.
- The file flags are accepted anywhere after the noun, not before it;
  `daemon`, `index` and the sync keep theirs, and the root parser is not
  touched.
- `grant list` with no argument prints only grant lines, one block per
  export plus `all:`; `show` keeps its layout rather than being
  restructured around grants.
- `rw` and `ro` stop being reserved export names; only `all` is.
- `host add` refuses the aliases `all` and `*`, since neither could ever
  be granted.
- Old-layout files get an error that names the new location rather than
  being migrated or accepted.
- `[all]` holds grants only, for now.
- Old command spellings are removed outright, not aliased.
