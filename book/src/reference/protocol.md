# Protocol

The protobuf definitions in
[`proto/irsync/v1/`](https://github.com/irsync-dev/irsync/tree/main/proto/irsync/v1)
(`common.proto`, `control.proto`, `data.proto`, `session.proto`) are
normative. This page summarises the constants, the streams and the messages;
where it disagrees with the `.proto` files, they win. The Rust types are
generated from them in the `irsync-proto` crate.

The protocol and the on-disk state (index, session records) change without
compatibility paths until the first release. A client that speaks another
version is refused with `VERSION_MISMATCH`.

## Constants

| Item | Value |
| --- | --- |
| ALPN | `irsync/1` |
| `PROTOCOL_VERSION` | `3`, carried in `Hello.protocol_version` and `HelloAck.protocol_version` |
| Framing | A 4-byte big-endian length, then that many bytes of one protobuf message |
| `MAX_FRAME` | 16 MiB. A longer length is refused before anything is allocated for it. |
| Content hash | BLAKE3, 32 bytes, for chunks and whole files |
| Chunking | FastCDC 2020, normalisation level 1; minimum 64 KiB, average 256 KiB, maximum 1 MiB |
| Chunk list part | At most 32,768 records |
| Chunk list | At most 2^24 records |
| List room | 2^24 records per connection, across all files |
| `NeedChunks` | At most 2^18 chunk indices per message |
| Chunk basis | Blocks of 512 B to 4 KiB, at most 8,192 blocks; at most 32 MiB of bases held per session |
| `DataChunk` | One chunk per frame, at most 1 MiB of data |
| Heartbeat | At most one every 500 ms |
| Hello timeout | The daemon waits 10 s for `Hello` |

The chunking parameters travel in the handshake (`Hello.chunk_min`,
`chunk_avg`, `chunk_max`); a client with others is refused with
`VERSION_MISMATCH`. A file's chunk list and whole-file hash are a pure function
of its bytes and these parameters.

## Connection

A client opens one QUIC connection to the daemon's `EndpointId`. Before it
accepts any stream, the daemon reads its grants afresh and looks up the
verified key of the client. A key that no grant names, or a configuration that
cannot be read, gets the connection closed with application code 1
(`unauthorized`) and no message. See
[Identity and trust](../explanation/identity-and-trust.md).

Close codes:

| Code | Reason | Client's response |
| --- | --- | --- |
| 0 | `done` | None. |
| 1 | `unauthorized`: the key has no grant | Final. |
| 2 | `rejected` after an `Error`, or `hello timeout` | Final. |
| 3 | `superseded`: a newer connection resumed this session | Final. |
| 4 | `idle`: the inactivity watchdog fired | Redial and resume. |

## Streams

A session uses two bidirectional streams, both opened by the client, and many
unidirectional ones.

- **The control stream** is the first bidirectional stream. It carries `Hello`
  and the reply, then every `Control` frame except the listing's.
- **The listing stream** is the second. The client opens it right after
  `HelloAck` and starts it with a `ListingStreamHeader` naming the session.
  Each half then carries one side's listing: `ListingEntry` frames in path
  order, a `ListingError` wherever entries are missing (a directory that could
  not be read, right after its own entry; an entry that could not be stat'ed),
  one `ListingEnd` with the count and an `incomplete` flag, and the end of the
  half. A side that does not list, a receiver resuming a complete plan, ends
  its half without a frame. The listing has its own stream so that a planner
  that stops taking it, while it waits for files in flight to finish, stalls
  only the listing and never the frames that finish those files.
- **Data streams** are unidirectional, from sender to receiver, one per run of
  contiguous requested chunks of one file, a run being cut at 64 MiB. Each
  starts with a `DataStreamHeader` (session id, file id, offset, length,
  index of the first chunk) and carries one `DataChunk` frame per chunk. A
  sender keeps at most `--parallel` of them open, 128 by default; a daemon that
  sends always uses 128.

## Handshake

The client sends `Hello` on the control stream:

- `protocol_version` and the three chunking parameters;
- `session_id`: 16 bytes to resume a session, empty for a new one;
- `direction`: `CLIENT_SENDS` (a push), `DAEMON_SENDS` (a pull) or
  `LIST_EXPORTS`;
- `path`, the remote `PATH` as typed, which the daemon resolves (see
  [Remote addresses](remote-addresses.md#resolution-order)); `contents_only`,
  the trailing-slash rule; and on a push of a source without a trailing slash,
  `name`, its basename;
- the client's preserve flags (`-l -p -t -g -o -D -H -S`, `-r`, `-d`), which
  the daemon honours in both directions; a `Hello` without them means
  everything;
- the include and exclude rules, compiled and in order. A sending daemon walks
  with them; a receiving daemon protects what they exclude from deletion;
- the receiver-side options, which the daemon honours only when it receives:
  the deletion mode, `--delete-excluded`, `--max-delete`, `-n`, `-u`, `-c`,
  `--inplace`, `--append` or `--append-verify`, `--partial-dir` (relative and
  inside the destination, or refused), `--no-rename-detection`, `--fsync`
  (field 29; a daemon older than it ignores the field and flushes every file,
  as every version did before it, while an export's `fsync = true` flushes
  whatever the client sent). `-i`,
  `--ignore-errors` and `--numeric-ids` apply in both directions;
- `traceparent` and `tracestate`, the client's W3C trace context (empty: no
  trace). A daemon that exports traces makes its `session` span a child of it;
- `build`: the client's `BuildInfo` (package version, short git commit, dirty
  flag).

For `LIST_EXPORTS` the daemon answers with an `ExportList`, the exports this
key may use with their paths and effective access, sorted by name, and closes
the connection. Otherwise it answers with `HelloAck` (the session id, whether
the session was resumed, its protocol version and its `BuildInfo`), or with an
`Error` and close code 2. Each side logs the other's build at info level once
the session is up.

## Errors

| Code | Sent when |
| --- | --- |
| `UNKNOWN` | A malformed request, such as an unknown direction. |
| `UNAUTHORIZED` | The key has grants, but none for an export that contains the path. The message is `no access to PATH`. |
| `READ_ONLY` | A push to an export that is `read_only` or that this key may only read. |
| `BAD_PATH` | The path has a `..` component or a NUL byte, or the session's root cannot be prepared. |
| `VERSION_MISMATCH` | Another protocol version or other chunking parameters. |
| `BAD_SESSION` | A resume of a session the daemon does not know, that is finished, or that another key started. The client starts a fresh session at once. |
| `INTERNAL` | The daemon failed, for example opening the export's index. |

Field number 2 is reserved. The answer for a path a key may not use is the
same whether the path lies outside every export, inside an export without a
grant, or does not exist at all, so a key cannot probe the daemon's layout.
`READ_ONLY` and a `BAD_PATH` about the root go only to a key that may use the
export.

## Control messages

| Message | Direction | Meaning |
| --- | --- | --- |
| `PlanItem` | receiver to sender | Send this file: `WHOLE` (nothing to reuse at that path), `DELTA` (the receiver holds an old version) or `APPEND` (the receiver trusts its first `append_from` bytes). |
| `ChunkListPart` | sender to receiver | Part of a file's chunk list. The first part also carries the mtime (`mtime_sec`, `mtime_nsec`) the file was hashed under; with the `size`, the receiver places the file with them instead of the walk's. |
| `ListRoom` | receiver to sender | Room for this many more chunk records, freed by lists the receiver dropped. |
| `ChunkBasis` | receiver to sender | Block signatures over a region of the receiver's old file, for one chunk. |
| `NeedChunks` | receiver to sender | The chunks to send; empty means none. |
| `MayPlace` | receiver to sender | The file is verified against chunk list number `gen`; may it be placed? |
| `PlaceVerdict` | sender to receiver | The answer: `changed` if the source differs from the stat it was hashed under. The receiver places the file only when it is unchanged; otherwise it drops it and waits for the new chunk list, or for `SOURCE_ERROR`. A `MayPlace` about a list older than the sender's latest is always answered `changed`. |
| `FileAck` | either | From the receiver: `DONE`, `HASH_MISMATCH`, `WRITE_ERROR`. From the sender: `SOURCE_ERROR`, it cannot read the file. `DONE` carries the bytes the receiver did not take from the sender. |
| `Itemized` | receiver to sender | One rsync `-i` line, for a client that pushes with `-i`. |
| `Heartbeat` | either | This side is busy; see below. |
| `SessionDone` | receiver to sender | The session's totals, errors and deletions. |
| `Error` | daemon to client | A refusal, as above. |

Every message on the control stream is a `Control` frame with exactly one of
these set. A file costs a `PlanItem`, at least one `ChunkListPart`, a
`NeedChunks`, a `MayPlace`, a `PlaceVerdict` and a `FileAck`; the room its list took comes back in a
`ListRoom`, which may cover several files.

### Chunk lists

A chunk list travels as `ChunkListPart` frames of at most 32,768 records, in
order, the last one flagged `last`; an empty file's list is one empty last
part. Every part repeats the file id, the size, the whole-file hash and the
total record count. The receiver reassembles them and fails the file when a
sequence skips, describes another file, or announces more records than 2^24 or
than the size allows at the minimum chunk length. The first part also holds the
mtime the file was hashed under; the size and mtime of a list replace the
walk's for the file. An `APPEND` tail's list carries the whole file's size and
mtime.

Lists travel in room the receiver grants. Each connection starts with room for
2^24 records on both sides; a list's first part takes its whole `total` of it,
and a sender sends a list only once it fits. The receiver gives room back with
`ListRoom` when it drops a list: the file was placed, failed or replaced. A
first part that does not fit, or a grant above the starting room, is a
protocol error. A first part for a file that already has a list replaces that
list; a sender that lists a file again first sends an empty unfinished first
part, so the old list is dropped without waiting for room.

For an `APPEND`, the list covers only the tail from `append_from` and carries
no whole-file hash; the receiver folds the hash from the bytes it places. Every
other list carries one.

### Requests, bases and operations

`NeedChunks` names chunk indices; a request for more than 2^18 comes as
several, which the sender treats as one. Before the `NeedChunks` that names a
chunk of a `DELTA` file, the receiver may send a `ChunkBasis` for it: a region
of its old file named by its offset there, cut into blocks of 512 B to 4 KiB,
and per block a 32-bit rolling sum and the first 8 bytes of the block's BLAKE3
hash. The control stream is ordered, so a basis precedes the request that uses
it. The sender keeps a basis for that one request and drops it; a chunk asked
for again comes raw. A malformed basis is ignored. A sender that holds more
than 32 MiB of bases fails the session.

A chunk with a basis may arrive as `DataChunk.ops` instead of `data`: a list
of literals and copies (`offset`, `len` in the receiver's old file) that add up
to the chunk. The sender uses operations only when they are at least a quarter
smaller than the chunk. The receiver rebuilds the chunk and verifies it against
the chunk's hash like any other. How the receiver chooses and uses bases is in
[Receiving a changed file](../explanation/receiving-a-changed-file.md#bases-and-operations).

### Heartbeats

A `Heartbeat` means "I am busy". A side sends one every 500 ms in which it made
progress: hashing, writing, finalizing, deleting, or traffic. It carries what
the other side cannot otherwise see: the directory this side's walk is in,
the bytes its planner found up to date, and its deletion count. An idle side sends none,
so a stuck side still times out. See
[Sessions and resume](../explanation/sessions-and-resume.md#timeouts).

## Persisted state

`session.proto` describes what each side stores in its index for a session
that may resume: the plan (`SessionPlan`, with `PlanEntry` per file) and the
progress (`SessionProgress`). These messages never go on the wire. Partial
files are stored as `RangeSet`s of verified bytes. See
[Sessions and resume](../explanation/sessions-and-resume.md).

## Cross-implementation check

`crates/irsync-proto/tests/golden/chunk_1mib.json` is a golden vector: the
whole-file hash and the exact chunk boundaries (offset, length, BLAKE3 hash)
of 1 MiB from the xorshift64 generator seeded with `0x9E3779B97F4A7C15` (see
`crates/irsync-proto/tests/chunk.rs`). An independent implementation must
reproduce it exactly to interoperate, because chunk boundaries decide which
chunks a client and a daemon consider equal.
`crates/irsync-proto/tests/golden_bytes.rs` pins the exact wire bytes of several messages, so a change of field number or
wire type shows.
