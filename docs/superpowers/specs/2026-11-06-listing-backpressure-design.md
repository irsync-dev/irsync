# Listing backpressure and peer limits — design

Closes open point 2.1(1) in `docs/IRSYNC_DECISIONS.md` (R31: bounded
channels / backpressure). Written 2026-11-06.

## 1. Intent

irsync has not hit this in practice. The goal is to close the gap before
someone points a large or hostile tree at a daemon: a session's memory may
grow with the size of the tree (a modest per-path cost), but never with a
rate mismatch between the two sides or with a peer that floods frames. A
slow link, a slow disk or a slow planner must make the other side wait,
not make this side buffer.

## 2. What is unbounded today

All channel types are `tokio::sync::mpsc::unbounded_channel` unless noted.

| Channel | Where | Grows when |
|---|---|---|
| Peer listing (`Listed`) | `engine.rs` `run_role`, consumed by the planner | The sender's listing arrives faster than the planner consumes it. The planner blocks on its in-flight semaphore (`MAX_INFLIGHT` = 4096), so on a fast link nearly the whole listing (one full `FileEntry` per path) queues here. |
| Local listing | `receiver.rs` `run` | The receiver's own walk outruns the planner. |
| Control writer (`Out`) | `control.rs` `spawn_writer` | The listing producer (`spawn_listing`) calls `out.send` per entry without waiting. On a slow link the whole outgoing listing queues here (the daemon in a pull). |
| Events (`Ev`) | `engine.rs` `run_role` | The control reader forwards every frame immediately. An honest peer stays within the in-flight window, but nothing enforces it: a pulling client can send any number of `Plan` frames, and each spawns a digest task. |
| `--delete-before` | `planner.rs` `plan_listings` | Buffers every source `Side` until both listings end (by design). |

**The deadlock R31 avoided.** The listing and the frames that free planner
capacity (`ChunkListPart`, `FileAck`, `SessionDone`) share the one ordered
control stream. Bounding the peer listing channel alone deadlocks: the
planner waits for an in-flight slot; the reader blocks on the full listing
channel; the `ChunkListPart` that would complete a file and free a slot
sits in the stream behind more listing entries. Bounding the writer queue
creates the same cycle across peers (each main loop blocked writing, each
reader blocked delivering).

**Proportional to the tree regardless, and accepted:** the sender's
`ListedFiles` map, the planner's `ok_dirs` / `regular_ok` / `touched` /
`kept_inodes` sets, the delete pass's candidates, and the `--delete-before`
buffer. These stay.

## 3. Goals, non-goals, anti-goals

**Goals**
- No queue in a session grows because one side is faster than the other.
  Every queue between the network and a consumer is bounded, and a full
  queue stops reading from the network so QUIC flow control slows the
  peer.
- Each side enforces the protocol's limits on what the other side can make
  it remember or spawn, failing the session with a protocol error when a
  peer breaks one.
- No deadlock, by construction (section 5), and proven by a stress test
  that throttles the planner.

**Non-goals**
- A fixed memory ceiling independent of tree size. Per-path state that
  grows with the tree (section 2, last paragraph) stays.
- Removing the `--delete-before` buffer (it grows with the tree, which is
  accepted). Shrinking what each buffered entry holds is welcome where it
  is cheap, not required.
- Backward compatibility. Nothing is released; the protocol and stored
  state may change freely, and `PROTOCOL_VERSION` stays 1.

**Anti-goals** (failure even if it technically works)
- A session that can hang. A bounded queue that can deadlock under some
  interleaving is worse than today's unbounded one.
- Throughput loss on the normal path: the 33k-file push benchmark must
  not get measurably slower.
- Rejecting traffic an honest peer legitimately sends (re-requests,
  incremental `NeedChunks`, resumed plans) as a protocol violation.

## 4. Decided shape

### 4.1 The listing gets its own QUIC stream (decided)

Each side that lists sends its listing on a dedicated stream, separate
from the control stream: listing entries, `ListingError` (gaps) and
`ListingEnd`, in that order, ending the stream after `ListingEnd`. These
three frame types no longer travel on the control stream.

- **Receiver, peer listing.** A listing reader task reads the peer's
  listing stream into a small bounded channel feeding the planner. When
  the planner stalls on its in-flight slots, the channel fills, the
  listing reader stops reading, and QUIC's per-stream flow control backs
  up the sender's listing writer and, through it, the sender's walk. The
  control stream keeps being read, so `ChunkListPart`, acks and
  `SessionDone` still arrive and slots still free.
- **Listing producer** (both roles, `spawn_listing`). Writes entries to
  its listing stream through a bounded channel (or directly), never
  through `Out`. A slow link stalls the walk instead of queueing the
  listing. Heartbeats the producer sends stay on the control stream via
  `Out`.
- **Receiver, local listing.** The channel to the planner becomes
  bounded; the receiver's walk waits on the planner (a local wait).
- **The receiver's own listing to the sender** (my call). Today the
  sender drops these entries and uses the listing only as a start signal
  (`Ev::PeerListing` / `ListingEnd` decide `Listing::Undecided`) and for
  the peer scan progress shown on the bus. It moves to a listing stream
  like the sender's, and the sender keeps draining it in full, so it can
  never stall the receiver's walk. Not sending it at all is a possible
  later optimisation, not part of this work.
- **Control stream.** Carries everything else (`Hello` exchange, `Plan`,
  `NeedChunks`, `FileAck`, `ChunkListPart`, `ChunkBasis`, `Itemized`,
  `Heartbeat`, `SessionDone`, `Error`). Without the listing, `Out` is
  still not window-bounded: `Itemized` and `ChunkBasis` can pile up in it
  (section 4.4).
- **Late listing frames.** The receiver's last listing frames can reach
  the sender after `SessionDone`, since they travel on another stream;
  the sender keeps reading (and dropping) them after `Done`.

### 4.2 Events become bounded (my call, follows from 4.1)

With the listing off the control stream, the `Ev` channel can be bounded.
Neither role's main loop ever waits on the peer (it sends through `Out`,
which does not block), so a full `Ev` queue only stops the control
reader, and flow control pushes a flooding peer's frames back into the
peer's own writer queue. Task completions (`Ev::Send`, `Ev::Recv`) share
the queue and simply wait their turn.

### 4.3 Each side enforces the protocol's limits (decided)

Principle: **anything a peer can make us remember or spawn is capped by a
protocol rule; anything else is throttled by the bounded queues.** A frame
that breaks a rule fails the session with a protocol error (an `Error`
frame to the peer, then the usual failure path).

The threat model includes a hostile **authorised** peer in either role,
in particular a client pushing to a daemon to exhaust its memory
(decided).

Rules I expect (my guess; the builder derives the exact list from the
code and must not reject legitimate traffic):
- **Sender checks the receiver.** At most `MAX_INFLIGHT` files in flight
  from `Plan` (including after a resumed plan's `Plan` burst, which the
  receiver already paces with `queued` and `inflight_cap`). Today an
  honest receiver can briefly exceed this: it frees a planner slot
  (`receiver.rs`, around the DONE `FileAck`) before queueing the ack, so
  the planner can slip a `Plan` in first. The receiver queues the ack
  before releasing the slot (my call). `Plan` only for listed paths
  (already enforced). `NeedChunks` must not let a receiver multiply
  concurrent send tasks per file without limit; several `NeedChunks` per
  file are legitimate (incremental requests, section 4.5, and re-requests
  for chunks a stream did not deliver), so the cap is on concurrent work,
  not on the frame count. `ChunkBasis` only for files in flight, within
  the basis budget of section 4.5. `FileAck` only for known file ids.
- **Receiver checks the sender.** `ChunkListPart` only for planned files,
  within the per-file cap (`parts.rs` `MAX_LIST_CHUNKS`) and the
  session-wide budget of section 4.6. Data streams only for files in
  flight. Late `ChunkListPart` frames and data streams for a file that
  has just failed or been retried are tolerated, not errors (my call).
- **Listing stream.** Paths strictly increasing (today `checked_order`
  only sets a flag that `check_order` reports; it becomes a protocol
  error), except that a gap may name the directory whose entry was just
  listed (my call; check `ScanEvent::Gap` placement). `ListingEnd.count`
  matches the entries received (listed as unchecked in decisions §2.3,
  Deletions); the `inject_listing` test hook counts the entries it
  injects so the existing tests in `containment.rs` and `ids_e2e.rs`
  keep passing (my call). Exactly one `ListingEnd`, and nothing after it.
- Frames harmless in themselves (`Heartbeat`, `Itemized`, `Error`) need
  no count limit.

### 4.4 Outgoing control frames are bounded by bytes (decided)

Two frames still pile up in `Out` from a rate mismatch: `Itemized` (with
`-i` the planner sends one per directory, symlink, special file or
metadata-only change, none of which takes an in-flight slot) and
`ChunkBasis` (`offer_bases` in `recv_io.rs` signs every missing chunk of
a delta file at disk speed; a mostly rewritten 100 GB file can queue
1-2 GB on a slow link).

`Out` gains a second, **waiting** send that blocks while the bytes queued
for the writer exceed a budget. Only tasks that may wait use it: the
planner and `offer_bases` (and any other spawned task the builder finds
producing per-path or per-chunk frames). The main loop keeps the
non-waiting send, so it never waits on the network. This is safe because
the peer's control reader never waits on us (its `Ev` drains through a
main loop that never waits on the peer), so the writer always makes
progress. The non-waiting send's traffic is window-bounded.

### 4.5 Bases go out in step with their requests (decided)

Today the receiver sends every `ChunkBasis` of a file before the one
`NeedChunks` that uses them, and the sender stores them all in
`SFile.bases` until it arrives: an honest large delta costs the sender
gigabytes per file, times the files in flight.

`offer_bases` sends `NeedChunks` in batches as it sends bases (a batch
of bases, then the request for exactly those chunks), so the sender uses
and drops each batch promptly. Incremental `NeedChunks` are already
legitimate, so the wire format does not change. The sender caps the
bytes of bases it holds per session and fails the session when a
receiver exceeds it. In-place mode (`--inplace`) currently wants one
ascending request covering every remote chunk; the builder keeps that
correct (ascending batches, or the single-request path with the basis
budget respected some other way).

### 4.6 A session-wide budget for chunk lists (decided)

The receiver caps each file's chunk list at `MAX_LIST_CHUNKS` (2^24)
against the size the sender declares, and only fails that file. A
hostile sender can declare huge sizes for up to 4096 files in flight,
which is terabytes in the worst case. Checking against the listing's
size would reject files that legitimately grew between the walk and the
digest (the sender re-stats when it opens the file).

The receiver therefore holds chunk lists under a session-wide byte
budget across all files in flight, and chunk lists are paced under it:
a file whose list does not fit waits rather than being buffered. A
sender that sends list parts the receiver has not made room for is a
protocol error. How the pacing works (the receiver granting room per
file, or the sender sending a file's list only once the receiver asks)
is the builder's call, subject to section 5; it may need a protocol
change, which is acceptable.

## 5. The deadlock invariant

The design is deadlock-free if this holds, and the implementation must
keep it (document it in the code where the channels are created):

> A task that drains a bounded queue fed from the network never waits on
> anything that needs the peer to make progress on **the same stream**.

Concretely:
- The planner (drains the listing channel) waits on in-flight slots and on
  the `DeleteBefore` reply; both are released by the main loop, which is
  fed from the control stream, never from the listing stream.
- The main loop (drains `Ev`) must never await a bounded send into `Ev`
  itself, and must not await the planner. Where it posts to `Ev` today
  through its own sender clone, use a non-blocking path or restructure.
- No main loop awaits a bounded send toward the network; only spawned
  tasks use `Out`'s waiting send (4.4).
- Waiting for chunk-list room (4.6) never blocks the control reader or a
  main loop: room is freed by files completing, which the control stream
  carries.
- The sender drains the receiver's listing stream in full, so the
  receiver's walk never waits on the sender's main loop.

## 6. Proving it

- **A stress test that throttles the planner** (decided). With the
  planner slowed (a new test hook, in the style of `slow_finalize`) and a
  tree of many files, a push and a pull complete without hanging and with
  the expected result, and the evidence shows backpressure worked: the
  peer listing channel never exceeds its bound and the sender's walk
  stalls rather than its listing queueing. How the test observes this
  (a peak-depth hook like `inflight_peak`, a counter of producer waits) is
  the builder's call.
- **The existing suite passes** (decided), including the reconnect,
  resume, `--delete-before/during/after`, gap and listing tests.
- One test per enforced rule in 4.3, using a crafted peer in the style of
  the `inject_listing` hook, asserting the session fails with a protocol
  error (my call).
- A large delta over a throttled link (`--bwlimit`) completes with the
  sender's stored bases and the receiver's queued `ChunkBasis` bytes
  staying within their budgets (my call).
- The 33k-file push benchmark shows no measurable slowdown (anti-goal).

## 7. Left to the builder

- How the listing stream is opened and told apart from data streams
  (direction, uni or bi, a stream-kind header), and how the sender's
  `Listing::Undecided` start signal maps onto it, including a resumed
  receiver that skips listing.
- Channel capacities (the walk already uses 1024) and the byte budgets
  of 4.4, 4.5 and 4.6.
- The pacing mechanism for chunk lists (4.6) and the batch size for
  bases (4.5).
- How heartbeats' `walking` field stops being reported after the peer's
  `ListingEnd`, now that the two arrive on different streams.
- The exact rule list in 4.3 and where each check lives.
- Updating `docs/IRSYNC_DECISIONS.md`: mark 2.1(1) done, amend R31, and
  remove the `ListingEnd.count` item from §2.3 once checked.
