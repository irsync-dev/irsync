# irsync design

Date: 2026-10-17
Status: implemented (plan: `../plans/2026-10-17-irsync-v1.md`); parts since superseded as noted below

## Intent

irsync is a one-way directory synchronisation tool in the mould of rsync,
rebuilt for today's networks. It runs on both ends of a transfer, connects
through [iroh](https://iroh.computer) so that two machines find each other
and traverse NATs and firewalls without port forwarding or SSH tunnels, and
it is designed to fill whatever bandwidth the link has, whether that is a
hotel wifi that drops every few minutes or a fast line where a single rsync
stream leaves most of the capacity unused.

### Why

The author's driving case: mirroring a large photo library from a laptop
abroad onto a home server. Today that is `rsync -a ... over ssh`, and it
fails in two compounding ways:

1. **The connection is the session.** rsync runs over one TCP stream. When
   hotel wifi drops, the stream dies and the whole rsync session dies with
   it.
2. **Every restart pays the full rescan tax.** Both sides re-walk a large
   tree, exchange the full listing, and discover that only ~5% of files
   changed. On a slow link the scan and listing exchange alone can take
   longer than the wifi stays up.

Beyond this case irsync must synchronise arbitrary directories, one way,
over bad links and over fast ones. It is not a photo tool.

## Goals

- **A sync session outlives any single network connection.** Wifi drops,
  IP changes, laptop lid closed and reopened in another cafe: the session
  pauses and resumes. The user runs one command and never babysits it.
- **No port forwarding, no SSH, no dynamic DNS.** The home side is reached
  by its iroh identity through iroh's address lookup (DNS/pkarr, the
  mainline DHT, mDNS on a LAN) and iroh's relays for hole punching.
- **Saturate the link.** Many files in flight in parallel, scanning
  concurrent with transfer, large flow-control windows, a modern
  congestion controller. Target for version one: fill a 1 Gbit/s link
  between two ordinary machines; be no slower than rsync on anything.
- **Never rescan what hasn't changed.** A persistent per-side index caches
  file chunk hashes so unchanged files are never re-read, and an
  interrupted session resumes from its persisted plan rather than
  recomputing it.
- **Send only what changed.** Content-defined chunking plus chunk hashes
  gives rsync-style in-file delta transfer, and the same mechanism gives
  chunk-granular resume of interrupted files.
- **Both directions, like rsync.** The remote side can be the source or the
  destination.
- **Full POSIX metadata in version one.** mtime, mode, uid/gid, symlinks,
  hard links, device and special files, sparse files.
- **Deletions only on request**, with rsync's flag spelling.
- **An rsync user feels at home.** Flags match rsync wherever the semantics
  match. Source/destination addressing follows the `host:path` shape.
- **Reuse SSH keys.** The user's existing Ed25519 SSH keys identify and
  authorise both ends; nothing new to generate or distribute.
- **A client config in ssh_config style**, with several named destinations
  and pattern matching.
- **Runs on Linux and macOS**, as both client and daemon. (The author's
  laptop is a Mac, the home server is Linux. Author's inference from the
  APFS mention; see open questions.)

## Non-goals (for version one)

- Spawning the remote side over SSH, mosh-style. Planned for version two;
  version one must not preclude it.
- Extended attributes and filesystem-specific translation between APFS,
  ext4 and ZFS. Planned for version two; the metadata representation must
  leave room for it.
- A daemon-maintained live index (inotify/FSEvents). Later.
- Daemon-to-daemon transfers where neither end is the invoking client.
- rsync's full `--filter` rule language. Version one has
  `--exclude`/`--include`/`--exclude-from`/`--include-from` with rsync
  pattern semantics; the full merge-file rule language is later.
- Compression (`-z`). Photos don't compress; add zstd later if wanted.
- Windows. Nothing should actively prevent it, but it is not tested or
  supported.
- Multi-Gbit/s throughput from one userspace QUIC connection.

## Anti-goals

Things that would make irsync a failure even if it technically works:

- **Bidirectional sync, conflict resolution, or any "smart" merging.** It is
  a mirror. One side is the truth.
- **Silently deleting or truncating anything at the destination.** No
  deletion without an explicit flag. Never leave a half-written file under
  its final name.
- **Inventing a new credential or account.** No signup, no cloud account,
  no irsync-specific key the user must manage, unless they choose to.
- **Leaking the home server's IP to a public DHT** when it doesn't have
  to. iroh's default of publishing relay-only addresses stands.
- **Flags that look like rsync's but mean something else.** If the
  semantics differ, use a different name.
- **Being slower than rsync** in any common case, including the
  "almost nothing changed" case.

## Constraints

- **Language: Rust**, stable toolchain. iroh is a Rust library and a single
  static binary on both ends is the goal.
- **Networking: iroh 1.3.x** (`iroh` crate), with `iroh-mainline-address-lookup`
  for the DHT and `iroh-mdns-address-lookup` for LAN discovery. iroh 1.x is
  semver-stable; pin to `1.x`.
- **Async runtime: tokio**, which iroh requires.
- **Keys: `ssh-key` crate** (0.6.x) for OpenSSH private/public key and
  authorized_keys parsing. Only Ed25519 keys can be used as identities,
  because the iroh identity *is* an Ed25519 key.
- **Chunking: `fastcdc` crate** (5.x, `v2020` module). **Hashing: `blake3`.**
- **Wire format: Protocol Buffers (proto3).** Every message on every
  stream is a protobuf message, so an irsync implementation in another
  language can interoperate. The `.proto` files live in the repository
  under `proto/` and are the normative protocol specification; the Rust
  types are generated from them at build time (`prost` + `prost-build`,
  author's suggestion; `protoc` must not be required at runtime). File
  data on data streams is carried as raw bytes inside length-prefixed
  protobuf frames, not as a bare byte stream, so the framing is also
  language-neutral. Protocol evolution follows protobuf rules: new fields
  are optional, field numbers are never reused, and `Hello` carries a
  protocol version for anything that can't be expressed as an added
  field.
- Both binaries (client and daemon) ship in one executable, `irsync`, with
  the daemon as a subcommand, as rsync does with `--daemon`.
- **The protocol is a public interface.** Nothing Rust-specific (serde
  layouts, enum discriminants, hash of a Rust struct) may leak into what
  goes on the wire. Chunking parameters, hash algorithm and the FastCDC
  variant are part of the protocol specification so another
  implementation produces identical chunk boundaries and hashes.

## Architecture

### Roles

Four roles, two axes:

- **client / daemon**: who dials whom. The client is the invoking
  command; the daemon is the long-running iroh endpoint on the other
  machine.
- **sender / receiver**: who has the source tree and who has the
  destination. Either the client or the daemon can be the sender.

`irsync -a ~/Photos home:photos/` makes the client the sender.
`irsync -a home:photos/ ~/Photos` makes the daemon the sender. The wire
protocol is symmetric with respect to sender/receiver; only connection
setup and authorisation know about client/daemon. This keeps the door open
for daemon-to-daemon later.

### Identity and authentication

> The [2026-10-23 zero-config design](2026-10-23-zero-config-cli-design.md) supersedes this section: keys are generated on demand, client and daemon use separate identities, and grants live in `exports.toml` instead of an `authorized_keys` file.

**iroh authenticates both ends by node key.** Every iroh connection is
mutually authenticated: after the handshake each side knows the other's
`EndpointId` (an Ed25519 public key), cryptographically. irsync needs no
authentication protocol of its own.

**Identities are Ed25519 keys, loaded from OpenSSH key files.** iroh's
`SecretKey::from_bytes(&[u8; 32])` takes a raw Ed25519 seed; the `ssh-key`
crate exposes exactly that from an OpenSSH Ed25519 private key. So:

- The **client** uses the user's SSH key (default `~/.ssh/id_ed25519`,
  overridable per destination with `IdentityFile`). Its iroh `EndpointId`
  equals its SSH public key.
- The **daemon** uses a key named in its config. Any OpenSSH Ed25519
  private key works. If none is configured, the daemon generates one on
  first start and stores it in its state directory in OpenSSH format, so
  the public key can be printed, copied and pinned like a host key. (Using
  the system SSH host key `/etc/ssh/ssh_host_ed25519_key` is possible when
  the daemon runs as root or has read access; it is one option, not the
  default.)
- **Passphrase-protected keys** are supported for the OpenSSH default
  cipher (bcrypt-pbkdf + aes256-ctr), with an interactive prompt or
  `IRSYNC_PASSPHRASE`/`--passphrase-file`. **Agent-held and hardware keys
  cannot be used**: irsync needs the private seed, not signatures. Users
  with only hardware SSH keys generate an irsync-specific Ed25519 key with
  `ssh-keygen -t ed25519` and point `IdentityFile` at it.

**Authorisation on the daemon is authorized_keys-style.** The daemon
config names an authorized-keys file (default alongside the config). Each
line is a standard OpenSSH public key line; the options field carries
irsync grants, e.g.

```
modules="photos:rw,docs:ro" ssh-ed25519 AAAAC3... mike@laptop
modules="*:ro" ssh-ed25519 AAAAC3... backup-reader
```

The daemon rejects connections from keys not in the file before any
protocol message is read. The `ssh-key` crate parses this file format
natively, so users can edit it with the tools and habits they already
have.

**The client pins the daemon's identity**, like known_hosts: the client
config names the daemon's `EndpointId` per destination. A mismatch is a
hard error. There is no "trust on first use" prompt in version one; the
user copies the id once, as they do with an SSH host key fingerprint.

### Addressing and configuration

> The [2026-10-23 zero-config design](2026-10-23-zero-config-cli-design.md) supersedes this section: modules are now exports, and configuration is `irsync.toml`, `hosts.toml` and `exports.toml` instead of the ssh_config-style client file and `irsyncd.toml`.

**Daemon modules.** As in rsyncd, the daemon exports named *modules*, each
rooted at a filesystem path, with optional per-module settings (read-only,
uid/gid mapping, max connections). Clients address paths as
`<destination>:<module>/<path>`. Nothing outside a module root is
reachable. Symlinks inside a module that point outside it are transferred
as symlinks, never followed, so they cannot be used to escape.

**Client config** lives at `~/.config/irsync/config` (XDG; `$IRSYNC_CONFIG`
or `-F` overrides), in ssh_config syntax and semantics:

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

Semantics copy ssh_config: `Host` blocks match with `*`, `?` and `!`
negation; for each option the **first** matching value wins; later blocks
only fill options not yet set. A destination name in `irsync src dst:path`
is resolved through this file. A raw `EndpointId` may also be used in
place of a name (`k51qzi5...:photos/`) for one-off transfers.

**Daemon config** lives at `/etc/irsync/irsyncd.toml` or
`~/.config/irsync/irsyncd.toml` (or `--config`), in TOML. It names the
identity key, the authorized-keys file, the state directory, relay and
discovery settings, and the modules:

```toml
identity = "~/.config/irsync/daemon_ed25519"      # OpenSSH format
authorized_keys = "~/.config/irsync/authorized_keys"
state_dir = "~/.local/state/irsync"

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

The daemon prints its `EndpointId` on start and on `irsync daemon --show-id`.

### Discovery and connectivity

- The daemon publishes its address record via n0's pkarr relay/DNS and
  via the mainline DHT (`DhtAddressLookup`), publishing **relay-only**
  addresses by default so its home IP is not in a public DHT. Direct
  addresses are exchanged during hole punching, not published. mDNS is
  on by default for LAN discovery.
- The client resolves with the same set: DNS/pkarr, DHT, mDNS. The first
  answer wins; iroh queries them concurrently.
- Relays default to n0's public relays. Both configs accept a
  self-hosted relay URL. `RelayMode::Disabled` is an option for LAN-only
  setups.
- **The throughput target applies to direct (hole-punched) paths.** A
  relayed path is a connectivity fallback at whatever rate the relay
  gives; n0's public relays are not sized for bulk transfer and may
  rate-limit. Users who routinely sit behind symmetric NAT on both ends
  should run their own relay, and the docs say so. Progress output shows
  `direct` or `relay` so the user knows which they are getting.
- ALPN: `irsync/1`. The protocol version negotiation inside the stream is
  independent of ALPN, so wire-compatible protocol additions don't need a
  new ALPN.

### Session over connections

This is the central mechanism.

A **session** is one logical sync job: a (source, destination, daemon
identity, semantics-affecting flags) tuple. It has a random 128-bit
**session id**, a **plan** (what to transfer), and **progress** (what has
arrived and been verified). A **connection** is one iroh QUIC connection.
A session uses one connection at a time, but many over its life.

**A session survives the client process dying.** The client persists
session state in its own state directory keyed by that tuple. A new
invocation with the same tuple finds the dormant session and resumes it
automatically (printing `resuming session <id8> from <time>`), so Ctrl-C,
a crash, a laptop reboot or an expired `Reconnect` window all cost
nothing but the restart itself. `--no-resume` (irsync-specific) forces a
fresh plan; partial files are still adopted (see write path). A resumed
`Hello` must come from the same client `EndpointId` that started the
session, or the daemon refuses it.

- The client opens a connection, opens a **control stream** (bidirectional),
  and sends `Hello{proto_version, session_id: None, ...}` to start a new
  session, or `Hello{session_id: Some(id)}` to resume one.
- Both sides persist session state (plan and progress) in their state
  directory keyed by session id. The daemon keeps session state for
  dormant sessions for a bounded time (default 7 days) and garbage
  collects after.
- When the connection dies (iroh reports `closed()`), the client
  **re-dials with exponential backoff** (1s to 60s, jittered) until
  `Reconnect` (default 24h, `--reconnect` flag, `0` = give up like rsync)
  elapses. On reconnect it sends the same session id and both sides
  resume from persisted progress. Nothing in flight is lost: every
  transferred chunk is acknowledged only after it is written, and the
  receiver's record of received chunks is what drives resumption.
- QUIC transport is configured for resilience: `max_idle_timeout` 60s
  (versus iroh's 30s default) so short drops don't tear the connection
  down, `keep_alive_interval` 5s. iroh's multipath handles path changes
  underneath; irsync only sees a connection close when all paths are
  gone for the idle timeout.

### Scanning and the index

Each side keeps a **persistent index** in its state directory, one per
module root (daemon) or per source/destination root (client), keyed by
canonical root path. It is a local database (SQLite via `rusqlite` with
the bundled feature; author's call, the builder may choose `redb` if it
prefers a pure-Rust store, but the index must be crash-safe). It stores,
per file:

- relative path, type, size, mtime (ns), ctime (ns), inode, mode,
  uid, gid, link target
- whole-file BLAKE3 hash
- the chunk list: (offset, length, BLAKE3 hash) per chunk

**Quick check.** A file whose size, mtime, ctime and inode match the index
is trusted and its hashes are reused without reading the file. This is
rsync's quick check plus ctime and inode, which catches `touch -d` tricks
and replaced files. `--checksum`/`-c` forces rehashing everything.

**The walk.** A full stat walk of the tree is still required per session
(directory mtimes do not change when file contents change, so they can't
be used to skip subtrees). It is done in parallel across directories
(several worker threads, `jwalk`-style), and it **streams**: entries flow
to the planner as they are found, the planner emits transfer work as soon
as it has both sides' view of a path, and the first bytes move within
seconds of start on trees of hundreds of thousands of files. Hashing of
files that fail the quick check happens on a bounded worker pool, also
concurrent with transfer.

**Hashes are lazy, never a precondition.** The listing carries metadata
and, *when the index already has it*, the whole-file hash. Nothing is read
just to produce a listing. On first contact with an existing multi-TB
mirror irsync does what rsync does: stat both trees, quick-check them, and
read nothing that matches. Hashes accumulate in the index as a side
effect of normal work (every file sent or received is hashed in passing),
so later runs have more to work with. Consequences the builder must
design for: the planner is two-phase (a quick-check phase from listings,
then per-file negotiation for files that fail it), the listing schema has
an optional hash, and delta/local-copy are only available where hashes
exist or can be computed on demand for that one file.

**Listing exchange.** Both sides stream their listing over the control
stream, sorted by path so the planner can merge-join them with bounded
memory. Chunk lists travel only for files that need delta transfer.

**Resume reuses the plan.** A resumed session does not re-walk. It
re-validates the plan lazily: before transferring a file it re-stats it
on the sender; if it changed since planning, that file is re-planned. The
receiver re-stats its temp file to confirm persisted progress.

**Later (version two):** a daemon-side watcher (inotify/FSEvents) keeps the
index hot so the walk itself can be skipped for recently seen trees.

### Transfer

**Chunking.** Files are chunked with FastCDC (2020 variant, normalisation
level 1), parameters min 64 KiB / average 256 KiB / max 1 MiB. These are
constants in the protocol: both sides must agree, and changing them
invalidates cached chunk lists, so they are versioned with the index
schema. Each chunk is hashed with BLAKE3; the whole file is hashed with
BLAKE3 too (a single pass computes both).

**Per-file decision**, made by the planner from the two listings:

- destination missing → **whole-file transfer** (the fast path; all
  chunks).
- present, quick-check equal (size, mtime; with `-c`, hash) → skip. No
  bytes read.
- present, quick-check differs → the sender chunks and hashes the file
  (one pass, cached in its index). If the whole-file hash equals the
  receiver's (which the receiver computes on demand if its index lacks
  it) → metadata-only update. Otherwise **delta**: the sender sends the
  new chunk list; the receiver answers with the chunk hashes it lacks,
  consulting the old file's chunk list (computed on demand, cached) and
  its index's hash→(path, offset) table; the sender sends only those.
- destination missing → the sender hashes the file ahead of sending it
  (bounded lookahead, pipelined, concurrent with transfer of earlier
  files) and sends the hash first. If the receiver's index knows a file
  with that whole-file hash under its root → **local copy** instead of
  transfer. This catches moved and renamed files, which photo libraries
  do a lot of, and on a slow link saving one photo pays for a lot of
  local reads. Otherwise → **whole-file transfer**. The cost is one extra
  local read of each new file on the sender, mostly served from page
  cache; author's call that this is worth it. `--no-rename-detection`
  (irsync-specific) skips the lookahead hash for fast-LAN cases, and the
  whole-file hash is then computed while streaming for verification only.
  With `--delete` the stale copy of a moved file goes in the deletion
  pass as usual.

**Streams.** The control stream carries listings, plans, chunk
negotiations and acknowledgements. File data goes over **unidirectional
data streams**, one per file in flight; a file larger than a threshold
(64 MiB, author's call) is split into ranges, each on its own stream, so
one huge file does not serialise the pipeline. `--parallel N`
(`Parallel` in config, default 32) bounds files/ranges in flight. Note
that QUIC streams on one connection share one congestion controller, so
parallelism here buys pipelining, disk concurrency and freedom from
head-of-line blocking, not more bandwidth per se; bandwidth comes from
the congestion controller and windows below.

**Transport tuning.** Congestion control: BBR (quinn's implementation),
configurable to Cubic. Receive and send windows sized for high
bandwidth-delay products (connection receive window 64 MiB,
per-stream 8 MiB; author's starting values, to be tuned by measurement).
`--bwlimit` rate-limits as rsync does.

**Write path on the receiver.** A file is written to a temp file in the
same directory (`.irsync.<name>.part`, no session id in the name), chunks
written at their offsets as they arrive (so ranges can arrive out of
order), fsynced, then the whole-file BLAKE3 is verified against the
sender's, metadata is applied, and the temp file is renamed over the
destination. Only a verified file ever appears under its final name.

**Partial files belong to the path, not the session.** The index records,
per destination path, which chunk ranges of the `.part` file have been
written and their hashes. Any session, resumed or fresh, that is about to
write that path first **adopts** the existing partial: it re-hashes the
recorded ranges, keeps those that verify, and asks the sender only for
the rest. So chunk-granular resume works across invocations, not just
across reconnects. A `.part` file with no index record is re-chunked and
its chunks offered as local material, then overwritten.

**Garbage collection of irsync's own files.** `.part` files and their
index records whose destination path is no longer in any live or dormant
session are removed by the daemon's GC after the dormant timeout (default
7 days), and by the client on its next run against that root. These are
irsync's own clearly marked temp files, not user data, so this does not
touch the no-silent-deletion anti-goal; `irsync gc --dry-run` lists them.
`--partial-dir` is accepted for rsync compatibility and relocates the
`.part` files; there is no mode that discards partial progress.

**Acknowledgement.** The receiver acknowledges each chunk range after it is
written to the temp file (not after fsync), and acknowledges the file after
rename. The sender treats a file as done only at file-ack. Session
progress is persisted at file granularity plus a received-chunk bitmap
for in-flight files.

### Metadata (version one: POSIX)

Preserved with `-a` (= `-rlptgoD`) exactly as rsync defines those letters:
recursive, symlinks as symlinks, permissions, mtime (nanosecond
precision), group, owner (when the receiver is privileged;
`--numeric-ids` default **on**, name mapping is opt-in since the two
machines rarely share a user database), devices and specials. `-H` for
hard links (detected by inode on the sender, recreated as links on the
receiver). Sparse files: detected via `SEEK_HOLE` where available and
recreated sparse; `-S` forces hole detection on zero runs. atime is not
preserved. Directory mtimes are set after their contents, as rsync does.

The on-wire and in-index metadata record is an extensible struct with
optional fields, so version two can add xattrs, ACLs, flags and
filesystem-specific attributes without a protocol break.

### Deletions

Off by default. `--delete` removes destination files not present on the
source, after the transfer pass by default (`--delete-after` semantics),
with `--delete-before`, `--delete-during`, `--delete-excluded` and
`--max-delete=N` matching rsync. `--dry-run`/`-n` lists what would be
deleted. A deletion pass never runs if the sender's listing was
incomplete (walk error, permission denied on a subtree) unless
`--ignore-errors` is given, mirroring rsync's safety behaviour.

### Command line

> Superseded: see `2026-10-23-zero-config-cli-design.md` and
> `2026-10-28-config-cli-design.md`. `--show-id` and `--foreground` are
> gone, `keygen` is `irsync id generate`, and the configuration commands
> are the `host`, `export`, `grant` and `id` nouns.

```
irsync [OPTIONS] SRC... DEST
irsync daemon [--config PATH] [--show-id] [--foreground]
irsync keygen [--out PATH]            # convenience wrapper: ssh-keygen -t ed25519
```

Exactly one of SRC/DEST may be remote in version one (`name:module/path`
or `<endpoint-id>:module/path`). rsync's trailing-slash rule on SRC is
kept: `src/` copies contents, `src` copies the directory itself.

Flags with rsync semantics, same spelling: `-a -r -l -p -t -g -o -D -H -S
-v -q -n/--dry-run -c/--checksum -u/--update --delete --delete-before
--delete-during --delete-after --delete-excluded --max-delete --exclude
--include --exclude-from --include-from --bwlimit --numeric-ids
--partial-dir --ignore-errors --stats --progress --info=progress2
-i/--itemize-changes --timeout`.

irsync-specific flags, deliberately not rsync-looking: `--parallel N`,
`--reconnect DURATION`, `--relay URL`, `--no-dht`, `-F CONFIG`,
`--identity PATH`.

Progress output follows rsync's `--info=progress2` aggregate style by
default when stdout is a tty: bytes, percent, rate, elapsed, files
done/total, plus a one-line connection state (`direct`, `relay`,
`reconnecting (attempt 3)`). `-v` adds per-file lines as rsync does.

### Platform

Linux and macOS, x86_64 and aarch64, both as client and daemon. The daemon
should run cleanly under systemd (Linux) and launchd (macOS) in the
foreground with logs to stderr; no self-daemonising. Logging via
`tracing`, `RUST_LOG` honoured, `-v`/`-q` mapped to levels.

## Version two (recorded so version one doesn't preclude it)

- **SSH spawn mode** (mosh-style): `irsync src user@host:/path` with an
  ordinary SSH host. irsync SSHes in, runs `irsync daemon --ephemeral`,
  which prints its `EndpointId` and a one-time session token on stdout;
  the client reads them over SSH, then connects via iroh and SSH is
  closed. The ephemeral daemon exports the single requested path as a
  module and exits when the session completes or the dormant timeout
  passes. **No one-time token is needed:** the client passes its own
  Ed25519 public key as an argument when spawning the ephemeral daemon
  over SSH, and the daemon's in-memory authorized list contains exactly
  that key. Authorisation stays "reject unknown `EndpointId` before any
  protocol message is read", identical to version one. Implication for
  version one: the daemon must be startable with a complete config given
  on the command line (identity, one module, one authorized key), and the
  authorizer is a small trait with file-backed and in-memory
  implementations.
- **Extended attributes and filesystem-specific metadata**: xattrs, ACLs,
  BSD flags, APFS/ext4/ZFS translation. Implication: optional fields in
  the metadata record, already planned.
- **Watcher-maintained daemon index** to skip the walk.
- **Daemon-to-daemon** transfers.
- **Full rsync `--filter` rules**, `-z` with zstd.

## Left to the builder

- Crate layout (single crate with modules vs. workspace). A workspace with
  `irsync-proto` (wire types, chunking, no I/O) and `irsync` (everything
  else) is a reasonable default.
- Framing details: length prefix width (a varint or fixed u32) and
  maximum frame size for control and data streams, within the protobuf
  constraint above. Chunk payload size per data frame.
- Organisation of the `.proto` files (one package `irsync.v1`, split by
  concern) and how generated code is exposed from the `irsync-proto`
  crate.
- Exact index schema and migration strategy; index location naming.
- Walker and hasher thread pool sizing.
- The protocol state machine details and error taxonomy, provided the
  session/connection split above holds.
- Testing approach. The author expects at minimum: in-process two-endpoint
  integration tests over real iroh connections; fault-injection tests that
  kill the connection mid-transfer and assert byte-exact resume;
  property tests for chunking/delta reconstruction; a comparison test that
  runs rsync and irsync on the same tree and diffs the results including
  metadata.
- Progress/TUI implementation details.

## Decisions made by the author of this document, not by the user

Listed so they can be checked while reading:

- Hashes are lazy (rsync-style quick check first); the first run against
  an existing mirror reads nothing that matches.
- New files are hashed on the sender ahead of sending (one extra local
  read) to enable rename/move detection; `--no-rename-detection` opts out.
- Sessions survive process death and are resumed automatically by
  (source, destination, daemon, flags) tuple.
- Partial files are keyed by path and adopted by any session; irsync
  garbage-collects its own `.part` files after the dormant timeout.
- No one-time tokens, ever: version two's SSH spawn passes the client's
  public key over SSH instead.
- The 1 Gbit/s target is for direct paths; relays are fallback.
- Platform: Linux and macOS, both roles (inferred from the APFS mention).
- Protobuf wire format was the user's requirement; `prost` as the Rust
  implementation and length-prefixed framing of data chunks are the
  author's calls.
- Module-based addressing (`dest:module/path`), TOML daemon config, XDG
  config locations, `--numeric-ids` on by default, BBR congestion control,
  FastCDC 64 KiB / 256 KiB / 1 MiB, 60 s idle timeout, 32 parallel
  streams, 64 MiB range-split threshold, 7-day dormant session timeout.
