# pbwalk: where every byte of a session goes

Date: 2026-11-22. Status: approved for planning.

Adds a command-line tool, `pbwalk`, that reads a protocol capture and
prints every message with every protobuf field and its exact size in
bytes, then totals by message and field, and by any wrapper field asked
for, such as the stream. To make that possible
without tying the tool to irsync, the capture file format moves out of
`irsync` into a small shared crate, `pbcap`.

## Intent

Mike wants to find out where exactly every byte of an irsync session goes
on the network: which messages, which fields, how much is payload and how
much is protobuf's own tags and lengths.

Today there are two ways to look at a session's messages
(`book/src/contributing/protocol-debugging.md`): the text trace and the
`IRSYNC_CAPTURE` file opened in Wireshark. Both show values. Neither shows
sizes or adds anything up.

This is about irsync's own messages, the bytes irsync hands to QUIC. What
QUIC, TLS and UDP add on top is out of scope (decided: "message level").
File content is cut out of captures; that is fine (decided), as long as
its size is still counted.

## Goals

- One command turns a capture file into a per-message, per-field byte
  account, with totals.
- The account is exact. For every message, the listed sizes add up to the
  message's size on the wire. What the capture cut out is counted from the
  lengths the capture records, and shown as cut.
- `pbwalk` holds no irsync-specific code. It reads `.proto` files at
  startup when asked, and has irsync's protos built in as the default.
- The capture file format (pcapng with Wireshark's "exported PDU" header)
  lives in one crate that both `irsync` (writer) and `pbwalk` (reader)
  link.
- By default `pbwalk` silently unwraps the capture's wrapper record and
  shows the message inside. A flag turns that off.

## Non-goals

- Real packets: QUIC, TLS, UDP overhead, retransmits, relays.
- Recovering file content.
- Using tshark. Wireshark stays the tool for browsing a capture; nothing
  about it changes.
- Changing the bytes irsync writes to a capture file. Old captures stay
  readable, by Wireshark and by `pbwalk`.
- Shipping `pbwalk` in releases. It is a contributor tool, built from the
  clone like `filegen`.
- Other input formats (a plain file of length-prefixed messages, JSON
  output). The code should not make them hard to add later.

## Anti-goals

- Totals that look exact but are not. If something cannot be attributed,
  it gets its own labelled row; it is never folded into another row or
  left out.
- A wrapper record so generic that Wireshark can no longer decode the
  message inside it (see Approaches, the wrapper).
- A panic or a huge allocation on a damaged or hostile capture file.
  People attach captures to issues.

## Constraints

- Wireshark's protobuf decoder only decodes a nested message whose type
  is named in the `.proto`. Tested with tshark 4.6.4: a payload carried as
  `google.protobuf.Any` shows as raw bytes, and there is no setting to
  change that. So irsync's `Record` must keep naming irsync's messages.
- `protox` (reads `.proto` files, no `protoc` needed) and `prost-reflect`
  (message and field definitions at run time) are already workspace
  dependencies. No new dependencies are expected.
- `prost-reflect` does not report byte offsets when it decodes. `pbwalk`
  needs its own walker over the encoded bytes.
- The repo's rules apply: `#![forbid(unsafe_code)]`, workspace lints,
  `tools/ci.sh check` passes, work goes through a PR.

## Approaches

### The tool

1. **Thin wrapper around `tshark -V`.** About 20 lines of shell. Shows
   values, not sizes, and no totals. Does not answer the question.
2. **tshark's XML output plus a Python script.** tshark's `-T pdml`
   reports each field's offset and size, so about 200 lines of Python
   could add them up. But the XML is about 1 KB per field, so a real
   session is slow and huge; it needs tshark and, on Ubuntu, the `/tmp`
   workaround for tshark's AppArmor profile; CI could only test it where
   tshark is installed; and it would be irsync-only.
3. **Our own Rust tool (picked).** Reads the capture file and walks the
   protobuf bytes itself. About 600 lines plus tests. Needs nothing
   installed, streams through big captures, is tested in CI on every
   platform, and works for any protobuf capture.

Mike picked 3 after asking for it to be explored: the tool can be general
and read `.proto` files at startup, and big captures decide against 2.

### The wrapper

`pbwalk` has to tell the wrapper from the message, and know the message's
true size.

1. **A fully generic record**, payload as `Any` or as bytes plus a type
   name. Breaks the Wireshark view (see Constraints). Rejected.
2. **Knowledge of irsync's `Record` inside `pbwalk`.** Works, but makes
   the tool irsync-only.
3. **A minimal envelope convention (picked).** `pbcap` defines the few
   wrapper fields a reader needs, at fixed field numbers. A project's own
   record keeps those numbers and adds whatever else it likes. irsync's
   `Record` already fits, so nothing in it changes.

Mike confirmed 3: file format and a minimal envelope in `pbcap`; `Record`
stays in irsync's protos.

## Design

### Crate `pbcap` (new, `crates/pbcap`)

A library. Three things:

**The file format.** `crates/irsync/src/net/tap/pcapng.rs` moves here
with its tests: the pcapng blocks, the exported-PDU tags, and
`exported_pdu(src, dst, src_port, dst_port, body)`. The protocol column
text, today fixed to `irsync`, becomes a parameter. New: a reader that
yields, for each packet, its timestamp, source and destination address,
ports, the other tags, and the body bytes (irsync's capture tests check
the raw tags). The reader accepts what the writer writes
(little-endian section, link type 252, Enhanced Packet Blocks), skips
block types it does not know, and returns an error, not a panic, on
anything malformed. It reads packet by packet, not the whole file at once.

**The packet size limit.** Wireshark refuses a packet over 256 KiB. The
constant behind `MAX_RECORD` in `capture.rs` moves here; irsync uses it
from here.

**The envelope.** A proto file inside the crate (as `fileset` keeps its
own), `pbcap.v1`:

```proto
// What a reader needs from a capture's wrapper record. A project's own
// record uses these numbers for these fields and adds its own.
message Envelope {
  Direction direction = 2;
  uint32 encoded_len = 10;   // the message's size on the wire, no prefix
  Trimmed trimmed = 11;      // set when anything was cut
}
enum Direction { DIRECTION_UNSPECIFIED = 0; DIRECTION_SENT = 1; DIRECTION_RECEIVED = 2; }
message ContentSummary { uint32 len = 1; bytes blake3_prefix = 2; }
message Trimmed {
  uint32 original_len = 1;
  uint32 dropped_entries = 2;
  repeated ContentSummary content = 3;
}
```

Plus the numbering rule, written down in the proto file and the crate
docs:

- 1 to 19: the wrapper's own fields. 2, 10 and 11 are as above; the rest
  are the project's.
- 20 to 39: the message. Exactly one message-typed field, the one that
  was sent or received.
- 40 and up: events. Message-typed fields for things the recorder saw
  that were never on the wire (irsync: `StreamEvent`, `PathChange`).

Because protobuf skips fields it does not know, decoding any conforming
record as `Envelope` yields just those three fields. That is how `pbwalk`
reads them.

irsync's `Record` and its `Direction`, `Trimmed` and `ContentSummary`
stay where they are, unchanged. A new test fails if `Record` stops
fitting the envelope: fields 2, 10 and 11 have the same types and shapes,
every field from 20 up is a message, and the messages irsync sends sit in
20 to 39.

What stays in `irsync`: the recorder (file locking, the writer thread),
the made-up addresses and ports, and `scrub` and `fit`. They know irsync's
messages or irsync's conventions.

### Crate `pbwalk` (new, `crates/pbwalk`)

A library and a binary, so tests can call the walker directly.

```text
pbwalk [OPTIONS] FILE.pcapng

  --proto-path DIR   load every .proto under DIR at startup, instead of
                     the built-in ones; may be given more than once
  --type NAME        the type of each packet
                     [default: irsync.capture.v1.Record]
  --no-unwrap        show each packet as it is in the file, wrapper
                     included; no envelope handling
  --prefix KIND      how a message is framed on the wire: varint or none
                     [default: varint]
  --where F=V        only records whose wrapper field F prints as V; may
                     be given more than once
  --group-by F,...   extra wrapper fields to break the totals down by
  --totals-only      skip the per-message listing
  --no-totals        skip the totals
  --full             print long repeated fields and long byte strings in
                     full
```

The built-in protos are `irsync_proto::FILE_DESCRIPTOR_SET`. That one
default is the only tie to irsync.

**The walker.** Given the bytes of a message and its type, it visits
every field in the order it appears and reports, for each: its path, its
name and type from the `.proto`, its value, and its size split into tag
bytes, length bytes and value bytes. It recurses into nested messages.
A packed repeated field is one field with its element count. A field
number the `.proto` does not have is reported by number and wire type,
with its size. Bytes that do not parse are reported as "N bytes not
decodable" at the point they start; the walker never panics and never
reads past the message. Nesting depth is capped.

**Unwrapping (the default).** For each packet:

1. Decode the envelope: `direction`, `encoded_len`, `trimmed`.
2. Find the payload: the set field numbered 20 or up. 20 to 39 is a wire
   message, 40 and up an event.
3. Walk the payload with its type from the `.proto`. The wrapper's own
   bytes are not listed or counted.
4. The wrapper's other fields print on the message's header line as
   `name=value`, generically, from the `.proto`. So do the packet's time,
   addresses and port.
5. A wire message's size on the wire is `encoded_len`, plus the length
   prefix: the size of `encoded_len` as a varint, or nothing with
   `--prefix none`.
6. What the capture cut is the difference between `encoded_len` and the
   payload's size in the file. Of that, the sum of `trimmed.content[].len`
   is listed as "cut content"; the rest is listed as "cut, not itemised"
   (the tags and lengths of the cut fields, and entries dropped to fit
   256 KiB, with `dropped_entries` as their count). If the payload is
   gone altogether, the whole `encoded_len` is one "message not captured"
   row. If there is a difference and `trimmed` is not set, it is listed
   as "not in the capture, unexplained", never as cut. That happens when
   a newer peer sent fields this build does not know: irsync stores a
   received message re-encoded, without them.
7. An event prints, and counts as 0 bytes.

With `--no-unwrap`, each packet is walked as `--type` from the top,
sizes are of the bytes in the file, and steps 1 to 7 do not apply.

**The listing.** One block per message; a sketch, exact layout left to
the builder:

```text
#6 0.012345 fd69:7273::2 → fd69:7273::1:0:0 port 1  Hello  38 B + 1 B prefix
   side=SIDE_DAEMON direction=DIRECTION_RECEIVED stream=STREAM_KIND_CONTROL connection=2
  protocol_version = 5           2 B  (tag 1, value 1)
  path = "m"                     3 B  (tag 1, len 1, value 1)
  preserve                      18 B  (tag 2, len 1, value 15)
    links = true                 2 B  (tag 1, value 1)
    ...
#41 ...  DataChunk  262,163 B + 3 B prefix
  offset = 0 ...
  cut content               262,144 B  (1 piece)
  cut, not itemised               4 B
```

Byte strings print as text when they are printable UTF-8, else as hex.
Long repeated fields and long byte strings are shortened to their first
few entries or bytes and a count, unless `--full`. Sizes are never
shortened.

**The totals.** Printed after the listing:

- by message type: count, bytes, share;
- by field path, such as `Control.plan.items.path`: bytes, share, with
  the cut rows shown as `DataChunk (cut content)` and so on;
- by overhead: length prefixes, tags, length bytes, against value bytes;
- by direction, and by each `--group-by` field.

Totals per stream are not on by default: `stream` is irsync's own wrapper
field, and `pbwalk` does not know it. `--group-by stream` gives them, and
the book's example uses it.

Totals count every record that passes `--where`, and show sent and
received separately, then their sum. A capture written by one side holds
each message once: what it sent and what it received. A file that both
sides wrote into (the in-process tests) holds each message twice, and the
sum is double; there, `--where side=SIDE_CLIENT` gives one side's view.
`pbwalk` cannot tell the two cases apart without knowing irsync, so it
does not try; the book says which to use.

**The check that keeps it honest.** For every wire message, the sizes of
everything listed, cut rows included, equal `encoded_len`. `pbwalk`
verifies this as it goes and, if it fails, says so on that message and
exits non-zero at the end. A wrapper whose `encoded_len` is smaller than
the payload found is reported the same way.

### Changes in `irsync`

- `net/tap/pcapng.rs` goes; `capture.rs` and the tests that parse capture
  files use `pbcap`.
- No change to what is written. The existing capture tests, including the
  tshark check, pass unchanged.

### Tests

- `pbcap`: the moved writer tests; reader round-trips what the writer
  writes; reader rejects truncated and malformed files without panicking.
- `pbwalk`, walker: for messages of every irsync type, the listed sizes
  add up to the encoded length; unknown fields, packed fields, nested
  messages, and garbage input each have a test.
- `pbwalk`, unwrapping: a `DataChunk` with cut content, a message trimmed
  to fit, a message dropped altogether, and an event each account as
  described above.
- End to end: the capture that `crates/irsync/tests/capture.rs` already
  produces goes through `pbwalk`; the honesty check passes and the total
  equals the sum of `encoded_len` plus prefixes computed from the records
  directly.
- A fuzz target for the pcapng reader and the walker, with seeds, next to
  the existing ones in `fuzz/`.

### Docs

- `book/src/contributing/protocol-debugging.md` gets a section on
  counting bytes with `pbwalk`: how to run it, how to read the listing
  and totals, what "cut" rows mean, and the one-side or both-sides note.
  The page's opening list of "three ways" is updated.
- `tools/wireshark/README.md` points to it in a line.
- Any page that names `net/tap/pcapng.rs` is updated.

## Left to the builder

- Module layout inside both crates, type names, error types.
- The exact text layout of the listing and totals, column widths, how
  numbers are grouped, where long values are cut off.
- How `--where` compares values (the printed form is enough).
- Whether the end-to-end test lives in `irsync` (dev-depending on
  `pbwalk`) or in `pbwalk`.
- Whether the compatibility test lives in `irsync` or `irsync-proto`.
- The depth cap and other limits on hostile input.

## Calls made while writing this up

These were not discussed; each is a proposal to check.

1. Events are told from wire messages by field number (20 to 39 against
   40 and up). irsync's numbering already fits. `encoded_len == 0` would
   not do: an all-defaults message really is 0 bytes.
2. Totals count all records and show sent and received separately,
   rather than counting sent records only as first proposed. Sent-only
   would lose half of a one-side capture, which is the usual kind.
3. Cut content is totalled per message type, not per field. The capture
   records how long each cut piece was, not which field it came from.
   Recording the field would mean changing what irsync writes.
4. `scrub`, `fit` and the recorder stay in `irsync`; only the file format
   and the size limit move.
5. The envelope's proto file lives in `crates/pbcap`, not in the
   top-level `proto/`, since no irsync proto imports it and Wireshark
   does not need it.
6. A fuzz target is part of the work.
7. `--prefix`, `--where`, `--group-by`, `--totals-only`, `--no-totals`
   and `--full` are the proposed flags beyond the three discussed
   (`--proto-path`, `--type`, `--no-unwrap`).
8. Totals per stream need `--group-by stream`; they are not a default.
   A default would be a second tie to irsync.
