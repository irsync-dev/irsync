# Receiving a changed file: the overlapped delta algorithm

How the receiver fills one file once the sender's chunk list is in hand,
after the restructuring of 2026-10-22, and how it settles a file whose
content turns out not to have changed at all. Everything here was checked against
the code in the working tree of that date; function names point at where
each step lives. The wire protocol (`protocol.md`) is unchanged by this
work: the receiver only reorders what it does between two messages it was
already sending.

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
5. **Partition** the missing chunks. One blocking pass holds the index lock
   and asks `find_chunk` for each missing chunk's hash. A hit yields a path
   (an index key, turned absolute with `ctx.full_key`) and an offset inside
   that file; the chunk goes into the `local` set as a
   `(index, path, offset)` triple. A miss goes into the `remote` set. If the
   lookup itself fails (index error, poisoned lock), the whole missing set
   goes remote, which is always correct, and a warning is logged.

The result is `Opened::Writer` carrying the writer, the two sets and the
digest. The sets are disjoint and together cover exactly the missing
chunks.

### 2.4 Dispatch: settle in place, or request first and then copy

`Receiver::on_opened` runs on the receiver's event loop. After the usual
generation check it looks at what `open_writer` returned.

**Identical.** The content is already there, so the file is settled in
place rather than placed. The receiver sends an empty `NeedChunks`, which
tells the sender to wait for the ack, emits the held-back itemize line if
any attribute differs (section 2.9), and spawns `settle_identical`. That
task discards a leftover `.part` of an earlier interrupted attempt along
with its progress record, unless another writer holds it, applies the
sender's mode, ownership and times to the existing inode through the open
descriptor, and records the file in the index with the sender's digest so
the next run's quick check passes. The receiver then acks `DONE`. The file
counts as a metadata-only update, not a transfer: no bytes, no placed
file, no file-done event. A metadata failure fails the file like a write
error, and the next run tries again. This is what a `touch` on either
side, a restore from backup, or `-c` against a cold index comes to: one
read of the old file on the side whose index was stale, four small
control messages, no write.

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
| `pending` | units of delivery still running, see 2.6 |
| `path` | the file's session path, for events and logs |

`pending` starts at `remote.len() + 1` if the local set is non-empty, or
`remote.len()` if it is empty: one unit per remote chunk, plus one for the
whole local-copy task.

Then, in this order:

1. the route is inserted into the routing table, so the earliest possible
   data stream finds it;
2. `NeedChunks` with the remote set goes out on the control stream;
3. if the local set is non-empty, `copy_local_chunks` is spawned.

Step 2 before step 3 is the point of the change. The sender starts reading
and streaming the moment the request lands, while the receiver's disk fills
in the rest. If the remote set is empty but the local set is not, the
receiver still sends an empty `NeedChunks`: the sender must know it may
move on to the next file, and the receiver's local work is not the sender's
concern.

### 2.5 Two fillers, one writer

**The network side** is unchanged in shape (`accept_streams`,
`read_stream`, `read_range`). Each unidirectional stream carries a
`DataStreamHeader` naming the session, the file and a contiguous chunk
range. The reader looks up the route, reads chunk by chunk, hashes each one,
takes the writer lock, and calls `FileWriter::write_chunk`, which checks
length and hash again and does a positioned write at the chunk's offset.

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
moves on. The chunk's bit stays clear. Nothing else happens yet; 2.7
describes how it is recovered.

### 2.6 Completion accounting

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

### 2.7 Recovering a failed local copy

When the last unit ends with chunks still missing, `Done::Drained` reaches
`Receiver::on_drained`. It re-reads the missing set from the writer, checks
that `pending` really is zero, counts a re-request against
`MAX_REREQUESTS` (3), adds the missing count back onto `pending`, and
sends another `NeedChunks` naming those chunks. The sender delivers them
over the network and the normal path verifies them.

This is the pre-existing path for a data stream that ended short; the local
copy task reuses it by being one unit of the same request. A stale local
source therefore costs exactly one round trip and the bytes of the chunks
that did not verify, and the file still finalizes with a verified whole-file
hash. The stale index row is not corrected here; the next scan of that
source file replaces it.

A file still incomplete after three re-requests fails with
`Status::WriteError`, as before.

### 2.8 Finalize

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

### 2.9 The itemize line of a `DELTA` file

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

Critical path before: up to 4 S plus the transfer. After: S (the
concurrent hashes) plus the longer of local copy and transfer. The
receiver reads the old file once at most and writes the new one once.

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
- **Reconnects and failures are safe.** A new chunk list bumps the
  generation and retires the writer. A copy task of the old generation sees
  the empty slot and exits; its late `finish_unit` finds nothing to report.
- **The protocol sees nothing new.** The sender observes the same message
  sequence as before, only earlier. A file settled in place looks to the
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
- `destination_replaced_after_hashing_falls_through_to_delta`: a test hook
  (`SideHooks::before_identical_check`) replaces the destination between
  the hash and the check; the file is received as a delta and ends up
  equal to the source.
- `settling_in_place_discards_a_leftover_part_file`: a stray `.part` from
  an earlier attempt is gone after the file is settled in place.

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
| local filler | `session/recv_io.rs::copy_local_chunks`, `read_local` |
| network filler | `session/recv_io.rs::accept_streams`, `read_stream`, `read_range` |
| completion accounting | `session/recv_io.rs::finish_unit` |
| re-request | `session/receiver.rs::on_drained`, `MAX_REREQUESTS` |
| verification on write | `recv.rs::FileWriter::write_chunk`, `missing_chunks` |
| chunk list in parts | `session/parts.rs::split`, `assemble`; `session/receiver.rs::on_chunk_list_part` |
| test hooks | `session/mod.rs::SideHooks::slow_local_copy`, `before_identical_check`, `chunk_list_part_records`, `drop_after_parts` |
