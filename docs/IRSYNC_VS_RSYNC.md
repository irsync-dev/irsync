# irsync versus rsync: the transfer algorithm, critically

A comparison of how irsync and rsync move a changed file, with the
shortcomings on the irsync side worked out in detail. Everything said about
irsync was checked against the code as of commit `d08b4de` (2026-10-22);
function names below point at where each behaviour lives. rsync facts refer
to protocol 30 and later (rsync 3.x).

Written 2026-10-22. The design document
(`superpowers/specs/2026-10-17-irsync-design.md`) is the binding authority
for what irsync *intends*; this file records where the implementation falls
short of that intent or of rsync, and what it would take to close each gap.

## 1. The two algorithms in brief

### rsync

The receiver keeps the old file as a *basis*. Its generator process cuts the
basis into fixed blocks (block size about the square root of the file size,
rounded to a multiple of 8, clamped to 700 bytes .. 128 KiB) and sends, per
block, a 4-byte rolling checksum plus a truncated MD5 (2 to 16 bytes, chosen
per file). The sender slides a one-byte window over the new file; on a weak
hit it checks the strong checksum, and emits a token stream of literal bytes
and block references. The receiver rebuilds the file into a temporary,
computing the whole-file MD5 as it writes, then renames it into place. On a
whole-file mismatch the file is redone once with full-length checksums.
Everything flows in order over one TCP or SSH stream, through three
pipelined processes (generator, sender, receiver).

### irsync

No basis is part of the protocol. The sender cuts the new file into
content-defined chunks (FastCDC 2020, 64 KiB min, 256 KiB average, 1 MiB max)
and BLAKE3-hashes each chunk and the whole file (`irsync_proto::chunk`).
After the receiver's planner requests the file (`PlanItem`), the sender
digests it (from its index when size, mtime, ctime and inode match, else by
reading it) and sends the `ChunkList`. The receiver opens
`.irsync.<name>.part`, adopts any verified progress, for a `DELTA` action
hashes the old destination file into its index, fills what it can from the
rename-detection source and from any indexed file holding a chunk with the
same hash (`recv_io::open_writer`), and asks for the rest (`NeedChunks`).
The sender streams those chunks over up to 32 parallel QUIC streams, one per
contiguous run (`send::Sender::send_chunks`), re-hashing each chunk as it
reads it. The receiver verifies each chunk before writing it, persists
verified ranges in its index, re-hashes the whole `.part` at finalize,
applies metadata through the open descriptor and renames (`recv::FileWriter::finalize`).
Only then does it send `FileAck DONE`.

## 2. Side by side

| Dimension | rsync | irsync |
| --- | --- | --- |
| Delta unit | basis block (700 B .. 128 KiB, ~sqrt(size)), matched at any byte offset | content-defined chunk (64 KiB .. 1 MiB, 256 KiB avg), matched whole |
| Smallest file that gets a delta | any (blocks go down to 700 B) | effectively ~1 MiB (below that a file is 1 to 3 chunks) |
| What a 1-byte edit in a 1 GiB file sends | ~32 KiB literal + tokens | ~256 KiB (one chunk, sometimes two) |
| Signature direction | receiver -> sender (basis blocks) | sender -> receiver (new file's chunks) |
| Signature size, 1 GiB file | ~640 KiB (32768 blocks x 20 B) | ~200 KiB (4096 chunks x 49 B) |
| Signature for a *new* file | none | full chunk list, ~0.02% of the data |
| Reuse scope | the basis at the same path (`--fuzzy`, `--link-dest`, `--compare-dest` widen it) | every chunk of every file the receiver's index knows |
| Rename detection | none (name similarity only, `--fuzzy`) | by whole-file hash, from the index |
| Digest caching across runs | none; `-c` re-reads everything | persistent SQLite index keyed on size, mtime, ctime, inode |
| Round trips before first data byte | ~0 (pipelined) | 1.5 (plan, chunk list, need) |
| Serial disk passes before first data byte, one big changed file | 1 (basis checksums) | 2 or 3 (sender hash, receiver old-file hash, receiver local copy) |
| Extra receiver pass after last byte | none (MD5 computed while writing) | whole-file re-hash |
| Streams | one, in order | many QUIC streams, 32 at a time, files split at 64 MiB |
| Files in flight | pipelined, bounded by the window | up to 4096 |
| Resume | `--partial` keeps bytes; next run re-checksums them | chunk-granular, verified ranges persisted; plan persisted |
| In-place / append | `--inplace`, `--append`, `--append-verify` | none; always `.part` + rename |
| Per-block integrity | 4-byte rolling + 2..16-byte MD5 | 32-byte BLAKE3, checked at send and at write |
| Whole-file integrity | MD5 (xxh3 negotiable in 3.2+), one redo | BLAKE3, retried once per side |
| Change during transfer | noticed at the final whole-file check; whole file redone | noticed at the sender per chunk, before the bytes leave; re-offered once |
| Compression | `-z` | none (deferred in the design) |
| Largest file | no protocol limit | ~80 GiB of typical data (see 3.5) |
| Transport | TCP or SSH | iroh QUIC, NAT traversal, pinned Ed25519 identities |

## 3. irsync's shortcomings in detail

### 3.1 Delta granularity: whole chunks instead of bytes

rsync's rolling checksum lets the sender find a basis block at *any* byte
offset of the new file. A one-byte change therefore costs the literal bytes
between the two surrounding block matches: at most one block plus a few
bytes of tokens, and the block is ~sqrt(size).

irsync can only reuse a chunk whose bytes are identical. A change inside a
chunk invalidates that chunk; FastCDC resynchronises its boundaries within
about one chunk after the edit, so the cost is one chunk, sometimes two, each
64 KiB to 1 MiB. The gap is a constant factor that grows with the number of
edits and shrinks with their size:

| Scenario, 1 GiB file | rsync sends (approx) | irsync sends (approx) | ratio |
| --- | --- | --- | --- |
| one byte changed | 32 KiB | 256 KiB | 8x |
| 100 scattered page writes (4 KiB each) | 3.2 MiB | 25 MiB | 8x |
| 1 MiB appended | 1 MiB | 1 to 2 MiB | 1 to 2x |
| 1 byte inserted near the start | 32 KiB | 256 KiB | 8x |
| 10% rewritten in one run | ~100 MiB | ~100 MiB | 1x |

The 8x figure is for 1 GiB; rsync's block shrinks with the file, so for a
100 MiB file it is 10 KiB against irsync's unchanged 256 KiB, a 25x gap. On
files that are updated in place with many small writes (databases, VM
images, mbox files, sqlite, append-plus-header-rewrite formats) rsync's
delta is categorically tighter. irsync is competitive only when edits are
large relative to a chunk, or when whole regions are moved or duplicated.

The design document promises "rsync-style in-file delta transfer". What is
implemented is chunk-level deduplication, which is a weaker thing. The code
does nothing below chunk granularity anywhere: `plan::decide` only chooses
`WHOLE` or `DELTA`, and `DELTA` differs from `WHOLE` solely in that
`open_writer` hashes the old destination file first so its chunks are in the
index.

### 3.2 No delta at all below about 1 MiB

FastCDC emits a single chunk for anything shorter than the 64 KiB minimum,
and a file under the 256 KiB average is usually one or two chunks. For the
bulk of a typical tree (source code, documents, configuration, thumbnails,
most photos' sidecar files) any change is a whole-file transfer. rsync's
block size floor is 700 bytes, so it still produces a useful delta for a
100 KiB file.

This is a direct consequence of the fixed protocol constants (`Hello`
carries them, `CHUNK_PARAMS` pins them, the index refuses to open with
others). rsync adapts block size per file; irsync cannot adapt anything per
file without a protocol revision.

### 3.3 Disk passes are serialised, and there are more of them

For one changed file the implementation does, in this order, with nothing
overlapping within the file:

1. Sender: `SenderRole::spawn_digest` reads and hashes the new file in full
   (a cache miss, because the file changed). Only then does the `ChunkList`
   go out.
2. Receiver: `open_writer` hashes the old destination in full for `DELTA`
   (a cache hit if irsync itself placed that file and nothing touched it
   since; a full read otherwise).
3. Receiver, still inside `open_writer`: every missing chunk is looked up in
   the index and, if found, copied from the old file into the `.part`
   (`try_copy`). For a small edit that is a full read plus a full write.
4. Only now is `NeedChunks` sent (`Receiver::on_opened`), and the network
   transfer begins.
5. Receiver: `finalize` runs `fsync`, then `digest_held` reads the whole
   `.part` again to check the whole-file hash.

So a 100 GiB image with one changed block, warm index, at 1 GiB/s of disk,
spends about 100 s hashing on the sender, then 100 s + 100 s copying on the
receiver, then sends a chunk, then 100 s re-hashing: roughly 400 s of
wall-clock in which the network is idle for all but a millisecond. rsync
does the generator pass (100 s), then the sender's rolling pass and the
receiver's reconstruction run *concurrently* with each other and with the
transfer (100 s), and the MD5 is computed during the write. About 200 s.

Receiver I/O volume for the same case:

| | reads of S | writes of S |
| --- | --- | --- |
| rsync, default | 2 | 1 |
| rsync `--inplace` | 1 | changed blocks only |
| irsync, old file's digest cached | 1 | 1 |
| irsync, old file's digest not cached | 2 | 1 |

Two of these costs were avoidable without a protocol change, and were
removed on 2026-10-22: `open_writer` now only partitions the missing chunks
into a remote and a local set, `NeedChunks` goes out at once, and a
local-copy task fills the local set while the sender delivers, counting as
one unit of the request so the existing drained-and-re-request path covers
a local source whose bytes no longer match the index. The planner also
starts hashing the old file when it sends the `PlanItem`, so steps 1 and 2
above run concurrently. The chain is now: sender hash in parallel with the
receiver's old-file hash, then index lookups, then the transfer with the
local copy running under it, then finalize, which since remedy 5 (3.4)
reads nothing. The table above reflects that. The flow is documented step
by step in `DELTA_RECEIVE.md`.

The first-time sync of a tree pays a related price. Every new file is read
twice on the sender (once to digest, once to send, with each chunk
re-hashed as it is read in `send::read_chunk`) and once more on the
receiver (finalize). rsync reads a new file once on the sender and writes it
once on the receiver. The page cache hides this for files that fit in RAM,
the 4-worker hasher overlaps it across files, and it is what makes every
later run cheap; but for the primary use case in the design document, the
first push of a photo library, irsync does about twice the disk I/O.

### 3.4 The finalize re-hash is a full extra pass by construction

Every chunk is verified against its 32-byte hash before it is written
(`FileWriter::write_chunk`). The whole-file hash is nevertheless a plain
BLAKE3 over the bytes, which cannot be derived from the chunk hashes, so
`finalize` must read the whole `.part` back. This defends against bits
flipped on disk between write and rename, against a second writer (already
excluded by the `.part` lock), and against bugs in the chunk path. It costs
one full read per file received, on the critical path, after the last byte
has arrived, and it is what the R35 heartbeat exists to hide from the idle
watchdog.

The alternative considered first was to define the whole-file hash as a
BLAKE3 over the concatenated chunk hashes (a two-level Merkle root), at the
cost of `b3sum` equivalence, of the meaning of the index's `whole_hash`
column, and of a protocol change. A second idea, aligning chunk cuts to
1 KiB so that chunks map onto BLAKE3's own tree, would have broken
content-defined chunking's resynchronisation after unaligned insertions.

What landed on 2026-10-22 needs neither. BLAKE3 is itself a Merkle tree
over 1 KiB leaves at absolute offsets, and the `blake3` crate's `hazmat`
module exposes its nodes. The writer (`tree_hash::TreeHash`) hashes the
leaf-aligned interior of every verified chunk as aligned power-of-two
subtrees, keeps the few bytes at each chunk end until the neighbouring
chunk completes their leaf, merges sibling nodes eagerly so only a small
frontier is held, and folds the root at finalize along the tree's own
split points. The result equals `blake3::hash` of the bytes, so nothing
on the wire or in the index changed; finalize is now `fsync`, fold,
metadata, rename. What is no longer checked is the disk after the write:
a byte flipped on the platter between write and rename goes unnoticed,
which is rsync's guarantee as well. The receiver reads a file it receives
exactly once, on adoption or local copy, and writes it once.

### 3.5 A chunk list is one control frame, capped at 16 MiB

`ChunkList` is sent as a single framed protobuf on the control stream, and
`net::frame::MAX_FRAME` is 16 MiB on both the writing and the reading side.
A `ChunkRecord` encodes to about 49 bytes at large offsets (tag and length,
offset varint, length varint, and a nested 32-byte `Hash` message), so a
chunk list holds at most ~342,000 chunks:

| Data | Chunk size | Largest describable file |
| --- | --- | --- |
| typical (CDC hits its average) | 256 KiB | ~84 GiB |
| all chunks at the minimum | 64 KiB | ~21 GiB |
| low-entropy data, forced max cuts | 1 MiB | ~334 GiB |

What happens above the ceiling is the worse part. `write_msg` refuses the
frame, the writer task reports `Ev::WriterFailed`, the sender role turns
that into a `TransportError`, and `reconnect::classify` treats a
`TransportError` as "dial again". The resumed session re-plans the same
file (or resumes a stored plan that contains it), the sender digests it
again from cache, and the same frame is refused again. The loop ends only
when the reconnect window (24 h by default, measured from the last
progress) runs out. Every other file in the session has been placed by
then, so the user sees a sync that spends a day retrying and then fails,
with the real cause one log line among many. The file-level `SOURCE_ERROR`
path exists but is not used for this.

rsync has no equivalent limit: block signatures are streamed, and even the
whole-file approach caps nothing.

The fix is either to stream the chunk list in bounded pieces (a protocol
change, backwards-compatible as a new message) or, much cheaper, to check
the encoded size before sending and fail the *file* with `SOURCE_ERROR`
instead of the session. The second landed on 2026-10-22: `on_digested`
measured the frame before queueing it and gave the file up with a message
naming the chunk count, the frame size and the limit, so the session
completed and no redial happened.

The first landed the same day and made the second moot. `ChunkListPart`
carries every chunk list in pieces of 32,768 records (about 1.6 MiB each)
on the control stream; the receiver reassembles them (`session/parts.rs`)
into the native digest and feeds it to the unchanged chunk-list path. The
one-frame `ChunkList` message, the sender's frame-size check and its
`SOURCE_ERROR` are gone; the version number stays 1, there being no
deployed peers. (A first cut
negotiated parts in the handshake, because a peer that predates a message
ignores it rather than refusing it; with no deployed peers to keep talking
to, the negotiation was dropped the same day.) The ceiling is now the
receiver's memory for the list, a 1 TiB file at the typical chunk size
being about 200 MB of records held twice (one copy shared by the file's
state, its route and its open result, one validated copy in the writer),
and a hard cap of 2^24 records per list, about 4 TiB of typical data,
which bounds what a hostile sender can make a receiver hold.

### 3.6 No in-place or append mode

Every file is built in `.irsync.<name>.part` and renamed. This gives the
atomicity the design's anti-goals demand, but it means a changed file needs
free space equal to its full size, and (per 3.3) a full local rewrite even
when one chunk changed. rsync's `--inplace` writes only the changed blocks
into the existing file, and `--append` / `--append-verify` turn a
grow-only log into a tail transfer with no basis pass at all. irsync has
nothing to offer for the "the disk is 90% full and the 200 GiB image
changed" case, and nothing cheaper than a full chunk-list exchange for a
log that only grows.

### 3.7 Reuse is only as good as the index

Cross-file reuse and rename detection read `chunks` and
`entries.whole_hash` from the receiver's index. The receiver's own listing
attaches a hash only when the index already has a fresh one
(`control::spawn_listing` calls `Hasher::cached`, which never reads a file).
So:

- A destination populated by anything other than irsync (a prior rsync, a
  restore from backup, a manual copy) contributes no chunks and no rename
  candidates until each file is hashed, and only a `DELTA` target or a
  `-c` run ever hashes a destination file on demand.
- Files irsync has placed are indexed with the digest the sender gave them
  (`recv_io::finalize`), so the index grows with use. A fresh destination
  after a fresh install is blind.
- `find_chunk` returns the first path holding a hash and the copy is
  re-verified, so staleness is safe, only wasteful.

rsync's basis is always the real file at the destination path, with no
warm-up. For a one-off transfer to a cold machine irsync behaves like
rsync `--whole-file`, plus the chunk list and the two extra passes.

### 3.8 The planner diverges from the specification on content-equal files

The design says that when the quick check fails the receiver "computes on
demand" the whole-file hash of its copy, and a match becomes a metadata-only
update. The implementation does not: `plan::decide` compares only the hashes
that came with the two listings, and the receiver's listing carries cached
hashes only. A file whose mtime moved but whose content did not (a `touch`,
an editor that rewrote the same bytes, a restored backup) is planned as
`DELTA` whenever the receiver's index lacks a fresh digest, and `DELTA` with
every chunk found locally still runs the full `open_writer` copy and
`finalize` re-hash: a complete local rewrite of the file to apply an mtime.

With `-c` the same gap appears: `quick_equal` requires `hashes_equal ==
Some(true)`, which a cold receiver index cannot supply, so `-c` against a
cold destination falls into `DELTA` instead of the metadata-only path the
specification describes. rsync `-c` computes both checksums, finds them
equal, and applies attributes only.

Closed on 2026-10-22 at the point where both hashes first meet, which is
the chunk list, not the plan: the sender's hash is absent from its listing
exactly when its file was touched, and hashing the destination inline in
the planner would stall the merge-join. `open_writer` now compares the old
file's whole-file hash (the planner's early hash, see 3.3) with the chunk
list's; on a match, and after a check that the destination is still the
file that was hashed, the receiver sends an empty `NeedChunks`, applies
the sender's metadata to the existing inode, records it in the index and
acks `DONE`. No `.part`, no write, no finalize re-hash. The planner's `-i`
line for a `DELTA` file is held back until then, so a `touch` itemizes as
`.f..t......`. See `DELTA_RECEIVE.md`, section 2.4. The other half of the
`-c` story, that the design says it "forces rehashing everything" while
the hasher is never put into forced mode for it, is a separate divergence
and still stands.

### 3.9 Per-file protocol chatter

Each file costs four control messages in strict alternation (`PlanItem`,
`ChunkList`, `NeedChunks`, `FileAck`) and 1.5 round trips before the first
data byte. rsync's generator runs ahead and the sender's token stream
follows with no synchronous turn-around. irsync hides the latency with up
to 4096 files in flight, which is enough that throughput on small files is
bounded by the control stream's serialisation rather than by RTT; but each
`PlanItem` is sent only after the planner has acquired an in-flight slot
(`Planner::one`), and each slot is released only at `DONE`, so one slow
file (a large `DELTA` in its serial passes, see 3.3) holds a slot for its
whole life. On the receiver, each missing chunk of each file is one SQLite
query under the global index mutex (`open_writer`'s `find_chunk` loop)
before `NeedChunks` can go out: a few hundred thousand queries for a large
new file, serialised with every other index user.

### 3.10 Smaller points

- **No compression.** The design defers `-z` to later. For text-heavy
  trees rsync's `-z` can halve the bytes on the wire.
- **Chunk parameters are a protocol constant.** The tuning that would help
  3.1 and 3.2 (smaller chunks for small files, a smaller minimum) cannot be
  done without a version bump, and every index on every peer would be
  invalidated (`IndexError::ChunkParamsChanged`).
- **Two chunk lists in memory per in-flight file** (the writer's and the
  route's), about 48 bytes per chunk each. Harmless until many large files
  are in flight at once; 4096 slots make that possible.
- **Sparse handling is chunk-granular**: a zero run shorter than a chunk is
  written. rsync `-S` detects zero runs at write granularity.

## 4. Where irsync is ahead

For balance, the cases where the comparison runs the other way:

- **Unchanged trees.** irsync's index makes the second run a stat pass on
  both sides, `-c` included. rsync `-c` re-reads everything every time.
- **Moved, renamed and duplicated data.** Any chunk anywhere in the indexed
  destination is reused; a renamed 10 GiB file transfers nothing. rsync
  sends it again.
- **Bad and fat links.** 32 parallel streams, files split at 64 MiB, a
  scan that runs alongside the transfer, and chunk-granular resume across
  disconnects, process deaths and reboots. rsync's single in-order stream is
  limited by one congestion window and `--partial` is byte-granular at
  best, re-checksummed on the next run.
- **Integrity.** Three independent 32-byte BLAKE3 checks per chunk and file
  against rsync's 2..16-byte truncated MD5 per block. A source file that
  changes mid-transfer is caught before stale bytes leave the sender.
- **Sender CPU.** One hash pass instead of a rolling checksum over every
  byte with a hash-table probe per position.
- **Reachability and identity.** NAT traversal and pinned keys without
  SSH or port forwarding, which is irsync's reason to exist and not an
  algorithmic property.

## 5. Remedies, ranked

Ordered by what they buy against what they cost. Items 1 and 2 need no
protocol change.

1. **Fail the file, not the session, when a chunk list will not fit**
   (3.5). Check the encoded size in `on_digested` and send `SOURCE_ERROR`.
   Small change, removes a day-long retry loop. *Done (2026-10-22), then
   removed again the same day once remedy 4 made every list fit.*
2. **Send `NeedChunks` before the local copies, and copy concurrently with
   the network** (3.3). Restructure `open_writer` so index lookups decide the
   two sets first; the sets are disjoint, so the writer can take both. Cuts
   one full pass off the critical path of every `DELTA`. *Done
   (2026-10-22), including the early old-file hash; see
   `DELTA_RECEIVE.md`.*
3. **Compute the receiver's whole-file hash on demand before deciding**
   (3.8). One read of the old file turns a `touch` into a metadata-only
   update instead of a rewrite, and makes `-c` match its documentation.
   The read is the same one `open_writer` does today for `DELTA`. *Done
   (2026-10-22), at chunk-list time rather than plan time; see 3.8 and
   `DELTA_RECEIVE.md`.*
4. **Stream the chunk list in pieces** (3.5). A `ChunkListPart` message
   with a `last` flag, the only form a chunk list takes; protocol version
   2. Removes the frame ceiling; memory and a 2^24-record cap remain.
   *Done (2026-10-22); see 3.5.*
5. **A Merkle whole-file hash** (3.4). Removes the finalize pass. *Done
   (2026-10-22) without a protocol or index change: the true BLAKE3 is
   folded from the verified chunks through the crate's tree API; see 3.4.*
6. **Sub-chunk delta for the `DELTA` action** (3.1, 3.2). The honest fix
   for in-place workloads: when a chunk is missing and the old file has a
   chunk at roughly the same offset, run an rsync-style rolling match
   within that chunk pair and send literals. It would be a per-chunk
   negotiation on top of the existing one, confined to files the receiver
   already holds, so it composes with everything else here. Substantial
   work, and the reason the comparison in section 3.1 exists.
7. **`--inplace` and `--append`** (3.6). `--append` is cheap once 6 exists
   or even without it (compare the old file's chunk list as a prefix).
   `--inplace` conflicts with the atomic-rename anti-goal and should stay
   an explicit opt-in as it is in rsync.
8. **Index warm-up for foreign destinations** (3.7). An `irsync index
   <root>` command, or hashing destination files lazily in the background
   of a session, so a destination that irsync did not create starts
   contributing chunks.

## 6. Where to look

- `crates/irsync-proto/src/chunk.rs`: chunking and hashing constants.
- `crates/irsync/src/plan.rs`: `decide`, the per-path decision.
- `crates/irsync/src/session/planner.rs`: `Planner::one`, in-flight slots,
  rename lookup.
- `crates/irsync/src/session/sender_role.rs`: `spawn_digest`,
  `on_digested`, `on_need`, retries.
- `crates/irsync/src/session/recv_io.rs`: `open_writer` (local reuse,
  index lookups), `finalize`, data-stream routing.
- `crates/irsync/src/recv.rs`: `FileWriter` (`write_chunk`, adoption,
  `finalize` re-hash).
- `crates/irsync/src/send.rs`: `send_chunks`, `split_ranges`,
  `read_chunk`.
- `crates/irsync/src/net/frame.rs`: `MAX_FRAME`.
- `crates/irsync/src/session/reconnect.rs`: `classify`.
- `crates/irsync/src/hasher.rs`: the cache's freshness rule.
- `crates/irsync/src/session/control.rs`: `spawn_listing` (cached hashes
  only).
