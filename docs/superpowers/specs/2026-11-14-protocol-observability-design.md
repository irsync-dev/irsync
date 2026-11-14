# Seeing the protocol: docs, trace, capture, key log

Date: 2026-11-14. Status: approved for planning.

Makes irsync's network protocol easier for contributors to learn and to
debug. Four parts: a new book page on how the network stack is put
together, a text trace of every protocol message, a capture file that
opens in Wireshark, and support for decrypting real QUIC captures. One
small wire change rides along: the frame length prefix becomes a varint.

## Intent

Contributors who open irsync today find a precise but dense protocol
reference (`book/src/reference/protocol.md`) and no way to watch the
protocol run. Nearly every message passes through two functions,
`write_msg` and `read_msg` in `crates/irsync/src/net/frame.rs`, and
nothing prints them. The exception is `read_frame` in
`crates/irsync/src/session/engine.rs`, which reads the daemon's reply to
`Hello` as raw bytes (it tries `HelloAck`, then `Error`) for
`read_hello_reply`, `list_exports` and `session/single.rs`. It also
assumes the 4-byte prefix. It moves into `frame.rs` so these replies get
the new framing and show up in the trace and the capture.

The readers are mostly learning how irsync works, sometimes debugging it.
They should be able to:

- read one page that explains the network stack from the bottom up and
  ties each piece to the iroh facility and the standard (RFC) behind it;
- turn on a trace and see the full protobuf conversation of a session,
  with whatever session context is already at hand;
- open a session in Wireshark, as installed, with no plugin: see the
  message flow across streams over time, and use Wireshark's filters,
  flow graph, conversation views and the rest of its interactive tools;
- capture the real QUIC packets and have Wireshark decrypt them.

## Goals

1. A book page "Networking" that explains the network stack layer by
   layer, mapping iroh's facilities to concepts and RFCs.
2. A text trace of every message sent and received, in full, tagged with
   session context, through the existing logging.
3. A capture file in pcapng format that stock Wireshark decodes with two
   settings passed on its command line.
4. TLS key logging (`SSLKEYLOGFILE`) so Wireshark can decrypt a real QUIC
   capture. Built up over several iterations; the first one only has to
   produce a key log Wireshark accepts on a direct connection.
5. Frames use a protobuf varint length prefix instead of a 4-byte
   big-endian one.
6. A contributor how-to covering the trace, the capture and the key log.

## Non-goals

- Decoding irsync messages inside a real QUIC capture. That needs a
  Wireshark decoder that recognises the `irsync/1` ALPN; it is a later
  project.
- An envelope message on the wire. The wire format keeps its current
  message types per stream; bytes on the wire are to be preserved. The
  envelope exists only inside the capture file (below).
- Shrinking protocol messages so they fit in a capture packet. The
  capture trims instead.
- Long-term storage, production monitoring, or tracing iroh's own
  internals.
- Recording file contents.

## Anti-goals

- Any cost when the trace and the capture are off. No extra allocation,
  no extra encoding, no context gathered that isn't already at hand.
- Collecting expensive context for the trace ("for free" only: no extra
  lookups, no extra stat calls, no path resolution just to print it).
- Requiring a Wireshark plugin or a permanent Wireshark setup to read a
  capture.

## Constraints

- No backward compatibility. There are no users yet; the protocol and the
  state files change freely.
- Stock Wireshark's protobuf decoder (checked against Wireshark master,
  November 2026, `epan/dissectors/packet-protobuf.c`):
  - does not split a stream at length prefixes and is not registered for
    TCP; it only decodes a message something else has already cut out;
  - picks a message type from gRPC, a UDP port mapping, an HTTP URI
    mapping, or, last, the `protobuf.default_type` preference;
  - loads `.proto` files from the `protobuf.search_paths` table.
- Stock Wireshark's QUIC decoder decrypts with a key log, understands the
  multipath drafts 07 to 15, and hands stream data on only for ALPNs it
  knows (`h3`, `doq`, `roq`, …). `irsync/1` is not one, and there is no
  setting for it.
- Wireshark refuses a packet larger than 256 KiB.
- Several irsync messages can be larger than that: a `ChunkListPart` of
  32,768 records (about 1.5 MiB), a `NeedChunks` of 2^18 indices (about
  768 KiB), a `DataChunk` (up to 1 MiB of data), and possibly a
  `SessionDone` with its capped lists.

## Decided

### 1. Wire format: varint length prefix

Each frame becomes a protobuf varint length followed by the message, the
standard protobuf length-delimited form (what `writeDelimitedTo` writes
and `prost::Message::encode_length_delimited` produces). `MAX_FRAME`
stays 16 MiB and is checked before anything is allocated, as now. A
clean end of stream before the first byte of a frame is still
`Ok(None)`; an end of stream inside the prefix or the body is still
`UnexpectedEof`.

`PROTOCOL_VERSION` goes from 4 to 5. `protocol.md` changes its Framing
row and anything else that says "4-byte".

### 2. The "Networking" page

`book/src/explanation/networking.md`, title "Networking", placed in
`SUMMARY.md` right after `connections.md`. It builds the stack from the
bottom up. For each layer it says what iroh provides, which standard or
concept it rests on (with links to RFCs and iroh's docs, not copies of
them), and where irsync's code touches it. A diagram shows the stack.
Layers, at least:

- addressing and finding the other side: endpoint IDs as public keys,
  address lookup (DNS, the mainline DHT, mDNS), relays;
- getting through NAT and firewalls: hole punching, relay fallback;
- QUIC (RFC 9000) with TLS 1.3 (RFC 8446) inside it (RFC 9001), raw
  public keys instead of certificates (RFC 7250), ALPN (RFC 7301) and
  `irsync/1`, streams and flow control, the multipath extension;
- irsync's own layer: varint-framed protobuf, the control, listing and
  data streams, close codes.

`protocol.md` stays the exact reference; the new page points into it.
`connections.md` should not repeat what the new page covers; trim or
link as needed. The page ends with a walk through one real session,
using output from the text trace.

### 3. Text trace

Turned on through the existing logging, under its own target,
`irsync::wire`: with `-vvvvv` on either side, or with `RUST_LOG` naming
the target (e.g. `RUST_LOG=info,irsync::wire=trace`) at any verbosity.
Off, it costs nothing beyond the `tracing` level check.

For `-vvvvv` to mean the same on both sides, the daemon counts `-v` the
way a sync does: none or `-v` warnings, `-vv` info, `-vvv` debug,
`-vvvv` trace, `-vvvvv` trace plus every protocol message. Its default
therefore drops from info to warnings. When its stdout is a terminal,
the daemon prints a short start-up message: `irsync daemon <version>
started`, its relay and its discovery methods, before the existing
`endpoint id:` and `listening on:` lines, which stay unchanged in every
case because parent processes and tests read them.

One event per message sent or received, printing the whole message field
by field, tagged with the context already at hand where the frame is
written or read: side (client or daemon), direction (sent or received),
stream kind (control, listing, data) and QUIC stream id, session id once
known, file id where the message or stream has one.

Byte fields print in a readable form: hashes in hex, paths as text where
they are valid UTF-8. File content never prints: a `DataChunk`'s `data`
and the `literal` bytes of its `DeltaOp`s print as their length and a
short hash.

### 4. Capture file

A separate switch from the trace (they are two recordings; disk use
doesn't matter, this is for debugging). irsync writes a pcapng file.

**Container.** pcapng with Wireshark's "exported PDU" link type
(`LINKTYPE_WIRESHARK_UPPER_PDU`, 252). Each packet carries tags naming
the decoder (`EXP_PDU_TAG_DISSECTOR_NAME` = `protobuf`) and fake source
and destination addresses and ports. Wireshark calls the protobuf
decoder directly, so no port mapping is needed, and the UDP size limit
doesn't apply. Real timestamps.

**Record.** Each packet holds one capture-only message, defined in its
own `.proto` file and never sent on the wire. Every packet is this one
type, so Wireshark needs only `protobuf.default_type`. It holds:

- a `oneof` with the irsync message exactly as sent or received
  (`Hello`, `HelloAck`, `Error`, `ExportList`, `PathInfo`, `Control`,
  `ListingStreamHeader`, `DataStreamHeader`, `DataChunk`, …);
- context: side, direction, stream kind and QUIC stream id, session id,
  file id where known, and the real addresses described under "Stream
  view";
- stream events (opened, finished, reset) as their own `oneof` cases;
- trimming info when the message was cut: its original encoded size and
  how many repeated entries were dropped.

**Stream view.** Fake addresses lay the capture out for Wireshark; real
addresses are fields in the record.

- *Layout:* fake IPv6 addresses, one pair per connection. The daemon has
  a fixed address. Each connection gets its own client address, numbered
  in the order connections appear in the file, with room in the address
  for a session number too, so one session can be filtered across its
  reconnects. Within a connection, each QUIC stream gets its own port
  pair numbered from its stream id. Ports wrap past 65,535, so only a
  sync of tens of thousands of files sees some data streams share ports;
  the control and listing streams always stay separate. This keeps
  resumed sessions, several clients in one daemon file, and stream ids
  that restart per connection apart in the flow graph and conversation
  views.
- *Real addresses, as record fields:* both sides' endpoint ids and the
  connection's current path (remote IP and port, or the relay URL). Real
  addresses are not used for layout: the path changes mid-connection
  (relay to direct, several paths with multipath), relayed traffic has
  no peer IP, netsim uses made-up transport addresses, a reconnecting
  client usually keeps its IP and port, and all streams share one UDP
  socket. If iroh can't give the current path cheaply per message, the
  record carries it only when it changes (a path-change event).

**Trimming.** A `DataChunk` keeps everything but file content: its
`data` and its `DeltaOp` `literal` bytes are dropped, and their lengths
and short hashes go in the record.
Any other message whose record would exceed Wireshark's 256 KiB limit
keeps its first repeated entries up to the limit and is marked trimmed.
The text trace always has the full message.

**Opening it.** The how-to gives one command that opens a capture with
nothing configured permanently:

```sh
wireshark -o protobuf.default_type:<record type> \
  -o 'uat:protobuf_search_paths:"<repo>/proto","TRUE"' capture.pcapng
```

We also ship a Wireshark profile for anyone who wants the settings kept.

**Works everywhere** the frame layer runs: direct connections, relayed
ones, and the in-process netsim tests.

### 5. Key log

iroh's `Endpoint::builder().keylog(bool)` (in the pinned fork,
`iroh/src/endpoint.rs`) makes rustls write TLS secrets to the file named
by `SSLKEYLOGFILE`. irsync turns it on when `SSLKEYLOGFILE` is set, for
the client and the daemon. Wireshark then decrypts a real capture and
shows QUIC-level behaviour (packets, loss, retransmits, flow control,
"Follow QUIC Stream"), not decoded irsync messages.

Later iterations, as needed: check the multipath extension decrypts,
and say plainly what can't be seen (traffic through a relay is wrapped
again in the relay connection's own TLS).

### 6. How-to

A contributor page under `book/src/contributing/` covering the text
trace, the capture file (with the Wireshark command and screenshots of
the flow graph) and the key log.

## Left to the builder

These are the builder's calls; the ones marked *call* were made during
design and can be revisited.

- Names: the record message and its package and file (*call:* something
  like `irsync.capture.v1.Record` in `proto/irsync/capture/v1/`), the
  trace target, the how-to's file name.
- *Call:* the capture is turned on by an environment variable naming the
  output file (e.g. `IRSYNC_CAPTURE=/tmp/x.pcapng`), so client, daemon
  and tests work the same way without a new command-line flag. A daemon
  serving several sessions writes them all to one file.
- *Call:* one capture writer per process. When client and daemon run in
  one process (netsim and other in-process tests), both record into the
  same file, so each message appears twice, once sent and once
  received, told apart by the record's side and direction. Two
  processes never share a file: a process that finds the file already
  in use writes to `<name>.<pid>.pcapng` instead and logs where.
- *Call:* `SSLKEYLOGFILE` is honoured in every build, as is the
  convention, but irsync logs a warning naming the file when it turns
  key logging on, so a variable left set for browser debugging doesn't
  go unnoticed.
- *Call:* the capture file is created readable by its owner only, and
  turning it on logs a warning, since it holds paths and file names.
- How context reaches `write_msg` and `read_msg` (a context argument, a
  wrapper around the stream, …), as long as the off path stays free.
- How the capture writer avoids slowing the session (buffering, a
  background task), and which clock it uses under netsim's paused clock.
- Whether to write pcapng by hand or use a crate. pcapng and exported PDU
  are simple; prefer no heavy dependency.
- Whether to fill the exported PDU's protocol and info column tags with a
  short summary (e.g. "irsync", "Control.need_chunks file=12").
- The exact varint read loop, and finding any code that assumes a 4-byte
  prefix (frame-size budgets in `session/limits.rs`, test helpers).
- Tests: the varint framing, the trimming, and a session whose capture is
  read back and checked record by record. If `tshark` is on `PATH`, a
  test may also decode a capture with it; without it, such a test
  passes without checking, like `rsync_compare.rs` does without rsync.
- A quick early check, before building out the stream view, that
  Wireshark's flow graph and conversation views work on exported-PDU
  packets with fake addresses and ports. If they don't, fall back to
  fake UDP packets with the protobuf UDP port mapping for one port range.
