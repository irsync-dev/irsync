# Trace and capture the protocol

Three ways to watch irsync talk, from the simplest to the most detailed:

- **The text trace** prints every protocol message to the log, one line
  each. Use it to read a session as it runs, or to `grep` a log.
- **The capture file** writes every message to a file that Wireshark opens.
  Use it to browse a session, filter it, and see the streams side by side.
- **The key log** lets Wireshark decrypt a capture of the real QUIC packets.
  Use it to see what happens below irsync: packets, loss, retransmits, flow
  control.

The trace and the capture see irsync's messages, after QUIC has decrypted
and ordered them. They work on any connection: direct, through a relay, and
in the in-process network tests. The key log works on the raw packets, and
only on a direct connection.
[Networking](../explanation/networking.md) explains the layers, and walks
through a traced push.

## The text trace

Give `-vvvvv` to either side:

```sh
irsync -avvvvv ~/Photos/ myserver:photos/
irsync daemon -vvvvv
```

Or name the trace's log target, `irsync::wire`, in `RUST_LOG`, which works at
any verbosity and overrides `-v`:

```sh
RUST_LOG=info,irsync::wire=trace irsync -av ~/Photos/ myserver:photos/
```

`irsync::wire` sits under `irsync`, so `RUST_LOG=irsync=trace` turns the
trace on too. To get irsync's trace logs without it, add `,irsync::wire=off`:

```sh
RUST_LOG=irsync=trace,irsync::wire=off irsync -av ~/Photos/ myserver:photos/
```

The trace goes to stderr with the other logs, at trace level. Each message
sent or received is one line. A line from a push, as the client logged it:

```text
2026-11-14T14:49:35.753947Z TRACE session{direction=ClientSends side="client" role="sender" peer=5d0afa5896 source_index=0 session_id="88f83bf99415409cb371c2144a9d1123" resumed=false}: irsync::wire: Control { need: NeedChunks { file_id: 1, chunk_idx: [0] } } side="SIDE_CLIENT" dir="DIRECTION_RECEIVED" stream="STREAM_KIND_CONTROL" quic_stream=0 conn=1 session=88f83bf99415409cb371c2144a9d1123 file=1
```

Before `irsync::wire:` is the usual log prefix: the time and the session's
log span. Then comes the message, in full, and these fields:

| Field | Meaning |
| --- | --- |
| `side` | Who logged it: `SIDE_CLIENT` or `SIDE_DAEMON`. |
| `dir` | `DIRECTION_SENT` or `DIRECTION_RECEIVED`, from that side's point of view. `DIRECTION_UNSPECIFIED` for a stream being opened and for a path change. |
| `stream` | `STREAM_KIND_CONTROL`, `STREAM_KIND_LISTING` or `STREAM_KIND_DATA`; see [Protocol](../reference/protocol.md#streams). |
| `quic_stream` | The QUIC stream id. |
| `conn` | The connection's number in this process, from 1. A reconnect gets a new one. |
| `session` | The session id in hex, `-` until the `HelloAck`. |
| `file` | The file id the message or the data stream is about, `-` if none. |

The message prints field by field. Fields at their default value (0, empty,
`false`, the first enum value) are left out. Hashes print in hex, paths as
text when they are valid UTF-8. File content never prints: a `DataChunk`'s
data and the literal bytes in a delta print as their length and the first 8
bytes of their BLAKE3 hash:

```text
DataChunk { data: <12 bytes, blake3 07c6fb3c62e3e041> }
```

Two more kinds of line are not messages but events: `StreamEvent` when a
stream is opened, finished or reset (with the error), and `PathChange` when
iroh moves the connection to another path, such as from the relay to a
direct address.

To follow one file, `grep` for its id, with the space after it:

```sh
grep 'irsync::wire' client.log | grep 'file=12 '
```

## The capture file

Set `IRSYNC_CAPTURE` to a file name, on the client, the daemon or both:

```sh
IRSYNC_CAPTURE=~/push.pcapng irsync -av ~/Photos/ myserver:photos/
IRSYNC_CAPTURE=~/daemon.pcapng irsync daemon
```

irsync logs a warning naming the file: `writing a protocol capture to
FILE; it holds paths and file names`. The file is pcapng, the format Wireshark saves in. An existing file is
overwritten. A daemon writes every session it serves into the one file.
Before it exits, irsync waits up to 2 seconds for the last records to reach
the disk. If another process is already writing the file, the second one
writes `<stem>.<pid>.pcapng` next to it instead, where `<stem>` is the name
without its extension: for `~/push.pcapng`, that is `~/push.<pid>.pcapng`.
The warning names that file. When a test runs the client and the daemon in
one process, both write into the same file, so each message is there twice:
once sent, once received.

### What is in it

Each packet holds one `irsync.capture.v1.Record`, a protobuf message that
exists only in the capture and is never sent. It is defined in
`proto/irsync/capture/v1/record.proto`. It holds:

- the irsync message exactly as sent or received, in the `msg` field, or a
  stream event or a path change instead;
- where it was seen: `side`, `direction`, `stream`, `quic_stream_id`,
  `session_id`, `file_id` and `connection`, as in the trace;
- `local_endpoint_id` and `remote_endpoint_id`, the two keys;
- `encoded_len`, the message's size on the wire, without the length prefix;
- `trimmed`, when something was cut out (below).

Each packet's timestamp is the time irsync wrote or read the message. Under the network tests' simulated clock it is simulated time.

### Trimming

Two things are cut so that a capture is safe to look at and Wireshark can
read it:

- **File content is never written.** A `DataChunk` keeps everything except
  its data and the literal bytes of its delta. For each piece removed,
  `trimmed.content` holds its length and the first 8 bytes of its BLAKE3
  hash.
- **Wireshark refuses a packet over 256 KiB.** A larger message, such as a
  long `ChunkListPart` or `NeedChunks`, keeps its first entries up to that
  size. `trimmed.original_len` gives its full size and
  `trimmed.dropped_entries` how many entries were dropped.

The text trace always shows the whole message, apart from file content.

### Addresses and ports

The packets carry made-up addresses and ports, so that Wireshark's
conversation and flow views keep connections and streams apart:

- The daemon is always `fd69:7273::1:0:0`.
- The client end of each connection is `fd69:7273::<n>`, where `n` is the
  connection's number in the file, counted from 1. A client that reconnects
  shows up as a new address; the record's `session_id` ties its connections
  together.
- Each QUIC stream gets the same port on both ends: its stream id plus 1,
  wrapping past 65,535. The control stream, QUIC stream 0, is port 1; the
  listing stream, 4, is port 5; the first data streams of a push, 2 and 6,
  are ports 3 and 7.

The real path, an IP address and port or a relay URL, is in the
`PathChange` records.

### Open it in Wireshark

From your clone:

```sh
tools/wireshark/open-capture.sh ~/push.pcapng
```

The script starts Wireshark with two settings, nothing saved: decode every
packet as `irsync.capture.v1.Record`, and load the `.proto` files from
`proto/`. Wireshark's own protobuf decoder does the rest; no plugin is
needed. To keep the settings, import the profile in `tools/wireshark/profile/`;
[`tools/wireshark/README.md`](https://github.com/irsync-dev/irsync/blob/main/tools/wireshark/README.md)
says how, and gives the same two settings for `tshark`.

The layout below has not yet been checked in Wireshark itself: the checks,
and the fallback if the flow graph comes out empty, are in
`tools/wireshark/README.md`.

In the packet details, each record opens up into its fields and the irsync
message inside it. Some ways in:

- **Filter.** Wireshark can turn protobuf fields into display-filter fields,
  so you can filter on a record's session, file or stream kind. The names
  follow the message's full name, something like
  `pbf.irsync.capture.v1.Record.session_id`, but check them in Wireshark:
  right-click the field in the packet details and pick Apply as Filter. If
  the fields aren't offered, look under Preferences, Protocols, Protobuf for
  the option that dissects protobuf fields as Wireshark fields.
- **Flow graph.** Statistics, Flow Graph draws the messages as arrows
  between the daemon and each client address, in time order, one column per
  address.
- **Conversations.** Statistics, Conversations lists each connection (an
  address pair) and each stream (a port pair) with its message count and
  bytes.

## The key log

The trace and the capture show irsync's messages. To see the QUIC packets
underneath, capture the real traffic and give Wireshark the TLS secrets to
decrypt it.

Set `SSLKEYLOGFILE` on the client or the daemon:

```sh
SSLKEYLOGFILE=~/irsync-keys.log irsync -av ~/Photos/ myserver:photos/
```

irsync logs a warning naming the file, `writing TLS secrets to FILE
(SSLKEYLOGFILE)`, and iroh logs one of its own. The secrets are added to the
end of the file, one block of lines per connection, in the key log format that
browsers also write (`CLIENT_HANDSHAKE_TRAFFIC_SECRET …`,
`CLIENT_TRAFFIC_SECRET_0 …` and so on). Either side's file is enough, since
both ends of a connection share its secrets.

Capture the UDP packets at the same time, with Wireshark or `tcpdump`. For a
client and a daemon on one machine, with the daemon listening on port 43210:

```sh
sudo tcpdump -i lo -w ~/quic.pcap udp port 43210
```

Open the capture and point Wireshark at the key log:

```sh
wireshark -o tls.keylog_file:"$HOME/irsync-keys.log" ~/quic.pcap
```

This has not yet been checked with irsync's traffic, so take the rest of
this section as what Wireshark should show. It should decrypt the QUIC
packets and show the handshake, the frames inside each packet, the
acknowledgements, losses and retransmits, flow control limits, and each
stream's bytes (Follow, QUIC Stream). It will not decode irsync's messages:
Wireshark only decodes stream data for protocols it knows, and `irsync/1` is
not one of them. The capture file above is the way to read the messages.

What stays opaque:

- **Relayed traffic.** Through a relay, the QUIC packets travel inside the
  relay connection's own TLS, whose keys are not in the key log. Turn
  relays off, or dial an address (see
  [Connect directly or through a relay](../how-to/direct-and-relay.md)), to
  get a direct connection.
- **Multipath.** Whether Wireshark can read and decrypt a connection that
  used several paths has not been checked yet.

## Safety

All three outputs hold things you may not want to share.

- **The trace and the capture** hold paths, file names, sizes, owner and
  group names, hashes and both endpoint ids. They never hold file content.
  The trace ends up wherever the logs go, such as the systemd journal for a
  daemon run as a service.
- **The key log** holds secrets that decrypt every connection it covers.
  Together with a packet capture of the same connection, it gives away
  everything sent, file content included.

The capture file is made readable by you only, and so is a key log file
that did not exist before. Delete
them when you are done, check them before attaching them to an issue, and
unset `SSLKEYLOGFILE` afterwards: every irsync run, and every other program
that honours the variable, keeps writing to it while it is set.
