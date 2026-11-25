# Configuration

One file holds everything: `~/.config/irsync/irsync.toml`, or
`$XDG_CONFIG_HOME/irsync/irsync.toml` when that is set. It is optional; a
missing file is the same as an empty one, and every key has a default except
where noted. The [management commands](management.md) edit it for you and keep
its comments and the order of its servers, clients and exports (they rewrite the rest in
one standard form); `irsync config` reads and writes any key; you can also edit it by
hand.

Every command takes `--config PATH` to use another file. Unknown keys and
tables are errors that name the file, the line and the key.

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
state_dir = "~/.local/state/irsync"

[defaults]                   # this machine as a client
identity = "~/.config/irsync/id_ed25519"
parallel = 128
reconnect = "1d"
dial_retries = 4
ssh_install = true           # false: copy nothing to the server
relay = "default"             # n0's relays; "disabled" or a URL
dht = true                   # the lookups
dns = true
mdns = true
congestion = "bbr"           # or "cubic", for pushes
pq_key_exchange = "prefer"   # or "require"
idle_timeout = "0"           # "0": never

[daemon]
identity = "~/.config/irsync/daemon_ed25519"
idle_timeout = "30m"         # "0" disables
dormant_session_timeout = "7d"
max_dormant_sessions_per_key = 25        # 0: no limit
# No default: any address, on a port picked at start.
listen = ["0.0.0.0:4000", "[::]:4000"]
portmap = true               # UPnP, PCP and NAT-PMP
relay = "default"             # n0's relays; "disabled" or a URL
dht = true                   # where clients may find the daemon
dns = true
mdns = true
congestion = "bbr"           # or "cubic", for pulls
pq_key_exchange = "prefer"   # or "require"
debug_addr = "127.0.0.1:9464"            # no default: off
allow_all_paths = true       # false: ignore all_paths
allow_root = false           # true: let the daemon run as root

[telemetry]
otlp_endpoint = "http://localhost:4318"  # no default: off
otlp_protocol = "http"       # or "grpc"

# One [servers.NAME] table per machine this one connects to.
[servers.myserver]
endpoint_id = "4a29f16a4f80a3fd171ef5573c106dbda470ec24f5a97933752746b567ab9aff"
relay = "https://relay.example.net"   # the rest is optional
dht = false
dns = false
mdns = false
congestion = "cubic"
pq_key_exchange = "require"
parallel = 64
reconnect = "1h"
dial_retries = 2
ssh = "myserver"             # set by irsync pair

# A server dialled at its address: no relay, no lookups.
[servers.mynas]
endpoint_id = "069ffbb125300737c2767fed918cd5f4bc12b3102fdd9009a2a1320e2ebd3191"
address = ["192.168.1.10:4000"]

# One [clients.NAME] table per machine this one's daemon lets in.
[clients.myclient]
client_id = "a3e4753e5ea74e64e745e9c4d10b0fd751dbe03049d742eb0ea545c89703dfba"
all_exports = "ro"           # every hand-made export
all_paths = "rw"             # any path this account can
admin = "rw"                 # manage this server remotely

# What the daemon shares and with whom. Entries in rw and ro are
# names from [clients.*] or EndpointIds.
[exports.photos]
path = "/srv/photos"
rw = ["myclient"]
index_at_start = false
fsync = false

[exports.docs]
path = "/srv/docs"
read_only = true
ro = ["myclient"]

# Made by the daemon when a client with all_paths first used it.
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
| `state_dir` | `~/.local/state/irsync`; `$XDG_STATE_HOME/irsync` when that is set | Where the client and the daemon keep their indexes, sessions and partial-file records. |

The client's key is `identity` in [`[defaults]`](#defaults). It is looked
up the way ssh picks one: `--identity`; else `identity` when the file sets
it; else `~/.config/irsync/id_ed25519` when it exists; else `~/.ssh/id_ed25519` when it exists, read as it is and never
written; else a key generated at `~/.config/irsync/id_ed25519`. A key named by
`--identity` or `identity` is the whole answer: when it is missing, a key is
generated there. An encrypted key needs a passphrase, from
`--passphrase-file`, `IRSYNC_PASSPHRASE` or a prompt on a terminal. To make
an encrypted key, run `irsync id generate --passphrase-file PATH`. Only
file-based Ed25519 keys work.

The client and the daemon use different keys, so both can run in one account
on one machine and reach each other.

## Defaults

`[defaults]` holds this machine's settings for when it connects to a server:
its key, and the defaults for every sync. Do not mix it up with
`[clients.NAME]`, the machines this machine's daemon lets in. A server's table
overrides these defaults for that server, and a command-line flag overrides both: a sync resolves each
setting from the flag, then the server's table, then `[defaults]`, then the
built-in default.

| Key | Default | Meaning |
| --- | --- | --- |
| `identity` | `~/.config/irsync/id_ed25519` | The client's private key, an OpenSSH Ed25519 key file. See [Identity and state](#identity-and-state). Flag: `--identity`. |
| `parallel` | `128` | Data streams a push keeps open at once; at least 1. Flag: `--parallel`. |
| `reconnect` | `1d` | How long a sync keeps redialling after a dropped connection; `0` gives up at once. Flag: `--reconnect`. |
| `dial_retries` | `4` | How often a sync retries a server that has not answered yet, waiting about 1, 2, 4, 8 seconds and so on between tries, before it gives up; `0` gives up on the first failure. `reconnect` applies only once the server has answered. A server `irsync pair` saved is dialled once when no `dial_retries` is set: not here, not in its table and not by the flag. Flag: `--dial-retries`. |
| `ssh_install` | `true` | Copy irsync to the server when `irsync pair` sets it up. `false` copies nothing: `pair` uses the irsync already at `~/.local/bin/irsync` there, and stops if it is missing or another version. Flag: `--no-ssh-install`. |
| `relay` | `default` | Which relay to use when no direct path works: `default` for n0's public relays, `disabled` for direct connections only (the lookups stay on), or the URL of a relay of your own. Flag: `--relay`. |
| `dht` | `true` | Look daemons up in the mainline DHT. Flag: `--no-dht`. |
| `dns` | `true` | Look daemons up through n0's DNS service. Flag: `--no-dns`. |
| `mdns` | `true` | Look daemons up with mDNS on the local network. Flag: `--no-mdns`. |
| `congestion` | `bbr` | QUIC's congestion controller when the client sends, that is for a push: `bbr` or `cubic`. |
| `pq_key_exchange` | `prefer` | `require`: refuse a daemon that does not offer the post-quantum key exchange. See [Post-quantum key exchange](#post-quantum-key-exchange). Flag: `--pq-key-exchange`. |
| `idle_timeout` | `0` | Drop a connection that moves no data and no control message for this long, and reconnect; `0` never does. The daemon's own is `[daemon] idle_timeout`. Flag: `--idle-timeout`. |

`congestion` picks the congestion controller of the side that sends: the
client's (the server's, else `[defaults]`'s) for a push, the daemon's for a pull.
When to choose `cubic` over `bbr`, with measurements, is in
[Connections](../explanation/connections.md#congestion-control).

### Post-quantum key exchange

irsync always offers a key exchange that also holds against a quantum
computer (`X25519MLKEM768`, see
[Networking](../explanation/networking.md#tls-13-inside-quic)), and uses it
when the other side offers it too. `pq_key_exchange` says what happens when
the other side does not:

- `prefer`, the default: the connection is made with the classical key
  exchange. Someone who records it could decrypt it once a large quantum
  computer exists. `-vv` shows which key exchange a connection got.
- `require`: the connection is refused, before any file name or data is
  sent. The side that refuses says why, and the other side prints it.

Each side decides for itself. On the client it is the flag, else the server's
table, else `[defaults]`. On the daemon it is `pq_key_exchange` in
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
| `address` | none | Dial these addresses directly, with the relay, DNS, DHT, mDNS and the port mapper off. The client binds to the local address it reaches the first address of each IP version from, and offers the daemon no other. `relay`, `dht`, `dns` and `mdns` cannot be set beside it, and `[defaults]`'s do not apply to this server. Cannot be combined with `--relay`. |
| `relay` | `[defaults]`'s | This server's relay: `default`, `disabled` or a URL, as in `[defaults]`. It wins over `[defaults]`'s, whatever each says. |
| `dht` | `[defaults]`'s | Look this server up in the mainline DHT. |
| `dns` | `[defaults]`'s | Look this server up through n0's DNS service. |
| `mdns` | `[defaults]`'s | Look this server up with mDNS. |
| `congestion` | `[defaults]`'s | The congestion controller for pushes to this server. |
| `pq_key_exchange` | `[defaults]`'s | `require`: refuse this server when it does not offer the post-quantum key exchange. |
| `parallel` | `[defaults]`'s | Data streams a push to this server keeps open at once; at least 1. |
| `reconnect` | `[defaults]`'s | How long to keep redialling. |
| `dial_retries` | `[defaults]`'s | How often to retry this server before it has answered. A server with `ssh` is dialled once when neither this nor `[defaults]` sets it. |
| `ssh` | none | The ssh destination this server was set up through, such as `myserver` or `me@myserver`. [`irsync pair`](../tutorial/first-sync.md#the-quick-way-over-ssh) writes it. It is what `irsync pair` logs in to. A sync never uses it, except to find the server when you type that address. `irsync pair` for a saved server checks it again, and never adds back `all_paths` that was removed on the server. |
| `ssh_aliases` | none | Other ssh destinations that were typed for this server, such as its address: `["192.168.1.5"]`. Typing one of them finds this server, as typing its `ssh` value does. `irsync pair` adds to it; nothing is ever dialled through it. These are not server names: they do not work with `irsync server`. |

`--address` on the command line replaces the server's `address` list.

## Clients

`[clients.NAME]` names a machine that this machine's daemon lets in. Grants
(`rw` and `ro` in an export) name clients, never servers. The name has the same rules as a server's.

| Key | Default | Meaning |
| --- | --- | --- |
| `client_id` | required | The machine's `EndpointId` as a client: the key it signs in with. A grant that names `NAME` is for this id. |
| `all_exports` | none | `"rw"` or `"ro"`: this client reaches every export you made by hand, read-write or read-only. See [Rights on a client](#rights-on-a-client). |
| `all_paths` | none | `"rw"` or `"ro"`: this client reaches any path the daemon's account can, with automatic exports. `"ro"` reads any path and writes none. |
| `admin` | none | `"rw"` or `"ro"`: this client may manage this server remotely. `"rw"` reads and changes exports, clients and grants; `"ro"` only reads. See [Rights on a client](#rights-on-a-client). |

Only a named client holds `all_exports`, `all_paths` or `admin`. A key you did not
name as a client cannot have them. A value left out means no right.
`irsync client set` sets them; see [Management commands](management.md#client).

A server and a client may have the same name. They are two entries, for the
two ids a machine has: its daemon's, which you dial, and its own as a client,
which grants are for. `irsync pair` writes both sides: the server's name on the
client, and the client's host name on the server.

## Config version

The file starts with `config_version = 2`. irsync writes it first, every time
it saves the file. You do not set it, and `irsync config set` refuses it. A
file without it is read as version 2. Any other version is an error.

## Daemon

`[daemon]` configures `irsync daemon`. On SIGHUP the daemon re-reads this
table and rebinds. Changes to `debug_addr`, to `[telemetry]` and to the
top-level `state_dir` take effect at the next start; the reload logs each at
`warn`.

| Key | Default | Meaning |
| --- | --- | --- |
| `identity` | `~/.config/irsync/daemon_ed25519` | The daemon's private key, generated when missing. |
| `idle_timeout` | `30m` | Drop an authenticated connection that has had no traffic and no local progress, such as hashing or writing, for this long. The client reconnects and resumes. `0` disables it. |
| `dormant_session_timeout` | `7d` | How long the daemon keeps an interrupted session, and its partial files, for a client to resume. |
| `max_dormant_sessions_per_key` | `25` | How many interrupted sessions one client key may leave waiting in an export. When a key has more, the daemon drops the one that went longest without progress; the sync it belonged to plans afresh on its next run. A session that a connection is using does not count. `0` removes the limit. |
| `listen` | none | The sockets to bind, at most one IPv4 and one IPv6; port `0` lets the system pick. Without it the daemon binds any address on a port picked at start. The daemon offers clients the addresses it binds for extra paths: a specific address offers only that one, any address offers every interface. Flag: `--listen`. |
| `portmap` | `true` | Ask the router for a port mapping with UPnP, PCP or NAT-PMP. Set `false` in isolated setups. |
| `relay` | `default` | `default` for n0's public relays, `disabled` for no relay (direct connections only), or the URL of a relay of your own. |
| `dht` | `true` | Publish the daemon's addresses in the mainline DHT. With relays disabled the records carry no addresses, and the daemon warns. |
| `dns` | `true` | Publish them through n0's DNS service. |
| `mdns` | `true` | Announce the daemon with mDNS on the local network. |
| `congestion` | `bbr` | The congestion controller when the daemon sends, that is for a pull. |
| `pq_key_exchange` | `prefer` | `require`: close the connection of a client that does not offer the post-quantum key exchange, for every export. The client is told to upgrade. See [Post-quantum key exchange](#post-quantum-key-exchange). |
| `debug_addr` | off | Serve metrics and span timings over HTTP at `IP:PORT`; a bare port means `127.0.0.1`. |
| `allow_all_paths` | `true` | Honour every client's `all_paths`. `false`: the daemon ignores `all_paths` and makes no automatic exports, so clients reach only the exports granted to them. |
| `allow_root` | `false` | Let the daemon run as root. Without it, or the flag `--run-as-root-i-know-what-i-am-doing`, a daemon started as root exits at once. See [Do not run it as root](../how-to/run-the-daemon.md#do-not-run-it-as-root). |

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
be `all`, `all_exports`, `all_paths` or `admin`. Those words are reserved.

| Key | Default | Meaning |
| --- | --- | --- |
| `path` | required | The directory, an absolute path; `~` is expanded. |
| `read_only` | `false` | Refuse pushes from everyone, even from peers under `rw`. |
| `index_at_start` | `false` | Hash the files already in the export into its index whenever the daemon starts, so files `irsync` did not place are reused. `irsync index --export NAME` does the same once. |
| `fsync` | `false` | Flush every file a push writes to disk before it is renamed into place, as if the client had passed `--fsync`. Read on every connection. |
| `rw` | `[]` | Peers with read-write access: client names or `EndpointId`s. |
| `ro` | `[]` | Peers with read-only access. |
| `auto` | `false` | Marks an automatic export, one the daemon made for a client with `all_paths`; see [Rights on a client](#rights-on-a-client). |

### Rights on a client

Three keys of `[clients.NAME]` are rights over the whole server. Each is
`"rw"` or `"ro"`. Left out, the client has no such right.

- `all_exports` reaches every export you made by hand, now and later. An
  export's own `rw` or `ro` entry for the client wins over it. It never
  reaches an automatic export (`auto = true`).
- `all_paths` reaches any path the daemon's account can. `"rw"` reads and
  writes; `"ro"` reads any path and writes none. `irsync client set NAME
  all_paths=rw` sets it, and `irsync pair` sets `all_paths = "rw"` on its
  client.
- `admin` lets the client manage this server from afar. `"rw"` reads the
  exports, clients and grants and changes them; `"ro"` only reads. It gives
  no access to any file. `irsync client set NAME admin=rw` sets it, and
  `irsync pair` sets `admin = "rw"` together with `all_paths = "rw"`.

When a client with `all_paths` uses a path that no export covers, the daemon
adds an automatic export for it, `auto = true`, named `auto-` and the path
with `-` between its parts (`/srv/music` becomes `auto-srv-music`), listing
that client only. The export is the directory itself, or the directory that
holds a file. A push to a directory that does not exist yet creates it, when
its parent exists; a client with `all_paths = "ro"` cannot push, so it
creates no directory. A push to the home itself (`myserver:`) exports the whole
home. `/` is never exported automatically; add that by hand. Remove an
automatic export with `irsync export remove`.

Access through an automatic export is always the client's current
`all_paths` level, whatever the export file says. Lowering `all_paths`, or
removing it, or setting `daemon.allow_all_paths = false`, applies at once,
also to the automatic exports already made. The exports themselves stay in
the file. To share such a directory with other peers, delete its `auto = true`
line: it is then an ordinary export.

How grants combine:

- A key that no entry names is rejected before any protocol message is read.
  There is no open access.
- A peer's entry in an export's own table decides its access to that export;
  without one, its `all_exports` does. An export's `ro` entry therefore limits
  a client whose `all_exports` is `"rw"`.
- Within one table, `rw` wins over `ro`. A client written under two names
  with one id gets the higher level of each right.
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
