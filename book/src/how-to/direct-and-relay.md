# Connect directly or through a relay

By default a client finds the daemon by its `EndpointId`, connects through a
relay and moves to a direct path when hole punching succeeds. This page shows
how to see which path a sync took, and how to change that: refuse relays, use
your own relay, turn the lookups off, or dial a known address. Why it works
this way is in [Connections](../explanation/connections.md).

## See which path a sync took

Run the sync with `-vv`:

```sh
irsync -avv ~/Photos/ myserver:photos/
```

The first line of the summary at the end of each session names the path,
`direct` or `relay`:

```text
session 1/1  push  ~/Photos/ -> myserver:photos/  wall 3:12  direct
```

The live progress block (`--progress`) shows the same word while the sync runs.

## Refuse relays

A sync that cannot get a direct path then fails instead of running through a
relay.

- For one sync, on the client:

  ```sh
  irsync -a --no-relay ~/Photos/ myserver:photos/
  ```

- For every sync to one server, or to every server:

  ```sh
  irsync server add myserver <id> --relay-mode disabled
  irsync config set defaults.relay_mode disabled
  ```

- On the daemon, which then neither uses a relay nor is reachable through one:

  ```sh
  irsync config set daemon.relay_mode disabled
  ```

The lookups stay on. A daemon's DNS and DHT records carry only its relay
address, though, so a client that refuses relays finds the daemon through mDNS
on the local network, or through an address you give it
([below](#dial-a-known-address)). A daemon with relays off and `dht` on logs a
warning at start, since its DHT records would be empty.

## Use your own relay

Clients and daemons behind a symmetric NAT on both ends cannot hole-punch and
stay on the relay. A relay of your own, near the daemon, serves them better
than the shared public relays. iroh ships one as `iroh-relay`.

1. Run `iroh-relay` on a host both sides can reach.

2. Point the daemon at it, then [reload](change-exports.md#reload-the-daemon-settings):

   ```sh
   irsync config set daemon.relay https://relay.example.net
   ```

3. Point the client at it, for one server or for all:

   ```sh
   irsync server add myserver <id> --relay https://relay.example.net
   irsync config set defaults.relay https://relay.example.net
   ```

   For one sync, `--relay URL` does the same.

## Turn the lookups off

- On the client, `--no-dht` skips the mainline DHT for one sync. A server's
  `dht`, `dns` and `mdns` keys, or `[defaults]`'s, turn a lookup off for
  good:

  ```sh
  irsync server add myserver <id> --dht no --mdns no
  irsync config set defaults.dns false
  ```

- On the daemon, the same three keys in `[daemon]` say where it publishes
  its addresses:

  ```sh
  irsync config set daemon.dht false
  irsync config set daemon.mdns false
  ```

- For a server paired over ssh, `irsync pair --no-dht` sets the
  daemon's `dht = false` for you (see
  [Your first sync](../tutorial/first-sync.md#the-quick-way-over-ssh)).

## Dial a known address

Two `irsync` processes on one machine, on one LAN, or anywhere without
internet access need no relay and no lookup. `listen` alone only pins the
sockets; it does not isolate the daemon. Turn everything off explicitly.

1. On the daemon, in `irsync.toml`:

   ```toml
   [daemon]
   listen = ["127.0.0.1:0"]   # at most one IPv4 and one IPv6 entry; port 0 = daemon picks
   relay_mode = "disabled"
   portmap = false            # no UPnP/PCP/NAT-PMP gateway probing
   dht = false
   dns = false
   mdns = false
   ```

   `--listen ADDR` on the command line replaces the `listen` list.

2. Start the daemon and read its address from stdout:

   ```text
   endpoint id: <id>
   listening on: 127.0.0.1:43210
   ```

3. On the client, dial that address, for one sync or for good:

   ```sh
   irsync -a --address localhost:43210 ./src <id>:data/
   irsync server add local <id> --address 127.0.0.1:43210
   ```

   The second command writes this table:

   ```toml
   [servers.local]
   endpoint_id = "<id>"
   address = ["127.0.0.1:43210"]
   ```

`--address` takes `ip:port`, `[ipv6]:port` or `host:port`, may be repeated,
and resolves host names with the system resolver. With an address the client
turns its relay, DNS, DHT, mDNS and port mapper off. The daemon's
`EndpointId` is still required and still checked. `--address` together with
`--relay` is refused.

### Keep to one path

An address picks where the connection starts. Once connected, the two sides
still tell each other their local addresses, and iroh may open more paths
between them and move the transfer to the fastest one: a LAN path instead of
the VPN you dialled, say. Neither irsync nor iroh can turn that exchange off,
but each side offers only the addresses its sockets are bound to:

- **The client**, given an address, binds to the local address the system
  sends from to reach it, one for IPv4 and one for IPv6, taken from the
  first given address of each. It offers the daemon nothing else. If that
  local address goes away, say when a VPN drops, the sync fails and redials
  instead of moving to another network. Addresses of one IP version share
  that local address, so a second address that needs another one may not be
  reachable.
- **The daemon** offers every address it listens on. `listen` with a
  specific address, as in step 1, offers only that one. Without `listen`,
  or with `0.0.0.0` or `[::]`, it offers each of its interfaces, and the
  client opens paths to them as well.

With both, the connection keeps to the path you dialled.

## See also

- [Configuration](../reference/configuration.md#servers): the `relay`,
  `relay_mode`, `address`, `dht`, `dns` and `mdns` keys.
- [Command line](../reference/command-line.md#irsync-flags): `--relay`,
  `--no-relay`, `--no-dht`, `--address`.
