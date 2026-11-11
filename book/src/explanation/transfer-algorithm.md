# The transfer algorithm

How `irsync` moves a changed file, set against how rsync does it. rsync facts
refer to protocol 30 and later (rsync 3.x). The receiver's side of one changed
file, step by step, is in
[Receiving a changed file](receiving-a-changed-file.md); the messages are in
[Protocol](../reference/protocol.md).

## The two algorithms

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
Everything flows in order over one TCP or SSH stream, through three pipelined
processes (generator, sender, receiver).

### irsync

The sender cuts the new file into content-defined chunks (FastCDC 2020,
64 KiB minimum, 256 KiB average, 1 MiB maximum) and hashes each chunk and the
whole file with BLAKE3. Both sides keep these hashes in a persistent index,
keyed on size, mtime, ctime and inode, so an unchanged file is never read
twice.

1. The receiver's planner compares the two listings and requests a file with a
   `PlanItem`: `WHOLE` when nothing is at the path, `DELTA` when it holds an
   old version. For a `DELTA` it starts hashing the old version at that
   moment.
2. The sender digests the new file, from its index when the file is unchanged
   there and by reading it otherwise, and sends the chunk list.
3. If the old file's hash equals the new one, the receiver applies the
   sender's metadata to the file in place and is done: no write.
4. Otherwise the receiver opens `.irsync.<name>.part`, as a reflink clone of
   the old file where the filesystem allows, and keeps the chunks the clone
   or an earlier interrupted attempt already holds. It looks the missing
   chunks up in its index: those any indexed file holds are copied locally,
   the rest are requested from the sender with `NeedChunks` at once.
5. For a `DELTA`, the receiver first offers the sender a *basis* for each
   requested chunk: block signatures over the old file around the chunk's
   place. The sender answers such a chunk with copies and literals instead of
   its bytes when that is smaller. This is rsync's algorithm, run inside one
   chunk.
6. The sender reads each requested chunk, hashes it again as it reads, and
   streams the chunks over parallel QUIC streams. The receiver verifies every
   chunk against its hash before writing it, whether it came raw, rebuilt from
   operations or from a local file.
7. The receiver folds the whole-file BLAKE3 from the verified chunks, without
   reading the file back, applies the metadata, renames the `.part` into
   place, and acknowledges `DONE`.

Local copies run while the network transfer runs, and the sender's hash runs
while the receiver hashes its old file. A whole file and a moved file take the
same path: for a moved or copied file, every chunk is found locally.

## The delta inside a chunk

A content-defined chunk resynchronises within about one chunk after an edit,
so whole-chunk reuse alone would cost one or two chunks, 64 KiB to 1 MiB, per
edit. irsync narrows that to blocks:

- For a missing chunk at offset `o` of length `n`, the basis is the old file's
  bytes from `o - n` to `o + 2n`, clamped to the old file. A chunk that starts
  at or past the old file's end (appended data) gets none. A file missing more
  than half of its chunks gets none, unless it has at most two chunks: that is
  a rewrite. The regions stop once they add up to the old file's size or three
  maximum chunk lengths, whichever is more.
- The block length is the square root of the region, clamped to 512 B ..
  4 KiB. Each block is signed with a 32-bit rolling sum and the first 8 bytes
  of its BLAKE3 hash, 12 bytes per block.
- The sender slides a window over the chunk's bytes, checks every position
  against a 64 Ki-bit filter, then the signatures, then the strong hash, and
  emits copies of old-file ranges and literals. If the result is not at least
  a quarter smaller than the chunk, the chunk goes raw.
- The receiver rebuilds the chunk and verifies its 32-byte hash. A chunk that
  does not verify (the old file changed in place, a hostile operation list, a
  collision of the 8-byte hash) is requested again, without a basis.

The integration tests in `crates/irsync/tests/sub_chunk_delta.rs` measure the
bytes on the data path, which leave out the chunk list and the signatures:

| Scenario | Whole chunks only | With the delta |
| --- | --- | --- |
| 4 MiB file, one byte changed | one chunk, ~256 KiB | 2,100 bytes |
| 16 MiB file, eight scattered 4 KiB writes | eight chunks, ~2 MiB | 51,475 bytes |
| 100 KiB file, six bytes changed | the whole file | 550 bytes |
| 4 MiB file rewritten | 4 MiB | 4 MiB, no bases offered |
| 2 MiB appended to 4 MiB | ~2 MiB | ~2 MiB, at most one basis |

The signatures cost the other way about 12 bytes per block: for a 256 KiB
chunk with a 768 KiB region at 886-byte blocks, 887 blocks, about 10 KiB, or
4% of the chunk.

## Files in flight and streams

Two numbers bound how much a session has going at once.

- **Files in flight**: at most 4,096 per session, counted from the
  `PlanItem` to the acknowledgement (`MAX_INFLIGHT` in
  `crates/irsync/src/session/planner.rs`). A file in flight may hold two
  descriptors, its `.part` and its old version, so the window shrinks to
  (soft open-file limit − 256) / 2 when that is smaller. At startup `irsync`
  raises its soft limit to the hard limit (on macOS to at most 10,240), so the
  window is the full 4,096 wherever the hard limit is at least 8,448. A fresh
  file's `.part` is opened by its first write, so files waiting for the
  sender's data hold no descriptor.
- **Data streams**: the sender keeps at most `--parallel` QUIC streams open at
  once, across all files of the session, 128 by default (`DEFAULT_PARALLEL` in
  `crates/irsync/src/send.rs`). A daemon that sends always uses 128. A run of
  contiguous chunks longer than 64 MiB is split, so one large file spreads
  over several streams.

The first hides the per-file round trips; the second fills the link.

## Compared with rsync

| Dimension | rsync | irsync |
| --- | --- | --- |
| Delta unit | basis block (700 B .. 128 KiB, ~sqrt(size)), matched at any byte offset | content-defined chunk (64 KiB .. 1 MiB, 256 KiB average), matched whole anywhere the index knows; inside a missing chunk of a `DELTA` file, blocks (512 B .. 4 KiB) matched at any byte offset of the old file's neighbourhood |
| Smallest file that gets a delta | any (blocks go down to 700 B) | any whose old version on the receiver is at least 1 KiB and whose new version is at least 512 B (blocks go down to 512 B) |
| What a 1-byte edit in a 1 GiB file sends | ~32 KiB literal + tokens | ~1 KiB literal + a few operations, with ~10 KiB of block signatures the other way |
| Signature direction | receiver to sender (basis blocks) | sender to receiver (new file's chunks); receiver to sender (block signatures, per missing chunk of a `DELTA`) |
| Signature size, 1 GiB file | ~224 KiB (32,768 blocks of 32 KiB x 7 B: 4 B rolling + 3 B strong checksum, the length rsync's `sum_sizes_sqroot` picks for this file size and block size) | ~200 KiB (4,096 chunks x 49 B), plus ~10 KiB per missing chunk offered a basis |
| Signature for a *new* file | none | full chunk list, ~0.02% of the data |
| Reuse scope | the basis at the same path (`--fuzzy`, `--link-dest`, `--compare-dest` widen it) | every chunk of every file the receiver's index knows; before a whole file is fetched, up to four destination files of its size are hashed into the index |
| Rename detection | none (name similarity only, `--fuzzy`) | by content: a renamed or copied file's chunks are found in the index and copied locally |
| Digest caching across runs | none; `-c` re-reads everything | persistent SQLite index keyed on size, mtime, ctime, inode; `-c` trusts none of it and re-reads both sides |
| Content unchanged, mtime changed | the file is rebuilt and rewritten | one read on each side whose index row is stale; metadata applied in place, no write |
| Round trips before first data byte | ~0 (pipelined) | 1.5 (plan, chunk list, request) |
| Serial disk passes before first data byte, one big changed file | 1 (basis checksums) | 1 (the sender's hash and the receiver's old-file hash run concurrently; local copies overlap the transfer) |
| Extra receiver pass after last byte | none (MD5 computed while writing) | none (BLAKE3 folded from the verified chunks) |
| Streams | one, in order | up to `--parallel` QUIC streams (128 by default), runs split at 64 MiB |
| Files in flight | pipelined, bounded by the window | up to 4,096 per session, fewer when the open-file limit is low |
| Resume | `--partial` keeps bytes; the next run re-checksums them | chunk-granular, verified ranges persisted; plan persisted |
| In place, append | `--inplace`, `--append`, `--append-verify` | the same three; by default a `.part` that is a reflink clone of the old file where the filesystem allows, so only changed chunks are written and the file is still renamed into place |
| Per-block integrity | 4-byte rolling + 2..16-byte MD5 | 32-byte BLAKE3, checked when the sender reads and before the receiver writes |
| Whole-file integrity | MD5 (xxh3 negotiable in 3.2+), one redo | BLAKE3, folded from the verified chunks; a mismatch re-offers the file once |
| Change during transfer | noticed at the final whole-file check; whole file redone | a change while the file is hashed, or after it, is noticed (the sender stats the file after hashing, the receiver asks the sender before placing, and each chunk is checked as it leaves), and the file is re-offered once, then given up with exit 23; `--paranoid` also sends from a reflink clone or hashes again, and re-checks every listed file at the end ([Files that change during a sync](correctness.md#files-that-change-during-a-sync)) |
| Sparse files | `-S` finds zero runs at write granularity | all-zero chunks are left as holes |
| Compression | `-z` | none |
| Largest file | no protocol limit | 2^24 chunks per list, about 4 TiB of typical data |
| Transport | TCP or SSH | iroh QUIC, NAT traversal, pinned Ed25519 identities |

## Where irsync is ahead

- **Touched files.** A file whose mtime changed and whose content did not (a
  `touch`, an editor that saved the same bytes, a restore) costs one read on
  each side whose index row is stale and four small messages; the destination
  keeps its inode and takes the new metadata. rsync rebuilds and rewrites it.
- **Moved, renamed and duplicated data.** Any chunk anywhere in the indexed
  destination is reused, so a renamed 10 GiB file transfers nothing; rsync
  sends it again. A destination that rsync or a restore filled is hashed into
  the index on demand, or ahead of time by
  [`irsync index`](../how-to/seed-the-index.md).
- **Small edits in large files.** The delta block is chosen per chunk, not per
  file, so a one-byte edit in a 1 GiB file costs about 1 KiB of literal
  against rsync's 32 KiB, and a hundred scattered 4 KiB writes about 0.6 MiB
  of literals and 1 MiB of signatures against rsync's 3.2 MiB of literals and
  224 KiB of signatures.
- **Atomic and cheap updates at once.** Where the filesystem clones files, a
  changed file costs only its changed chunks in space and writes and still
  lands by rename. rsync gets the first only with `--inplace`, which gives up
  the second.
- **No read-back.** The receiver reads a received file at most once, to verify
  chunks it adopted or copied locally, and writes it once; the whole-file hash
  is folded from what it verified.
- **Bad and fat links.** Many parallel streams, large files split across
  them, a scan that runs alongside the transfer, and chunk-granular resume
  across disconnects, process deaths and reboots (see
  [Sessions and resume](sessions-and-resume.md)). rsync's single in-order
  stream is limited by one congestion window, and `--partial` keeps bytes that
  the next run checksums again.
- **Integrity.** Every chunk is checked with a 32-byte BLAKE3 hash when the
  sender reads it and again before the receiver writes it, and the whole file
  against its BLAKE3 hash, against rsync's 2- to 16-byte truncated MD5 per
  block. A chunk that changed after the file was hashed is caught when the
  sender reads it, before stale bytes leave. A change while it is being
  hashed, or after it in chunks the receiver takes from its own copy, is
  caught by a stat of the source after hashing and before placing. Writers
  that leave the times alone, such as memory-mapped databases, need
  `--paranoid`. See
  [Files that change during a sync](correctness.md#files-that-change-during-a-sync).
- **Sender CPU.** A changed or new file costs the sender BLAKE3 hashing
  only: one pass for its chunk list, and each requested chunk hashed again as
  it is read for sending. The rolling match runs only over chunks the receiver
  offered a basis for, not over every byte of every changed file.
- **Reachability and identity.** NAT traversal and pinned keys without SSH or
  port forwarding (see [Identity and trust](identity-and-trust.md)). This is
  irsync's reason to exist, not a property of the algorithm.

## Where rsync is ahead

- **Compression.** rsync's `-z` reduces the bytes on the wire for
  compressible data; irsync has no compression (see
  [Known limitations](../reference/limitations.md)).
- **Zero round trips before the first data byte.** rsync's generator runs
  ahead and the sender's token stream follows without waiting. irsync spends
  four control messages per file and 1.5 round trips before its first data
  byte: `PlanItem`, chunk list, `NeedChunks`, then data, then `FileAck`. Up to
  4,096 files in flight hide this, but do not remove it: a file holds its slot
  from its `PlanItem` until its `DONE`, so one slow file holds a slot for its
  whole life. Before `NeedChunks` goes out, the receiver also runs one SQLite
  lookup per missing chunk.
- **The first push reads the source twice.** Each new file is read once for
  its chunk list and once to send it. rsync reads a new file once. The page
  cache hides the second read for files that fit in memory, and four hashing
  workers overlap it across files.
- **Delta reach.** A missing chunk is matched only against the old file's
  neighbourhood of its own offset. Content that moved by more than about a
  chunk, and is not a whole chunk, is sent as literal; rsync finds it anywhere
  in the basis. Whole chunks are found anywhere through the index.
- **Sparse files.** irsync leaves only whole zero chunks as holes, so a run of
  zeros shorter than a chunk is written. rsync `-S` finds zero runs at write
  granularity.
- **Fixed chunk parameters.** The chunk sizes are protocol constants. Changing
  them needs a new protocol version and invalidates every index on every machine.
- **Memory per file in flight.** The receiver holds two copies of a file's
  chunk list while it is in flight, about 48 bytes per chunk each, which adds
  up when many large files are in flight at once.
- **Opportunistic candidates.** Before fetching a whole file, the receiver
  hashes only destination files of exactly its size, at most four, of at
  least 64 KiB, and only those its walk has already listed. Seeding the index with `irsync index` or
  `warm_index` covers the rest.
- **Largest file.** A chunk list holds at most 2^24 records, about 4 TiB of
  typical data; rsync has no such limit.
- **Extended attributes and ACLs.** rsync copies them with `-X` and `-A`;
  irsync copies neither (see
  [Known limitations](../reference/limitations.md#rsync-options-irsync-does-not-have)).

A limit both share: with `--inplace` reuse is forward-only, so an insertion
near the start of a file loses the reuse below it, in rsync and in irsync
alike. Without `--inplace`, irsync does not pay it.
