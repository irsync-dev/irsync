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

The examples push a photo library from a laptop to a home server, then pull it
back.

### 1. Generate a key on both machines

```sh
irsync keygen
```

This writes `~/.config/irsync/id_ed25519` (and `.pub`), refuses to overwrite an
existing key, and prints two lines: the OpenSSH public key line, to paste into a
daemon's `authorized_keys`, and the `EndpointId`, to pin in a client config.
Only file-based Ed25519 keys work; an existing `~/.ssh/id_ed25519` is fine too.

### 2. Configure the daemon (home server)

`~/.config/irsync/irsyncd.toml` (or `/etc/irsync/irsyncd.toml`, which wins if both exist, or `--config`):

```toml
identity = "~/.config/irsync/daemon_ed25519"      # OpenSSH format
authorized_keys = "~/.config/irsync/authorized_keys"
state_dir = "~/.local/state/irsync"
idle_timeout = "30m"  # drop a connection idle this long (no traffic, no local work); "0" disables

[discovery]
dht = true        # publish to mainline DHT (relay-only addresses)
dns = true        # n0 pkarr/DNS
mdns = true       # LAN

[modules.photos]
path = "/srv/photos"
read_only = false

[modules.docs]
path = "/srv/docs"
read_only = true
```

If the identity file does not exist the daemon generates it on first start.

### 3. Authorise the laptop

Append the laptop's public key line (printed by `irsync keygen`) to the daemon's
`authorized_keys`, with the modules it may use:

```
modules="photos:rw,docs:ro" ssh-ed25519 AAAAC3... mike@laptop
```

`photos:rw` allows both directions, `docs:ro` only pulling from the daemon, and
`*` matches every module (`modules="*:ro"`). Keys not in the file are rejected
before any protocol message is read.

### 4. Start the daemon and read its id

```sh
irsync daemon
irsync daemon --show-id        # prints only the EndpointId
```

The daemon runs in the foreground and logs to stderr (see
[docs/daemon.md](docs/daemon.md) for systemd and launchd).

### 5. Configure the client (laptop)

`~/.config/irsync/config`, in ssh_config syntax (`-F` or `$IRSYNC_CONFIG`
override the path):

```
Host home
    EndpointId  k51qzi5uqu5dj...        # z-base-32 or hex, as iroh prints it
    IdentityFile ~/.ssh/id_ed25519
    Relay       https://relay.example.net   # optional: self-hosted relay

Host nas-*
    IdentityFile ~/.ssh/id_ed25519_nas

Host *
    Parallel    32
    Reconnect   24h
```

The client pins the daemon's `EndpointId`; a mismatch is a hard error. There is
no trust-on-first-use prompt. For a one-off transfer the raw id can stand in for
the name: `irsync -av ~/Photos k51qzi5...:photos/`.

### 6. First sync, and the other direction

```sh
irsync -av ~/Photos home:photos/       # push: laptop to daemon
irsync -av home:photos/ ~/Photos       # pull: daemon to laptop
```

Addresses are `<name>:<module>/<path>`, and rsync's trailing-slash rule holds:
`Photos/` copies the contents, `Photos` copies the directory itself. Exactly one
of source and destination is remote.

### Resuming

Just run the same command again. A dropped connection is redialled
automatically for up to `Reconnect` (default 24h); if the process itself dies,
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
| `-c`, `--checksum` | compare by content, not size and time |
| `-u`, `--update` | skip files newer on the destination |
| `--delete`, `--delete-before`, `--delete-during`, `--delete-after`, `--delete-excluded`, `--max-delete` | deletion modes and guards |
| `--exclude`, `--include`, `--exclude-from`, `--include-from` | filter rules, in command-line order |
| `--bwlimit` | rate limit (KiB/s, or `k`/`m`/`g` suffix) |
| `--numeric-ids` | accepted; always on |
| `--partial-dir` | where partial files live (pull only) |
| `--ignore-errors` | delete even after I/O errors |
| `--stats`, `--progress`, `--info=progress2`, `-i` / `--itemize-changes` | reporting |
| `--timeout` | inactivity timeout in seconds: a connection that moves nothing for this long is dropped and the session reconnects |

irsync-specific:

| Flag | Meaning |
| --- | --- |
| `--parallel N` | files in flight at once (default: the host's `Parallel`, else 32) |
| `--reconnect DURATION` | how long to keep redialling (`0` = give up like rsync) |
| `--relay URL` | use this relay (self-hosted) instead of n0's default |
| `--no-dht` | do not use the mainline DHT for lookup |
| `-F FILE` | client config file |
| `--identity PATH` | private key to use |
| `--passphrase-file PATH` | passphrase for an encrypted key (first line); otherwise `IRSYNC_PASSPHRASE`, otherwise a prompt on a terminal (also for `irsync daemon`) |
| `--no-resume` | start a fresh plan instead of resuming |
| `--no-rename-detection` | do not reuse content of moved files |

Subcommands: `irsync daemon [--config PATH] [--show-id]`,
`irsync keygen [--out PATH]`, and `irsync gc [-n]`, which removes abandoned
partial files and dormant sessions (older than 7 days) for every local root
irsync has synced. The client also does this for each root it syncs (at most
daily, or sooner once a partial file has been left untouched for an hour), and
the daemon checks hourly but walks a module at most daily unless collectable
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
  cannot be used; generate an irsync-specific key with `irsync keygen` or
  `ssh-keygen -t ed25519`.
- In `authorized_keys`, only the `modules=` option is understood; other options
  are ignored with a warning, and escaped quotes inside option values are not
  supported.
- `--numeric-ids` is always on: owners and groups are transferred as numbers,
  never mapped by name.
- The throughput target (filling a 1 Gbit/s link) applies to direct
  connections. Relays are a fallback and are much slower; behind a symmetric NAT
  run a self-hosted relay (see [docs/daemon.md](docs/daemon.md)).
- Linux and macOS only.

More: [docs/daemon.md](docs/daemon.md) for running the daemon,
[docs/protocol.md](docs/protocol.md) for the wire protocol.
