# Connect directly or through a relay

By default a client finds the daemon by its `EndpointId`, connects through a
relay and moves to a direct path when hole punching succeeds. This page shows
how to see which path a sync took, and how to change that: refuse relays, use
your own relay, turn the lookups off, or dial a known address. Why it works
this way is in [Connections](../explanation/connections.md).

## See which path a sync took

Run the sync with `-v`:

```sh
irsync -av ~/Photos/ home:photos/
```

The first line of the summary at the end of each session names the path,
`direct` or `relay`:

```text
session 1/1  push  /home/me/Photos/ -> home:photos/  wall 3:12  direct
```

The live progress block shows the same word while the sync runs.

## Refuse relays

A sync that cannot get a direct path then fails instead of running through a
relay.

- For one sync, on the client:

  ```sh
  irsync -a --no-relay ~/Photos/ home:photos/
  ```

- For every sync to one host, or to every host:

  ```sh
  irsync host add home <id> --relay-mode disabled
  irsync config set client.relay_mode disabled
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

3. Point the client at it, for one host or for all:

   ```sh
   irsync host add home <id> --relay https://relay.example.net
   irsync config set client.relay https://relay.example.net
   ```

   For one sync, `--relay URL` does the same.

## Turn the lookups off

- On the client, `--no-dht` skips the mainline DHT for one sync. A host's
  `dht` and `mdns` keys, or `[client]`'s, turn either lookup off for good:

  ```sh
  irsync host add home <id> --dht no --mdns no
  ```

- On the daemon, `[daemon.discovery]` says where it publishes its addresses:

  ```sh
  irsync config set daemon.discovery.dht false
  irsync config set daemon.discovery.mdns false
  ```

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

   [daemon.discovery]
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
   irsync host add local <id> --address 127.0.0.1:43210
   ```

   The second command writes this table:

   ```toml
   [hosts.local]
   endpoint_id = "<id>"
   address = ["127.0.0.1:43210"]
   ```

`--address` takes `ip:port`, `[ipv6]:port` or `host:port`, may be repeated,
and resolves host names with the system resolver. With an address the client
turns its relay, DNS, DHT, mDNS and port mapper off and talks to nothing but
the addresses it was given. The daemon's `EndpointId` is still required and
still checked. `--address` together with `--relay` is refused.

## See also

- [Configuration](../reference/configuration.md#hosts): the `relay`,
  `relay_mode`, `address`, `dht` and `mdns` keys.
- [Command line](../reference/command-line.md#irsync-flags): `--relay`,
  `--no-relay`, `--no-dht`, `--address`.
