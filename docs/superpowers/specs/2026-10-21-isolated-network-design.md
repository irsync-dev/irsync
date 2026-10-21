# irsync isolated network mode — design

Date: 2026-10-21
Status: draft, awaiting approval

## Intent

irsync today always reaches a daemon through iroh's discovery and relay
machinery: the client asks n0's DNS, the mainline DHT and mDNS for the
daemon's addresses, and every connection starts through a relay before it
upgrades to a direct path. That is right for the internet and wrong for two
situations this design serves:

- Two irsync processes on one machine, or on one LAN, that already know
  exactly where the other side is.
- Environments with no internet access at all, where any dependency on n0's
  services or a relay is a failure, not a slow path.

Two later projects depend on this one and shaped it, but are not part of it:

1. A stress-test harness that spawns an `irsync daemon` and an `irsync`
   client on localhost and drives them for a long time.
2. A local disk-to-disk mode, where `irsync` forks its own daemon as the
   receiver on the destination directory and connects to it from the
   source directory.

Both are programs driving irsync, not people. The daemon therefore has to be
able to choose its own port and report it, and everything has to be settable
from the command line without a config file edit.

## Goals

- A daemon can be told exactly which sockets to bind (IPv4, IPv6 or both),
  with a pinned port or a port of the daemon's choosing, and it reports the
  addresses it actually bound in a form a parent process can parse.
- A client can be given one or more socket addresses for a daemon and will
  then use the network for nothing but dialling them: no relay, no n0 DNS,
  no DHT, no mDNS.
- Everything is expressible both in the config files and on the command
  line, on both sides.
- Addresses may be `ip:port` (IPv4 or bracketed IPv6) or `host:port`.
- Existing behaviour is unchanged when none of the new settings are used.

## Non-goals

- The stress harness and the fork-a-receiver mode themselves, including any
  temporary identities or authorized_keys handling they will need.
- Changing how the daemon authenticates clients. A direct address never
  replaces the EndpointId; the client still needs the daemon's id and the
  daemon still checks the client's key against authorized_keys.
- Isolating the daemon automatically. A pinned listen socket is also what an
  internet-facing daemon behind a port forward wants, so `listen` only pins
  sockets. Turning off the daemon's relay and discovery stays explicit.
- Restricting which interface the client binds. The client sends packets
  only to the addresses it was given; its bind address is irrelevant.

## Anti-goals

- A client that has been given addresses must not talk to any third party:
  no relay, no n0 DNS, no DHT, no mDNS, no gateway (UPnP, PCP, NAT-PMP).
  If it does, this project has failed even if the sync works. Packets to
  other addresses of the daemon itself are not covered: iroh 1.3 exchanges
  candidate addresses in-band over the QUIC connection and may probe them,
  there is no public knob to turn that off, and against a loopback-only
  daemon the candidates are loopback anyway. (Author's call.)
- A daemon configured for isolation must not talk to any third party
  either. Today it still does: iroh's port mapper (UPnP, PCP, NAT-PMP) is
  on by default and runs at bind regardless of relay mode. See `portmap`
  below.

## Constraints

- iroh 1.3. Relevant API, already used by the test harness in
  `crates/irsync/tests/common/mod.rs`: `Endpoint::builder(presets::Minimal)`
  with `RelayMode::Disabled` and no address lookups; `clear_ip_transports()`
  followed by one `bind_addr` per socket; `Endpoint::bound_sockets()` for
  the actual bound addresses; `EndpointAddr::from_parts(id, addrs)` or
  `with_ip_addr` to dial by id plus direct addresses.
- iroh 1.3 enables its port mapper by default and starts a gateway lookup
  and mapping at bind time regardless of relay mode. It is turned off with
  `Builder::portmapper_config(PortmapperConfig::Disabled)`.
- iroh accepts at most one user-defined bind socket per address family
  with default bind options: a second one in the same family is rejected
  at build time as a duplicate default route.
- `crates/irsync/src/net/endpoint.rs` already models all of this through
  `EndpointOpts` and `RelayChoice::Disabled`. The only gap in that module is
  that `bind_addr` holds one address; it becomes a list.
- iroh connections are authenticated by the daemon's key, so the dial target
  is always id plus addresses. There is no id-less mode.
- Workspace CI: fmt, clippy with warnings denied, tests, on Linux and macOS.

## Behaviour

### Daemon

A new `listen` setting: a list of socket addresses, at most one IPv4 and
one IPv6 entry (an iroh constraint, see above; more is a config error with
the line number). Port 0 means the daemon picks. Each entry is a numeric
`ip:port` or `[ipv6]:port`. A hostname is accepted too and resolved with
the system resolver; the first IPv4 and the first IPv6 address it yields
are bound, and a bind that fails is fatal, as for a numeric entry. The
default is unset, which keeps iroh's current behaviour (unspecified IPv4
and IPv6, random ports).

```toml
listen = ["127.0.0.1:0", "[::1]:0"]
```

The same on the command line, repeatable: `irsync daemon --listen
127.0.0.1:0 --listen [::1]:0`. When given, the flag replaces the config's
list entirely rather than adding to it.

With `listen` set, the daemon binds only those sockets. On startup it prints
to stdout, after the existing `endpoint id:` line, one line per bound socket
with the real address and port, in this exact form:

```
endpoint id: <id as iroh prints it>
listening on: 127.0.0.1:43210
listening on: [::1]:43210
```

These lines are printed whether or not `listen` was set, so a parent can
always learn the bound sockets. They are the only startup output on stdout;
logging stays on stderr. The lines are flushed before the daemon starts
accepting connections, so a parent that sees them may dial at once.

`listen` says nothing about relays, discovery or port mapping. A new
top-level boolean `portmap` (default `true`, the current behaviour) turns
iroh's UPnP/PCP/NAT-PMP port mapper off. The fully isolated daemon is:

```toml
listen = ["127.0.0.1:0"]
relay_mode = "disabled"
portmap = false
[discovery]
dht = false
dns = false
mdns = false
```

The existing warning about `dht` with relays disabled stays.

### Client

A new `Address` option in the ssh_config-style client config, repeatable
within a `Host` block, and a matching repeatable `--address` flag:

```
Host local
    EndpointId  <id>
    Address     127.0.0.1:43210
    Address     [::1]:43210
```

```
irsync --address 127.0.0.1:43210 ./src <id>:module/path
```

Each value is `ip:port`, `[ipv6]:port` or `host:port`. A hostname is
resolved with the system resolver and every resulting address is used, so
`localhost:43210` yields both loopback addresses and iroh races them.

Giving at least one address switches the client into isolated mode:

- Relay: disabled (`RelayChoice::Disabled`, Minimal preset).
- Address lookup: none. n0 DNS, DHT and mDNS are all off, whatever the
  config or other flags say.
- Port mapper: off. A mapping for the client's own port serves nothing
  when the client already knows where the daemon is.
- Dial target: the EndpointId plus all given addresses, and nothing else.

The EndpointId comes from where it comes from today: the `Host` block's
`EndpointId`, else the host part of the remote argument. `--address` with
a bare EndpointId as the host therefore needs no config file at all.

Addresses from the command line replace, not extend, the config's list.
`Address` lines in the config merge across matching blocks the way every
other option does, except that all values are kept rather than first-wins.

The client logs one line at info when it enters isolated mode, naming the
addresses. Reconnects dial the same configured target. iroh may still
learn and probe further addresses of the daemon in-band during a session
(see anti-goals); irsync does not store or reuse them.

### Conflicts

- `--address` together with `--relay` on the command line is a usage error:
  both are explicit and they contradict.
- `Address` together with `Relay`, `Dht` or `Mdns` in the resolved client
  config is not an error. `Address` wins and the others are ignored, with a
  debug log line. A global `Relay` line above several `Host` blocks must
  not break the one host that has an `Address`.
- `--no-dht` with `--address` is redundant and accepted silently.

### Documentation

README's option table and `docs/daemon.md`'s "Relays, DHT and discovery"
section gain the new settings, with the two-config example above as the
recipe for an isolated setup.

## Testing

- Config parsing on both sides: the new keys, repeatability, port 0, IPv6
  brackets, hostnames, the first-wins exception for `Address`, the
  one-per-family limit on `listen`, `portmap`, and the command line
  replacing the config.
- Endpoint building with several bind addresses, and `bound_sockets`
  reporting real ports after binding to port 0.
- An end-to-end test that spawns the real `irsync daemon` binary with
  `--listen 127.0.0.1:0`, parses the two kinds of stdout line, runs the real
  `irsync` binary with `--address` against it, and checks the sync. This is
  the exact shape the stress harness will use.
- A test that a client in isolated mode has no relay, no address lookup
  and no port mapper configured on its endpoint, and the same for a daemon
  built from the isolated config above. Observing "no packets elsewhere" directly is
  out of reach of a unit test; checking the endpoint's configuration is the
  substitute.

## Left to the builder

- Internal representation of the parsed address list and where hostname
  resolution happens (parse time or endpoint-build time), as long as a bad
  value is reported with the config line number like other options.
- IPv6 scope ids (`fe80::1%eth0`): accept what Rust's `SocketAddr` parser
  accepts; anything more is not required.
- Whether `EndpointOpts::bind_addr` becomes `Vec<SocketAddr>` or a new
  field, and how `portmap` is carried in `EndpointOpts`; the test
  harness's `offline_opts` is updated either way and turns the port
  mapper off.
- Exact wording of log and error messages.
- Where the stdout lines are produced so that they are flushed before the
  accept loop starts.

## Later: revisiting the transport

This design stays inside iroh and only narrows what it does. Two further
steps were noted for later and are deliberately not part of this project:

- Speak QUIC directly, without iroh, for the localhost and local-network
  cases. The protocol above the connection would not change; what goes is
  the dependency on iroh's endpoint machinery, including the in-band
  address exchange and the port mapper that this design can only switch
  off piece by piece.
- Add transports that are not TCP/IP at all, such as Unix domain sockets
  (named sockets) for the fork-a-receiver local mode, where two processes
  on one host have no reason to go through the IP stack.

Either would replace the `Address`/`listen` plumbing here rather than build
on it, so this project should keep the transport choice behind the existing
`net` module boundary and not let socket addresses leak further into the
session code than they already do.
