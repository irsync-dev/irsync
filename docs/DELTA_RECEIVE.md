# Receiving a changed file: the overlapped delta algorithm

How the receiver fills one file once the sender's chunk list is in hand,
after the restructuring of 2026-10-22. Everything here was checked against
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

`Receiver::on_chunk_list` (`session/receiver.rs`) bumps the file's
generation, retires any earlier writer for that file (`retire_writer`
removes the route and drops the writer out of its slot, so no stale task
can touch the `.part` afterwards), takes `prehash` out of the per-file
state, and spawns `open_writer`.

The handle is *taken*, not cloned. A second chunk list for the same file,
which happens after a reconnect, finds no handle, and `open_writer` simply
calls `index_old_file` itself; by then that is a cache hit.

### 2.3 Open and partition

`recv_io::open_writer` does four things and copies nothing:

1. **Decode** the chunk list into a native `FileDigest` (`native_digest`).
2. **Open** the locked `.part` (`FileWriter::open_tracked`). The writer
   adopts earlier progress recorded for that `.part` and keeps a `verified`
   bit per chunk; `missing_chunks()` lists the indices whose bit is clear.
3. **Ensure the old file is indexed** (`DELTA` only): await the prehash
   handle, then call `index_old_file` once more. The second call is the
   deduplication point. The task has finished, so the index row is fresh
   and the hasher returns from cache without reading the file. If the
   planner never started a task (reconnect, or a `WHOLE` file promoted to
   this path), this call does the work.
4. **Partition** the missing chunks. One blocking pass holds the index lock
   and asks `find_chunk` for each missing chunk's hash. A hit yields a path
   (an index key, turned absolute with `ctx.full_key`) and an offset inside
   that file; the chunk goes into the `local` set as a
   `(index, path, offset)` triple. A miss goes into the `remote` set. If the
   lookup itself fails (index error, poisoned lock), the whole missing set
   goes remote, which is always correct, and a warning is logged.

The result is `Opened { writer, remote, local, digest }`. The two sets are
disjoint and together cover exactly the missing chunks.

### 2.4 Dispatch: request first, then copy

`Receiver::on_opened` runs on the receiver's event loop. After the usual
generation check it looks at the two sets.

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

Unchanged (`recv_io::finalize`): `fsync`, re-hash of the whole `.part`
against the sender's whole-file hash, metadata applied through the
descriptor, rename over the destination, `FileAck DONE`.

## 3. The chain, before and after

For a large file with a small change, warm index, where *S* is the time of
one full pass over the file on either disk:

| phase | before | after |
| --- | --- | --- |
| sender digest | S | S, concurrent with the old-file hash |
| receiver old-file hash | S (0 if cached) | overlapped with the sender's digest |
| local copy into `.part` | S, before the request | S, concurrent with the transfer |
| network transfer | after all of the above | starts right after the index lookups |
| finalize re-hash | S | S |

Critical path before: up to 4 S plus the transfer. After: S (the
concurrent hashes) plus the longer of local copy and transfer, plus S for
finalize. The finalize pass is the subject of remedy 5 in
`IRSYNC_VS_RSYNC.md` and is outside this change.

The receiver's I/O volume is the same as before; only its placement on the
timeline changed.

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
  sequence as before, only earlier.

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

## 6. Code pointers

| what | where |
| --- | --- |
| early hash at plan time | `session/planner.rs`, the `Delta` arm building `Planned.prehash` |
| hashing the old file | `session/recv_io.rs::index_old_file` |
| open, adopt, partition | `session/recv_io.rs::open_writer`, result `Opened` |
| route and dispatch | `session/receiver.rs::on_opened`, `recv_io::Route` |
| local filler | `session/recv_io.rs::copy_local_chunks`, `read_local` |
| network filler | `session/recv_io.rs::accept_streams`, `read_stream`, `read_range` |
| completion accounting | `session/recv_io.rs::finish_unit` |
| re-request | `session/receiver.rs::on_drained`, `MAX_REREQUESTS` |
| verification on write | `recv.rs::FileWriter::write_chunk`, `missing_chunks` |
| test hook | `session/mod.rs::SideHooks::slow_local_copy` |
