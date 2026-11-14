# Networking

`irsync` sends its messages over iroh, a library that gives two programs an
encrypted QUIC connection to each other, wherever they are. This page goes
through the layers from the bottom up. For each one it says what iroh does,
which standard it rests on, and where irsync's code touches it.

The other pages go into detail. [Connections](connections.md) covers the
choices irsync makes on top of iroh and the settings that change them:
lookups, relays, direct addresses, timers, congestion control.
[Protocol](../reference/protocol.md) is the exact reference for irsync's own
messages. iroh's own documentation is at [iroh.computer](https://www.iroh.computer/docs)
and its API at [docs.rs/iroh](https://docs.rs/iroh).

## The stack

```text
  irsync messages     Hello, Control, DataChunk, ...       proto/irsync/v1/
  frames              varint length + one protobuf         net/frame.rs
  ------------------------------------------------------------------------
  QUIC streams        control, listing, data               session/, send.rs
  QUIC connection     RFC 9000, multipath extension        iroh
  TLS 1.3 in QUIC     RFC 8446, RFC 9001, RFC 7250, ALPN   iroh
  paths               direct UDP (hole punched) or relay   iroh
  finding the peer    EndpointId -> addresses              iroh lookups
  identity            Ed25519 key pair                     iroh, identity.rs
```

Read it from the bottom up. Above the line is irsync's own protocol. Below it
is iroh, which irsync configures in `crates/irsync/src/net/endpoint.rs`;
irsync supplies the key and decides which streams to open.

## Identity: an endpoint is a key

Every iroh endpoint has an Ed25519 key pair
([RFC 8032](https://www.rfc-editor.org/rfc/rfc8032)). Its `EndpointId` is the
32-byte public key. That id is the address: a client dials an id, not a host
name or an IP address, and iroh makes sure the far end holds the matching
private key.

In irsync, the client and the daemon each have their own key
(`crates/irsync/src/identity.rs`). [Identity and trust](identity-and-trust.md)
explains where the keys come from and how the daemon decides what a key may
reach (`crates/irsync/src/authz.rs`).

## Finding the other side

An id says whom to reach, not where. iroh turns an id into addresses with
*address lookups*, pluggable services that each publish and resolve records:

- **DNS, through pkarr.** [pkarr](https://github.com/pubky/pkarr) records are
  small DNS records signed with the endpoint's own key, so nobody else can
  forge them. n0, the company behind iroh, runs the DNS server that serves
  them.
- **The mainline DHT.** The same signed records, stored in BitTorrent's
  distributed hash table ([BEP 5](https://www.bittorrent.org/beps/bep_0005.html)),
  as signed mutable items
  ([BEP 44](https://www.bittorrent.org/beps/bep_0044.html)). No single
  company runs it.
- **mDNS**, multicast DNS on the local network
  ([RFC 6762](https://www.rfc-editor.org/rfc/rfc6762)).

irsync adds each lookup that is turned on in `build` in
`crates/irsync/src/net/endpoint.rs`. Which are on by default, and what each
one publishes, is in [Connections](connections.md#finding-the-daemon). With
an address given by hand (`--address`), irsync turns every lookup off and
dials that address.

## Paths: relays and hole punching

Most machines sit behind a NAT, a router that shares one public address
among the machines behind it and drops unsolicited packets from outside. Two
such machines cannot simply send each other packets. iroh deals with this in
two ways.

**Relays.** A relay is a server both sides can reach. Each endpoint keeps a
connection to its home relay, over HTTPS upgraded to a WebSocket
([RFC 6455](https://www.rfc-editor.org/rfc/rfc6455)); the relay protocol is
derived from Tailscale's DERP. The relay forwards QUIC packets between the
two endpoints. It cannot read them: they are encrypted end to end.

**Hole punching.** Both sides send packets to each other's public address at
about the same time, so that each NAT sees outgoing traffic and lets the
answers in ([RFC 5128](https://www.rfc-editor.org/rfc/rfc5128) describes the
technique). To learn its own public address, an endpoint asks the relay
servers, using QUIC address discovery
([draft-ietf-quic-address-discovery](https://datatracker.ietf.org/doc/draft-ietf-quic-address-discovery/)).
The two sides exchange their candidate addresses inside the connection, with
an extension based on
[draft-seemann-quic-nat-traversal](https://datatracker.ietf.org/doc/draft-seemann-quic-nat-traversal/).

A connection starts on the relay and moves to a direct path once hole
punching works. When it doesn't, the connection stays on the relay. A sync
logs which it got at info level (`connected directly` or
`connected through a relay`);
[Connect directly or through a relay](../how-to/direct-and-relay.md) shows
how to check and how to change it.

In irsync, the relay choice and the port mapper are set in
`crates/irsync/src/net/endpoint.rs`. The in-process network tests replace all
of this with a simulated link, plugged in as an iroh custom transport
(`crates/irsync/src/net/netsim/`).

## QUIC

QUIC ([RFC 9000](https://www.rfc-editor.org/rfc/rfc9000)) is a transport
protocol over UDP. Like TCP, it delivers bytes reliably and in order, and it
backs off when the network is congested. Unlike TCP, it carries many
independent streams in one connection, encrypts everything, and can keep a
connection alive while the network path under it changes.

iroh's QUIC implementation is noq, a fork of the Rust library quinn. irsync
sets its transport parameters in `transport_config` in
`crates/irsync/src/net/endpoint.rs`: the idle timeout and keep-alive, the
flow-control windows, the stream limits, the congestion controller and how
often the peer acknowledges.

## TLS 1.3 inside QUIC

QUIC has a handshake of its own (RFC 9000, section 7), and that handshake
carries a TLS 1.3 handshake
([RFC 8446](https://www.rfc-editor.org/rfc/rfc8446)) in its first packets.
The keys TLS agrees on then encrypt every packet after that
([RFC 9001](https://www.rfc-editor.org/rfc/rfc9001)). iroh uses the rustls
TLS library.

Two TLS extensions matter here:

- **Raw public keys** ([RFC 7250](https://www.rfc-editor.org/rfc/rfc7250)).
  Instead of a certificate signed by an authority, each side presents its
  bare Ed25519 public key and proves it holds the private key. The key is
  the `EndpointId`, so the handshake proves identity directly. Both sides
  present a key: the daemon learns the client's id from the handshake
  (`conn.remote_id()` in `crates/irsync/src/daemon.rs`), before any irsync
  message.
- **ALPN**, application-layer protocol negotiation
  ([RFC 7301](https://www.rfc-editor.org/rfc/rfc7301)). The client names the
  protocol it wants to speak, and the server accepts it only if it speaks it
  too. irsync's is `irsync/1` (`ALPN` in `crates/irsync-proto/src/lib.rs`).
  The daemon's endpoint accepts nothing else. The version of irsync's own
  protocol travels separately, in `Hello`.

With `SSLKEYLOGFILE` set, rustls writes the TLS secrets to a file so that
Wireshark can decrypt a packet capture; see
[Trace and capture the protocol](../contributing/protocol-debugging.md#the-key-log).

## Streams and flow control

A QUIC stream is an ordered byte stream inside the connection. Either side
can open one, and it is either bidirectional or unidirectional (one side
writes, the other reads). Streams are independent: a lost packet holds up
only the streams whose data it carried.

Each stream has an id. Its two lowest bits say who opened it and what kind it
is (RFC 9000, section 2.1): 0, 4, 8, … are bidirectional streams opened by
the client, 2, 6, 10, … unidirectional ones opened by the client, and 3, 7,
11, … unidirectional ones opened by the server. The trace below shows these
ids.

Flow control keeps a fast sender from overrunning a slow reader. The reader
grants how many bytes it will take, per stream and for the whole connection,
and raises the limit as it reads. It also grants how many streams the other
side may have open at once. irsync's numbers are in `transport_config`: 8 MiB
per stream, 64 MiB per connection, up to 256 unidirectional and 8
bidirectional streams.

## Multipath

QUIC's multipath extension
([draft-ietf-quic-multipath](https://datatracker.ietf.org/doc/draft-ietf-quic-multipath/))
lets one connection use several network paths, each with its own packet
numbers and congestion state. iroh turns it on and treats the relay and each
direct address as paths of the same connection. Moving from the relay to a
direct path is moving traffic to another path, not opening a new connection:
streams carry on untouched, and irsync never notices.

irsync only watches which path is in use: the trace and the capture record a
`PathChange` whenever it changes (`selected_path` in
`crates/irsync/src/net/tap/mod.rs`).

## irsync's layer

On top of the streams, irsync sends protobuf messages, defined in
`proto/irsync/v1/`. Each message is a *frame*: its length as a protobuf
varint (a number written 7 bits per byte), then the message's bytes. This is
protobuf's standard length-delimited form. `write_msg` and `read_msg` in
`crates/irsync/src/net/frame.rs` write and read frames; a frame over 16 MiB is
refused before anything is allocated for it.

A session uses three kinds of stream, all opened by the side that dials,
except data streams on a pull:

- the **control stream**, the client's first bidirectional stream: the
  handshake, then the plan, the chunk lists, the requests and the
  acknowledgements (`crates/irsync/src/session/engine.rs`);
- the **listing stream**, the second bidirectional one: each half carries one
  side's listing (`crates/irsync/src/session/listing_stream.rs`);
- **data streams**, unidirectional, from the side that sends files to the side
  that receives them, one per run of chunks of one file
  (`crates/irsync/src/send.rs` opens them, `crates/irsync/src/session/recv_io.rs`
  accepts them). On a push the client opens them; on a pull the daemon does.

When a connection ends, the side that closes it gives a QUIC application
close code: 0 done, 1 unauthorized, 2 rejected, 3 superseded, 4 idle.
[Protocol](../reference/protocol.md#connection) lists what each one means for
the client.

## One push, message by message

Here is a real push of two small files, `a.txt` (6 bytes) and `b.txt` (12
bytes), from a client to a daemon on the same machine, into an empty export
`photos`. The client ran with the protocol trace on:

```sh
RUST_LOG=info,irsync::wire=trace irsync -av --address 127.0.0.1:57528 ./src/ <daemon-id>:photos/
```

These are the client's `irsync::wire` lines, trimmed to fit: the timestamp,
the log span and the fields that are the same on every line (`side`, `conn`,
`session`) are cut, `STREAM_KIND_` and `DIRECTION_` are dropped from the
values, the session id is shortened, and some file metadata is replaced by
`…`. [Trace and capture the protocol](../contributing/protocol-debugging.md#the-text-trace)
shows a full line.

**The connection.** iroh reports the path it chose, here a direct UDP path to
the address given, and the client opens the control stream, QUIC stream 0.

```text
PathChange { remote: "ip:127.0.0.1:57528" } dir="UNSPECIFIED" stream="UNSPECIFIED" quic_stream=0 file=-
StreamEvent { kind: OPENED } dir="UNSPECIFIED" stream="CONTROL" quic_stream=0 file=-
```

**The handshake.** `Hello` names the protocol version, the remote path, the
chunking parameters and the rsync flags. The daemon has already checked the
client's key during the TLS handshake. It answers with `HelloAck` and a new
session id.

```text
Hello { protocol_version: 5, path: "photos", chunk_min: 65536, chunk_avg: 262144, chunk_max: 1048576, contents_only: true, build: BuildInfo { … }, preserve: Preserve { links: true, perms: true, times: true, … } } dir="SENT" stream="CONTROL" quic_stream=0 file=-
HelloAck { session_id: 0x88f83bf9…, protocol_version: 5, build: BuildInfo { … } } dir="RECEIVED" stream="CONTROL" quic_stream=0 file=-
```

**The listings.** The client opens the listing stream, QUIC stream 4 (the
next client bidirectional stream), and names the session on it. Then both
sides list at once, each on its own half: the client its source directory and
two files, the daemon its export, which holds only the directory itself. Each
half ends with `ListingEnd` and the end of the half.

```text
StreamEvent { kind: OPENED } dir="UNSPECIFIED" stream="LISTING" quic_stream=4 file=-
ListingStreamHeader { session_id: 0x88f83bf9… } dir="SENT" stream="LISTING" quic_stream=4 file=-
Control { listing: ListingEntry { entry: FileEntry { kind: DIRECTORY, size: 80, … } } } dir="SENT" stream="LISTING" quic_stream=4 file=-
Control { listing: ListingEntry { entry: FileEntry { path: "a.txt", size: 6, … } } } dir="SENT" stream="LISTING" quic_stream=4 file=-
Control { listing: ListingEntry { entry: FileEntry { path: "b.txt", size: 12, … } } } dir="SENT" stream="LISTING" quic_stream=4 file=-
Control { listing_end: ListingEnd { count: 3 } } dir="SENT" stream="LISTING" quic_stream=4 file=-
StreamEvent { kind: FINISHED } dir="SENT" stream="LISTING" quic_stream=4 file=-
Control { listing: ListingEntry { entry: FileEntry { kind: DIRECTORY, size: 40, … } } } dir="RECEIVED" stream="LISTING" quic_stream=4 file=-
Control { listing_end: ListingEnd { count: 1 } } dir="RECEIVED" stream="LISTING" quic_stream=4 file=-
StreamEvent { kind: FINISHED } dir="RECEIVED" stream="LISTING" quic_stream=4 file=-
```

Heartbeats, which say "this side is busy", go back and forth on the control
stream throughout; they are left out here.

**The plan.** The daemon, the receiver on a push, compares the listings and
asks for both files. It gives each a file id, which later messages use
instead of the path. No `action` is printed: fields at their default value
are left out, and the default is `WHOLE`, send the whole file.

```text
Control { plan: PlanItem { file_id: 1, path: "a.txt" } } dir="RECEIVED" stream="CONTROL" quic_stream=0 file=1
Control { plan: PlanItem { file_id: 2, path: "b.txt" } } dir="RECEIVED" stream="CONTROL" quic_stream=0 file=2
```

**Chunk lists and requests.** The client hashes each file and sends its chunk
list: here one chunk per file, whose hash equals the whole file's. The daemon
has none of the chunks and asks for chunk 0 of each.

```text
Control { chunk_list_part: ChunkListPart { file_id: 1, size: 6, whole_hash: Hash { bytes: 8e4c7c1b99dbfd50e7a95185fead5ee1448fa904a2fdd778eaf5f2dbfd629a99 }, total: 1, chunks: [ChunkRecord { len: 6, hash: Hash { bytes: 8e4c7c1b… } }], last: true, … } } dir="SENT" stream="CONTROL" quic_stream=0 file=1
Control { chunk_list_part: ChunkListPart { file_id: 2, size: 12, whole_hash: Hash { bytes: 07c6fb3c62e3e04169dbe5acf2dba4a25213680c5f60e0fc557d94a2085db649 }, total: 1, chunks: [ChunkRecord { len: 12, hash: Hash { bytes: 07c6fb3c… } }], last: true, … } } dir="SENT" stream="CONTROL" quic_stream=0 file=2
Control { need: NeedChunks { file_id: 2, chunk_idx: [0] } } dir="RECEIVED" stream="CONTROL" quic_stream=0 file=2
Control { need: NeedChunks { file_id: 1, chunk_idx: [0] } } dir="RECEIVED" stream="CONTROL" quic_stream=0 file=1
```

**The data.** The client opens one unidirectional stream per file, QUIC
streams 2 and 6, and sends each chunk on it after a `DataStreamHeader`. The
two streams run side by side. The trace never prints file content: a chunk
shows as its length and the start of its BLAKE3 hash, which matches the chunk
list above.

```text
StreamEvent { kind: OPENED } dir="UNSPECIFIED" stream="DATA" quic_stream=2 file=-
DataStreamHeader { session_id: 0x88f83bf9…, file_id: 2, length: 12 } dir="SENT" stream="DATA" quic_stream=2 file=2
StreamEvent { kind: OPENED } dir="UNSPECIFIED" stream="DATA" quic_stream=6 file=-
DataChunk { data: <12 bytes, blake3 07c6fb3c62e3e041> } dir="SENT" stream="DATA" quic_stream=2 file=2
DataStreamHeader { session_id: 0x88f83bf9…, file_id: 1, length: 6 } dir="SENT" stream="DATA" quic_stream=6 file=1
StreamEvent { kind: FINISHED } dir="SENT" stream="DATA" quic_stream=2 file=2
DataChunk { data: <6 bytes, blake3 8e4c7c1b99dbfd50> } dir="SENT" stream="DATA" quic_stream=6 file=1
StreamEvent { kind: FINISHED } dir="SENT" stream="DATA" quic_stream=6 file=1
```

**Placing the files.** The daemon has verified each file against its chunk
list and asks whether it may place it. The client checks that the source has
not changed since it was hashed and says yes (`changed: false`, the default,
so not printed). The daemon then places the file and acknowledges it, and
gives back the room the chunk list took.

```text
Control { may_place: MayPlace { file_id: 1, gen: 1 } } dir="RECEIVED" stream="CONTROL" quic_stream=0 file=1
Control { may_place: MayPlace { file_id: 2, gen: 1 } } dir="RECEIVED" stream="CONTROL" quic_stream=0 file=2
Control { place_verdict: PlaceVerdict { file_id: 1, gen: 1 } } dir="SENT" stream="CONTROL" quic_stream=0 file=1
Control { place_verdict: PlaceVerdict { file_id: 2, gen: 1 } } dir="SENT" stream="CONTROL" quic_stream=0 file=2
Control { ack: FileAck { file_id: 1 } } dir="RECEIVED" stream="CONTROL" quic_stream=0 file=1
Control { list_room: ListRoom { records: 1 } } dir="RECEIVED" stream="CONTROL" quic_stream=0 file=-
Control { ack: FileAck { file_id: 2 } } dir="RECEIVED" stream="CONTROL" quic_stream=0 file=2
Control { list_room: ListRoom { records: 1 } } dir="RECEIVED" stream="CONTROL" quic_stream=0 file=-
```

**The end.** The daemon sends the session's totals, and both halves of the
control stream finish. The connection then closes with code 0, `done`.

```text
Control { done: SessionDone { files: 2, bytes: 18, created: 2, transferred_size: 18 } } dir="RECEIVED" stream="CONTROL" quic_stream=0 file=-
StreamEvent { kind: FINISHED } dir="RECEIVED" stream="CONTROL" quic_stream=0 file=-
StreamEvent { kind: FINISHED } dir="SENT" stream="CONTROL" quic_stream=0 file=-
```

The daemon's trace of the same push shows the same messages with `sent` and
`received` swapped. What each message means in full is in
[Protocol](../reference/protocol.md#control-messages); why the receiver
plans and the sender only answers is in
[The transfer algorithm](transfer-algorithm.md).
