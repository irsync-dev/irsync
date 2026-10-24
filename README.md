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
irsync id                              # public key line, then the client EndpointId

# home server
irsync add photos:/srv/photos          # prints the daemon's EndpointId
irsync host add laptop <laptop-id>     # a readable name for the client
irsync allow photos laptop
irsync daemon

# laptop
irsync host add home <server-id>       # a readable name for the server
irsync -av ~/Photos home:photos/       # push: laptop to daemon
irsync -av home:photos/ ~/Photos       # pull: daemon to laptop
```

Two strings are copied by hand, one in each direction: the laptop's
`EndpointId` (the second line of `irsync id`) to the server, and the server's
(printed by `irsync add`, `irsync daemon` and `irsync id --daemon`) to the
laptop. The client pins the daemon's `EndpointId`; a mismatch is a hard error.
There is no trust-on-first-use prompt. The alias is optional: the raw id can
stand in for it, as in `irsync -av ~/Photos k51qzi5...:photos/`, and `irsync
allow photos <laptop-id>` works without `host add`. `allow` also takes a
pasted `ssh-ed25519 AAAA...` line or the path of a `.pub` file in place of the
id; `--ro` grants pulling only.

Addresses are `<name>:<export>/<path>`, and rsync's trailing-slash rule holds:
`Photos/` copies the contents, `Photos` copies the directory itself. Exactly one
of source and destination is remote.

The client and the daemon use different keys (`id_ed25519` and
`daemon_ed25519`), so both can run in one account on one machine and talk to
each other: add `--address 127.0.0.1:PORT` (or `irsync host add home ID
--address 127.0.0.1:PORT`), with the port the daemon prints, to dial it
directly.

Only file-based Ed25519 keys work. Keys live in `~/.config/irsync/`;
`irsync keygen [--out PATH]` makes one explicitly and refuses to overwrite.

## Files

Everything lives in `~/.config/irsync/` and every file is optional: a missing
file is the same as an empty one. The commands below edit `hosts.toml` and
`exports.toml` for you, preserving comments; you can also edit them by hand.
The management commands (`add`, `remove`, `allow`, `revoke`, `host`, `id`,
`show`) and `daemon` accept `--config`, `--hosts` and `--exports` to use
other paths; a sync takes `--config` and `--hosts`, `index` takes `--config`
and `--exports`, and `keygen` and `gc` take none. Unknown keys are errors that name the
file, table and key.

`irsync.toml`: settings for the client and the daemon. These are the defaults
except where marked.

```toml
identity = "~/.config/irsync/id_ed25519"
state_dir = "~/.local/state/irsync"

[client]                     # defaults for every host
parallel = 32
reconnect = "24h"
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
```

`hosts.toml`: one table per remote, named by its alias. On a daemon's machine
the same file names the clients that exports can be granted to.

```toml
[home]
endpoint_id = "k51qzi5uqu5dj..."      # hex or z-base-32, as iroh prints it
relay = "https://relay.example.net"   # the rest is optional
address = ["192.168.1.10:4000"]       # dial directly: no relay or lookup
parallel = 64
reconnect = "1h"
dht = false
mdns = false

[laptop]
endpoint_id = "7f3a0c..."
```

A sync resolves each setting from the command-line flag, then the alias's
table, then `[client]`, then the built-in default. Aliases match exactly; there
are no patterns.

`exports.toml`: what the daemon shares and with whom. Entries in `rw` and `ro`
are aliases from `hosts.toml` or EndpointIds.

```toml
rw = ["9c4d..."]             # optional: grants for every export
ro = []

[photos]
path = "/srv/photos"
rw = ["laptop", "k51qzi5..."]
ro = ["b2e1..."]
warm_index = false           # hash what is already there at start, so files irsync did not place are reused

[docs]
path = "/srv/docs"
read_only = true             # caps everyone, even ids under rw
ro = ["7f3a0c..."]
```

A key that no entry names is rejected before any protocol message is read;
there is no open access. `read_only` refuses pushes from everyone.

## Managing exports and hosts

| Command | Effect |
| --- | --- |
| `irsync add NAME:PATH \| PATH [--ro] [--warm-index]` | export a directory (a bare `PATH` is named after its last component); prints the daemon's `EndpointId` on stdout |
| `irsync remove NAME` | stop exporting; its grants go with it |
| `irsync allow EXPORT\|all WHO [--ro]` | grant read-write (or read-only) access; `all` is every export; `WHO` is an alias, an `EndpointId`, a pasted `ssh-ed25519` line or a `.pub` path |
| `irsync revoke EXPORT\|all WHO\|all` | remove grants; `irsync revoke WHO` removes `WHO` everywhere |
| `irsync host add ALIAS ID [--relay URL] [--address ADDR]... [--parallel N] [--reconnect DUR] [--dht yes\|no] [--mdns yes\|no]` | add an alias, or change the id and the keys given for an existing one |
| `irsync host remove ALIAS`, `irsync host list` | remove or list aliases |
| `irsync id [--daemon \| -a\|--all]` | print the public key line and `EndpointId` of the client (default), the daemon, or both, labelled `client:` and `daemon:` |
| `irsync show` | print the file paths, identities, aliases, exports and grants |

A running daemon picks up `add`, `remove`, `allow`, `revoke` and `host`
changes on the next connection; see [docs/daemon.md](docs/daemon.md).

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
| `-a`, `-r`, `-l`, `-p`, `-t`, `-g`, `-o`, `-D`, `-H`, `-S` | archive, recursion, links, permissions, times, group, owner, devices/specials, hard links, sparse files |
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
| `--config PATH`, `--hosts PATH` | settings file and host aliases file (default: `irsync.toml` and `hosts.toml` in `~/.config/irsync/`) |
| `--identity PATH` | private key to use (default: `identity` in `irsync.toml`, else `~/.config/irsync/id_ed25519`, generated if missing) |
| `--passphrase-file PATH` | passphrase for an encrypted key (first line); otherwise `IRSYNC_PASSPHRASE`, otherwise a prompt on a terminal (also for `irsync daemon`) |
| `--no-resume` | start a fresh plan instead of resuming |
| `--no-rename-detection` | do not reuse content of moved files |

Subcommands: `irsync daemon [--config PATH] [--hosts PATH] [--exports PATH] [--show-id] [--listen ADDR]...`,
the management commands above, `irsync keygen [--out PATH]`, `irsync index [-n] ROOT...` (or `irsync index
--daemon [--exports PATH] [NAME...]`), which hashes the files under a root
into its index so that a destination irsync did not create (a prior rsync, a
restore) is reused for renamed and copied files from the first session on
(`-n` counts what would be read; a session also does this on its own for
destination files of the size of a file it is about to fetch), and `irsync gc
[-n]`, which removes abandoned
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
- Daemon settings in `irsync.toml` need a restart; exports, grants and host
  aliases do not.
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
