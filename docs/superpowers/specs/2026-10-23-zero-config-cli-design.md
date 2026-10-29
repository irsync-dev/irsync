# Zero-config defaults and the exports CLI

Status: implemented 2026-10-23 (plan: `../plans/2026-10-23-zero-config-cli.md`); parts since superseded as noted below. Supersedes the identity, configuration and
authorisation sections of `2026-10-17-irsync-design.md` where they differ.
There are no users yet; nothing here keeps compatibility with the current
files, flags or wire protocol.

## Intent

irsync should work end to end, on both machines, without anyone writing a
config file. Today it does not: `irsync keygen` writes a key the client never
looks for, `irsync daemon` without a config dies on a missing
`authorized_keys` with an error that names no file, and there is no way to
declare a module or authorise a key except by editing two files in two
syntaxes. The design spec promised a daemon startable from the command line
alone; that never landed.

The fix has three parts:

1. **Identities that appear on their own.** Both sides generate their key the
   first time they need it and print what the other side needs to know.
2. **One settings file, one hosts file, one exports file, all optional.**
   Client and daemon settings live in `irsync.toml`. Aliases for remote
   identities live in `hosts.toml`. What the daemon shares, and with whom,
   lives in `exports.toml`. The daemon re-reads the hosts and exports
   files on every connection.
3. **A small set of top-level commands** that edit those files so the user
   never has to open them: `add`, `remove`, `allow`, `revoke`, `host`,
   `show`, `id`.

The result for the simple case, two machines, one account each:

```sh
# laptop
irsync id                              # prints the client id to hand to the server

# home server
irsync add photos:/srv/photos          # prints the daemon's EndpointId
irsync host add laptop <laptop-id>     # a readable name for the client
irsync allow photos laptop
irsync daemon

# laptop
irsync host add home <server-id>       # a readable name for the server
irsync -av ~/Photos home:photos/       # or <server-id>:photos/
```

Two strings are copied by hand, one in each direction. Nothing else is
typed into a file.

## Goals

- A first sync works with no config file on either machine.
- Every command that needs a key generates it if it is missing, says so once,
  and prints the public identity.
- Exports, grants and aliases are managed by commands, and the daemon
  honours a change on the next connection without a restart.
- A loopback setup, client and daemon in the same account on the same
  machine, works by default.
- Every error that involves a file names the file.

## Non-goals

- Picking up changes to the daemon's own settings (`listen`, relay, discovery,
  `idle_timeout`) without a restart. Only exports and grants are live.
- Anonymous or open exports. A connection from a key not listed in the
  exports file is still rejected before any protocol message is read.
- Deriving an identity from an ssh-agent key, or using agent-held or
  hardware keys at all. iroh signs with the raw seed; the design spec's
  statement on this stands. Reading `~/.ssh/id_ed25519` by default is also
  dropped (see "Identity").
- Trust-on-first-use pairing, pairing codes, or any protocol change for
  authorisation.
- Glob patterns and negation in host names (the old `Host nas-*`, `Host !x`).
- Windows.

## Anti-goals

- Writing anything into the config or exports file that the user did not ask
  for. `add` and `allow` write only the keys they set; defaults stay implicit.
- Silent identity changes. A key is generated only when the file is missing;
  an unreadable or encrypted key is an error with the path in it, never a
  reason to make a new one.
- A daemon that serves a stale view after `irsync allow`. The whole point of
  the per-connection read is that what the file says is what the daemon
  enforces.

## Constraints

- The workspace already depends on `toml` (parsing), `ssh-key` (key files),
  `dirs` (XDG paths) and `clap` (CLI). The in-place editor is the `toml_edit`
  crate, which preserves comments and formatting; it is the one new
  dependency.
- The stress harness (`crates/stress`) writes the daemon's config files,
  generates the client key with `keygen --out` and relies on the daemon
  generating its own key at the configured path; it is part of this change
  and keeps using explicit paths.
- Exit codes, rsync-compatible flags, the session protocol, the index and
  resume machinery are untouched except for the one rename below.

## Vocabulary: exports, not modules

The thing a daemon shares is an **export**, as in NFS. The word *module*
goes away everywhere: CLI help, config keys, the exports file, log lines,
docs, error messages, Rust identifiers, and the wire protocol
(`Hello.module` becomes `Hello.export`, `NO_SUCH_MODULE` becomes
`NO_SUCH_EXPORT`). The address syntax `<host>:<export>/<path>` is unchanged
in form.

> Superseded 2026-10-28 by `2026-10-28-remote-path-design.md`: the address
> is `HOST:PATH`, resolved on the daemon (export name, home-relative or
> absolute); `Hello.export` and `NO_SUCH_EXPORT` are gone.

## Files

> Superseded 2026-10-28 by `2026-10-28-single-config-design.md`: one
> `irsync.toml` holds every table; `hosts.toml` and `exports.toml` are
> read when they sit beside it but never edited; the only flag is
> `--config`.

All paths are defaults; every command accepts `--config PATH` and
`--hosts PATH`, and the daemon and management commands accept
`--exports PATH`. The old `-F`,
`$IRSYNC_CONFIG`, the `/etc/irsync` lookup and `irsyncd.toml` are gone.

| File | Purpose | Created by |
| --- | --- | --- |
| `~/.config/irsync/irsync.toml` | client and daemon settings | the user |
| `~/.config/irsync/hosts.toml` | aliases for remote identities | `irsync host add` |
| `~/.config/irsync/exports.toml` | what the daemon shares and who may use it | `add` and `allow` |
| `~/.config/irsync/id_ed25519` (+ `.pub`) | the client identity | any command that needs it |
| `~/.config/irsync/daemon_ed25519` (+ `.pub`) | the daemon identity | any command that needs it |
| `~/.local/state/irsync/` | indexes, sessions (unchanged) | irsync |

A missing `irsync.toml`, `hosts.toml` or `exports.toml` is the same as an
empty one.

### irsync.toml

```toml
# Everything is optional; these are the defaults.
identity = "~/.config/irsync/id_ed25519"
state_dir = "~/.local/state/irsync"

[client]                     # was the `Host *` block
parallel = 32
reconnect = "24h"
dht = true
mdns = true
relay = "https://relay.example.net"   # no default: n0's relays

[daemon]
identity = "~/.config/irsync/daemon_ed25519"
idle_timeout = "30m"
dormant_session_timeout = "7d"
listen = ["0.0.0.0:0", "[::]:0"]
portmap = true
relay = "https://relay.example.net"
relay_mode = "default"       # or "disabled"

[daemon.discovery]
dht = true
dns = true
mdns = true
```

Resolution for a sync: command-line flag, then the alias's table in
`hosts.toml` if the destination names an alias, then `[client]`, then the
built-in default. Per-host `identity_file` is dropped: one client identity
per account.
`~/.ssh/id_ed25519` is no longer consulted; a user who wants it sets
`identity` or passes `--identity`. (Superseded 2026-10-29, decision R33:
it is consulted again, after the irsync key and before generating one.)

Unknown keys anywhere are an error naming the file, the table and the key.

### hosts.toml

> **Superseded in part** by [2026-10-28-config-cli-design.md](2026-10-28-config-cli-design.md): aliases now live under `[hosts.ALIAS]` tables, and `all` and `*` are not aliases.

```toml
[home]                       # the alias is the table name; was `Host home`
endpoint_id = "k51qzi5uqu5dj..."      # hex or z-base-32
relay = "https://relay.example.net"
address = ["192.168.1.10:4000"]       # was repeated `Address` lines
parallel = 64
reconnect = "1h"
dht = false
mdns = false

[laptop]                     # on a daemon's machine: a name for a client
endpoint_id = "7f3a0c..."
```

An alias is any table name; exact match only. `NAME:photos/` in a
destination means the `endpoint_id` of `[NAME]`; a destination that is not
an alias must be a raw EndpointId. The other keys matter only when the
alias is dialled as a destination. On a daemon's machine the file doubles
as a list of known clients, so grants in `exports.toml` can name them.

### exports.toml

> **Superseded in part** by [2026-10-28-config-cli-design.md](2026-10-28-config-cli-design.md): every-export grants now live in an `[all]` table and each export under `[exports.NAME]`; `all` is the only reserved name.

```toml
# Grants for every export, present and future (was modules="*:...").
rw = ["9c4d..."]
ro = []

[photos]
path = "/srv/photos"
rw = [
  "laptop",      # an alias from hosts.toml
  "k51qzi5...",  # or a raw id
]
ro = ["b2e1..."]
warm_index = false

[docs]
path = "/srv/docs"
read_only = true     # caps everyone, even ids under rw
ro = ["7f3a0c..."]
```

- Export names are the TOML table names. `rw` and `ro` are arrays whose
  entries are either aliases from `hosts.toml` or EndpointIds in either
  spelling iroh prints. An entry is tried as an alias first, then as an
  id. An id in both arrays has `rw`.
- Top-level `rw`/`ro` grant every export, with the same precedence as before:
  a per-export entry for an id wins over the top-level one.
- `read_only` on an export refuses pushes from everyone.
- A comment after an id is the builder's way of keeping the key's label;
  `allow` writes one when it has it (see below).
- `path` is absolute. `add` makes it absolute at write time.

## Identity

Two keys, generated on demand:

- **Client identity**, `identity` in `irsync.toml`, default
  `~/.config/irsync/id_ed25519`. Needed by a sync, `irsync id`, and
  `irsync show`.
- **Daemon identity**, `[daemon].identity`, default
  `~/.config/irsync/daemon_ed25519`. Needed by `irsync daemon`,
  `irsync add`, `irsync show` and `irsync daemon --show-id`.

Generation applies to whichever path is in effect: the default, the
configured one, or `--identity`. Today's daemon already does this for its
configured path and the stress harness and several tests depend on it;
the client simply gains the same rule. When the file does not exist,
create the parent directory, write an unencrypted OpenSSH Ed25519 key with mode 0600 and its `.pub`, log
one line on stderr naming the file, and print the public key line and the
EndpointId on stderr (client) or carry on (daemon, which prints its id on
stdout at startup as today). The comment in the generated key is `irsync`
for the client and `irsync-daemon` for the daemon, as now. When the file
exists but cannot be read or decrypted, that is an error with the path;
no new key is ever generated over an existing one. Passphrase handling
(`--passphrase-file`, `IRSYNC_PASSPHRASE`, prompt) is unchanged.

`irsync keygen [--out PATH]` stays as the explicit way to make a key, and
still refuses to overwrite.

> Superseded 2026-10-28: `irsync id generate [--daemon | --out PATH]`.

Because the two identities differ, a daemon and a client in the same account
on one machine have distinct EndpointIds and can talk to each other.

## Commands

> **Superseded in part** by [2026-10-28-config-cli-design.md](2026-10-28-config-cli-design.md): the management commands are now `host`, `export` and `grant`, each with `add`, `remove` and `list`; the bare `add`, `remove`, `allow` and `revoke` below no longer exist.

All are top-level subcommands of `irsync`, beside `daemon`, `keygen`, `gc`
and `index`. They take `--config PATH`, `--hosts PATH` and `--exports
PATH`. They write the hosts and exports files atomically (write to a
temporary file in the same directory, then rename) because a daemon may be
reading them at the same moment. A missing file is created; its directory
too.

### `irsync add NAME:PATH | PATH [--ro] [--warm-index]`

Adds an export table. `NAME:PATH` names it; a bare `PATH` uses the
directory's basename as the name. `PATH` is made absolute from the current
directory and must exist and be a directory. An existing export of that
name is an error (use `remove` first). `--ro` writes `read_only = true`,
`--warm-index` writes `warm_index = true`; nothing else is written.

On success it prints, on stdout, the daemon's EndpointId (generating the
daemon identity if needed), so the next step is visible:

```
added export photos (/srv/photos) to ~/.config/irsync/exports.toml
daemon id: 8d01073f44cd...
```

The first line goes to stderr, the id to stdout, so the id is scriptable.

### `irsync remove NAME`

Deletes the export's table, grants included. Unknown name: error.

### `irsync allow EXPORT|all WHO [--ro]`

`WHO` is one of an EndpointId (hex or z-base-32), a pasted
`ssh-ed25519 AAAA... comment` line, or the path of a `.pub` file. All three
resolve to an EndpointId; a non-Ed25519 key is an error.

`WHO` may also be an alias from `hosts.toml`. An alias is stored as the
alias, so renaming or re-keying a host in `hosts.toml` updates every
grant; an id, key line or `.pub` file is stored as the EndpointId.

Adds the entry to the export's `rw` array, or `ro` with `--ro`, removing
it from the other array first so a peer is in one array per export. An
entry already present under another spelling (the alias of an id given
raw, or the reverse) is replaced by the new one. When the input carried a
comment (the public key line's comment, or the `.pub` file's), it is
written as a trailing TOML comment on that array entry. `all` means the
top-level arrays. Unknown export, or an alias not in `hosts.toml`: error.
`WHO` of `all` or `*`: error, with a message saying there is no open
access.

### `irsync revoke EXPORT|all WHO|all`, `irsync revoke WHO`

With an export: removes `WHO` from both arrays of that export, matching
an entry by its literal text or by the EndpointId it resolves to; `WHO` of
`all` empties both arrays. With only a `WHO`: removes it from every export
and from the top-level arrays. Nothing to remove is an error naming what
was looked for.

### `irsync host add ALIAS ID [flags]`, `irsync host remove ALIAS`, `irsync host list`

`host add` writes `[ALIAS]` with `endpoint_id = ID` into `hosts.toml`,
creating the file if needed. One flag per key of the host
table, each written only when given:

```
--relay URL            relay = "URL"
--address ADDR         address = ["ADDR", ...]; repeatable, replaces the array
--parallel N           parallel = N
--reconnect DUR        reconnect = "DUR"
--dht yes|no           dht = true|false
--mdns yes|no          mdns = true|false
```

Values are validated as the config parser would (a URL, a `host:port`, a
duration). Running `host add` for an existing alias updates the id and the
keys named by flags and leaves the rest of the table untouched; when the
id changes, the old and new ids are printed so a mistake is visible.
`ALIAS` must not parse as an EndpointId and must not contain `:` or `/`.
`ID` is an EndpointId in either spelling.

`host remove` deletes the whole table; unknown alias: error. It does not
touch `exports.toml`; a grant that names the removed alias stops matching
anyone (see the per-connection read) and `show` flags it. `host list`
prints each alias with its id and any other keys, one alias per block; it
is the part of `show` that concerns hosts.

Aliases are accepted wherever an EndpointId is: as the host part of a
destination, as `WHO` in `allow` and `revoke`, and as entries in
`exports.toml`. An alias is looked up before the string is tried as a raw
id.

### `irsync show`

Prints, in this order: the config path and whether it exists; the client
identity's EndpointId and public key line; the hosts path and the aliases
with their ids; the exports path and whether it exists; the daemon
identity's EndpointId; then one block per export with its path, flags and
grants (each entry as written, with the resolved id beside an alias and a
marker on an alias that is not in `hosts.toml`), and the top-level
grants. Both identities are generated if missing, with the usual stderr
line. Output is for humans; `irsync id` (with `--daemon` or `--all`) and
`irsync daemon --show-id` are the scriptable forms.

### `irsync id [--daemon | -a|--all]`

> Superseded 2026-10-28: `irsync id show [--daemon|--all]`; `daemon
> --show-id` is gone.

Prints the client identity's public key line, then its EndpointId, on
stdout, generating the key if needed. This is what a user hands to a daemon
admin for `irsync allow`. `--daemon` prints the daemon identity's two lines
instead; `-a`/`--all` prints both, each pair preceded by a line reading
`client:` or `daemon:` so the output stays parseable. The two flags
conflict. `irsync daemon --show-id` stays as a synonym for `irsync id
--daemon` printing only the id, since the harness and tests use it.

### `irsync daemon`

Flags: `--config`, `--hosts`, `--exports`, `--show-id`,
`--passphrase-file`, `--listen`. `--foreground` is removed (it never did anything). Startup:

1. Load `irsync.toml`; the daemon identity, generating it if missing.
2. Read `exports.toml` once for startup tasks (below). A missing file is
   an empty set. A file that does not parse is a fatal error at startup
   naming the file and the line.
3. If no export is defined, log a warning saying so and naming the exports
   file, and keep running: `irsync add` can fix it without a restart.
4. Bind, print `endpoint id:` and `listening on:` lines as today.

### `irsync index --daemon [NAME...]`

> Superseded 2026-10-28: this is now `irsync export update NAME...|all`,
> taking `--config` and `--exports` from the `export` noun.

Reads the exports file instead of the daemon config; targets are export
names.

## The daemon's per-connection read

On every accepted connection, after the QUIC handshake has proved the
peer's EndpointId and before any stream is touched:

1. Read and parse `hosts.toml` and `exports.toml` (fresh, from disk,
   every time).
2. If either file cannot be read or parsed: log the error at `warn`, close
   the connection with the existing "unauthorized" close code, and
   continue serving. The daemon does not crash and does not fall back to
   an earlier copy. A missing file is empty, not an error. (Rationale: a
   half-written or broken file must fail closed, and the commands write
   atomically so this is rare.)
3. Resolve every grant entry: an alias becomes the `endpoint_id` of its
   table in the fresh `hosts.toml`; an entry that is neither a known alias
   nor a valid id is logged at `warn` once per connection and matches
   nobody. Compute the peer's grants: every export whose `rw` or `ro`
   resolves to the id, plus the top-level arrays. An id with no grant at
   all is rejected as today, before the Hello is read.
4. Read the Hello. Resolve the named export from the same fresh snapshot.
   Apply `read_only`, direction and path containment exactly as now.

The `Authorizer` trait changes shape: one call returns a snapshot holding
the export definitions and a grants lookup by id, so authorisation and
export lookup never see two different versions of the file. The file-backed
implementation reads and parses `hosts.toml` and `exports.toml` on each
call. The existing
in-memory implementation (`MemoryAuthorizer`, used by the test harness in
`tests/common` and the planned ephemeral daemon) holds a fixed export set
and fixed grants and returns the same snapshot every time. `Daemon::new`
takes the authorizer alone; the static module list it takes today goes
away, and the test helper changes once to match.

Performance: both files are small and a connection is rare (one per
session, plus reconnects); reading and parsing them per connection is
well under a millisecond and needs no caching.

### Indexes and background tasks

Today every module's index is opened at startup and GC and warm-up tasks are
spawned then. With live exports:

- Index handles are cached in the daemon keyed by export **path**, opened
  on first use (startup for exports present then, otherwise on the first
  connection that names the export). Two exports with the same path share
  one index, as they share one state file today.
- The hourly GC task and the one-time `warm_index` task start when an
  export's index is first opened, whether at startup or later.
- An export removed from the file keeps its open index and GC task until
  the daemon restarts; it merely cannot be connected to. This is harmless
  and keeps the design simple.
- `warm_index` is read at the moment the export is first opened; changing
  it later takes effect at restart.

## Error messages

Every error about a file carries its path. In particular:

- an unknown host in a destination: `"home" is not an alias in
  ~/.config/irsync/hosts.toml and not an EndpointId` (or `... ; no config
  file at <path>` when it is missing);
- a missing identity is never an error: it is generated at the path in
  effect, with a stderr line naming it; an identity that exists but cannot
  be read or decrypted is `identity <path>: <reason>`;
- an unknown export in a Hello: `no such export "photos"`;
- a hosts or exports file that does not parse: `<path>:<line>: <message>`.

## Documentation

- README quick start is rewritten around the six-line example at the top
  of this document; the config and exports sections show the files above
  and say they are optional.
- `docs/daemon.md`: file locations, the per-connection read, the index
  lifecycle, and the systemd and launchd examples (which keep passing
  explicit `--config`, `--hosts` and `--exports`).
- "Known limitations": the authorized_keys bullets go; add that daemon
  settings need a restart while exports do not.
- The 2026-10-17 design spec gets a one-paragraph note at the top of its
  identity and configuration sections pointing here.

## Tests

Existing tests that touch the changed surface are rewritten, not deleted:
`config.rs` (all three files, defaults, unknown keys, alias resolution),
`authz.rs` and `daemon_auth.rs` (grants from the exports file by id and by
alias, precedence of per-export over top-level, `read_only` cap, fresh read
of both files after a change, a broken file fails closed, an alias missing
from `hosts.toml` matches nobody), `cli.rs` (new subcommands and flags), `identity.rs`
(generation on demand, refusal to overwrite, error on unreadable). New
tests: each management command against a temporary config directory,
including comment preservation through `toml_edit`, atomic write, and the
`add` then `allow` then connect flow against a running daemon without a
restart. The isolated end-to-end tests and the stress harness exercise the
new files on loopback.

## Left to the builder

- Internal module layout for the new commands (one `cmd/` module or several).
- The exact wording of help text and log lines, within the rules above.
- How `toml_edit` is used to write the trailing comment on an array entry.
- Whether the exports snapshot type is shared with `irsync index --daemon`
  and `irsync show`, or those parse separately.
- The order of the `show` output within each block.

## Decisions made in the conversation

Theirs (Mike's):

- One default config file for client and daemon settings; a separate
  exports file; both optional; custom paths still accepted on every command.
- Separate client and daemon identities, both generated transparently, so
  loopback setups work.
- Management commands are top-level (`irsync add`), not under `daemon`.
- "modules" are renamed "exports"; grants live per export in the exports
  file; the `authorized_keys` file is removed.
- The daemon reads the exports file on every connection and checks the key
  and the export against that read.
- No identity derivation from ssh-agent signatures.
- `irsync host add ALIAS ID` with a flag per host setting, and
  `irsync host remove ALIAS`, for readable names; aliases accepted beside
  raw ids; the merged config file is named `irsync.toml`.
- Aliases live in a separate `hosts.toml` with the alias as the table
  name; the daemon re-reads it with `exports.toml` on every connection.
- `irsync id` prints the client identity by default, `--daemon` the
  daemon's, `-a`/`--all` both.

Mine (the designer's calls, open to change):

- TOML for the merged config, with `[client]` and `[daemon]` tables;
  exact alias names only; per-host identity dropped.
- The exports file is named `exports.toml`; `-F`, `$IRSYNC_CONFIG` and
  the `/etc/irsync` lookup dropped.
- `~/.ssh/id_ed25519` no longer a default.
- `rw`/`ro` arrays of EndpointIds, top-level arrays for all exports,
  `read_only` kept as a cap; `allow` defaults to `rw`.
- `allow` accepts an id, a public key line or a `.pub` path, and keeps the
  comment as a TOML comment.
- `add` prints the daemon id; `remove` drops grants; `revoke WHO` alone
  means everywhere; `show` and `id` exist.
- `host list` exists; `host add` on an existing alias updates only the
  id and the keys given by flags; aliases also work as `WHO` in `allow`
  and `revoke`.
- `allow` stores an alias as the alias, not its id, so re-keying a host
  updates its grants; a grant entry that resolves to nothing matches
  nobody and is flagged by `show`; a broken `hosts.toml` fails closed like
  a broken `exports.toml`; `host remove` leaves `exports.toml` alone.
- `irsync id --all` labels each pair with a `client:` or `daemon:` line;
  `irsync daemon --show-id` is kept as a synonym.
- An unparsable exports file fails closed per connection and is fatal at
  startup.
- The wire protocol field and error code are renamed too.
- `--foreground` removed.
- Indexes opened lazily by path; removed exports keep their tasks until
  restart.
- A missing identity is generated at any path in effect, not only the
  default (keeps the daemon's current behaviour and the harness working).
- The `Authorizer` trait becomes a snapshot source (exports plus grants)
  rather than staying frozen; the in-memory implementation holds a fixed
  snapshot.
