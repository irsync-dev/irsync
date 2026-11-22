# Trace and capture the protocol

Four ways to watch irsync talk, from the simplest to the most detailed:

- **The text trace** prints every protocol message to the log, one line
  each. Use it to read a session as it runs, or to `grep` a log.
- **The capture file** writes every message to a file that Wireshark opens.
  Use it to browse a session, filter it, and see the streams side by side.
- **The byte count** reads a capture file and says what each message and
  field cost in bytes. Use it to see where a session's bytes go.
- **The key log** lets Wireshark decrypt a capture of the real QUIC packets.
  Use it to see what happens below irsync: packets, loss, retransmits, flow
  control.

The trace, the capture and the byte count see irsync's messages, after
QUIC has decrypted and ordered them. They work on any connection: direct,
through a relay, and in the in-process network tests. The key log works on
the raw packets, and only on a direct connection.
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

The decoding and the flow graph below were checked with tshark 4.6. On
Ubuntu, tshark cannot read `.proto` files in your home directory and shows
every packet as `<UNKNOWN>`; `tools/wireshark/README.md` gives the
workaround. The Wireshark GUI is not affected.

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

## Count the bytes

The capture shows what each message holds. `pbwalk` shows what each part of
it costs. It reads a capture file and lists every protobuf field of every
message with its size in bytes, then adds up the totals: per message type,
per field, and per kind of overhead. Use it to see where the bytes of a
session go, or to check what a protocol change saves.

It counts irsync's messages as they are on the wire, with their length
prefixes. It leaves out what QUIC, TLS and UDP add below that. File content
is not in a capture, but its size is still counted: `pbwalk` takes it from
the record's `encoded_len` and `trimmed` fields, and prints it as cut.

It needs only the clone:

```sh
cargo run -p pbwalk -- ~/push.pcapng
```

First comes a listing, one block per message. This is a `Hello`, with its
last fields cut for space, from a capture of a small push:

```text
#3 0.000188 fd69:7273::1 → fd69:7273::1:0:0 port 1  Hello  38 B + 1 B prefix
   side=SIDE_CLIENT(1) direction=DIRECTION_SENT(1) stream=STREAM_KIND_CONTROL(1) connection=1 local_endpoint_id=628d53b962da7234… remote_endpoint_id=9af7fe23a076f62b…
  protocol_version = 5                                     2 B  (tag 1, value 1)
  path = "m"                                               3 B  (tag 1, len 1, value 1)
  chunk_min = 65536                                        4 B  (tag 1, value 3)
  chunk_avg = 262144                                       4 B  (tag 1, value 3)
  chunk_max = 1048576                                      4 B  (tag 1, value 3)
  contents_only = true                                     2 B  (tag 1, value 1)
  preserve: Hello.Preserve                                19 B  (tag 2, len 1, value 16)
    links = true                                           2 B  (tag 1, value 1)
    ...
```

Each line is a field, its value, and its size. The size is the sum of its
tag (which holds the field number and the kind of value), its length (for
text, bytes and nested messages) and its value. In `path = "m"`, the
tag takes 1 byte, the length 1 byte and the text 1 byte, so 3 B. A nested
message such as `preserve` counts its own tag and length as well as
everything inside it. The first line, after the message's type, gives its
size on the wire: 38 B, plus 1 B for the length prefix in front of it.

Then come the totals. For the client's side of a small push, shortened:

```text
Totals: 41 messages and 13 events of 108 packets read; 901,120 B on the wire, length prefixes (50 B) included

By message
  type                          count             bytes   share
  DataChunk                         3         900,012 B   99.9%
  Control                          32             883 B    0.1%
  DataStreamHeader                  3              72 B   <0.1%
  HelloAck                          1              47 B   <0.1%
  Hello                             1              38 B   <0.1%
  ListingStreamHeader               1              18 B   <0.1%
  total                            41         901,070 B  100.0%
```

```text
By field
  path                                                  bytes   share
  DataChunk (cut content)                           900,000 B   99.9%
  Control.chunk_list_part.chunks.hash.bytes             102 B   <0.1%
  Control.chunk_list_part.whole_hash.bytes              102 B   <0.1%
  ...
  DataChunk (cut, not itemised)                          12 B   <0.1%
  ...
  total                                             901,070 B  100.0%
```

```text
Overhead
                                       bytes   share
  not in the capture               900,012 B   99.9%
  values                               715 B    0.1%
  tags                                 250 B   <0.1%
  length bytes                          93 B   <0.1%
  length prefixes                       50 B   <0.1%
  total                            901,120 B  100.0%
```

`Overhead` splits the same bytes by what they are: the values themselves,
the tags, the length bytes, the length prefixes, and what the capture
does not hold.

### Cut rows

A `DataChunk` in the listing shows what the capture left out:

```text
#59 0.018612 fd69:7273::1 → fd69:7273::1:0:0 port 3  DataChunk  300,004 B + 3 B prefix
   side=SIDE_CLIENT(1) direction=DIRECTION_SENT(1) stream=STREAM_KIND_DATA(3) quic_stream_id=2 session_id=b9db550af3b718a1335d80f16246cd08 file_id=3 connection=1 local_endpoint_id=628d53b962da7234… remote_endpoint_id=9af7fe23a076f62b…
  cut content                                        300,000 B  (1 piece)
  cut, not itemised                                        4 B
```

Four rows can appear, and each is counted in the totals:

- **`cut content`**: file content the capture never writes. The size is
  exact. A message that had content cut shows how many pieces.
- **`cut, not itemised`**: the rest of what the capture cut. It is the tag
  and length bytes of the cut fields. If a message was too big for
  Wireshark and entries were dropped, it also holds their size, and the
  row says how many were dropped.
- **`message not captured`**: the whole message is missing from the
  capture. Its full size is this one row.
- **`not in the capture, unexplained`**: bytes are missing and the record
  does not say why. This is not a cut. It can happen when a newer peer sent
  fields this build does not know.

### One side or both

A capture written by a client or by a daemon holds each message once.
A file that both wrote, as the tests do when they run client and daemon in
one process, holds each message twice: once sent, once received. Its totals
count every message twice. Pick one side:

```sh
cargo run -p pbwalk -- --where side=SIDE_CLIENT ~/push.pcapng
```

`--where FIELD=VALUE` keeps the records whose field of the capture's
wrapper prints as that value. Give it more than once to narrow further.
The fields it takes are those on the second line of each message's block:
`side`, `direction`, `stream` and so on. `encoded_len`,
`trimmed` and the message itself are not among them.

### More views

- `--group-by stream` adds totals per stream kind (control, listing, data).
  `--group-by side,stream` adds one table per field, not one table of the
  two combined. It takes the same fields as `--where`. `direction` adds
  nothing, since the totals always have a `By direction` table.
- `--totals-only` skips the listing; `--no-totals` skips the totals.
- Long byte strings, long repeated fields and long header values are
  shortened in the listing. `--full` prints them whole.
- `-C` puts each field's bytes in front of its line; see
  [See the bytes](#see-the-bytes).

The `By stream` totals of the same push:

```text
By stream (with prefixes)
  stream                        count             bytes   share
  STREAM_KIND_DATA(3)               6         900,096 B   99.9%
  STREAM_KIND_CONTROL(1)           27             701 B    0.1%
  STREAM_KIND_LISTING(2)            8             323 B   <0.1%
  total                            41         901,120 B  100.0%
```

### See the bytes

`-C`, or `--canonical-hex-dump`, puts the bytes of each field in front of
its line: first in hex, 16 to a line, then the same bytes as characters,
with a `.` for every byte that is not a printable ASCII character. It is
the layout of `hexdump -C` without the offsets. A `HelloAck`:

```sh
cargo run -p pbwalk -- -C --no-totals ~/push.pcapng
```

```text
                                                                      #7 0.010483 fd69:7273::1:0:0 → fd69:7273::2 port 1  HelloAck  47 B + 1 B prefix
                                                                         side=SIDE_DAEMON(2) direction=DIRECTION_SENT(1) stream=STREAM_KIND_CONTROL(1) …
0a 10 b9 db 55 0a f3 b7  18 a1 33 5d 80 f1 62 46  |....U.....3]..bF|    session_id = b9db550af3b718a1335d80f16246cd08           18 B  (tag 1, len 1, value 16)
cd 08                                             |..|
18 05                                             |..|                  protocol_version = 5                                     2 B  (tag 1, value 1)
22 19                                             |".|                  build: BuildInfo                                        27 B  (tag 1, len 1, value 25)
0a 0c 30 2e 31 2e 30 2d  62 65 74 61 2e 36        |..0.1.0-beta.6|        version = "0.1.0-beta.6"                              14 B  (tag 1, len 1, value 12)
12 07 63 39 64 35 64 64  65                       |..c9d5dde|             commit = "c9d5dde"                                     9 B  (tag 1, len 1, value 7)
18 01                                             |..|                    dirty = true                                           2 B  (tag 1, value 1)
```

A field with more than 16 bytes, such as `session_id` with its 18,
continues on the next line. A nested message such as `build` shows only
its own two bytes, the tag and the length; the fields under it show the
rest. So no byte is printed twice and none is left out: the hex of one
block, read from top to bottom, is the message. Lines that stand for no
bytes in the file, such as a block's first two lines and the `cut content`
row, leave both columns empty. `--full` changes the text only; the hex is
always complete.

The hex column is the first 48 characters of each line, so the bytes can
be had back:

```sh
cargo run -p pbwalk -- -C --no-totals ~/push.pcapng | cut -c1-48 | xxd -r -p > messages.bin
```

That gives every message of the capture, one after the other; narrow it
with `--where`. `--no-totals` is needed, since the totals are not
indented. `-C` cannot be combined with `--totals-only`.

Two things the hex is not:

- **The message as it was on the wire, when the capture cut something.**
  The bytes are those in the file. File content, and the entries dropped
  from a message over 256 KiB, are not there (see [Trimming](#trimming)).
- **The length prefix.** It is not in the file; `pbwalk` works its size
  out. The hex of a block starts with the message's first field.

With `--no-unwrap`, a block is the whole packet, and so is its hex.

### Other protocols

`pbwalk` knows nothing of irsync except its default. `--proto-path DIR`
loads every `.proto` under `DIR`, and `--type NAME` says which message each
packet holds. `--prefix none` is for protocols without a length prefix.

A capture wraps each message in an envelope, and `pbwalk` unwraps it by
the numbering of the envelope's fields:

- 1 to 19 are the wrapper's own: where the message was seen, `encoded_len`
  and `trimmed`.
- 20 to 39 hold the message.
- 40 and up are events, such as a stream opening, which are listed but
  count as 0 bytes.

`--no-unwrap` skips that and walks each packet as `--type` from the top,
sizes being those of the bytes in the file. It cannot be combined with
`--where` or `--group-by`, which need the envelope unwrapped. The file
format and the envelope are in `crates/pbcap`.

### Exit status

`pbwalk` exits with 0 when every message adds up, 1 when a message does not
(what is present, cut and missing does not equal its size on the wire) or
when it stops reading early, because the file is cut off or a block in it
is broken, and 2 when it cannot read the file or the protos. With status 1
it still prints its listing and totals, and says on stderr which problem
it found.

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
sudo tcpdump -i lo -U -w /tmp/quic.pcap udp port 43210
```

`tcpdump` gives up root before it writes, so write to `/tmp`: the
`tcpdump` user cannot write to your home directory. Open the capture and
point Wireshark at the key log:

```sh
wireshark -o tls.keylog_file:"$HOME/irsync-keys.log" /tmp/quic.pcap
```

Wireshark finds the QUIC packets by itself. With the key log it decrypts the
handshake and the client's packets: the frames inside each packet, the
acknowledgements, flow control limits and the stream frames. Follow, QUIC
Stream shows a stream's bytes. On the control stream, stream 0, they start
with the first message's length prefix and the `Hello`. Wireshark does not
decode the messages themselves, since it does not know `irsync/1`. The
capture file above is the way to read them.

What stays opaque:

- **Frames Wireshark does not know.** iroh's QUIC library, `noq`, speaks
  QUIC multipath (frames `0x3e`, `0x3f`, `0x3e75` to `0x3e7c`) and iroh's
  own NAT traversal (frames `0x3d7f90` to `0x3d7f94`). Wireshark 4.6 knows
  neither. It cannot skip a frame it does not know, so it misreads the rest
  of that packet: expect `Unknown` frames with random-looking types and
  `[Malformed Packet]` ACKs after one of these.
- **Most packets from the server side.** Each side hands out its connection
  IDs in multipath `PATH_NEW_CONNECTION_ID` frames, not the standard ones,
  so Wireshark cannot tie packets sent to those IDs to the connection. It
  marks them "Unknown QUIC connection" and does not decrypt them. In a
  checked push, the client's packets on the main path all decrypted, and of
  the daemon's, only the first two did; the rest were mostly
  acknowledgements.
- **Probes to other addresses.** NAT traversal tries the other side's other
  local addresses too, so a capture on one interface can miss packets, and
  one on `lo` can show packets between `127.0.0.1` and the machine's LAN
  or Docker addresses.
- **Relayed traffic.** Through a relay, the QUIC packets travel inside the
  relay connection's own TLS, whose keys are not in the key log. Turn
  relays off, or dial an address (see
  [Connect directly or through a relay](../how-to/direct-and-relay.md)), to
  get a direct connection.

Checked with tshark 4.6.4 and irsync 0.1.0-beta.1, client and daemon on one
machine, dialled by address.

On Ubuntu, `tshark` (not the Wireshark GUI) reads only files you own in
`/tmp`: copy the capture and the key log there first, as your own user.

## Safety

All these outputs hold things you may not want to share.

- **The trace and the capture** hold paths, file names, sizes, owner and
  group names, hashes and both endpoint ids. They never hold file content.
  The trace ends up wherever the logs go, such as the systemd journal for a
  daemon run as a service.
- **`pbwalk`'s output** holds what the capture holds.
- **The key log** holds secrets that decrypt every connection it covers.
  Together with a packet capture of the same connection, it gives away
  everything sent, file content included.

The capture file is made readable by you only, and so is a key log file
that did not exist before. Delete
them when you are done, check them before attaching them to an issue, and
unset `SSLKEYLOGFILE` afterwards: every irsync run, and every other program
that honours the variable, keeps writing to it while it is set.
