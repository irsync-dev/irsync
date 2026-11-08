# Receiving a changed file

How the receiver fills one changed file once the sender's chunk list is in
hand: how it overlaps the passes over the file with each other and with the
network, how it settles a file whose content turns out not to have changed,
and how a chunk it lacks is rebuilt from block matches against the old file.
Function names point at where each step lives, under
`crates/irsync/src/`. The comparison with rsync is in
[The transfer algorithm](transfer-algorithm.md); the messages are in
[Protocol](../reference/protocol.md).

## The problem

A `DELTA` file exists on both sides as a regular file and differs in size or
mtime. The sender digests the new content and sends its chunk list; the
receiver must produce a file whose every chunk verifies against that list,
then put it into place.

Four things have to happen for that: the sender hashes the new file, the
receiver hashes the old one, chunks the receiver already holds are copied
locally, and the rest cross the network. Done one after another, the first
three are each a full pass over the file, and the transfer, the only part that
needs the network, waits for all of them. The receiver runs them overlapped
instead, and keeps these guarantees: every byte of the file is verified
against the sender's hash before it counts, local data is trusted no more than
network data, and a stale local source degrades to a network fetch.

## The timeline of one DELTA file

### Plan time: the old file's hash starts early

`Planner` (`session/planner.rs`) merge-joins the two listings and calls
`plan::decide`. When the decision is `Delta`, at the moment the `PlanItem` is
handed to the control stream, the planner spawns a task that runs
`recv_io::index_old_file` on the destination path.

`index_old_file` takes an `lstat` (size, mtime, ctime, inode) and asks the
session's hasher for the file's digest. A fresh index row answers at once;
otherwise the file is read in full, cut with FastCDC, and every chunk's BLAKE3
hash and offset is stored in the index with the whole-file hash. If the file
vanished, is not regular or cannot be read, the failure is logged at debug
level and nothing else follows from it: no local chunks are found, and
everything comes over the network.

The task's handle travels with the file to the receiver. Meanwhile the sender
has received the `PlanItem` and begun its own digest of the new file, so the
two full reads run on two machines at the same time.

A `WHOLE` file has no old content, so no task is spawned. A moved or copied
file needs no step of its own: its chunks are in the index like any other
file's and surface through the lookup below, which `--no-rename-detection`
turns off.

### Chunk list arrival

The chunk list arrives as `ChunkListPart` frames, which
`Receiver::on_chunk_list_part` reassembles (`session/parts.rs`) into one
digest. A malformed sequence fails the file. A connection lost in the middle
of a sequence drops the half-built list; the resumed session gets a fresh
sequence.

`Receiver::on_chunk_list` (`session/receiver.rs`) bumps the file's
generation, retires any earlier writer for the file, so that no stale task can
touch the `.part` afterwards, takes the hash task's handle out of the file's
state, and spawns `open_writer`. The handle is taken, not cloned: a second
chunk list for the same file, after a reconnect, finds none, and `open_writer`
calls `index_old_file` itself, which by then is a cache hit.

### Open and partition

`recv_io::open_writer` copies nothing. It:

1. **Takes the old file's hash** (`DELTA` only): it awaits the handle, which
   yields the old file's digest with the stat it was computed under. Without a
   handle it calls `index_old_file`.
2. **Compares whole-file hashes.** If the old file's size and BLAKE3 hash
   equal the chunk list's, the destination already holds the new content. The
   receiver opens it without following symlinks and checks with `fstat` that
   size, mtime, ctime, device and inode are those the hash was computed under
   (`unchanged_file`). If they are, it returns `Opened::Identical` with that
   descriptor, and no `.part` is created; see
   [Settle in place](#settle-in-place). If the file changed meanwhile, or the
   hashes differ, it goes on.
3. **Opens the `.part`**, locked (`FileWriter::open_tracked`). The writer
   adopts the progress recorded for an earlier `.part` of the same expected
   file and keeps a *verified* bit per chunk. When there is no `.part` yet and
   no old file to clone, the open is deferred: the writer holds no descriptor
   until its first write creates and locks the file. A file waiting for the
   sender's data thus costs no open file.
4. **Partitions the missing chunks** (`local_sources`). On one of the index's
   read connections, `find_chunks` lists the files that hold each missing
   chunk, with the inode and size each entry records: the first alone, since
   it nearly always is still there, then pages of 16. The first candidate that
   is still the file the index describes (same inode and size on disk) gives a
   path and an offset, and the chunk goes into the *local* set. Files that are
   gone or were replaced are passed over and queued for removal from the
   index, so a deleted tree's leftovers cost one `lstat` each, once. A chunk
   with no such file goes into the *remote* set. If the lookup itself fails,
   the whole missing set goes remote, which is always correct.

   For a `WHOLE` file the partition is preceded by `warm_candidates`: the
   receiver's walk noted every regular file of at least 64 KiB without a fresh
   index row, by size (`warm::Cold`), and up to four of exactly this file's
   size are hashed now, so a renamed or copied file the index did not know is
   found by the lookup. Each candidate is tried once per session.
5. **Picks bases** (`DELTA` with an old file and a non-empty remote set).
   `basis_specs` chooses which remote chunks to offer a basis for, and the
   old-file region of each: from one chunk length before the chunk's offset to
   two after, clamped to the old file. A chunk starting at or past the old
   file's end gets none; no chunk gets one when more than half the chunks are
   missing, unless the file has at most two; a region under 1 KiB is skipped;
   and the regions stop once they add up to the old file's size or three
   maximum chunk lengths, whichever is more. If any basis is chosen, the old
   file is opened without following symlinks, and that descriptor is used for
   the signatures and later for the copies, so a rename of the destination
   meanwhile changes nothing.

The result carries the writer, the two sets, the digest, the basis choices and
the old file's descriptor. The sets are disjoint and together cover exactly
the missing chunks; the chunks with a basis are a subset of the remote set.

### Request first, then copy

`Receiver::on_opened` runs on the receiver's event loop. When both sets are
empty, every chunk was adopted: the receiver sends an empty `NeedChunks`
(every chunk list needs an answer, and an empty one tells the sender to move
on) and finalizes. Otherwise it builds a route for the data streams:

| Field | Meaning |
| --- | --- |
| `gen` | The file's generation; stale streams and task results carry an older one and are ignored. |
| `slot` | The one writer that both fillers share, behind a mutex. |
| `digest` | The chunk table: offset, length and hash per chunk. |
| `pending` | Units of delivery still running; see [Completion](#completion). |
| `path` | The file's path, for events and logs. |
| `old` | The old file's descriptor while bases over it are out, for rebuilding chunks from operations. |

`pending` starts at the number of remote chunks, plus one if the local set is
not empty: one unit per remote chunk and one for the whole local copy task.
Then, in this order:

1. the route goes into the routing table, so the earliest data stream finds
   it;
2. `NeedChunks` for the remote chunks without a basis goes out;
3. if any basis was chosen, `offer_bases` is spawned, which sends the bases
   and requests their chunks;
4. if the local set is not empty, `copy_local_chunks` is spawned.

Step 2 comes before step 4 on purpose: the sender starts reading and
streaming as soon as the request lands, while the receiver's disk fills in
the rest and the signatures are computed. If the remote set is empty but the
local set is not, the receiver still sends an empty `NeedChunks`, so the
sender may move on. `pending` counts every remote chunk from the start,
whichever request carries it, so the file cannot complete before its last
request has even been sent.

### Bases and operations

`recv_io::offer_bases` runs as one task per file. For each chosen chunk it
reads the region from the old file's descriptor, outside every lock, signs it
(`delta::Basis::sign`: blocks of `sqrt(region)` bytes clamped to 512 B ..
4 KiB, per block a 32-bit rolling sum and the first 8 bytes of the block's
BLAKE3 hash), and sends it as a `ChunkBasis`. The bases go out in batches of
at most 64, each batch followed by the `NeedChunks` for its chunks, and a
session holds at most 32 MiB of bases that are sent but not yet requested. A
region that cannot be read ends the signing, and the chunks left over are
requested raw. The control stream is ordered, so the sender holds every basis
before it sees the request that uses it.

On the sender (`sender_role::on_basis`, `send::payload`) a basis is kept per
file and chunk until the request for that chunk arrives, then handed to the
data-stream task. For such a chunk the task runs `delta::encode`: a window of
one block slides over the chunk's bytes one byte at a time, the rolling sum is
updated in place, and each position is checked against a 64 Ki-bit filter,
then the signature map, then the strong hash. A hit emits a copy of that
block's range in the old file and jumps a block ahead; a miss extends the
current literal. Adjacent copies and literals are coalesced. If the operations
are not at least a quarter smaller than the chunk, the chunk's bytes go
instead; otherwise the `DataChunk` carries `ops` and no `data`. A chunk
requested a second time has no basis any more and goes raw.

On the receiver, `read_range` sees a frame with operations and calls
`rebuild`. The operations must add up to exactly the chunk's length, checked
before anything is read; literals are copied in, and copies are read from the
old file's descriptor with positioned reads, outside the writer lock. The
result goes through the same `write_chunk` as every other chunk. A rebuild
that fails for any reason (an operation list that does not add up, a copy past
the old file's end, a read error) or a rebuilt chunk that does not hash to
its record leaves the chunk missing, and [the re-request](#recovery) asks for
it raw. Nothing in this path is trusted: the old file may have changed in
place since it was signed, the strong hash is 8 bytes, the sender may be
hostile, and each of these ends in a chunk that does not verify and is asked
for again.

### Two fillers, one writer

**The network side** (`accept_streams`, `read_stream`, `read_range`). Each
data stream's header names the session, the file and a run of chunks. The
reader looks up the route, reads chunk by chunk (rebuilding one that arrived
as operations), hashes each, takes the writer lock and calls
`FileWriter::write_chunk`, which checks length and hash again and writes at
the chunk's offset.

**The local side** (`copy_local_chunks`) walks the local set in order. For
each chunk it reads the bytes from the source file with a positioned read,
outside the writer lock, so network chunks keep landing meanwhile; then it
locks the slot, stops if the slot is empty (the writer was retired by a newer
chunk list or a failure), and calls the same `write_chunk`.

A local chunk is thus verified against the sender's hash exactly like a
network chunk, and both fillers set the same verified bit. Writes from the two
sides land at disjoint offsets; the lock serialises only the writes, never the
reads or the network. A local chunk that fails (the source vanished or shrank,
a read error, a hash mismatch because the source was changed in place without
a new mtime) is logged at debug level and skipped; its bit stays clear.

### Completion

Both fillers end through `finish_unit`: a stream reader with the number of
chunks its run accounted for, delivered or not; the copy task once, with one
unit, after its whole list. `finish_unit` subtracts the units from `pending`
and notes whether this call brought it to zero, then locks the slot:

- an empty slot means the writer was retired or already reported: nothing to
  do;
- if no chunk is missing, it takes the writer out of the slot and reports the
  file ready, and the receiver finalizes it;
- else, if this call drained `pending`, it reports the file drained;
- else another unit is still running and will report.

Taking the writer out of the slot makes a late unit harmless: a stream that
straggles in after the file is complete finds the slot empty.

### Recovery

A drained file with chunks still missing reaches `Receiver::on_drained`. It
reads the missing set from the writer, checks that `pending` is zero, counts a
re-request against `MAX_REREQUESTS` (3), adds the missing count back onto
`pending`, and sends `NeedChunks` for those chunks, without bases. The sender
delivers them raw and the normal path verifies them.

This is the path for a data stream that ended short, and the local copy task
and the rebuild from operations use it too, by being units of the same
request. A stale local source or a stale basis therefore costs one round trip
and the bytes of the chunks that did not verify, and the file still ends with
a verified whole-file hash. The stale index row is not corrected here; the
next scan of that source file replaces it. A file still incomplete after
three re-requests fails with `WRITE_ERROR`.

### Finalize

`recv_io::finalize` runs `fsync`, compares the whole-file hash with the
sender's, applies metadata through the descriptor, renames the `.part` over
the destination, records the file in the index and sends `FileAck DONE`.

The hash is not computed by reading the `.part` back. BLAKE3 is a Merkle tree
over 1 KiB leaves at absolute offsets, and the writer folds the file's hash
from the chunks it verified (`tree_hash::TreeHash`): the leaf-aligned interior
of each chunk is hashed as aligned power-of-two subtrees through the `blake3`
crate's `hazmat` API, the up to 1,023 bytes at either end of a chunk wait
until the neighbouring chunk completes their leaf, sibling nodes merge eagerly
so that a few dozen chaining values are held at most, and the root is folded
at finalize along the tree's own split points. A chunk adopted from an earlier
`.part` or a clone is read once to verify it and feeds the tree from that
read; a sparse hole feeds its zeros. The result equals `b3sum` of the file.
What finalize does not catch is a byte changed on disk after the verified
write, which rsync does not catch either.

### Settle in place

When `open_writer` returns `Opened::Identical`, the content is already there,
and the file is settled in place rather than placed. The receiver sends an
empty `NeedChunks`, which tells the sender to wait for the acknowledgement,
emits the held-back `-i` line if any attribute differs, and spawns
`settle_identical`. That task discards a leftover `.part` of an earlier
attempt with its progress record, unless another writer holds it, applies the
sender's mode, ownership and times to the existing inode through the open
descriptor, and records the file in the index with the sender's digest, so
that the next run's quick check passes. The receiver then acknowledges `DONE`.

The file counts as a metadata-only update: no bytes, no placed file. A
metadata failure fails the file like a write error, and the next run tries
again. A `touch` on either side, or a restore from backup, comes to one read
of the old file on the side whose index row was stale, four small control
messages and no write. Under `-c` no index row is trusted on either side, so
every file present on both sides takes this path, read once per side.

### Where the bytes land: a cloned .part, in place, or appended

By default the `.part` is a separate file, renamed over the destination at the
end. Three variations change where the bytes are written.

**A cloned `.part`.** `open_writer` hands `FileWriter::open_from` the old
file's descriptor and chunk list whenever the old file is still the one that
was hashed. An empty `.part` is first cloned from it with `FICLONE`
(`recv::reflink`): btrfs, XFS, bcachefs, ZFS with block cloning. On success
every chunk the old list holds at an offset where the new list wants the same
chunk is read, verified and fed to the tree hash (`adopt_from`), and neither
copied nor written. Only changed and moved chunks are written, and only those
take new space. A refused clone (tmpfs, ext4, another device with
`--partial-dir`) costs only the attempt, and the `.part` starts empty. A
`.part` that already holds bytes is adopted as before.

**In place** (`--inplace`). `FileWriter::open_inplace` locks the destination
itself, checks that it is the hashed inode, adopts the old chunks at unchanged
offsets the same way, grows the file at once and shrinks it only at finalize,
which also skips the rename. Reads of the old content and writes of the new
one share a file, so writes are ordered by a gate on the route: `admit(idx)`
waits until every chunk before `idx` is settled (written, rejected, or given
up by a stream that ended early), and `settle(idx)` advances the cursor. A
chunk below the cursor is admitted at once, which is how a re-request
rewrites it. Both fillers go through the gate, and a retired writer closes it,
releasing every waiter. The old content is used only at or above the chunk
being written: a local copy whose old offset is below the chunk's new offset
becomes a remote chunk, and a basis starts at the chunk's own offset. That is
rsync's rule, with rsync's consequence: a deletion near the start keeps its
reuse, an insertion loses it. An in-place file gets one ascending request
after its bases, so the sender opens its runs in order and the one the cursor
needs is never behind the others. What `--inplace` costs, as in rsync:
readers see a torn file during the transfer, a crash leaves a mixture of old
and new, and hard links to the destination see the new content.

**Append** (`--append`; planner decision `Append`, action `APPEND`). The
sender's list covers only the tail. `open_writer` awaits the old file's hash,
requires it to be the planned prefix (its length equals the tail's first
offset, and the inode is unchanged), and builds the full list from the old
chunk list plus the tail, with no whole-file hash to compare: `finalize`
returns the hash folded from the bytes, which goes into the index, so the
next run quick-checks the file. Chunk indices on the wire are the sender's,
relative to the tail. A destination that is not the planned prefix fails the
file with a message naming append. `--append-verify` is the ordinary `DELTA`
path with the planner's skip rule, so a changed head is repaired.

### The itemize line of a DELTA file

The planner decides `DELTA` before anyone knows whether the content changed,
so for a `DELTA` file it does not print the `-i` line at plan time. It
computes two lines, the transfer as planned and the attributes-only line that
applies if the content proves unchanged (none if no preserved attribute
differs), and the receiver prints the first when it opens the `.part` and the
second when it settles the file in place. A touched file therefore itemizes as
`.f..t......`, as in rsync. A dry run requests nothing, so the planner
itemizes at once. A file of a resumed plan carries no held-back line.

## The chain

For a large file with a small change and a warm index, where *S* is the time
of one full pass over the file on either disk:

| Phase | Done one after another | As the receiver runs it |
| --- | --- | --- |
| Sender's digest | S | S, concurrent with the old file's hash |
| Receiver's hash of the old file | S (0 if cached) | overlapped with the sender's digest |
| Local copy into the `.part` | S, before the request | concurrent with the transfer |
| Network transfer | after all of the above | right after the index lookups |
| Whole-file check | S, reading the `.part` back | none: folded from the verified chunks |
| Bytes for a small change inside a chunk | the chunk (64 KiB .. 1 MiB) | about a block (512 B .. 4 KiB) of literal, plus about 4% of the chunk in signatures the other way |
| Writes of unchanged chunks | S, copied into the `.part` | none with a cloned `.part` or in place; a read of them for the tree hash |
| Free space for a changed file | its full size | the changed chunks (clone) or none (in place) |

Done one after another, the critical path is up to 4 S plus the transfer. As
the receiver runs it: S for the concurrent hashes, plus the longer of the
local copy and the transfer. The receiver reads the old file once for its
hash and once more, only around the missing chunks and capped at the old
file's size, for the signatures; it writes the new one once.

Receiver I/O for the same case, in reads and writes of the file's size:

| | Reads | Writes |
| --- | --- | --- |
| rsync, default | 2 | 1 |
| rsync `--inplace` | 1 | changed blocks only |
| irsync, cloned `.part`, old file's hash cached | 1, verifying the clone's chunks | changed chunks only |
| irsync, cloned `.part`, old file's hash not cached | 2 | changed chunks only |
| irsync `--inplace` | 1 or 2, as above | changed chunks only |
| irsync, plain `.part` (no clone support) | 1 or 2, as above | 1 |

## Invariants

- **A request cannot miss its streams.** The route is in the table before
  `NeedChunks` leaves the receiver.
- **Every chunk is verified once, by the same code, whatever its source.**
  `write_chunk` is the only way into the `.part`.
- **Nobody reports twice.** A ready file's writer leaves the slot; only the
  call that brought `pending` to zero reports a drained file; a re-request
  raises `pending` before the new streams arrive.
- **The old file is read at most once per session for its hash.** The early
  hash is awaited before the lookup; any later hash of it is a cache hit.
- **Local data is never trusted.** A mismatch degrades to a network fetch,
  not an error, and never reaches the `.part`.
- **Operations are never trusted either.** A chunk rebuilt from them is
  verified by `write_chunk` like any other; the old file is only read, through
  a descriptor opened once; a rebuild that fails leaves the chunk missing, and
  its re-request carries no basis.
- **A cloned chunk is verified before it counts.** Each adopted chunk is read
  and hashed before its bit is set, as a resumed `.part`'s chunks are.
- **In place, nothing above the cursor is written before it is read.** Writes
  ascend through the gate; old content is read only at or above the chunk
  being written; a rejected or undelivered chunk settles the gate, so the
  cursor never waits for it.
- **An append never guesses the prefix.** The sender's tail is accepted only
  when the destination is the exact inode and length that was planned;
  otherwise the file fails, and a run without `--append` repairs it.
- **Reconnects and failures are safe.** A new chunk list bumps the generation
  and retires the writer. A copy task of the old generation finds the slot
  empty and stops; its late `finish_unit` has nothing to report.
- **The sender's view stays simple.** It answers every `NeedChunks` with data
  streams; the bases are optional extras it may use for the one request that
  follows them. A file settled in place looks to the sender like one whose
  every chunk was adopted: an empty request, then `DONE`.
- **Settling in place trusts the quick check, not the clock.** The destination
  is settled only if `fstat` on the open descriptor matches the stat under
  which it was hashed, in size, mtime, ctime, device and inode; otherwise it is
  received as a delta.

The tests that pin these properties are listed in
[Testing](../contributing/testing.md#receiving-a-changed-file).
