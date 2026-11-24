# Configuration

One file holds everything: `~/.config/irsync/irsync.toml`. It is optional; a
missing file is the same as an empty one, and every key has a default except
where noted. The [management commands](management.md) edit it for you and keep
its comments and the order of its servers, clients and exports (they rewrite the rest in
one standard form); `irsync config` reads and writes any key; you can also edit it by
hand.

Every command takes `--config PATH` to use another file. Unknown keys and
tables are errors that name the file, the line and the key.

A `hosts.toml` or `exports.toml` beside the config file is ignored, with a
warning. Earlier versions read them as part of the config. Move their tables
into `irsync.toml`.

## Values

- A **duration** is `N[s|m|h|d]`, such as `90s`, `30m`, `1h` or `7d`; a
  number without a unit, as a string or as a TOML integer, is seconds.
- A **path** may start with `~`, which stands for the home directory.
- An **`EndpointId`** is hex or z-base-32, as iroh prints it.
- An **address** is `ip:port`, `[ipv6]:port` or `host:port`.

## The whole file

Every table, with the defaults except where marked:

```toml
config_version = 2              # written by irsync; always first
unsafe_config_allowed = false   # true: skip the permission checks
identity = "~/.config/irsync/id_ed25519"
state_dir = "~/.local/state/irsync"

[client]                     # defaults for every server
parallel = 128
reconnect = "1d"
dial_retries = 4
dht = true
ssh_install = true           # false: copy nothing to the server
mdns = true
relay = "https://relay.example.net"   # no default: n0's relays
relay_mode = "default"       # or "disabled": no relay
congestion = "bbr"           # or "cubic", for pushes
pq_key_exchange = "prefer"   # or "require"

[daemon]
identity = "~/.config/irsync/daemon_ed25519"
idle_timeout = "30m"         # "0" disables
dormant_session_timeout = "7d"
max_dormant_sessions_per_key = 25        # 0: no limit
# No default: any address, on a port picked at start.
listen = ["0.0.0.0:4000", "[::]:4000"]
portmap = true               # UPnP, PCP and NAT-PMP
relay = "https://relay.example.net"      # no default: n0's relays
relay_mode = "default"       # or "disabled"
congestion = "bbr"           # or "cubic", for pulls
pq_key_exchange = "prefer"   # or "require"
debug_addr = "127.0.0.1:9464"            # no default: off
immediate = true             # false: ignore [immediate]
allow_root = false           # true: let the daemon run as root

[daemon.discovery]
dht = true
dns = true
mdns = true

[telemetry]
otlp_endpoint = "http://localhost:4318"  # no default: off
otlp_protocol = "http"       # or "grpc"

# One [servers.NAME] table per machine this one connects to.
[servers.myserver]
endpoint_id = "4a29f16a4f80a3fd171ef5573c106dbda470ec24f5a97933752746b567ab9aff"
relay = "https://relay.example.net"   # the rest is optional
relay_mode = "disabled"
congestion = "cubic"
pq_key_exchange = "require"
address = ["192.168.1.10:4000"]
parallel = 64
reconnect = "1h"
dial_retries = 2
dht = false
mdns = false
ssh = "myserver"             # set by irsync pair

# One [clients.NAME] table per machine this one's daemon lets in.
[clients.myclient]
client_id = "a3e4753e5ea74e64e745e9c4d10b0fd751dbe03049d742eb0ea545c89703dfba"

# What the daemon shares and with whom. Entries in rw and ro are
# names from [clients.*] or EndpointIds.
[all]                        # grants for every export
rw = []
ro = []

[exports.photos]
path = "/srv/photos"
rw = ["myclient"]
warm_index = false
fsync = false

[exports.docs]
path = "/srv/docs"
read_only = true
ro = ["myclient"]

# Peers that may reach any path this account can, read-write.
# irsync pair adds the client here, under its host name.
[immediate]
rw = ["myclient"]

# Made by the daemon when an [immediate] peer first used it.
[exports.auto-srv-music]
path = "/srv/music"
auto = true
rw = ["myclient"]
```

## File permissions

The config file, a private key and a passphrase file are for their owner
only, as sshd asks of a key. irsync stops when one of them:

- can be read, written or run by its group or by other users (any mode bit
  beyond `0700`), or
- belongs to another user who is not root. Root itself may use a file of any
  owner.

The config file is checked first, before anything else in it counts and
before irsync does anything else. The error names the file, its mode and
the fix.

The management commands refuse such a file too, so fix it with `chmod 600`.
A key or a passphrase file is checked when a command loads it. A key is
checked whether it is encrypted or not.

| Key | Default | Meaning |
| --- | --- | --- |
| `unsafe_config_allowed` | `false` | `true` turns these checks off for the config file itself, the keys and the passphrase file. It goes at the top of the file, outside every table. |

Leave it off unless you have to share a file. Whoever can write the config
file can grant themselves access, and whoever can read a key or its
passphrase can act as that identity. A running daemon reads the config file
for every connection: if the file is opened up while the daemon runs, the
daemon refuses connections until it is closed again.

Only regular files are checked. A passphrase read from a pipe, such as
`--passphrase-file <(pass show irsync)`, is used as it is.

## Identity and state

| Key | Default | Meaning |
| --- | --- | --- |
| `identity` | `~/.config/irsync/id_ed25519` | The client's private key, an OpenSSH Ed25519 key file. |
| `state_dir` | `~/.local/state/irsync`; `$XDG_STATE_HOME/irsync` when that is set | Where the client and the daemon keep their indexes, sessions and partial-file records. |

The client's key is looked up the way ssh picks one: `--identity`; else
`identity` when the file sets it; else `~/.config/irsync/id_ed25519` when it
exists; else `~/.ssh/id_ed25519` when it exists, read as it is and never
written; else a key generated at `~/.config/irsync/id_ed25519`. A key named by
`--identity` or `identity` is the whole answer: when it is missing, a key is
generated there. An encrypted key needs a passphrase, from
`--passphrase-file`, `IRSYNC_PASSPHRASE` or a prompt on a terminal. To make
an encrypted key, run `irsync id generate --passphrase-file PATH`. Only
file-based Ed25519 keys work.

The client and the daemon use different keys, so both can run in one account
on one machine and reach each other.

## Client

`[client]` holds the defaults for every sync. A server's table overrides them
for that server, and a command-line flag overrides both: a sync resolves each
setting from the flag, then the server's table, then `[client]`, then the
built-in default.

| Key | Default | Meaning |
| --- | --- | --- |
| `parallel` | `128` | Data streams a push keeps open at once. Flag: `--parallel`. |
| `reconnect` | `1d` | How long a sync keeps redialling after a dropped connection; `0` gives up at once. Flag: `--reconnect`. |
| `dial_retries` | `4` | How often a sync retries a server that has not answered yet, waiting about 1, 2, 4, 8 seconds and so on between tries, before it gives up; `0` gives up on the first failure. `reconnect` applies only once the server has answered. Flag: `--dial-retries`. |
| `dht` | `true` | Look daemons up in the mainline DHT. Flag: `--no-dht`. |
| `ssh_install` | `true` | Copy irsync to the server when `irsync pair` sets it up. `false` copies nothing: `pair` uses the irsync already at `~/.local/bin/irsync` there, and stops if it is missing or another version. Flag: `--no-install`. |
| `mdns` | `true` | Look daemons up with mDNS on the local network. |
| `relay` | n0's relays | The relay server to use, for a self-hosted relay. Flag: `--relay`. |
| `relay_mode` | `default` | `disabled`: direct connections only, never through a relay; the lookups stay on. Flag: `--no-relay`. |
| `congestion` | `bbr` | QUIC's congestion controller when the client sends, that is for a push: `bbr` or `cubic`. |
| `pq_key_exchange` | `prefer` | `require`: refuse a daemon that does not offer the post-quantum key exchange. See [Post-quantum key exchange](#post-quantum-key-exchange). Flag: `--pq-key-exchange`. |

`congestion` picks the congestion controller of the side that sends: the
client's (the server's, else `[client]`'s) for a push, the daemon's for a pull.
When to choose `cubic` over `bbr`, with measurements, is in
[Connections](../explanation/connections.md#congestion-control).

### Post-quantum key exchange

irsync always offers a key exchange that also holds against a quantum
computer (`X25519MLKEM768`, see
[Networking](../explanation/networking.md#tls-13-inside-quic)), and uses it
when the other side offers it too. `pq_key_exchange` says what happens when
the other side does not, as irsync 0.1.0-beta.6 and older do not:

- `prefer`, the default: the connection is made with the classical key
  exchange. Someone who records it could decrypt it once a large quantum
  computer exists. `-vv` shows which key exchange a connection got.
- `require`: the connection is refused, before any file name or data is
  sent. The side that refuses says why, and the other side prints it.

Each side decides for itself. On the client it is the flag, else the server's
table, else `[client]`. On the daemon it is `pq_key_exchange` in
[`[daemon]`](#daemon), and it holds for every client: there is no setting
per export or per grant yet. A daemon tells only a client that has a grant;
a key without one is turned away as before.

## Servers

`[servers.NAME]` names a machine this one connects to. The name is what you
type before the colon in `NAME:PATH`. The
[`irsync pair`](../how-to/pair.md#the-names-pair-saves) command writes the
entry for you. A name may not be empty, contain `:` or `/`, be `all` or `*`, or be
an `EndpointId`.

| Key | Default | Meaning |
| --- | --- | --- |
| `endpoint_id` | required | The `EndpointId` of the server's daemon: the one dialled for `NAME:PATH`. The client pins it. |
| `relay` | `[client]`'s | This server's relay server. |
| `relay_mode` | `[client]`'s | `disabled`: reach this server directly only. |
| `congestion` | `[client]`'s | The congestion controller for pushes to this server. |
| `pq_key_exchange` | `[client]`'s | `require`: refuse this server when it does not offer the post-quantum key exchange. |
| `address` | none | Dial these addresses directly, with the relay, DNS, DHT, mDNS and the port mapper off. The client binds to the local address it reaches the first address of each IP version from, and offers the daemon no other. Cannot be combined with `--relay`. |
| `parallel` | `[client]`'s | Data streams a push to this server keeps open at once. |
| `reconnect` | `[client]`'s | How long to keep redialling. |
| `dial_retries` | `[client]`'s | How often to retry this server before it has answered. |
| `dht` | `[client]`'s | Look this server up in the mainline DHT. |
| `mdns` | `[client]`'s | Look this server up with mDNS. |
| `ssh` | none | The ssh destination this server was set up through, such as `myserver` or `me@myserver`. [`irsync pair`](../tutorial/first-sync.md#the-quick-way-over-ssh) writes it. When a sync to a server with `ssh` gets no answer within about 15 seconds, `irsync` checks it over ssh once: it starts the daemon again, and updates `endpoint_id` if the server's identity changed. It does not add back an immediate grant that was removed on the server. The export listing, `irsync NAME:`, is not checked this way. A server without `ssh` is never touched over ssh. |
| `ssh_aliases` | none | Other ssh destinations that were typed for this server, such as its address: `["192.168.1.5"]`. Typing one of them finds this server, as typing its `ssh` value does. `irsync pair` adds to it; nothing is ever dialled through it. These are not server names: they do not work with `irsync server`. |

`--address` on the command line replaces the server's `address` list.

## Clients

`[clients.NAME]` names a machine that this machine's daemon lets in. Grants
(`rw` and `ro` in an export, `[all]` and `[immediate]`) name clients, never
servers. The name has the same rules as a server's.

| Key | Default | Meaning |
| --- | --- | --- |
| `client_id` | required | The machine's `EndpointId` as a client: the key it signs in with. A grant that names `NAME` is for this id. |

A server and a client may have the same name. They are two entries, for the
two ids a machine has: its daemon's, which you dial, and its own as a client,
which grants are for. `irsync pair` writes both sides: the server's name on the
client, and the client's host name on the server.

## Config versions

The file starts with `config_version = 2`. irsync writes it first, every time
it saves the file. You do not set it, and `irsync config set` refuses it.

Version 1 had one table, `[hosts.NAME]`, for servers and clients alike. An old
file works as it is. irsync reads it as before, and it grants the same access.
The first change rewrites it as version 2. That change is any `irsync` command
that edits the file, `irsync pair`, or the daemon adding an automatic export.
Comments stay.

The rewrite splits each `[hosts.NAME]`:

- A host with an `endpoint_id` becomes `[servers.NAME]`.
- When a grant names that host, and the host has no `client_id`, it also
  becomes `[clients.NAME]`, with that same id and a comment that says so. The
  grant was for that id before, and still is.
- A host with a `client_id` becomes `[clients.NAME]`. If it also has an
  `endpoint_id`, it is a server too.
- A host with only a `client_id` has no use as a server. Its dial keys and its
  `ssh` keys are dropped. The command names them on stderr, in lines that start
  with `irsync: note:`.

An older irsync cannot read a version 2 file. While an older daemon is running
on the file, irsync refuses to rewrite it, and says to restart the daemon
first. The daemon needs a restart, not a new config. Do that, then run the
command again.

## Daemon

`[daemon]` configures `irsync daemon`. On SIGHUP the daemon re-reads this
table and rebinds; changes to `debug_addr` and to the top-level `state_dir`
take effect at the next start.

| Key | Default | Meaning |
| --- | --- | --- |
| `identity` | `~/.config/irsync/daemon_ed25519` | The daemon's private key, generated when missing. |
| `idle_timeout` | `30m` | Drop an authenticated connection that has had no traffic and no local progress, such as hashing or writing, for this long. The client reconnects and resumes. `0` disables it. |
| `dormant_session_timeout` | `7d` | How long the daemon keeps an interrupted session, and its partial files, for a client to resume. |
| `max_dormant_sessions_per_key` | `25` | How many interrupted sessions one client key may leave waiting in an export. When a key has more, the daemon drops the one that went longest without progress; the sync it belonged to plans afresh on its next run. A session that a connection is using does not count. `0` removes the limit. |
| `listen` | none | The sockets to bind, at most one IPv4 and one IPv6; port `0` lets the system pick. Without it the daemon binds any address on a port picked at start. The daemon offers clients the addresses it binds for extra paths: a specific address offers only that one, any address offers every interface. Flag: `--listen`. |
| `portmap` | `true` | Ask the router for a port mapping with UPnP, PCP or NAT-PMP. Set `false` in isolated setups. |
| `relay` | n0's relays | The relay server to use, for a self-hosted relay. |
| `relay_mode` | `default` | `disabled`: no relay, direct connections only. |
| `congestion` | `bbr` | The congestion controller when the daemon sends, that is for a pull. |
| `pq_key_exchange` | `prefer` | `require`: close the connection of a client that does not offer the post-quantum key exchange, for every export. The client is told to upgrade. See [Post-quantum key exchange](#post-quantum-key-exchange). |
| `debug_addr` | off | Serve metrics and span timings over HTTP at `IP:PORT`; a bare port means `127.0.0.1`. |
| `immediate` | `true` | Honour the `[immediate]` grants. `false`: the daemon ignores `[immediate]` and makes no automatic exports, so peers reach only the exports granted to them. |
| `allow_root` | `false` | Let the daemon run as root. Without it, or the flag `--run-as-root-i-know-what-i-am-doing`, a daemon started as root exits at once. See [Do not run it as root](../how-to/run-the-daemon.md#do-not-run-it-as-root). |

`[daemon.discovery]` says how clients may find the daemon:

| Key | Default | Meaning |
| --- | --- | --- |
| `dht` | `true` | Publish the daemon's addresses in the mainline DHT. With relays disabled the records carry no addresses, and the daemon warns. |
| `dns` | `true` | Publish them through n0's DNS service. |
| `mdns` | `true` | Announce the daemon with mDNS on the local network. |

## Telemetry

`[telemetry]` turns on OpenTelemetry export, for the client and the daemon.
The `OTEL_EXPORTER_OTLP_*` environment variables take precedence.

| Key | Default | Meaning |
| --- | --- | --- |
| `otlp_endpoint` | off | The collector's base URL. |
| `otlp_protocol` | `http` | `http` (HTTP with protobuf) or `grpc`. |

## Exports

`[exports.NAME]` shares a directory under `NAME`. The name is the first
segment of a `HOST:PATH`, so it may not be empty or contain `/`, and it may not
be `all`.

| Key | Default | Meaning |
| --- | --- | --- |
| `path` | required | The directory, an absolute path; `~` is expanded. |
| `read_only` | `false` | Refuse pushes from everyone, even from peers under `rw`. |
| `warm_index` | `false` | Hash the files already in the export into its index whenever the daemon starts, so files `irsync` did not place are reused. |
| `fsync` | `false` | Flush every file a push writes to disk before it is renamed into place, as if the client had passed `--fsync`. Read on every connection. |
| `rw` | `[]` | Peers with read-write access: client names or `EndpointId`s. |
| `ro` | `[]` | Peers with read-only access. |
| `auto` | `false` | Marks an automatic export, one the daemon made for an `[immediate]` peer; see [Immediate grants](#immediate-grants). |

`[all]` has `rw` and `ro` too; its grants apply to every export except
automatic ones (`auto = true`).

### Immediate grants

`[immediate]` has one key, `rw`: peers that may reach any path the daemon's
account can, read-write. There is no read-only form. `irsync grant add
immediate WHO` adds a peer, `irsync grant remove immediate WHO` takes it out,
and `irsync pair` adds its client here, by the host name it saved the client
under in `[clients]`.

When such a peer uses a path that no export covers, the daemon adds an
automatic export for it, `auto = true`, named `auto-` and the path with `-`
between its parts (`/srv/music` becomes `auto-srv-music`), granted read-write
to that peer only. The export is the directory itself, or the directory that
holds a file. A push to a directory that does not exist yet creates it, when
its parent exists. A push to the home itself (`myserver:`) exports the whole
home. `/` is never exported automatically; add that by hand. Remove an
automatic export with `irsync export remove`.

An automatic export belongs to the `[immediate]` peers: `[all]` never reaches
it, and a grant on it counts only while that peer's immediate grant is in
effect. Removing a peer from `[immediate]`, or setting `daemon.immediate =
false`, therefore also ends its access through the automatic exports made
for it; the exports themselves stay in the file. To share such a directory
with other peers, delete its `auto = true` line: it is then an ordinary
export.

How grants combine:

- A key that no entry names is rejected before any protocol message is read.
  There is no open access.
- A peer's entry in an export's own table decides its access to that export;
  without one, its `[all]` entry does. An export's `ro` entry therefore limits
  a peer that `[all]` grants `rw`.
- Within one table, `rw` wins over `ro`.
- `read_only` caps everyone at read-only.
- An entry that names no client and is no `EndpointId` matches nobody; the
  daemon logs a warning for it. A server's name is no client name.
- Whenever `irsync` saves the file, it writes a peer by its `[clients]` name:
  an `EndpointId` in a grant list that has a client becomes that name, also
  one that was already in the file. With two clients for one `EndpointId` it
  writes the first in alphabetical order. An entry named twice is kept once.
- A grant by name follows the client. If the client entry later gets another
  `client_id`, the grant is for that one.

How a remote path finds its export, nested exports included, is described in
[Remote addresses](remote-addresses.md#exports-decide-access).
