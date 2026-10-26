# Receiving a changed file: the overlapped delta algorithm

How the receiver fills one file once the sender's chunk list is in hand,
after the restructuring of 2026-10-22, how it settles a file whose
content turns out not to have changed at all, and how a chunk it lacks is
rebuilt from block matches against the old file instead of being sent
whole. Everything here was checked against the code in the working tree
of that date; function names point at where each step lives. The wire
protocol (`protocol.md`) gained two things for the last part: a
`ChunkBasis` control message from the receiver and an operation form of
the `DataChunk` frame; everything else only reorders what the receiver
does between messages it was already sending.

Companion documents: `IRSYNC_VS_RSYNC.md` (section 3.3 describes the
serialised flow this replaces, and why it mattered), `protocol.md` (the
messages named below), `superpowers/specs/2026-10-17-irsync-design.md`
(the design intent).

## 1. The problem

A `DELTA` file is one that exists on both sides as a regular file and
differs in size or mtime. For such a file the sender digests the new
content and sends a `ChunkList`; the receiver must produce a `.part` whose
every chunk verifies against that list, then finalize it into place.

Before the change, the receiver did the following strictly in order inside
one task (`open_writer`), with the network idle throughout:

1. hash the old destination file (cache hit if irsync itself placed it and
   nothing touched it since, a full read otherwise);
2. for every missing chunk, look it up in the index and, if some indexed
   file holds it, copy it into the `.part` at once;
3. only then send `NeedChunks` naming what was still missing.

The sender's digest, the old-file hash, the local copy and the network
transfer were four serial phases. For a large file with a small change, the
first three are each a full pass over the file's size, and the transfer,
which is the only part that needs the network, waited for all of them.

The new algorithm keeps every guarantee of the old one (every byte of the
`.part` is verified against the sender's hash before it counts, local data
is trusted no more than network data, a stale local source degrades to a
network fetch) and removes the serialisation.

## 2. The timeline of one `DELTA` file

### 2.1 Plan time: the old-file hash starts early

`Planner` (`session/planner.rs`) merge-joins the two listings and calls
`plan::decide`. When the decision is `Delta`, at the moment the `PlanItem`
is handed to the control stream, the planner spawns a background task that
runs `recv_io::index_old_file` on the destination path.

`index_old_file` does an `lstat`, builds a `ScanItem` carrying size, mtime,
ctime and inode, and calls the shared hasher (`ctx.hasher.digest`). The
hasher consults the index cache keyed on those attributes. A fresh row
returns at once; otherwise the file is read in full, cut with FastCDC, and
every chunk's BLAKE3 hash and offset is stored in the index along with the
whole-file hash. Failure (the file vanished, is not regular, cannot be
read) is logged at debug level and otherwise ignored: the only consequence
is that no local chunks will be found and everything comes over the network.

The `JoinHandle` of this task travels in `Planned.prehash` to the receiver,
which keeps it in the per-file `RFile.prehash`. Meanwhile the sender has
received the `PlanItem` and begun its own digest of the new file. The two
full-file reads that used to be back to back now run on two machines at the
same time.

A `WHOLE` file has no old content to hash, so no task is spawned. The
rename-detection source found by the planner (`Decision::LocalCopy`) needs
no special handling any more: it is an indexed file like any other, and its
chunks surface through the ordinary lookup in 2.3. The `local_copy` field
that used to carry its path is gone.

### 2.2 Chunk list arrival

A chunk list arrives as a sequence of `ChunkListPart` frames that
`Receiver::on_chunk_list_part` reassembles (`session/parts.rs`) into one
native digest before handing it on. That digest is shared by reference
between the file's state, its route and the open result; the writer keeps
its own validated copy. A connection loss mid-sequence drops the
half-built list with the connection; the resumed session gets a fresh
sequence from the sender. A malformed sequence fails the file.

`Receiver::on_chunk_list` (`session/receiver.rs`) bumps the file's
generation, retires any earlier writer for that file (`retire_writer`
removes the route and drops the writer out of its slot, so no stale task
can touch the `.part` afterwards), takes `prehash` out of the per-file
state, and spawns `open_writer`.

The handle is *taken*, not cloned. A second chunk list for the same file,
which happens after a reconnect, finds no handle, and `open_writer` simply
calls `index_old_file` itself; by then that is a cache hit.

### 2.3 Open and partition

`recv_io::open_writer` does five things and copies nothing:

1. **Take the assembled list**, already a native `FileDigest` (section 2.2).
2. **Take the old file's hash** (`DELTA` only): await the prehash handle,
   which yields the old file's digest together with the stat it was
   computed under (`OldFile`). If the planner never started a task
   (reconnect), call `index_old_file` here; that is a cache hit when the
   earlier connection already hashed the file.
3. **Compare whole-file hashes.** If the old file's size and BLAKE3 hash
   equal the chunk list's, the destination already holds the new content.
   The receiver then opens the destination without following symlinks and
   checks by `fstat` that size, mtime, ctime, device and inode are the ones
   the digest was computed under (`unchanged_file`). If they are, the
   function returns `Opened::Identical` with that open descriptor and no
   `.part` is created; section 2.4 settles it. If the file changed
   meanwhile, or the hashes differ, the normal path continues.
4. **Open** the locked `.part` (`FileWriter::open_tracked`). The writer
   adopts earlier progress recorded for that `.part` and keeps a `verified`
   bit per chunk; `missing_chunks()` lists the indices whose bit is clear.
   When nothing is at the `.part` name and there is no old file to clone,
   the open is deferred: the writer holds no descriptor until its first
   write (or `finalize`, for an empty file) creates and locks the `.part`.
   A file waiting for the sender's stream thus costs no open file, however
   many are in flight; the lock still covers every byte written to the
   `.part` up to its rename.
5. **Partition** the missing chunks. One blocking pass holds the index lock
   and asks `find_chunk` for each missing chunk's hash. A hit yields a path
   (an index key, turned absolute with `ctx.full_key`) and an offset inside
   that file; the chunk goes into the `local` set as a
   `(index, path, offset)` triple. A miss goes into the `remote` set. If the
   lookup itself fails (index error, poisoned lock), the whole missing set
   goes remote, which is always correct, and a warning is logged.

   For a `WHOLE` file (no old file at the path) the partition is preceded
   by `warm_candidates`: the receiver's walk noted every regular file of
   at least one minimum chunk length without a fresh index row, by size
   (`warm::Cold`), and up to four of exactly this file's size are hashed
   now through the session's hasher, so a renamed or copied file the
   index did not know is found by the lookup. Each candidate is tried
   once per session; `warm.rs` has the whole story, including
   `irsync index` for warming a tree ahead of time.

6. **Pick bases** (`DELTA` with an old file, remote set non-empty).
   `basis_specs` chooses which remote chunks to offer a basis for and the
   old-file region of each: from one chunk length before the chunk's
   offset to two after, clamped to the old file. No basis for a chunk
   starting at or past the old file's end (appended data), none at all
   when more than half the chunks are missing (a rewrite; a file of one
   or two chunks is exempt, see 3.2 of the comparison), and the regions
   stop once they add up to the old file's size or three maximum chunk
   lengths, whichever is more. If any basis is chosen, the old file is
   opened without following symlinks and the descriptor travels with the
   result: the signatures are computed from it and the copies later read
   from it, so a rename of the destination meanwhile changes nothing.

The result is `Opened::Writer` carrying the writer, the two sets, the
digest, the basis specs and the old-file descriptor. The sets are disjoint
and together cover exactly the missing chunks; the basis specs name a
subset of the remote set.

### 2.4 Dispatch: settle in place, or request first and then copy

`Receiver::on_opened` runs on the receiver's event loop. After the usual
generation check it looks at what `open_writer` returned.

**Identical.** The content is already there, so the file is settled in
place rather than placed. The receiver sends an empty `NeedChunks`, which
tells the sender to wait for the ack, emits the held-back itemize line if
any attribute differs (section 2.11), and spawns `settle_identical`. That
task discards a leftover `.part` of an earlier interrupted attempt along
with its progress record, unless another writer holds it, applies the
sender's mode, ownership and times to the existing inode through the open
descriptor, and records the file in the index with the sender's digest so
the next run's quick check passes. The receiver then acks `DONE`. The file
counts as a metadata-only update, not a transfer: no bytes, no placed
file, no file-done event. A metadata failure fails the file like a write
error, and the next run tries again. This is what a `touch` on either
side or a restore from backup comes to: one read of the old file on the
side whose index was stale, four small control messages, no write. Under
`-c` no index row is trusted on either side, so every file present on
both sides takes this path, read once per side.

**Otherwise** it looks at the two sets.

**Both empty.** Every chunk was adopted from an earlier `.part`. The
receiver sends an empty `NeedChunks` (the protocol requires an answer to
every chunk list, and an empty one tells the sender to move on) and goes
straight to finalize.

**Otherwise** it builds a `Route` for the data streams:

| field | meaning |
| --- | --- |
| `gen` | the file's generation; stale streams and task results carry an older value and are ignored |
| `slot` | `Arc<Mutex<Option<FileWriter>>>`, the one writer both fillers share |
| `chunks` | the chunk table (offset, length, hash per chunk) |
| `pending` | units of delivery still running, see 2.7 |
| `path` | the file's session path, for events and logs |
| `old` | the old file's descriptor while bases over it are out (`DELTA`), read by the rebuild of an operation frame |

`pending` starts at `remote.len() + 1` if the local set is non-empty, or
`remote.len()` if it is empty: one unit per remote chunk, plus one for the
whole local-copy task.

Then, in this order:

1. the route is inserted into the routing table, so the earliest possible
   data stream finds it;
2. `NeedChunks` with the remote chunks that have no basis goes out on the
   control stream;
3. if any basis was chosen, `offer_bases` is spawned (section 2.5); it
   ends with the `NeedChunks` for the chunks it signed;
4. if the local set is non-empty, `copy_local_chunks` is spawned.

Step 2 before step 4 is the point of the change. The sender starts reading
and streaming the moment the request lands, while the receiver's disk fills
in the rest and the signatures are computed. If the remote set is empty but
the local set is not, the receiver still sends an empty `NeedChunks`: the
sender must know it may move on to the next file, and the receiver's local
work is not the sender's concern. `pending` counts every remote chunk from
the start, whichever of the two requests carries it, so the request cannot
drain before the second half of it has even been sent.

### 2.5 Bases and operations: rsync inside a chunk

`recv_io::offer_bases` runs as one task per file. For each basis spec it
reads the region from the old file's descriptor (blocking, outside every
lock), signs it (`delta::Basis::sign`: blocks of `sqrt(region)` bytes
clamped to 512 B .. 4 KiB, per block a 32-bit rolling sum and the first
8 bytes of the block's BLAKE3), and sends it as a `ChunkBasis` on the
control stream. A region that cannot be read ends the signing; the rest
of the chunks come raw. Then, unless the writer has been retired
meanwhile, one `NeedChunks` naming every chunk of the specs goes out. The
control stream is ordered, so the sender holds every basis before it sees
the request.

On the sender (`sender_role::on_basis`, `send::payload`) a basis is kept
per file and chunk index until the request for that chunk arrives, then
taken out of the map and given to the data-stream task. For such a chunk
the task runs `delta::encode` on the chunk's bytes: a window of one block
slides over them one byte at a time, the rolling sum is updated in place,
each position is checked against a 64 Ki-bit filter, then the signature
map, then the strong hash; a hit emits a copy of that block's range in the
old file and jumps a block ahead, a miss extends the pending literal. Runs
of adjacent copies and of literals are coalesced. If the operations are
not at least a quarter smaller than the chunk, the chunk's bytes go out as
before; otherwise the `DataChunk` carries `ops` and no `data`. A chunk
asked for a second time (section 2.8) has no basis any more and goes raw.

Back on the receiver, `read_range` sees a frame with operations, converts
them, charges their wire size to the byte counters, and calls `rebuild`:
the operations must add up to exactly the chunk's length (checked before
anything is read), literals are copied in, copies are read from the old
file's descriptor with positioned reads, all outside the writer lock. The
result goes through the same `write_chunk` as every other chunk. A rebuild
that fails for any reason (no old file, an operation list that does not
add up, a copy past the old file's end, a read error) or a rebuilt chunk
that does not hash to its record is logged at debug level and the chunk
stays missing. Section 2.8 then has it re-sent raw. Nothing in this path
is trusted: the old file may have changed in place since it was signed,
the strong hash is 8 bytes, the sender may be hostile, and every one of
those ends in the same place, a chunk that is not verified and is asked
for again.

The receiver logs `basis` per basis sent and `ops` per chunk verified from
operations (test hooks).

### 2.6 Two fillers, one writer

**The network side** is unchanged in shape (`accept_streams`,
`read_stream`, `read_range`). Each unidirectional stream carries a
`DataStreamHeader` naming the session, the file and a contiguous chunk
range. The reader looks up the route, reads chunk by chunk (rebuilding
one that arrived as operations, section 2.5), hashes each one, takes the
writer lock, and calls `FileWriter::write_chunk`, which checks length and
hash again and does a positioned write at the chunk's offset.

**The local side** (`copy_local_chunks`) walks the local set in order. For
each triple it:

1. looks up the chunk's length in the route's chunk table;
2. reads the bytes from the source file with a positioned read
   (`read_local`: `open_regular_nofollow` plus `read_exact_at`), *outside*
   the writer lock, so network streams keep writing meanwhile;
3. locks the slot; if the slot is empty the writer was retired by a newer
   chunk list or a failure, and the task stops;
4. calls the very same `write_chunk`.

So a local chunk is verified against the sender's hash exactly like a
network chunk, and both fillers set the same `verified` bit on success.
Writes from the two sides land at disjoint offsets. The lock serialises only
the write calls, never the reads or the network receive.

Any failure of a local chunk (source vanished or shrank, read error, hash
mismatch because the source was modified in place without changing its
mtime so the index row is stale) is logged at debug level and the task
moves on. The chunk's bit stays clear. Nothing else happens yet; 2.8
describes how it is recovered.

### 2.7 Completion accounting

Both fillers end through `finish_unit(route, events, file_id, units)`:

- a stream reader calls it with the number of chunks its range accounted
  for, delivered or not (the stream may have ended early);
- the copy task calls it once, with one unit, after its whole list,
  regardless of how many copies succeeded.

`finish_unit`:

1. subtracts `units` from `pending`, saturating at zero, and notes whether
   this call brought the counter to zero (`drained`);
2. locks the slot; an empty slot means the writer was retired or already
   reported, and the call returns;
3. if `missing_chunks()` is empty, takes the writer out of the slot and
   sends `Done::Ready`; the receiver moves the file to finalize;
4. else if `drained`, sends `Done::Drained`;
5. else returns: another unit is still running and will make the report.

Taking the writer out of the slot on `Ready` is what makes a late unit
harmless: a stream that straggles in after the file is complete finds an
empty slot and does nothing.

### 2.8 Recovering a failed local copy or a failed rebuild

When the last unit ends with chunks still missing, `Done::Drained` reaches
`Receiver::on_drained`. It re-reads the missing set from the writer, checks
that `pending` really is zero, counts a re-request against
`MAX_REREQUESTS` (3), adds the missing count back onto `pending`, and
sends another `NeedChunks` naming those chunks. The sender delivers them
over the network and the normal path verifies them. No basis accompanies
a re-request, so these chunks come raw.

This is the pre-existing path for a data stream that ended short; the local
copy task reuses it by being one unit of the same request, and so does a
chunk whose rebuild from operations did not verify. A stale local source
or a stale basis therefore costs exactly one round trip and the bytes of
the chunks that did not verify, and the file still finalizes with a
verified whole-file hash. The stale index row is not corrected here; the
next scan of that source file replaces it.

A file still incomplete after three re-requests fails with
`Status::WriteError`, as before.

### 2.9 Finalize

`recv_io::finalize`: `fsync`, then the whole-file hash is compared with the
sender's, metadata is applied through the descriptor, the `.part` is
renamed over the destination, and `FileAck DONE` goes out. The index
record at the end is shared with the settle-in-place path (`record`).

The hash is not computed by reading the `.part` back. BLAKE3 is a Merkle
tree over 1 KiB leaves at absolute offsets, and the writer folds the
file's real hash from the chunks it verified in memory
(`tree_hash::TreeHash`): the leaf-aligned interior of each chunk is hashed
as aligned power-of-two subtrees through the `blake3` crate's `hazmat`
API, the up-to-1023 bytes at either end of a chunk wait until the
neighbouring chunk completes their leaf, sibling nodes merge eagerly so a
few dozen chaining values are held at most, and the root is folded at
finalize along the tree's own split points. A chunk adopted from an
earlier `.part` is read once to verify it and feeds the tree from that
read; a sparse hole feeds its zeros. The result equals `b3sum` of the
file. What finalize no longer catches is a byte changed on disk after the
verified write, which rsync does not catch either.

### 2.10 Where the bytes land: a cloned `.part`, in place, or appended

By default the `.part` is a separate file renamed over the destination at
the end. Three things changed with remedy 7 (`IRSYNC_VS_RSYNC.md` 3.6):

**Clone.** `open_writer` hands `FileWriter::open_from` the old file's
descriptor and chunk list (the `CloneSource`) whenever the old file is
still the one that was hashed. A `.part` that is empty is first cloned
from it with `FICLONE` (`recv::reflink`); on success every chunk the old
list holds at an offset where the new list wants the same chunk is read,
verified and fed to the tree (`adopt_from`), so it is neither copied nor
written. A refused clone (tmpfs, ext4, another device) costs only the
attempt. A `.part` that already holds bytes is adopted as before. The
receiver logs `clone` for it.

**In place** (`--inplace`, `ctx.opts.inplace`). `FileWriter::open_inplace`
locks the destination itself, checks it is the hashed inode, adopts the
old chunks at unchanged offsets the same way, and grows the file now but
shrinks it only in `finalize`, which also skips the rename. Writes are
ordered by a `Gate` on the route: `admit(idx)` waits until every chunk
before `idx` is settled (written, rejected, or given up by a stream that
ended early), and `settle(idx)` advances the cursor; a chunk below the
cursor is admitted at once, which is how a re-request rewrites it. Both
fillers go through the gate, and a retired writer closes it, releasing
every waiter. The old content is used only above the cursor: a self-copy
whose old offset is below the chunk's new offset becomes a remote chunk in
the partition, and `basis_specs` starts a basis at the chunk's own offset.
An in-place file gets one ascending `NeedChunks` after its bases, so the
sender opens its ranges in order and the range the cursor needs is never
behind the others. The receiver logs `inplace`.

**Append** (`--append`, planner decision `Append { from }`, action
`APPEND`). The sender's list covers the tail only. `open_writer` awaits
the prehash of the old file, requires it to be the planned prefix (its
length equals the tail's first offset and the inode is unchanged), and
builds the full list from the old chunk list plus the tail, with a zero
whole-file hash and `verify_whole` off: `finalize` then compares nothing
and returns the hash folded from the bytes, which `record` writes to the
index. Chunk indices on the wire are the sender's (tail-relative): the
route's `idx_base` is subtracted from every request and basis and added to
every stream header. A destination that is not the planned prefix fails
the file with a message naming append. The receiver logs `append`.
`--append-verify` is the ordinary `DELTA` path with the planner's skip
rule.

### 2.11 The itemize line of a `DELTA` file

The planner decides `DELTA` before anyone knows whether the content really
changed, so for a `DELTA` file it no longer emits the `-i` line at plan
time. It computes two lines, the transfer line as planned and the
attributes-only line that applies if the content proves unchanged (none
if no preserved attribute differs), and hands both to the receiver in
`Deferred.itemize`. The receiver emits the transfer line when it opens the
`.part`, and the attributes-only line when it settles the file in place.
A touched file therefore itemizes as `.f..t......`, as rsync prints it.
Dry runs are not affected: nothing is requested, so the planner itemizes
at once as before. A file of a resumed plan carries no held-back line.

## 3. The chain, before and after

For a large file with a small change, warm index, where *S* is the time of
one full pass over the file on either disk:

| phase | before | after |
| --- | --- | --- |
| sender digest | S | S, concurrent with the old-file hash |
| receiver old-file hash | S (0 if cached) | overlapped with the sender's digest |
| local copy into `.part` | S, before the request | S, concurrent with the transfer |
| network transfer | after all of the above | starts right after the index lookups |
| finalize re-hash | S | none (folded from the verified chunks) |
| bytes for a small change inside a chunk | the chunk (64 KiB .. 1 MiB) | about a block (512 B .. 4 KiB) of literal, plus ~3.5% of the chunk in signatures the other way |
| local write of unchanged chunks | S (copied into the `.part`) | none with a cloned `.part` or in place; a read of them for the tree hash |
| free space for a changed file | its full size | the changed chunks (clone) or none (in place) |

Critical path before: up to 4 S plus the transfer. After: S (the
concurrent hashes) plus the longer of local copy and transfer. The
receiver reads the old file once for its hash and once more, only around
the missing chunks and capped at its size, for the signatures; it writes
the new one once.

## 4. Invariants

- **A request cannot miss its streams.** The route is in the table before
  `NeedChunks` leaves the receiver.
- **Every chunk is verified once, by the same code, whatever its source.**
  `write_chunk` is the only way into the `.part`.
- **Nobody reports twice.** `Ready` takes the writer out of the slot;
  `Drained` is sent only by the call that observed `pending` reach zero; a
  re-request raises `pending` before the new streams arrive.
- **The old file is read at most once per session.** The prehash is awaited
  before the lookup; the lookup's own hash call is a cache hit by
  construction.
- **Local data is never trusted.** A mismatch degrades to a network fetch,
  not an error, and never reaches the `.part`.
- **Operations are never trusted either.** A chunk rebuilt from them is
  verified by `write_chunk` like any other; the old file is only ever
  read, through a descriptor opened once; a rebuild that fails leaves the
  chunk missing, and its re-request carries no basis.
- **A cloned chunk is verified before it counts.** The clone is a copy of
  bytes the index described; each adopted chunk is read and hashed before
  its bit is set, as a resumed `.part`'s chunks are.
- **In place, nothing above the cursor is written before it is read.**
  Writes ascend through the gate; old-content reads (self-copies, bases)
  are only ever at or above the chunk being written; a rejected or
  undelivered chunk settles the gate so the cursor never waits for it.
- **An append never guesses the prefix.** The sender's tail is accepted
  only when the destination is the exact inode and length that was
  planned; otherwise the file fails and a run without `--append` repairs it.
- **Reconnects and failures are safe.** A new chunk list bumps the
  generation and retires the writer. A copy task of the old generation sees
  the empty slot and exits; its late `finish_unit` finds nothing to report.
- **The sender's view stays simple.** It answers every `NeedChunks` with
  data streams as before; the bases are optional extras it may use for
  the one request that follows them. A file settled in place looks to the
  sender like one whose every chunk was adopted: an empty request, then
  `DONE`.
- **Settling in place trusts the quick check, not the clock.** The
  destination is settled only if `fstat` on the open descriptor matches
  the stat under which it was hashed, in size, mtime, ctime, device and
  inode; otherwise it is received as a delta.

## 5. Tests

`crates/irsync/tests/delta.rs` pins each property with end-to-end syncs
between two engines. The receiver's event log (`SideHooks::event`,
`test-hooks` feature) records `prehash`, `plan`, `chunklist`, `need`,
`chunk` (a network chunk written) and `copied` (the local task finished),
each tagged with the path.

- `delta_requests_remote_chunks_before_copying_local_ones`: a 4 MiB file is
  synced, its last MiB is changed, and every local copy is slowed by
  150 ms (`SideHooks::slow_local_copy`). The log must show `need` before
  any `chunk` and `chunk` before `copied`: the request left before the
  copies started, and network chunks landed while copies were still
  running. Fewer than 2 MiB cross the wire.
- `stale_local_chunk_is_re_requested_from_the_sender`: a placed file is
  corrupted in place with its mtime restored, so the planner skips it (no
  `plan` event for it) and its index rows are stale. A new file with the
  original content is then pushed. Every chunk is found locally, exactly one
  fails verification, the re-request counter reads 1, and the file verifies.
- `delta_hashes_the_old_file_once_starting_at_plan_time`: a pull with a
  fresh state directory reports exactly one file hashed on the receiver,
  with `prehash` logged before `chunklist`.

`crates/irsync/tests/sub_chunk.rs` pins the codec on its own
(`irsync::delta`): the rolling sum equals a fresh computation at every
position and block length; an unchanged chunk is one copy; one changed
byte, an insertion and a deletion cost at most two blocks of literal and
rebuild exactly; copies carry the basis offset in the old file; an
unrelated basis yields one literal as large as the chunk; a chunk shorter
than a block is all literal; adjacent operations coalesce; `decode`
refuses operations that do not add up to the chunk or overflow, and
reports a copy the old file cannot supply; a basis and the operations
round-trip through their wire forms, with malformed ones refused.

`crates/irsync/tests/sub_chunk_delta.rs` runs the path end to end; the
receiver logs `basis` per basis sent and `ops` per chunk verified from
operations.

- `one_changed_byte_in_a_large_file_costs_a_block_not_a_chunk`: 4 MiB,
  one byte changed, pulled: one basis, one chunk from operations, fewer
  than 16 KiB on the data path (measured 2,100 bytes).
- `scattered_page_writes_cost_blocks_not_chunks`: 16 MiB, eight 4 KiB
  writes, pushed (the sender's count is what it put on the wire): at
  least eight chunks from operations, fewer than 128 KiB (measured
  51,475 bytes).
- `a_single_chunk_file_with_one_edit_is_not_resent_whole`: 100 KiB, six
  bytes changed: fewer than 4 KiB (measured 550 bytes).
- `a_rewritten_file_offers_no_bases`: 4 MiB replaced: no `basis`, no
  `ops`, the whole file crosses.
- `chunks_past_the_end_of_the_old_file_get_no_basis`: 2 MiB appended to
  4 MiB: at most one basis (the old last chunk, whose end moved), about
  2 MiB on the wire.
- `old_file_modified_in_place_after_the_bases_falls_back_to_raw`: a test
  hook (`SideHooks::after_bases`) overwrites the destination in place
  once the bases are out; the rebuilt chunk is rejected (no `ops`), the
  re-request counter (`SideHooks::rerequests`) reads 1, and the file ends
  up equal to the source with no `.part` left.
- `a_connection_lost_mid_delta_resumes_and_completes`: the sending daemon
  drops the connection during the delta; the client reconnects, the
  session resumes, and the file verifies.

`crates/irsync/tests/inplace.rs` covers remedy 7 end to end (the clone
tests run only where `target/` is on a filesystem that clones, and say so
when they skip). `a_cloned_part_writes_only_the_changed_chunk`: one
`chunk`, no `copied`, a new inode after the rename.
`inplace_keeps_the_inode_and_writes_only_the_changed_chunk`,
`inplace_deletion_near_the_start_reuses_chunks_from_above` (one local copy,
under 1 MiB on the wire), `inplace_insertion_near_the_start_takes_shifted_chunks_from_the_network`
(byte-exact, over 2 MiB on the wire; it fails if the gate or the
backward-reference rule is removed), `inplace_on_a_push_is_honoured_by_the_daemon`,
`inplace_resumes_after_a_connection_loss`. `append_sends_only_the_tail_and_reads_only_the_tail`
(the sending daemon hashes nothing; the next run finds the file up to
date), `append_skips_a_changed_file_that_did_not_grow`,
`append_trusts_the_prefix_and_append_verify_does_not`,
`append_on_a_push_is_honoured_by_the_daemon`,
`append_fails_the_file_when_the_destination_is_not_the_planned_prefix`
(hook `SideHooks::before_open` truncates the destination between the plan
and the chunk list). `crates/irsync/tests/recv.rs` has the writer-level
tests: clone adoption without writes, no clone over an existing `.part`,
the refused clone, in-place adoption and finalize without a rename, the
shrink deferred to finalize, and the refusal of a replaced destination.
`crates/irsync/tests/plan.rs` pins the planner's append decisions.

`crates/irsync/tests/content_equal.rs` covers the settle-in-place path; the
receiver logs `identical` for it.

- `touched_source_is_a_metadata_only_update`: the source's mtime moves. No
  bytes cross, no `need` or `chunk` or `copied` event, the receiver hashes
  nothing (warm index), the sender hashes once, the destination keeps its
  inode and takes the new mtime, and the itemize line is `.f..t......`.
- `touched_destination_is_a_metadata_only_update`: the mirror image on a
  push; the receiver hashes once and restores the mtime.
- `checksum_against_a_cold_index_reads_once_and_changes_nothing`: `-c`
  with a fresh receiver state directory hashes once per side, rewrites
  nothing, and itemizes nothing because no attribute differs.
- `checksum_reads_every_file_on_both_sides_and_rewrites_nothing`: with
  warm indexes, `-c` still reads every file on both sides, settles each in
  place, reports no transfer, and leaves the rows fresh so the next plain
  run reads nothing.
- `checksum_catches_a_destination_whose_row_is_fresh_but_wrong`,
  `checksum_catches_a_source_whose_row_is_fresh_but_wrong`,
  `checksum_forces_the_daemon_sender_to_read`: an index row planted with
  the file's current stat but the wrong content (what bit rot looks like
  to the quick check) is skipped by a plain run and repaired by `-c`, on
  either side and in either direction.
- `destination_replaced_after_hashing_falls_through_to_delta`: a test hook
  (`SideHooks::before_identical_check`) replaces the destination between
  the hash and the check; the file is received as a delta and ends up
  equal to the source.
- `settling_in_place_discards_a_leftover_part_file`: a stray `.part` from
  an earlier attempt is gone after the file is settled in place.

`crates/irsync/tests/warm.rs` covers remedy 8: `index_root` hashes a
tree once and only re-reads changed files (and counts without reading on
a dry run); a file the destination holds under another name, placed by
something other than irsync, is copied locally with zero wire bytes
after one read of the one same-sized candidate (both directions); files
of other sizes and files below the size floor are not candidates; the
cap bounds the hashing and no candidate is hashed twice; after
`index_root` or a daemon started with `warm_index` the session hashes
nothing. `tests/delta.rs::stale_local_chunk_is_re_requested_from_the_sender`
keeps its stale file out of the candidate set on purpose (the sizes
differ), since a candidate is re-hashed, and so no longer stale, before
the copy.

## 6. Code pointers

| what | where |
| --- | --- |
| early hash at plan time | `session/planner.rs`, the `Delta` arm building `Deferred.prehash` |
| held-back itemize line | `session/planner.rs`, `Deferred.itemize`; `session/receiver.rs::emit_itemized` |
| hashing the old file | `session/recv_io.rs::index_old_file`, result `OldFile` |
| open, compare, adopt, partition | `session/recv_io.rs::open_writer`, result `Opened` |
| is it still the hashed file | `session/recv_io.rs::unchanged_file` |
| settle in place | `session/recv_io.rs::settle_identical`, `recv.rs::discard_part` |
| whole-file hash without a read-back | `tree_hash.rs::TreeHash`; fed from `recv.rs::FileWriter::write_chunk` and the adoption paths, folded in `finalize` |
| route and dispatch | `session/receiver.rs::on_opened`, `recv_io::Route` |
| choosing bases | `session/recv_io.rs::basis_specs` |
| signing and offering them | `session/recv_io.rs::offer_bases`; `delta.rs::Basis::sign` |
| holding and using a basis on the sender | `session/sender_role.rs::on_basis`, `on_need`; `send.rs::payload`; `delta.rs::encode` |
| rebuilding a chunk from operations | `session/recv_io.rs::rebuild`; `delta.rs::decode` |
| cloning the old file into the `.part` | `recv.rs::reflink`, `FileWriter::open_from`, `adopt_from` |
| writing in place | `recv.rs::FileWriter::open_inplace`; `session/recv_io.rs::Gate`, `settle` |
| append | `plan.rs::decide` (`Decision::Append`); `session/sender_role.rs::tail_digest`; `session/recv_io.rs::open_writer` (the `APPEND` arm, `idx_base`) |
| same-size candidates for a whole file | `session/recv_io.rs::warm_candidates`, `hash_key`; `warm.rs::Cold`; noted in `session/control.rs::spawn_listing` |
| warming a tree ahead of time | `warm.rs::index_root`; `main.rs::run_index`; `daemon.rs::Daemon::run` (`warm_index`) |
| local filler | `session/recv_io.rs::copy_local_chunks`, `read_local` |
| network filler | `session/recv_io.rs::accept_streams`, `read_stream`, `read_range` |
| completion accounting | `session/recv_io.rs::finish_unit` |
| re-request | `session/receiver.rs::on_drained`, `MAX_REREQUESTS` |
| verification on write | `recv.rs::FileWriter::write_chunk`, `missing_chunks` |
| chunk list in parts | `session/parts.rs::split`, `assemble`; `session/receiver.rs::on_chunk_list_part` |
| test hooks | `session/mod.rs::SideHooks::slow_local_copy`, `before_identical_check`, `chunk_list_part_records`, `drop_after_parts`, `after_bases`, `rerequests`, `before_open` |
