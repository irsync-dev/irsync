# Connections

`irsync` connects a client and a daemon with iroh, over QUIC. A daemon is
addressed by its `EndpointId`, the public half of its key, not by a host name
or an IP address. This page explains how a client gets from that id to a
working connection, and what keeps the connection going. The steps to change
each part are in
[Connect directly or through a relay](../how-to/direct-and-relay.md).

## Finding the daemon

A daemon publishes where it can be reached, in up to three places, each turned
on or off under [`[daemon.discovery]`](../reference/configuration.md#daemon):

- `dns`: n0's DNS service, through signed pkarr records.
- `dht`: the mainline DHT, with the same kind of record.
- `mdns`: the local network.

A client looks the `EndpointId` up in the same places. DNS is always on for a
client; the DHT and mDNS can be turned off per host or in `[client]`, and
`--no-dht` turns the DHT off for one sync.

The DNS and DHT records carry only the daemon's relay address, never its IP
addresses: that is iroh's default, so that publishing an id does not publish
where the machine is. mDNS announces the daemon's local addresses, and only on
the local network.

Whatever a lookup returns, the connection is to the key and nothing else. The
QUIC handshake proves that the far end holds the key for the `EndpointId` the
client dialled, and the daemon learns the client's `EndpointId` the same way.
A lookup that returns a wrong address leads to a failed connection, not to a
wrong peer.

## Relays

A connection starts through a relay, a server both sides can reach, and moves
to a direct path when hole punching succeeds. The relay forwards QUIC packets
that are encrypted end to end between the client and the daemon; it cannot
read or change them. n0 runs public relays, which are the default.

Relays are the fallback. Every packet takes the detour through the relay
server and shares it with other users, so a relayed transfer is slower than a
direct one. When one end sits behind a symmetric NAT, as many mobile and
carrier-grade NATs are, and the other behind a symmetric or port-restricted
NAT, hole punching fails and the connection stays on the relay. For them a self-hosted relay near the daemon is the better choice than
the shared public ones.

Relays can be refused, on either side. A client with relays off still looks
the daemon up, but since the DNS and DHT records carry only a relay address,
it finds a daemon through mDNS on the local network or through an address it
is given. A daemon with relays off has nothing to publish in DNS or the
DHT; it warns at start when `dht` is on.

## Direct addresses

Given an address, through `--address` or a host's `address` key, the client
dials it and nothing else: no relay, no DNS, no DHT, no mDNS and no port
mapper. This is how two `irsync` processes talk on one machine, on a LAN, or
without internet access. The `EndpointId` is still required and still checked,
so an address only says where to knock, not whom to trust. Over the
connection, iroh may still learn and try further addresses of the daemon; for a
daemon on loopback those are loopback too.

On the daemon's side, `listen` pins the sockets it binds, at most one IPv4 and
one IPv6, and `portmap` asks the router for a port mapping with UPnP, PCP or
NAT-PMP. Pinning the sockets does not stop the publishing or the relay; an
isolated daemon turns those off one by one.

## Keeping a connection alive

Two timers watch a connection, at different scales:

- **QUIC's own idle timeout, 60 seconds.** Both sides send a keep-alive every
  5 seconds, so this fires only when the path is gone: a laptop that sleeps,
  a network that drops.
- **The daemon's `idle_timeout`, 30 minutes by default.** The daemon drops an
  authenticated connection on which nothing has happened for that long.
  Something happening means any control message or file data on the wire, and
  also local progress on either side: chunks written or copied, hashing,
  finalizing a file (the `fsync` and the whole-file check of a large `.part`),
  and each deletion. Hashing counts as progress every 64 MiB read, so a large
  file being hashed keeps the connection alive. A side with local progress
  sends a heartbeat every half second, so that the other side sees it.

When a connection drops, the client redials for up to `reconnect` (one day by
default) and resumes the session. A file that was being finalized when the
connection dropped does not fail: the resumed session waits up to 60 seconds
for the earlier connection's finalize to release the `.part` file before it
reports the file busy.

`idle_timeout = "0"` disables the daemon's limit; see
[Configuration](../reference/configuration.md#daemon).

## Congestion control

The side that sends picks QUIC's congestion controller: the client for a push,
the daemon for a pull. The setting is `congestion`, in `[client]`, in a host's
table or in `[daemon]`, and takes `bbr`, the default, or `cubic`.

- **BBR** keeps the queue at the bottleneck short and copes with random loss,
  as on mobile links.
- **Cubic** keeps the link busier when the return path delays the
  acknowledgements. That is typically an upload on a home line whose download
  is busy with other traffic: the queued download holds up the upload's
  acknowledgements, and BBR then keeps too little data in flight to bridge the
  delay. Cubic fills the line's queue instead, which adds latency for
  everything else on the line while it sends.

Measured in the netstress `dsl-video` profile, a 1 Mbit/s uplink beside a busy
downlink, Cubic was 1.6 to 1.8 times faster than BBR. In the lossy
`4g-vehicle` profile BBR was 23 to 50% faster, and Cubic's full queue cost
other traffic on the line about 0.5 s of latency. Neither wins everywhere, so
BBR stays the default. The runs are in
[NETSTRESS_DECISIONS.md](https://github.com/irsync-dev/irsync/blob/main/docs/NETSTRESS_DECISIONS.md).
