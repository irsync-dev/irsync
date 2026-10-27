# irsync wire protocol (version 1)

The protocol and the on-disk state (index, session records) change without
compatibility paths until the first release: there are no deployed peers to
keep talking to, so the version number stays 1 through those changes. A
peer with another version is refused with `VERSION_MISMATCH`.

The protobuf definitions in [`proto/irsync/v1/`](../proto/irsync/v1/)
(`common.proto`, `control.proto`, `data.proto`, `session.proto`) are
**normative**. This page only summarises the constants and the model; where it
disagrees with the `.proto` files, they win.

## Constants

| Item | Value |
| --- | --- |
| ALPN | `irsync/1` |
| `PROTOCOL_VERSION` | `1` (carried in `Hello.protocol_version`) |
| Framing | 4-byte big-endian length, then that many bytes of protobuf |
| `MAX_FRAME` | 16 MiB; a larger length is a protocol error, nothing is allocated for it |
| Content hash | BLAKE3 (file hashes and chunk hashes, 32 bytes) |
| Chunking | FastCDC 2020, level 1; minimum 64 KiB, average 256 KiB, maximum 1 MiB |

The chunking parameters are part of the handshake (`Hello.chunk_min/avg/max`);
a peer with different parameters is refused with `VERSION_MISMATCH`. A file's
chunk list and whole-file hash are a pure function of its bytes and these
parameters.

## Streams

One QUIC connection carries:

- a **control stream**: the first bidirectional stream, opened by the client.
  It carries `Hello`, the file listing, plan, acknowledgements, completion,
  `Error` and `Heartbeat` messages, each as one framed protobuf. A
  `Heartbeat` is empty and means "I am busy": a side sends one every 500 ms
  in which it made progress the peer cannot see (hashing, writing,
  finalizing), so the peer's inactivity watchdog stays quiet; an idle side
  sends none, so a stuck peer still times out. It is additive within
  version 1: a peer that predates it decodes an empty `Control` and ignores
  it.
  A chunk list travels as a sequence of `ChunkListPart` frames of at most
  32,768 records each, in order, the last one flagged; an empty file's list
  is one empty last part. Every part repeats the file id, size, whole-file
  hash and total record count; the receiver reassembles them and treats a
  sequence that skips, repeats a different file or stops short as that
  file's error. A receiver refuses a list that announces more than 2^24
  records, or more than its size allows at the minimum chunk length. No
  list is too long for the control stream (an earlier draft sent each list
  as one frame, which capped a file at about 342,000 chunks).
  A `PlanItem` of action `APPEND` names the length of the prefix the
  receiver holds and trusts (`append_from`); the sender's chunk list for it
  covers only the tail from there and carries no whole-file hash (every
  other list carries one). `Hello` carries the receiver-side options the
  daemon honours when it receives, among them `inplace` and `append`.
  Before it asks for a chunk of a `DELTA` file, the receiver may send a
  `ChunkBasis` for it: block signatures (a 32-bit rolling sum and 8 bytes
  of BLAKE3 per block, blocks of 512 B to 4 KiB, at most 8192 of them)
  over a region of its old file, named by its offset in that file. The
  `NeedChunks` naming the chunk follows the basis on the same ordered
  stream. A basis is good for that one request; a chunk asked for again
  is sent raw. A malformed basis is dropped by the sender, not a protocol
  error.
- **data streams**: unidirectional streams from sender to receiver, one per
  byte range of a file. Each starts with a `DataStreamHeader` (session id, file
  id, offset, length, first chunk index) followed by `DataChunk` frames of at
  most 1 MiB, one chunk per frame. Many are open at once; this is what lets a
  transfer fill the link. A chunk the receiver offered a basis for may
  arrive as `DataChunk.ops` instead of `data`: a list of literals and
  copies (`offset`, `len` in the receiver's old file) that add up to the
  chunk, which the receiver rebuilds and verifies against the chunk's hash
  like any other chunk. The sender uses operations only when they are
  clearly smaller than the bytes.

Authorization happens on the verified peer key before any stream is read.

## Sessions and resume

A session is one logical sync job with a random 128-bit id, a plan and
progress, both persisted on each side.

1. A new session starts with `Hello` having an empty `session_id`; the daemon
   answers with `HelloAck`, carrying the id it assigned. Both messages carry
   the sender's `BuildInfo` (package version, short git commit, dirty flag),
   which the other side logs at info once the session is up.
2. After a connection loss the client redials and sends `Hello` with the same
   `session_id`. Both sides continue from their persisted progress; only
   unverified chunks are sent again.
3. The daemon refuses a resume from any `EndpointId` other than the one that
   started the session, and answers `Error{BAD_SESSION}` when it no longer
   knows the id (for example after dormant-session garbage collection). The
   client then starts a fresh session.

Error codes: `UNKNOWN`, `UNAUTHORIZED`, `NO_SUCH_EXPORT`, `READ_ONLY`,
`BAD_PATH`, `VERSION_MISMATCH`, `BAD_SESSION`, `INTERNAL`.

The daemon checks the key's grant on the named export before it looks at
the export itself. A name the key may not use always gets
`UNAUTHORIZED`, whether or not such an export exists and whatever the
direction or path, so a key granted one export cannot probe for the
names of others. `NO_SUCH_EXPORT`, `BAD_PATH` and `READ_ONLY` are only
ever sent to a key whose grant covers the name (in practice a `*`
grant, for `NO_SUCH_EXPORT`).

## Cross-implementation check

`crates/irsync-proto/tests/golden/chunk_1mib.json` is a golden vector: the
whole-file hash and the exact chunk boundaries (offset, length, BLAKE3 hash)
for a fixed 1 MiB input (generated by the xorshift64 generator seeded with
`0x9E3779B97F4A7C15`, see `crates/irsync-proto/tests/chunk.rs`). An independent implementation must reproduce it
exactly to interoperate, because chunk boundaries decide which chunks two
peers consider equal.
