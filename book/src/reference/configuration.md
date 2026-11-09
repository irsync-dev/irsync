# Configuration

One file holds everything: `~/.config/irsync/irsync.toml`. It is optional; a
missing file is the same as an empty one, and every key has a default except
where noted. The [management commands](management.md) edit it for you and keep
its comments; `irsync config` reads and writes any key; you can also edit it by
hand.

Every command takes `--config PATH` to use another file. Unknown keys and
tables are errors that name the file, the line and the key.

## Split files

To keep the aliases or the exports apart, put a `hosts.toml` holding the
`[hosts.*]` tables, or an `exports.toml` holding `[all]` and `[exports.*]`,
beside the config file. They are read as if they were part of it. A table must
then live in only one of the two files. `irsync` never edits a split file:
`host`, `export`, `grant` and `config set` refuse a table it holds. `irsync
show` names any split file it read.

## Values

- A **duration** is `<N>[s|m|h|d]`, such as `90s`, `30m`, `1h` or `7d`; a
  number without a unit, as a string or as a TOML integer, is seconds.
- A **path** may start with `~`, which stands for the home directory.
- An **`EndpointId`** is hex or z-base-32, as iroh prints it.
- An **address** is `ip:port`, `[ipv6]:port` or `host:port`.

## The whole file

Every table, with the defaults except where marked:

```toml
identity = "~/.config/irsync/id_ed25519"
state_dir = "~/.local/state/irsync"

[client]                     # defaults for every host
parallel = 128
reconnect = "1d"
dht = true
mdns = true
relay = "https://relay.example.net"   # no default: n0's relays
relay_mode = "default"       # or "disabled": direct connections only
congestion = "bbr"           # or "cubic", for pushes

[daemon]
identity = "~/.config/irsync/daemon_ed25519"
idle_timeout = "30m"         # "0" disables
dormant_session_timeout = "7d"
listen = ["0.0.0.0:4000", "[::]:4000"]   # no default: any address, a port picked at start
portmap = true               # UPnP, PCP and NAT-PMP
relay = "https://relay.example.net"      # no default: n0's relays
relay_mode = "default"       # or "disabled"
congestion = "bbr"           # or "cubic", for pulls
debug_addr = "127.0.0.1:9464"            # no default: off

[daemon.discovery]
dht = true
dns = true
mdns = true

[telemetry]
otlp_endpoint = "http://localhost:4318"  # no default: off
otlp_protocol = "http"       # or "grpc"

# One [hosts.ALIAS] table per remote. On a daemon's machine the same tables
# name the clients that exports are granted to.
[hosts.myserver]
endpoint_id = "4a29f16a4f80a3fd171ef5573c106dbda470ec24f5a97933752746b567ab9aff"
relay = "https://relay.example.net"   # the rest is optional
relay_mode = "disabled"
congestion = "cubic"
address = ["192.168.1.10:4000"]
parallel = 64
reconnect = "1h"
dht = false
mdns = false

[hosts.myclient]
endpoint_id = "a3e4753e5ea74e64e745e9c4d10b0fd751dbe03049d742eb0ea545c89703dfba"

# What the daemon shares and with whom. Entries in rw and ro are aliases
# from [hosts.*] or EndpointIds.
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
```

## Identity and state

| Key | Default | Meaning |
| --- | --- | --- |
| `identity` | `~/.config/irsync/id_ed25519` | The client's private key, an OpenSSH Ed25519 key file. |
| `state_dir` | `~/.local/state/irsync`; on Linux `$XDG_STATE_HOME/irsync` when that is set | Where the client and the daemon keep their indexes, sessions and partial-file records. |

The client's key is looked up the way ssh picks one: `--identity`; else
`identity` when the file sets it; else `~/.config/irsync/id_ed25519` when it
exists; else `~/.ssh/id_ed25519` when it exists, read as it is and never
written; else a key generated at `~/.config/irsync/id_ed25519`. A key named by
`--identity` or `identity` is the whole answer: when it is missing, a key is
generated there. An encrypted key needs a passphrase, from
`--passphrase-file`, `IRSYNC_PASSPHRASE` or a prompt on a terminal. Only
file-based Ed25519 keys work.

The client and the daemon use different keys, so both can run in one account
on one machine and reach each other.

## Client

`[client]` holds the defaults for every sync. A host's table overrides them
for that host, and a command-line flag overrides both: a sync resolves each
setting from the flag, then the alias's table, then `[client]`, then the
built-in default.

| Key | Default | Meaning |
| --- | --- | --- |
| `parallel` | `128` | Data streams a push keeps open at once. Flag: `--parallel`. |
| `reconnect` | `1d` | How long a sync keeps redialling after a dropped connection; `0` gives up at once. Flag: `--reconnect`. |
| `dht` | `true` | Look daemons up in the mainline DHT. Flag: `--no-dht`. |
| `mdns` | `true` | Look daemons up with mDNS on the local network. |
| `relay` | n0's relays | The relay server to use, for a self-hosted relay. Flag: `--relay`. |
| `relay_mode` | `default` | `disabled`: direct connections only, never through a relay; the lookups stay on. Flag: `--no-relay`. |
| `congestion` | `bbr` | QUIC's congestion controller when the client sends, that is for a push: `bbr` or `cubic`. |

`congestion` picks the congestion controller of the side that sends: the
client's (the host's, else `[client]`'s) for a push, the daemon's for a pull.
When to choose `cubic` over `bbr`, with measurements, is in
[Connections](../explanation/connections.md#congestion-control).

## Hosts

`[hosts.ALIAS]` names a remote by its `EndpointId`. On a client it names the
daemons you dial; on a daemon's machine it names the clients that grants may
refer to. An alias may not be empty, contain `:` or `/`, be `all` or `*`, or
be an `EndpointId`.

| Key | Default | Meaning |
| --- | --- | --- |
| `endpoint_id` | required | The remote's `EndpointId`. The client pins it. |
| `relay` | `[client]`'s | This host's relay server. |
| `relay_mode` | `[client]`'s | `disabled`: reach this host directly only. |
| `congestion` | `[client]`'s | The congestion controller for pushes to this host. |
| `address` | none | Dial these addresses directly, with the relay, DNS, DHT, mDNS and the port mapper off. Cannot be combined with `--relay`. |
| `parallel` | `[client]`'s | Data streams a push to this host keeps open at once. |
| `reconnect` | `[client]`'s | How long to keep redialling. |
| `dht` | `[client]`'s | Look this host up in the mainline DHT. |
| `mdns` | `[client]`'s | Look this host up with mDNS. |

`--address` on the command line replaces the host's `address` list.

## Daemon

`[daemon]` configures `irsync daemon`. On SIGHUP the daemon re-reads this
table and rebinds; changes to `debug_addr` and to the top-level `state_dir`
take effect at the next start.

| Key | Default | Meaning |
| --- | --- | --- |
| `identity` | `~/.config/irsync/daemon_ed25519` | The daemon's private key, generated when missing. |
| `idle_timeout` | `30m` | Drop an authenticated connection that has had no traffic and no local progress, such as hashing or writing, for this long. The client reconnects and resumes. `0` disables it. |
| `dormant_session_timeout` | `7d` | How long the daemon keeps an interrupted session, and its partial files, for a client to resume. |
| `listen` | none | The sockets to bind, at most one IPv4 and one IPv6; port `0` lets the system pick. Without it the daemon binds any address on a port picked at start. Flag: `--listen`. |
| `portmap` | `true` | Ask the router for a port mapping with UPnP, PCP or NAT-PMP. Set `false` in isolated setups. |
| `relay` | n0's relays | The relay server to use, for a self-hosted relay. |
| `relay_mode` | `default` | `disabled`: no relay, direct connections only. |
| `congestion` | `bbr` | The congestion controller when the daemon sends, that is for a pull. |
| `debug_addr` | off | Serve metrics and span timings over HTTP at `IP:PORT`; a bare port means `127.0.0.1`. |

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
| `rw` | `[]` | Peers with read-write access: aliases or `EndpointId`s. |
| `ro` | `[]` | Peers with read-only access. |

`[all]` has `rw` and `ro` too; its grants apply to every export.

How grants combine:

- A key that no entry names is rejected before any protocol message is read.
  There is no open access.
- A peer's entry in an export's own table decides its access to that export;
  without one, its `[all]` entry does. An export's `ro` entry therefore limits
  a peer that `[all]` grants `rw`.
- Within one table, `rw` wins over `ro`.
- `read_only` caps everyone at read-only.
- An entry that names no alias and is no `EndpointId` matches nobody; the
  daemon logs a warning for it.

How a remote path finds its export, nested exports included, is described in
[Remote addresses](remote-addresses.md#exports-decide-access).
