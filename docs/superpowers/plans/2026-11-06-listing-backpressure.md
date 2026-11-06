# Listing Backpressure and Peer Limits Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** No queue in an irsync session grows from a rate mismatch or a flooding peer; each side enforces the protocol's limits on the other.

**Architecture:** The listing moves to its own bidirectional QUIC stream so flow control can slow a fast lister without blocking the control frames that free planner slots. Peer-fed queues become bounded, per-path and per-chunk control frames are paced by bytes, `ChunkBasis` frames travel in batches with their `NeedChunks`, and chunk lists are sent only into room the receiver grants. Every limit lives in one module, `session/limits.rs`.

**Tech Stack:** Rust, tokio (`mpsc`, `Semaphore`, `Notify`), iroh QUIC streams, prost (`proto/irsync/v1/control.proto`).

**Spec:** `docs/superpowers/specs/2026-11-06-listing-backpressure-design.md` (read it first; section numbers below refer to it).

## Global Constraints

- No backward compatibility: change the protocol and stored state freely; `PROTOCOL_VERSION` stays 1.
- Memory may grow with the tree (per-path sets, the `--delete-before` buffer); it must not grow with a rate mismatch.
- Deadlock invariant (spec §5), to be stated in a comment where the channels are created: *a task that drains a bounded queue fed from the network never waits on anything that needs the peer to make progress on the same stream.* No main loop awaits a bounded send toward the network or into a queue it drains.
- Never reject traffic an honest peer sends: re-requests, incremental `NeedChunks`, resumed plans, late frames for files that just failed or were retried.
- A rule violation fails the session with a protocol error: an `anyhow` error whose message starts `protocol error:` (as `sender_role.rs` already does), reported to the peer by `run_role`'s failure path.
- Test hooks follow the existing `SideHooks` pattern (`#[cfg(feature = "test-hooks")]`, documented field, `None` by default).
- `cargo fmt`, `cargo clippy --all-targets --all-features -- -D warnings` and `cargo test --all-features` pass at the end of every task.

## Review Focus

1. **Reconnect while a listing is mid-stream**: the next connection opens fresh streams and the session resumes and completes. Test added to Task 1.
2. **Resumed session with a complete stored plan**: the receiver lists nothing; the sender's listing reader sees its half end with no frames and must treat that as "the receiver did not list", not as a truncated listing. Test added to Task 1.
3. **Dry run into a missing destination root**: the receiver sends only `ListingEnd` (now on the listing stream). Covered by the existing suite; Task 1 step 6 names the test to watch.
4. **`--delete-before` on a tree larger than the bounded queues**: the planner drains both listings into its buffer before deleting, so nothing stalls. Test added to Task 2.
5. **The peer's scan progress on the client** (`Event::Scanned { party: Peer }`, `Event::Total`): still reported now that the listing reader is a separate task. Covered by `progress_e2e.rs`; Task 1 step 6 names it.

---

## File Structure

- Create `crates/irsync/src/session/limits.rs`: every queue size and byte budget (constants), plus `ByteBudget` (a byte-counting async budget used by `Out` and by the basis budget).
- Create `crates/irsync/src/session/listing_stream.rs`: the listing stream's writer and reader (moved out of `control.rs`; `control.rs` keeps the control stream).
- Modify `proto/irsync/v1/control.proto`: `ListingStreamHeader`, `ListRoom`.
- Modify `crates/irsync/src/session/{engine.rs, control.rs, receiver.rs, sender_role.rs, planner.rs, recv_io.rs, parts.rs, mod.rs}`.
- Create `crates/irsync/tests/backpressure.rs`: the stress test and the rule tests.
- Modify `docs/IRSYNC_DECISIONS.md` (Task 8).

---

### Task 1: The listing gets its own stream

**Files:**
- Modify: `proto/irsync/v1/control.proto`
- Create: `crates/irsync/src/session/limits.rs`, `crates/irsync/src/session/listing_stream.rs`
- Modify: `crates/irsync/src/session/engine.rs` (`run_client_inner`, the daemon's session entry that calls `run_role`, `run_role`), `crates/irsync/src/daemon.rs` (only if the accept lives there), `control.rs`, `receiver.rs`, `sender_role.rs`, `planner.rs`, `mod.rs` (hooks)
- Test: `crates/irsync/tests/backpressure.rs`

**Interfaces:**
- Produces (proto): `message ListingStreamHeader { bytes session_id = 1; }` (sent once, first frame of the client's half).
- Produces (`limits.rs`): `pub(super) const LISTING_QUEUE: usize = 1024;` (peer listing channel, local listing channel, producer→writer channel).
- Produces (`listing_stream.rs`):
  - `pub(super) struct ListingOut { tx: mpsc::Sender<Control> }` with `fn send_blocking(&self, msg: Msg)` (called from the walk's blocking thread; `blocking_send`) and `fn finish(self)`.
  - `pub(super) fn spawn_listing_writer(send: SendStream, events: mpsc::UnboundedSender<Ev>) -> (ListingOut, JoinHandle<()>)`.
  - `pub(super) fn spawn_listing_reader(recv: RecvStream, sink: ListingSink, ids: Option<IdMap>, events: mpsc::UnboundedSender<Ev> /* becomes the bounded peer-events sender in Task 3 */, bus: EventBus, activity: Arc<AtomicU64>) -> JoinHandle<()>` where `pub(super) enum ListingSink { Planner(mpsc::Sender<Listed>), Drain }`.
  - Planner signatures change to `peer: mpsc::Receiver<Listed>, local: mpsc::Receiver<Listed>`.
- Produces (`mod.rs` hooks): `pub tamper_frames: Option<FrameHook>` with `pub type FrameHook = Arc<dyn Fn(StreamKind, Msg) -> Vec<Msg> + Send + Sync>` and `pub enum StreamKind { Control, Listing }`, applied by both writers to each outgoing frame (a test can drop, duplicate, alter or inject frames). Used by every later task's rule tests.

- [ ] **Step 1: Write the failing tests** in `crates/irsync/tests/backpressure.rs` (use `Harness`, `within`/a 60 s timeout like `chunk_list_parts.rs`):
  - `a_listing_frame_on_the_control_stream_fails_the_session`: client-side `tamper_frames`, on `StreamKind::Control` and the first `Heartbeat`, returns `[Heartbeat, Listing(ListingEntry { entry: Some(<a regular FileEntry at path "zz">) })]`. Push; assert `Err` whose chain contains `"protocol error"`.
  - `a_listing_cut_short_fails_instead_of_planning_deletions`: push with `--delete`, client hook drops the `ListingEnd` frame (returns `vec![]`) and the listing writer then finishes the stream; assert the run errors or reports `"listing is incomplete"` and that a destination-only file is **not** deleted.
  - `a_resumed_plan_needs_no_receiver_listing` (Review Focus 2): copy the shape of the existing complete-stored-plan resume test in `tests/resume.rs` (find it with `grep -n "plan_complete\|stored plan" tests/resume.rs`); assert it still completes. It fails today only if the new code is wrong, so it is a guard; write it now.
  - `a_connection_lost_mid_listing_resumes` (Review Focus 1): 2000 small files, client-side `tamper_frames` closes the connection (`conn` via the existing `FaultInjector`, or by returning frames after which a `PartsFault`-style hook closes) after 500 `Listing` frames; `run_reconnect` with fast backoff; assert success and `assert_trees_equal`.
- [ ] **Step 2: Run** `cargo test -p irsync --all-features --test backpressure` — expected: compile failure (`tamper_frames`, `StreamKind` not defined).
- [ ] **Step 3: Proto and stream setup.** Add `ListingStreamHeader`. Client: right after a successful `HelloAck` in `run_client_inner`, `conn.open_bi()`, write `ListingStreamHeader { session_id }`. Daemon: after sending `HelloAck`, `accept_bi()` under the existing `HELLO_TIMEOUT`, read the header, refuse the session if the id differs. Pass `(SendStream, RecvStream)` for the listing into `run_role` (new parameter `listing: (SendStream, RecvStream)`).
- [ ] **Step 4: Move listing frames.** `spawn_listing` takes a `ListingOut` instead of using `out` for `Listing`, `ListingError` and `ListingEnd`; heartbeats stay on `out`. `ListingEnd.count` counts every entry sent, including `inject_listing` ones. The receiver's local channel becomes `mpsc::channel(LISTING_QUEUE)` with `blocking_send`. Move `read_control`'s listing arms into the listing reader: it does the id mapping, `seen.count`, the `Scanned`/`Total` bus events, posts `Ev::PeerListing` at its first frame and `Ev::Ctrl(Msg::ListingEnd(..))` at the end, and forwards `Listed` to a `ListingSink::Planner` sender with `.send().await`. The control reader treats `Listing`, `ListingError`, `ListingEnd` as `protocol error: listing frame on the control stream`; the listing reader treats anything else, a second `ListingEnd`, or a frame after it as protocol errors. EOF with no frame at all is "the peer did not list" (no event); EOF after entries but before `ListingEnd` is `PeerFailed`. The heartbeat `walking` suppression (`listing_ended`) becomes a shared `Arc<AtomicBool>` set by the listing reader and read by the control reader. The sender keeps the listing reader running after `Done` and aborts it at session end, never failing on it after `Done`.
- [ ] **Step 5: Document the invariant** (Global Constraints) in `run_role` where the channels are created, and note in `sender_role.rs` near `Listing::Undecided` why `PeerListing` still precedes any `Plan` in a fresh listing: the receiver's first `Plan` needs the sender's entries, and the sender starts walking only after `PeerListing`.
- [ ] **Step 6: Run** the new tests, then the whole suite: `cargo test --all-features`. Watch especially `listing_e2e`, `progress_e2e` (Review Focus 5), `resume`, `delete`, `containment`, `ids_e2e`, and the dry-run-into-missing-root test (`grep -rn "root_missing\|missing root" crates/irsync/tests`). Expected: all PASS.
- [ ] **Step 7: Commit** `feat(session): send the listing on its own stream`.

### Task 2: Stress test that throttles the planner

**Files:**
- Modify: `crates/irsync/src/session/mod.rs` (hooks), `planner.rs`, `listing_stream.rs`
- Test: `crates/irsync/tests/backpressure.rs`

**Interfaces:**
- Consumes: `LISTING_QUEUE`, `ListingOut`, the listing reader (Task 1).
- Produces (hooks): `slow_plan: Option<Duration>` (receiver: each `Planner::one` sleeps this long first), `listing_peak: Option<Arc<AtomicUsize>>` (receiver: the listing reader records `max_capacity - capacity` after each send into the planner channel), `listing_waits: Option<Arc<AtomicU64>>` (lister: incremented each time `ListingOut::send_blocking` found the channel full, i.e. `try_send` returned `Full` before the blocking send).

- [ ] **Step 1: Write the failing tests:**
  - `a_slow_planner_slows_the_sender_walk_instead_of_queueing` — run for `Flow::Push` and `Flow::Pull` (two `#[tokio::test(flavor = "multi_thread", worker_threads = 4)]` functions sharing a helper): 5000 files of 1–4 KiB in 50 directories; receiver side `slow_plan = 1 ms`, `max_inflight = 4`, `listing_peak` set; sender side `listing_waits` set. Assert within 120 s: `out.errors.is_empty()`, `files_transferred == 5000`, `assert_trees_equal`, `listing_peak <= LISTING_QUEUE` (export the constant through the test-hooks surface, or assert `<= 1024`), `listing_waits > 0`. Receiver hooks for a push go to the daemon (the way `slow_finalize` reaches it through `spawn_daemon_full`); for a pull, to `local.hooks`.
  - `delete_before_with_a_listing_larger_than_the_queues_completes` (Review Focus 4): 3000 source files, 200 destination-only files, `--delete-before`, `slow_plan = 0`; assert the 200 are deleted, trees equal.
- [ ] **Step 2: Run** `cargo test --all-features --test backpressure slow_planner` — expected: compile failure (hooks missing).
- [ ] **Step 3: Add the three hooks** where the Interfaces block says.
- [ ] **Step 4: Run** the two tests — expected PASS. If the push or pull hangs, the invariant is broken; find which await sits on the same stream before changing any capacity.
- [ ] **Step 5: Commit** `test(session): prove listing backpressure under a slow planner`.

### Task 3: Peer events become bounded

**Files:**
- Modify: `limits.rs`, `engine.rs` (`run_role`, `Events`), `control.rs` (`read_control`), `listing_stream.rs`, `receiver.rs` and `sender_role.rs` (main loops, the end-of-session `PeerEof` wait in `receiver::run`)
- Test: `crates/irsync/tests/backpressure.rs`

**Interfaces:**
- Produces: `pub(super) const PEER_EVENTS: usize = 1024;` and `pub(super) type PeerEvents = mpsc::Receiver<Ev>;` fed only by the control reader and the listing reader (`.send().await`). Task completions (`Ev::Recv`, `Ev::Send`, `WriterFailed`) stay on the existing unbounded channel: each is bounded by the work in flight and some are sent from contexts that must not wait. Both main loops `select!` over both receivers (`biased;` toward local completions, so freed capacity is seen before new peer work).

- [ ] **Step 1: Write the failing test** `a_peer_flooding_heartbeats_is_throttled_not_fatal`: client `tamper_frames` returns 2000 copies of every outgoing `Heartbeat`; push 200 files; assert success and trees equal. (It passes today too, but with unbounded growth; it guards that bounding does not deadlock or fail.) Add a hook `peer_events_peak: Option<Arc<AtomicUsize>>` (daemon side) and assert `<= PEER_EVENTS`; this is the part that fails to compile first.
- [ ] **Step 2: Run** it — expected: compile failure.
- [ ] **Step 3: Split the channel** as in the Interfaces block; the receiver's final "let the sender read `SessionDone`" wait reads `PeerEvents`.
- [ ] **Step 4: Run** the test and the whole suite — expected PASS.
- [ ] **Step 5: Commit** `feat(session): bound the queue of peer frames`.

### Task 4: Outgoing control frames paced by bytes

**Files:**
- Modify: `limits.rs`, `control.rs` (`Out`, `spawn_writer`), `planner.rs` (`Itemized`, `Plan` sends), `recv_io.rs` (`offer_bases`)
- Test: unit tests in `limits.rs`; `crates/irsync/tests/backpressure.rs`

**Interfaces:**
- Produces (`limits.rs`): `pub(super) const OUT_BUDGET: usize = 4 * 1024 * 1024;` and
  `pub(super) struct ByteBudget` with `fn new(limit: usize) -> Self`, `async fn acquire(&self, n: usize)` (waits while `held > 0 && held + n > limit`, so a single frame larger than the limit still passes alone), `fn try_acquire(&self, n: usize) -> bool`, `fn release(&self, n: usize)`, `fn held(&self) -> usize`. Built on a `Mutex<usize>` + `Notify`.
- Produces (`control.rs`): `Out::send_paced(&self, msg: Msg) -> impl Future<Output = ()>` (acquires the encoded length from the writer's `ByteBudget`, then queues); `Out::send` unchanged (non-waiting, never acquires) — both share one FIFO so order is preserved. The writer releases a paced frame's bytes after `write_msg`; the queue item carries whether it was paced and its length.
- Produces (hook): `out_peak: Option<Arc<AtomicUsize>>` (peak `held()`).

- [ ] **Step 1: Write the failing unit tests** in `limits.rs`: `acquire_waits_until_release` (limit 10, hold 8, `acquire(5)` not ready after a yield, ready after `release(8)`), `an_oversized_request_passes_alone` (limit 10, `acquire(50)` ready when nothing held), `try_acquire_never_waits`.
- [ ] **Step 2: Run** `cargo test -p irsync --lib limits` — expected FAIL (no `ByteBudget`).
- [ ] **Step 3: Implement `ByteBudget`**, then `send_paced`; switch the planner's `Itemized` and `Plan` sends and every send in `offer_bases` to `send_paced(..).await`. Everything a main loop sends stays on `send`.
- [ ] **Step 4: Write the e2e test** `itemize_over_a_slow_link_stays_within_the_out_budget`: destination already in sync, then `chmod` every one of 20 000 entries in 2 000 directories at the source; push with `-i` (itemize) and `--bwlimit 200K`; daemon `out_peak` hook; assert success and `out_peak <= OUT_BUDGET + 64 * 1024`.
- [ ] **Step 5: Run** unit and e2e tests — expected PASS; then the whole suite.
- [ ] **Step 6: Commit** `feat(session): pace per-path control frames by bytes`.

### Task 5: Bases in batches under a session budget

**Files:**
- Modify: `limits.rs`, `recv_io.rs` (`offer_bases`), `receiver.rs` (owns the session's basis budget, passes it to `offer_bases`), `sender_role.rs` (`on_basis`, `on_need`, `retry`, file removal paths), `mod.rs` (hooks)
- Test: `crates/irsync/tests/backpressure.rs`

**Interfaces:**
- Consumes: `ByteBudget` (Task 4).
- Produces (`limits.rs`): `pub(super) const BASIS_BUDGET: usize = 32 * 1024 * 1024;` (wire bytes: `4 * weak.len() + strong.len()` per `ChunkBasis`), `pub(super) const BASIS_BATCH: usize = 64;`, `pub(super) fn basis_wire_len(b: &ChunkBasis) -> usize`.
- Produces (hooks, both sides read the same value): `basis_budget: Option<usize>`, `basis_peak: Option<Arc<AtomicUsize>>` (sender's peak held bytes).

Algorithm for `offer_bases` (non-in-place), because neither the signature nor the tests fix it:
```
batch = []
for each spec:
    sign basis (as today)
    n = basis_wire_len
    if batch.len() == BASIS_BATCH or !budget.try_acquire(n):
        flush(batch)            # send_paced(Need for batch's chunk idxs), then release batch's bytes
        budget.acquire(n).await # never waits while holding its own batch
    send_paced(ChunkBasis); batch.push((idx, n))
flush(batch)
send_paced(Need for the request's remaining idxs not covered by any batch)  # raw chunks; always sent, even empty, as today
```
In-place (`--inplace`, one ascending request): never wait; offer a basis only if `try_acquire` succeeds (otherwise the chunk goes raw), send the single `Need` at the end as today, then release everything acquired. Keep the "retired writer" check (`route.slot`) before every `Need`.

Sender: a session-wide `basis_held: usize`; `on_basis` adds `basis_wire_len` and fails the session with `protocol error: chunk bases above the budget` when it exceeds the budget; `on_need` (bases taken), `retry` (`f.bases.clear()`) and every removal of a file from `files` subtract that file's held bytes (track per file in `SFile`).

Before implementing, check how the receiver tracks which chunks it requested (re-requests, `rerequests` hook, `Done::Drained`): several `NeedChunks` per file must not be mistaken for a re-request or a completed request. If that bookkeeping assumes one `Need`, make it accumulate.

- [ ] **Step 1: Write the failing tests:**
  - `a_large_delta_keeps_bases_within_the_budget`: 48 MiB file, seeded pseudo-random (`data()` as in `chunk_list_parts.rs`), pushed once; then rewrite 90% of it in place with small edits every 64 KiB (so most chunks change but have a basis); both sides `basis_budget = 256 KiB`, sender `basis_peak`; push with `--bwlimit 20M`; assert success, contents equal, `basis_peak <= 256 * 1024`.
  - The same for `--inplace`: `a_large_inplace_delta_keeps_bases_within_the_budget`.
  - `bases_above_the_budget_fail_the_session`: receiver-side `tamper_frames` (pull, so the client is the receiver) repeats each `ChunkBasis` 10 000 times; `basis_budget = 64 KiB`; assert `Err` containing `"protocol error"`.
- [ ] **Step 2: Run** them — expected: compile failure, then FAIL on the budget assertions.
- [ ] **Step 3: Implement** as above.
- [ ] **Step 4: Run** the tests, then `sub_chunk_delta`, `delta`, `inplace`, `chunk_list_parts`, then the whole suite — expected PASS.
- [ ] **Step 5: Commit** `feat(session): send bases in batches under a session budget`.

### Task 6: Chunk lists sent into granted room

**Files:**
- Modify: `proto/irsync/v1/control.proto`, `limits.rs`, `parts.rs`, `sender_role.rs` (`on_digested`, new pump), `receiver.rs` (where parts are fed and where files leave the session), `mod.rs` (hooks)
- Test: unit tests in `parts.rs`; `crates/irsync/tests/backpressure.rs`

**Interfaces:**
- Produces (proto): `message ListRoom { uint64 records = 1; }` as `Control.msg` field 14 (receiver → sender: this many chunk records freed).
- Produces (`limits.rs`): `pub(super) const LIST_BUDGET: u64 = MAX_LIST_CHUNKS as u64;` (records; at least one maximal list, so any legal list can eventually be sent). Both sides start each connection with this room; no initial grant frame.
- Produces (`parts.rs`):
  - Receiver: `pub(super) struct ListBudget { limit: u64, held: HashMap<u64, u64>, total: u64 }` with `fn charge(&mut self, file_id: u64, part_records: u64, first_part: bool) -> Result<u64, String>` (a first part of a file that already holds records releases them first and returns the freed count to report; `Err` when `total` would exceed `limit`) and `fn release(&mut self, file_id: u64) -> u64`.
  - Sender: `pub(super) struct ListCredit { room: u64, queue: VecDeque<(u64, Vec<ChunkListPart>)> }` with `fn push(&mut self, file_id: u64, parts: Vec<ChunkListPart>)`, `fn grant(&mut self, records: u64)`, `fn ready(&mut self) -> Vec<ChunkListPart>`: pops whole files from the front while the file's **total** fits in `room`, subtracting it. A file never goes out partially: two half-sent lists could otherwise wait on each other forever. FIFO order.
- Produces (hooks): `list_budget: Option<u64>` (both sides), `list_peak: Option<Arc<AtomicU64>>` (receiver's peak `total`).

Rules: a sender whose digest has more chunks than the budget fails that file with a source error (it could never be sent). On the receiver, every path that drops a file's digest or assembly (DONE, failure, abandonment, a new list for the same id) calls `release` and sends `ListRoom { records }` when the count is non-zero; find them all by searching for removals from the receiver's `files` map and `Assembly` slots. A part that does not fit is `protocol error: chunk list beyond the granted room`. A part for a file id that was never planned is a protocol error; for a known id that already failed or was retried, it is ignored (late).

- [ ] **Step 1: Write the failing unit tests** in `parts.rs`: `charge_beyond_the_limit_is_refused`, `a_new_first_part_releases_the_old_list` (charge 10 for file 1, first part again with 4 → returns 10 freed, total 4), `credit_sends_whole_files_in_order` (room 10; push file A total 8, file B total 5; `ready()` yields A's parts only; `grant(8)` then yields B), `credit_never_splits_a_file`.
- [ ] **Step 2: Run** `cargo test -p irsync --lib parts` — expected FAIL.
- [ ] **Step 3: Implement** `ListBudget`, `ListCredit`, `ListRoom`, the sender pump (called after `on_digested` and on `ListRoom`), and the receiver releases.
- [ ] **Step 4: Write the e2e tests:**
  - `chunk_lists_wait_for_room`: both sides `list_budget = 100`, `chunk_list_part_records = 3`; push 30 files of about 40 chunks each (size them from `CHUNK_PARAMS` so each list is 30–60 records); receiver `list_peak`; assert success, trees equal, `list_peak <= 100`.
  - `a_chunk_list_beyond_the_room_fails_the_session`: push, sender-side `tamper_frames` duplicates every `ChunkListPart` with `first = 0` for a fresh file id the receiver planned (or, simpler, sets `total` to `list_budget + 1` on the first part); `list_budget = 100`; assert `Err` containing `"protocol error"`.
- [ ] **Step 5: Run** them and the whole suite (`chunk_list_parts` and `resume` especially, since reconnects reset room) — expected PASS.
- [ ] **Step 6: Commit** `feat(session): send chunk lists into room the receiver grants`.

### Task 7: The remaining protocol rules

**Files:**
- Modify: `sender_role.rs` (`on_plan`, `on_need`, send tasks), `receiver.rs` (DONE ack order), `listing_stream.rs` (order and count checks), `planner.rs` (`checked_order`, `check_order`)
- Test: `crates/irsync/tests/backpressure.rs`

**Interfaces:**
- Consumes: `tamper_frames` (Task 1), `ListingEnd` handling in the listing reader (Task 1).

Rules (spec §4.3), each with the honest case it must keep accepting:
- **Files in flight.** Receiver: queue the DONE `FileAck` before releasing the planner slot (today the slot is freed first, `receiver.rs` near the DONE ack). Sender: `on_plan` fails with `protocol error: more than {MAX_INFLIGHT} files in flight` when `files.len()` would exceed `MAX_INFLIGHT`.
- **Sends per file.** Instead of rejecting repeated `NeedChunks`, the sender runs at most one send task per file: a `NeedChunks` that arrives while one runs merges its indices (and the bases it takes) into the file's pending request (deduplicated, so it is bounded by the file's chunk count), started when the running task finishes. This keeps memory and tasks bounded without rejecting honest re-requests or batches.
- **Listing order and count.** The listing reader checks strictly increasing paths and fails with `protocol error: listing out of order`, except that a `ListingError` may name the directory whose entry was just listed (check `ScanEvent::Gap` placement in `scan.rs` for other legal placements before writing the check); `checked_order`/`check_order` in the planner go away. At `ListingEnd` it checks `count` against the entries received: `protocol error: listing count mismatch`.
- **Unchanged on purpose:** acks and data streams for unknown or finished file ids stay ignored (they create no state).

- [ ] **Step 1: Write the failing tests:**
  - `more_plans_than_the_window_fail_the_session`: pull (client is receiver), client `tamper_frames` repeats every `Plan` with fresh `file_id`s (`id + 1_000_000 * k`, k in 1..=5000 for the first Plan); assert `Err` containing `"protocol error"`.
  - `repeated_needs_do_not_multiply_sends`: pull, client `tamper_frames` repeats every non-empty `NeedChunks` 500 times; assert success, trees equal, and a new sender hook `send_tasks_peak: Option<Arc<AtomicUsize>>` stays `<= max_inflight` (set `max_inflight = 8`).
  - `an_out_of_order_listing_fails_the_session`: `inject_listing` after entry `b` injects an entry `a`; assert `Err` containing `"protocol error"`. Replace any existing test that expected the old disordered-listing behaviour (`grep -rn "disorder\|out of order" crates/irsync/tests`).
  - `a_listing_count_mismatch_fails_the_session`: `tamper_frames` adds 1 to `ListingEnd.count`; assert `Err` containing `"protocol error"`.
- [ ] **Step 2: Run** them — expected FAIL.
- [ ] **Step 3: Implement** the rules above.
- [ ] **Step 4: Run** the tests, then the whole suite — expected PASS (`containment.rs` and `ids_e2e.rs` must still pass with injected entries counted).
- [ ] **Step 5: Commit** `feat(session): enforce the protocol's limits on the peer`.

### Task 8: Benchmark and decisions

**Files:**
- Modify: `docs/IRSYNC_DECISIONS.md`

- [ ] **Step 1: Benchmark.** Release build of `main` before this branch and of the branch; the 33k-small-file push to tmpfs as measured for R42 (six interleaved runs, wall time to the millisecond, daemon CPU). Expected: branch wall time within the runs' spread of `main` (about 1.0 s). If slower, profile before tuning `LISTING_QUEUE`, `PEER_EVENTS` or `OUT_BUDGET`.
- [ ] **Step 2: Update the decisions.** Mark 2.1(1) done with today's date and a summary (own listing stream, bounded peer queues, byte-paced frames, batched bases, granted list room, enforced rules); amend R31 to point at it; remove the `ListingEnd.count` item from §2.3 Deletions; add to §2.2 that a hostile receiver can still make a sender digest up to `MAX_INFLIGHT` of the export's largest files (memory bounded by real file sizes, not by the peer); record the benchmark numbers.
- [ ] **Step 3: Run** `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features` — expected: clean, all PASS.
- [ ] **Step 4: Commit** `docs(decisions): close open point 1, listing backpressure`.
