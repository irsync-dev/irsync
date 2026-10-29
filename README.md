# irsync

irsync is a one-way directory synchroniser in the mould of rsync, rebuilt for
today's networks. It runs on both ends of a transfer and connects through
[iroh](https://iroh.computer), so two machines find each other and traverse
NATs and firewalls without port forwarding, dynamic DNS or SSH tunnels. The
daemon side is reached by its iroh identity, not by an address.

It is designed to fill whatever bandwidth the link has, and to survive a bad
one. A sync session outlives any single network connection: when hotel wifi
drops, the laptop sleeps or the process is killed, the next run of the same
command resumes where it stopped, without re-walking and re-comparing the
whole tree. Many files are in flight at once, scanning runs alongside the
transfer, and a persistent index makes repeat runs over a large, mostly
unchanged tree cheap.

## Install

```sh
cargo install --path crates/irsync
```

Linux and macOS (x86_64 and aarch64) are supported, as client and as daemon.

## Quick start

The example pushes a photo library from a laptop to a home server, then pulls
it back. No file is written by hand. Each side generates its key the first
time a command needs it and says so on stderr (`generated identity <path>`).

```sh
# laptop
irsync id show                         # public key line, then the client EndpointId

# home server
irsync export add photos:/srv/photos   # prints the daemon's EndpointId
irsync export add backup:/backup
irsync host add laptop <laptop-id>     # a readable name for the client
irsync grant add photos laptop
irsync grant add backup laptop
irsync daemon

# laptop
irsync host add home <server-id>       # a readable name for the server
irsync home:                           # what this laptop may reach, with real paths
irsync -av ~/Photos home:photos/       # push: laptop to daemon, by export name
irsync -av home:photos/ ~/Photos       # pull: daemon to laptop
irsync -av ~/Documents/ home:/backup/laptop/Documents/   # push, by real path
```

Two strings are copied by hand, one in each direction: the laptop's
`EndpointId` (the second line of `irsync id show`) to the server, and the
server's (printed by `irsync export add`, `irsync daemon` and `irsync id show
--daemon`) to
the laptop. The client pins the daemon's `EndpointId`; a mismatch is a hard error.
There is no trust-on-first-use prompt. The alias is optional: the raw id can
stand in for it, as in `irsync -av ~/Photos k51qzi5...:photos/`, and `irsync
grant add photos <laptop-id>` works without `host add`. `grant add` also takes
a pasted `ssh-ed25519 AAAA...` line or the path of a `.pub` file in place of
the id; `--ro` grants pulling only.

A remote address is `HOST:PATH`, where `HOST` is an alias or an `EndpointId`
and `PATH` is resolved on the daemon's machine, in this order: an export
name followed by a path inside it (`home:photos/2024`), a path relative to
the daemon account's home directory (`home:Pictures`, `home:` is the home
itself), or an absolute path (`home:/backup/laptop`). Whatever the spelling,
the path must lie inside an exported directory the client has a grant for,
and that export's grants apply. A grant reaches everything below the
export's directory, exports nested inside it included: when a client may
use several nested exports, the most specific one decides the session's
root and access, but a grant on an outer export is not stopped by an inner
one, not even a `read_only` inner one. A path outside every export the
client may use is refused as "no access", whether or not it exists. To open everything the daemon's account can reach, export the
root: `irsync export add root:/`; `irsync export add home:~` exports its
home. rsync's trailing-slash rule holds on both sides: `Photos/` copies the
contents, `Photos` copies the directory itself. Exactly one of source and
destination is remote.

A single remote operand lists instead of copying: `irsync home:` prints the
exports this client may reach, one `NAME PATH ACCESS` line each (`rw` or
`ro`, with `read_only` applied), and `irsync home:photos/` prints the files a
pull would fetch, writing nothing.

The client and the daemon use different keys (`id_ed25519` and
`daemon_ed25519`), so both can run in one account on one machine and talk to
each other: add `--address 127.0.0.1:PORT` (or `irsync host add home ID
--address 127.0.0.1:PORT`), with the port the daemon prints, to dial it
directly.

Only file-based Ed25519 keys work. Keys live in `~/.config/irsync/`;
`irsync id generate [--daemon | --out PATH]` makes one explicitly and refuses
to overwrite unless given `-f`/`--force`.

## Files

One file holds everything: `~/.config/irsync/irsync.toml`, optional, a
missing file being the same as an empty one. The commands below edit it for
you, preserving comments; `irsync config` reads and writes any key in it;
you can also edit it by hand. While a command edits it, `irsync.toml.lock`
sits beside it and a second command is refused; delete the lock only if no
irsync is running. Every command takes `--config PATH` to use
another file (for a noun the flag goes anywhere after it: `irsync export
--config C add ...` or `irsync export add ... --config C`); `index` and `gc`
take none. Unknown keys are errors that name the file, table and key.

If you prefer to keep the host aliases or the exports apart, put a
`hosts.toml` holding the `[hosts.*]` tables, or an `exports.toml` holding
`[all]` and `[exports.*]`, beside `irsync.toml`: they are read as if they
were part of it, a table must then live in only one of the two files, and
irsync never edits them: `host`, `export`, `grant` and `config set` refuse a
table a split file owns and tell you to move it or edit that file by hand.
`show` names any split file it read.

`irsync.toml`, every table shown, with the defaults except where marked:

```toml
identity = "~/.config/irsync/id_ed25519"
state_dir = "~/.local/state/irsync"

[client]                     # defaults for every host
parallel = 32
reconnect = "1d"
dht = true
mdns = true
relay = "https://relay.example.net"   # no default: n0's relays

[daemon]
identity = "~/.config/irsync/daemon_ed25519"
idle_timeout = "30m"         # "0" disables
dormant_session_timeout = "7d"
listen = ["0.0.0.0:0", "[::]:0"]      # at most one IPv4 and one IPv6; port 0 = daemon picks
portmap = true               # UPnP/PCP/NAT-PMP; false in isolated setups
relay = "https://relay.example.net"
relay_mode = "default"       # or "disabled"

[daemon.discovery]
dht = true
dns = true
mdns = true

# Host aliases: one [hosts.ALIAS] table per remote. On a daemon's machine
# the same tables name the clients that exports can be granted to.
[hosts.home]
endpoint_id = "k51qzi5uqu5dj..."      # hex or z-base-32, as iroh prints it
relay = "https://relay.example.net"   # the rest is optional
address = ["192.168.1.10:4000"]       # dial directly: no relay or lookup
parallel = 64
reconnect = "1h"
dht = false
mdns = false

[hosts.laptop]
endpoint_id = "7f3a0c..."

# What the daemon shares and with whom. [all] holds the grants that apply
# to every export; each export is an [exports.NAME] table. Entries in rw
# and ro are aliases from [hosts.*] or EndpointIds.
[all]                        # optional: grants for every export
rw = ["9c4d..."]
ro = []

[exports.photos]
path = "/srv/photos"
rw = ["laptop", "k51qzi5..."]
ro = ["b2e1..."]
warm_index = false           # hash what is already there at start, so files irsync did not place are reused

[exports.docs]
path = "/srv/docs"
read_only = true             # caps everyone, even ids under rw
ro = ["7f3a0c..."]
```

A sync resolves each setting from the command-line flag, then the alias's
table, then `[client]`, then the built-in default. Aliases match exactly; there
are no patterns.

A key that no entry names is rejected before any protocol message is read;
there is no open access. `read_only` refuses pushes from everyone. The one
name an export cannot have is `all`, the word `grant add all WHO` uses for
every export.

## Managing hosts, exports, grants and settings

Three nouns, each with `add`, `remove` and `list` (and `export update`),
`id`, and `config` for any key:

| Command | Effect |
| --- | --- |
| `irsync host add ALIAS ID [--relay URL] [--address ADDR]... [--parallel N] [--reconnect DUR] [--dht yes\|no] [--mdns yes\|no]` | add an alias, or change the id and the keys given for an existing one |
| `irsync host remove ALIAS` | remove an alias |
| `irsync host list` | list aliases with their ids and keys |
| `irsync export add NAME:PATH \| PATH [--ro] [--warm-index]` | export a directory (a bare `PATH` is named after its last component); prints the daemon's `EndpointId` on stdout |
| `irsync export remove NAME` | stop exporting; its grants go with it |
| `irsync export list` | list exports with their paths, flags and grants (the exports section of `show`) |
| `irsync export update [-n] [-v\|-q] NAME...\|all` | hash the files of the named exports (`all`: every export) into their indexes under the daemon's `state_dir`, so files irsync did not place are reused from the next session on; `-n` counts what would be read |
| `irsync grant add EXPORT\|all WHO [--ro]` | grant read-write (or read-only) access; `all` is every export; `WHO` is an alias, an `EndpointId`, a pasted `ssh-ed25519` line or a `.pub` path |
| `irsync grant remove EXPORT\|all WHO\|all` | remove grants: `grant remove all WHO` removes `WHO` everywhere, `grant remove EXPORT all` empties an export's grants |
| `irsync grant list [EXPORT\|all \| WHO]` | every grant; one export's (or `[all]`'s); or what one peer may reach, marking grants inherited from `[all]` and read-write grants capped by a `read_only` export |
| `irsync id show [--daemon \| -a\|--all] [--passphrase-file PATH]` | print the public key line and `EndpointId` of the client (default), the daemon, or both, labelled `client:` and `daemon:` |
| `irsync id generate [--daemon \| --out PATH] [-f\|--force]` | write a new Ed25519 key pair at the client's (default), the daemon's or the given path and print its public key line and `EndpointId`; overwrites only with `--force` |
| `irsync config get KEY` | print a key by its dotted path (`daemon.listen`, `hosts.home.relay`, `exports."www.example.com".path`): a bare scalar, one array element per line, or a table's keys as `key = value` lines, all as the file holds them (a path's `~` is not expanded); an unset setting prints its default, a key with no value and no default exits 1 |
| `irsync config set KEY VALUE...` | set a key in its type (`true`/`false`, an integer, a duration like `30m`, a path, a URL, an id); an array key takes one `VALUE` per element, none for an empty array; tables are created as needed and the edited file is validated before it is written |
| `irsync config unset KEY` | remove a key, or a whole table (`config unset hosts.home`); a table left empty goes with it |
| `irsync config list` | every key the file sets, as `dotted.key = value`, in file order |
| `irsync show [--passphrase-file PATH]` | print the file paths, identities, aliases, exports and grants; the aliases are `host list` and the exports `export list`, line for line |

`config` writes what you say, the nouns know what you mean (both refuse an
export name that is empty or has a `/`, the one thing a name cannot be, since
it is the first segment of a `HOST:PATH`): `export add`
joins a relative path onto the working directory and checks the directory
exists, `grant add` takes a pasted key line or a `.pub` file, while `config
set exports.photos.path ./x` is refused (the file wants an absolute path)
and `config set exports.photos.rw ~/laptop.pub` writes that string as a
grant entry that matches nobody.

A running daemon picks up `host`, `export` and `grant` changes on the next
connection; see [docs/daemon.md](docs/daemon.md).

### Resuming

Just run the same command again. A dropped connection is redialled
automatically for up to the reconnect window (default 24h); if the process itself dies,
or the reconnect window runs out, the next identical invocation finds the
dormant session and prints `resuming session <id> from <time>`. Partly
transferred files continue from the last verified chunk. `--no-resume` forces a
fresh plan (partial files are still adopted).

### Deletions

Like rsync, nothing is deleted by default. `--delete` removes destination files
that are gone from the source. `-n` / `--dry-run` shows what would happen
without changing anything; combine it with `--delete` first. `--max-delete N`
caps the number of removals (exit code 25 when the cap stops a run), and
`--delete-excluded` also removes excluded files.

## Flags

rsync-compatible, same spelling and semantics:

| Flag | Meaning |
| --- | --- |
| `-a`, `-r`, `-l`, `-p`, `-t`, `-g`, `-o`, `-D`, `-H`, `-S` | archive, recursion, links, permissions, times, group, owner, devices and specials, hard links, sparse files |
| `--devices`, `--specials` | device nodes, FIFOs (`-D` is both, as in rsync) |
| `-v`, `-q` | more / less output |
| `-n`, `--dry-run` | change nothing |
| `-c`, `--checksum` | compare by content, not size and time: every file on both sides is read, no cached hash is trusted, and the index is refreshed from what was read |
| `-u`, `--update` | skip files newer on the destination |
| `--delete`, `--delete-before`, `--delete-during`, `--delete-after`, `--delete-excluded`, `--max-delete` | deletion modes and guards |
| `--exclude`, `--include`, `--exclude-from`, `--include-from` | filter rules, in command-line order |
| `--bwlimit` | rate limit (KiB/s, or `k`/`m`/`g` suffix) |
| `--numeric-ids` | accepted; always on |
| `--partial-dir` | where partial files live (pull only) |
| `--ignore-errors` | delete even after I/O errors |
| `--stats`, `-i` / `--itemize-changes` | reporting |
| `--progress`, `--info=progress2` | accepted; the live progress block is always on when stdout is a terminal and off with `-q` |
| `--timeout` | inactivity timeout in seconds: a connection that moves nothing for this long is dropped and the session reconnects |

On a terminal the client draws a live block at the bottom of the screen: the listing walk on both sides, a transfer bar with rsync's percent semantics, one bar per file in flight, and the deletion pass. `-q` turns it off; piped output is unchanged.

irsync-specific:

| Flag | Meaning |
| --- | --- |
| `--parallel N` | files in flight at once (default: the host's `parallel`, else `[client]`'s, else 32) |
| `--reconnect DURATION` | how long to keep redialling (`0` = give up like rsync) |
| `--relay URL` | use this relay (self-hosted) instead of n0's default |
| `--no-dht` | do not use the mainline DHT for lookup |
| `--address ADDR` | dial this `ip:port` or `host:port` directly (repeatable); turns relay, DNS, DHT, mDNS and the port mapper off |
| `--config PATH` | the config file (default: `~/.config/irsync/irsync.toml`); a `hosts.toml` beside it is read too |
| `--identity PATH` | private key to use (default: `identity` in `irsync.toml`, else `~/.config/irsync/id_ed25519`, generated if missing) |
| `--passphrase-file PATH` | passphrase for an encrypted key (first line); otherwise `IRSYNC_PASSPHRASE`, otherwise a prompt on a terminal (also for `irsync daemon`, `irsync id show` and `irsync show`) |
| `--no-resume` | start a fresh plan instead of resuming |
| `--no-rename-detection` | do not reuse content of moved files |

Subcommands: `irsync daemon [--config PATH] [--listen ADDR]... [-v|-q]`,
the management commands above, `irsync index [-n] [-v|-q] ROOT...`, which
hashes the files under a local root into its index so that a destination
irsync did not create (a prior rsync, a restore) is reused for renamed and
copied files from the first session on (`irsync export update` is the same
for the daemon's exports; `-n` counts what would be read; a session also does
this on its own for destination files of the size of a file it is about to
fetch), and `irsync gc
[-n] [-v|-q]`, which removes abandoned
partial files and dormant sessions (older than 7 days) for every local root
irsync has synced. The client also does this for each root it syncs (at most
daily, or sooner once a partial file has been left untouched for an hour), and
the daemon checks hourly but walks an export at most daily unless collectable
partial files are waiting.

## Known limitations in version one

- `--delete-during` runs as `--delete-after`.
- irsync always recurses: `-r` is accepted and implied, with or without `-a`.
- The daemon drops an authenticated connection that has been idle (no traffic
  and no local progress such as hashing or writing) for `idle_timeout`,
  30 minutes by default (the client reconnects and resumes).
- Only the delete, dry-run, itemize, `-u` and `-c` options are sent to the
  daemon. Filters (`--exclude`, `--include`, ...) apply only on the sending
  side, so a push with `--delete` plus filters also needs `--delete-excluded`,
  otherwise the daemon would delete files your filters merely hid.
- On a push the daemon applies full archive metadata (permissions, times,
  owner and group where it may, symlinks, devices, hard links) whatever the
  client's `-p -t -o -g -l -D` say.
- `--partial-dir` is not supported on a push (it would name a directory on the
  daemon); a push with it is refused.
- Only file-based Ed25519 keys are supported. Hardware keys and ssh-agent keys
  cannot be used, and keys under `~/.ssh` are not read unless `identity` or
  `--identity` names it.
- The `[daemon]` settings need a restart; exports, grants and host aliases
  take effect on the next connection.
- `--numeric-ids` is always on: owners and groups are transferred as numbers,
  never mapped by name.
- The throughput target (filling a 1 Gbit/s link) applies to direct
  connections. Relays are a fallback and are much slower; behind a symmetric NAT
  run a self-hosted relay (see [docs/daemon.md](docs/daemon.md)).
- Linux and macOS only.

More: [docs/daemon.md](docs/daemon.md) for running the daemon,
[docs/protocol.md](docs/protocol.md) for the wire protocol,
[docs/DELTA_RECEIVE.md](docs/DELTA_RECEIVE.md) for how a changed file is
received.

## Developing

The repository's git remote is not on GitHub, so the CI workflow in
`.github/workflows/ci.yml` does not run on pushes. The pre-push hook in
`.githooks/` enforces its formatting check locally instead: every commit
being pushed is checked with `cargo fmt --all -- --check` in a throwaway
worktree, and the push is refused if any file is not rustfmt-clean. Install
it once per clone:

```sh
git config core.hooksPath .githooks
```

Clippy (`cargo clippy --workspace --all-targets --all-features -- -D
warnings`) and the tests are not part of the hook; run them before pushing.

## fileset

This workspace also contains `fileset`, an independent manifest-and-verify tool
that shares no code with irsync. It records a directory tree in a manifest and
later checks the tree against it, for example to validate an irsync transfer.
See [crates/fileset/README.md](crates/fileset/README.md).

## filegen

`filegen`, also in this workspace and also sharing no code with irsync, is a
deterministic, resumable filesystem workload generator: from a seed it
applies a reproducible sequence of creates, writes, renames, links, deletes
and metadata changes to a directory tree, logging and timing every step. It
is the mutating half of a stress loop: filegen changes a tree, `fileset
update` records it, irsync carries it to another host, and `fileset check`
there confirms every change arrived.
See [crates/filegen/README.md](crates/filegen/README.md).

## irsync-stress

`irsync-stress` closes the loop: it starts an irsync daemon on loopback and
repeatedly mutates a source tree with filegen, syncs it with irsync, and
verifies the result with `fileset check`, keeping logs, both manifests and a
`run.toml` for every failure. Its first runs found two real irsync bugs
(long file names failed to transfer, FIFO timestamps never updated), both
since fixed.
See [crates/stress/README.md](crates/stress/README.md).
