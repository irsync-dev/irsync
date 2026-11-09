# irsync v1 — Decisions and Open Points

This file records the design and implementation decisions made while building
irsync v1 that are not derivable from the code itself, plus everything that was
knowingly left open. The design document
(`docs/superpowers/specs/2026-10-17-irsync-design.md`) is the binding authority;
the plan (`docs/superpowers/plans/2026-10-17-irsync-v1.md`) is its argument.
Where the two disagreed, or where neither answered, the rulings below settled it.

Each decision states what was decided, why, and what it costs to reverse.
Numbers match the implementation ledger's ruling numbers (R1–R35) so they can be
cross-referenced from commit messages and review notes.

---

## 1. Decisions

### 1.1 Wire protocol (`proto/irsync/v1/*.proto`, normative)

- **R1 — `Range`/`RangeSet` live in `common.proto`.** The index needed the
  types before the data protocol existed. Reversal cost: moving a message.
- **R9 — Session plan/progress are protobuf messages (`session.proto`).**
  The index stores them as opaque blobs (R2), and protobuf is the project
  standard, so non-Rust implementations can read them too. Reversal cost: a
  proto edit.
- **R12 — `SessionDone.errors` carries receiver-side path errors.** Without
  it a push could not exit 23 on partial failure. Capped at 1000 entries.
  Wire-compatible addition.
- **R19 — `Hello` fields 10–14: `delete_mode`, `delete_excluded`,
  `max_delete`, `dry_run`, `ignore_errors`.** Deletion and dry-run are
  receiver actions; a push needs the daemon to know. Honoured only with a
  write grant on a writable export. Dry-run gates *every* receiver-side
  mutation. Wire-compatible addition.
- **R25 — `Hello.itemize` (15) and `Control.Itemized` (9).** The daemon's
  planner is the only place that knows the attribute diff on a push, so it
  sends per-item itemize codes back; the client flips `>`↔`<`.
  Wire-compatible addition.
- **R30 — `Hello.update_only` (16) and `Hello.checksum` (17).** Silently
  ignoring `-u` on a push would overwrite newer files. `--partial-dir` on a
  push was refused (it names a daemon-local path); see R39.
- **R10 — No other client options cross the wire in v1.** The daemon side
  uses full archive preservation, checksum off, and filters apply only to the
  sender's own walk. Reversal cost: a `Hello` extension (see Open Points,
  daemon-side filters). (Superseded by R36 and R38: the preserve flags and
  the filter rules cross; R39 adds a relative `--partial-dir`.)
- **R36 — `Hello.preserve` (22) carries the client's `-l -p -t -g -o -D
  -H -S`** (2026-10-29). A receiving daemon applies exactly what the client
  asked for, as the client does on a pull; a sending daemon lists hard
  links only with `-H`. A `Hello` without it (an older client) means
  everything, as before.
- **R39 — `Hello.partial_dir` (24) carries a relative `--partial-dir`**
  (2026-10-29). A receiving daemon keeps the `.part` files of interrupted
  transfers under it, below the session root (a push into `m/sub` with
  `.p` writes `sub/.p/`), and leaves it out of its own listing as the
  client does. The client refuses an absolute path or one with `..` on a
  push, and the daemon refuses the same with `BAD_PATH`: a client may not
  name a directory outside the destination on the daemon's side.
- **R38 — `Hello.filters` (23) carries the client's include/exclude rules**
  (2026-10-29), compiled on the client (`--exclude-from` and
  `--include-from` files read there), in command-line order. A sending
  daemon walks with them, so a pull honours `--exclude`; a receiving daemon
  protects what they exclude from deletion, so a push with `--delete` no
  longer needs `--delete-excluded` (R22 reversed).
- **R37 — `--no-relay` and `relay_mode` for the client** (2026-10-29): the
  client's counterpart of the daemon's `relay_mode = "disabled"`, in
  `[client]` and per host alias (`host add --relay-mode`), with lookups and
  the port mapper left on. Precedence: `--relay` on the command line, then
  `--no-relay`, then the host entry's mode, then `[client]`'s.
- **R35 — No application-level heartbeat in v1.** A `Control.Heartbeat`
  frame is a protocol change better made deliberately; see Open Points.

### 1.2 Identity and authorization

- SSH Ed25519 keys are iroh identities (32-byte seed → `SecretKey`);
  grants as `exports="photos:rw,docs:ro"` lines in a key list file.
  (Superseded: grants now live in `exports.toml`; see
  `docs/superpowers/specs/2026-10-23-zero-config-cli-design.md`.)
- The daemon rejects unknown keys and keys with an empty grant set before
  reading a single byte of the stream (close code 1).
- `irsync id generate` refuses to overwrite an existing key (without `--force`) and chmods the private
  key to 0600 explicitly.
- Duplicate keys in the v1 key list file: first match wins (OpenSSH semantics).
  Non-Ed25519 lines are skipped with a warning.
- Passphrase-protected keys unlock via `--passphrase-file`,
  `IRSYNC_PASSPHRASE`, or a tty prompt.
- **R33 — The client identity is looked up like an ssh key** (2026-10-29,
  reversing the zero-config design's choice to never read `~/.ssh`):
  `--identity`, else `identity` in the config, else the irsync key when it
  exists, else `~/.ssh/id_ed25519` when it exists (loaded as it is, never
  written), else a key generated at the irsync path. A named path is the
  whole answer. The daemon keeps its own key. Cost if wrong: a user with an
  encrypted ssh key and no irsync key is asked for a passphrase on first
  use instead of getting a silent new key; `irsync id generate` opts out.
- **R34 — `SIGHUP` reloads the `[daemon]` settings** (2026-10-29): a new
  endpoint is bound from the re-read file (a command-line `--listen` kept),
  the old one refuses newcomers and closes once its sessions end;
  `idle_timeout` and `dormant_session_timeout` apply from then on; a reload
  that cannot be applied changes nothing. `state_dir` still needs a
  restart. Design: `superpowers/specs/2026-10-29-daemon-reload-design.md`.

### 1.3 Index (SQLite, WAL)

- **R2 — Sessions are stored as an opaque `SessionRow`** (id, key, plan blob,
  progress blob, timestamps) so the index does not depend on engine types.
- **R6 — A new session with the same key evicts the dormant one** (newest
  wins, needed by `--no-resume`); the evicted id is returned for logging so
  eviction is never silent.
- **R13 — One index per daemon export root; per-session paths are prefixed
  via `IndexScope`** rather than opening one index per session root, so GC,
  partials and resume all resolve against the export root and cache reuse
  works across sessions.
- **R28 — `meta.root_path` is stored** so `irsync gc` can locate each index's
  root without being told.
- Index rows carry a `has_digest` flag; the index never fabricates a digest
  for a row that has none.

### 1.4 Session, resume and reconnect

- **R14 — Resume is bound to the creating remote `EndpointId`.** A foreign
  or unknown session id yields `BAD_SESSION`; the client retries once with a
  fresh id.
- **R15 — An incomplete persisted plan falls back to a fresh listing exchange
  under the same session id**, honouring the done set, so a session
  interrupted during listing still resumes its partials.
- **R16 — The receiver re-requests still-missing chunks** once a request's
  data streams drain, at most 3 times per file per connection, then
  `WRITE_ERROR`.
- **R17 — A finished session's row is deleted on both sides at
  `SessionDone`.** Resuming a completed session would skip changed files.
  Partials stay keyed by path so adoption still works.
- **R18 — `--reconnect` bounds each *outage* (time since the last productive
  connection), not total runtime.** A connection closed as superseded (close
  code 3) is fatal to the stale client so a dead process cannot steal a
  session back.
- **R33 — `--timeout` is the per-connection inactivity timeout** before a
  reconnect attempt; `Parallel` from the client config is applied. (`-r`
  was implied and `--no-r` refused until R41.)
- **R41 — `-r` is real** (2026-10-29): recursion comes from `-r` or `-a`,
  `--no-recursive` (`--no-r`) turns it off, and `-d`/`--dirs` transfers
  directories without their contents. Without recursion both walks stop at
  the top level (`WalkOpts::max_depth`), a sending side skips directories
  (logged at `info`) unless `-d`, and `--delete` never makes a directory a
  candidate. The flags travel in `Hello.preserve` (`recursive`, `dirs`).
- **R34 — Activity for both idle watchdogs includes local progress** (chunk
  writes incl. local copies, digest completion, finalize start/end, delete
  pass), not only wire traffic. The daemon idle limit is
  `DaemonConfig.idle_timeout` (default 30 min, `0` disables) and is suspended
  while a session has in-flight local work. A `.part` locked by this same
  session's previous connection is waited for up to 60 s (dev+ino checked)
  instead of failing with `PART_BUSY`.
- **R11 — Moved-file detection (`LocalCopy`) reuses the normal
  `PlanItem`/`ChunkList` path** with every chunk answered locally, so there
  is one write path. Cost: one extra round-trip per moved file.

### 1.5 Receiver and filesystem safety

- **R4 — `RelPath` has a validating constructor** rejecting a leading `/`,
  empty components, `.`/`..`, and NUL; canonical form keeps `Eq`/`Hash`
  consistent with component-wise `Ord`.
- **R7 — The planner's merge-join drops any listing key that is `<=` the
  last emitted key** (duplicates and out-of-order alike) with an error log,
  because emitting them can manufacture a spurious delete. Cost: a missed
  file only if a listing is already corrupt.
- **R8 — A directory at the destination is never replaced by a file**,
  and `FileWriter::open` restores `u+w` on irsync's own `.part` on
  `EACCES`, so a read-only temp file can never be stranded. Since
  2026-11-05 the check is the rename itself (`EISDIR`, reported as
  "refusing to replace directory") rather than an lstat before it, so the
  `.part`'s metadata is applied first and `u+w` is restored when the
  rename fails.
- **R42 — `place` stays its own blocking call** (2026-11-05). Folding it
  into the write that completes the file (that call also adding the last
  chunk to the hash tree, then syncing, applying metadata and renaming)
  was built and measured on a push of 33k small files after gc: futex
  calls 823k -> 683k and context switches ~304k -> ~278k, but wall time
  ~1041 -> ~1098 ms (six interleaved runs, timed to the millisecond) and
  daemon CPU unchanged. Not merged. The likely cause, unconfirmed: the
  place work then runs behind the file's last write instead of
  overlapping other files' writes; the files-in-flight window (up to
  4096) is not the limit. An off-CPU profile of both would settle it
  before trying again.
- **R43 — The sender's digest trusts its walk's stat** (2026-11-06). A
  digest request for a file this connection listed uses the walk's stat
  (size, mtime, ctime, inode) for the index quick check; it no longer
  lstats the file again. On a miss the row is the open file's `fstat`.
  That is one stat per file fewer on the sender. A file changed between
  its listing and the request can get the cached digest of what the walk
  saw. The send then reads chunks that do not match that digest, or the
  receiver's whole-file hash fails. Either way the file is retried with a
  fresh stat and a forced re-read, so stale data never lands. A changed
  file costs a failed attempt instead of every file costing a stat. If
  the receiver already holds every chunk, it gets the walk-time content,
  which matches the listed metadata. Retries and resumed plans still
  re-stat. The walk stats are kept in the listed-files map (48
  bytes per regular file).
- Temp files are `.irsync.<name>.part` in the destination directory (or
  `--partial-dir`), and `.irsync.<name>.tmp` for symlinks, specials and
  hard links; both come from `recv::temp_name`, which cuts the name and
  appends `~` plus 16 hex digits of its BLAKE3 hash when the plain form
  would exceed `NAME_MAX` (255 bytes), so a 255-byte entry name still has
  a temporary (found by irsync-stress, 2026-10-21). The GC therefore never
  inverts a part name; it maps destinations forward. Parts are opened
  `O_NOFOLLOW` with the fd held; all metadata and
  rename operations work on held fds / dirfds with no-follow resolution;
  chunk adoption from an existing `.part` is verified by hash before use.
- Hard-link targets are containment-checked and resolved with no-follow
  dirfd lookups.
- **R3 — `sparse` is computed only for regular files.** Sparseness on the
  receiver is chunk-granular (an all-zero chunk is not written).

### 1.6 Deletions

- **R20 — Exit code 25 when `--max-delete` stopped the pass**, taking
  precedence over 23; combined exit code order is 25 > 23 > 1 > 0.
- **R21 — `--delete-during` deletes per directory, guarded by inline
  listing gaps** (reversed 2026-10-29; it first ran as `--delete-after`
  because an incomplete listing was only known at `ListingEnd`). Each
  listing now reports a directory it could not read or an entry it could
  not stat as a `ListingError` at that path's position, so the receiver
  knows whether the listing is complete up to any point. At every
  directory boundary of the merge, the candidates left behind are deleted
  in one pass; a gap at or before a pass's last candidate refuses that pass
  and every later one (unless `--ignore-errors`), while deletions already
  made stand, as with rsync. Root-level entries wait for the final pass
  after the transfers. Design: `superpowers/specs/2026-10-29-delete-during-design.md`.
- **R22 — A push with `--delete` plus any include/exclude filter is refused
  unless `--delete-excluded` is also given.** (Reversed 2026-10-29 by R38:
  the filters travel in `Hello`, and the daemon protects what they exclude
  like any receiver.)
- A deletion pass is refused outright when the source listing was
  incomplete or arrived out of order.

### 1.7 Metadata and special files

- **R5 — `--numeric-ids` is accepted and always on**; there is no name
  mapping in v1. (Reversed 2026-10-29 by R40.)
- **R40 — Owner and group names travel with each entry
  (`FileEntry.user/group`, 32 and 33) and the receiver maps them to its
  own ids** (2026-10-29), rsync's rule: a name that exists on the receiving
  side gives its id, any other keeps the number as sent. `--numeric-ids`
  skips the mapping; a receiving daemon learns it from `Hello.numeric_ids`
  (25). Names are looked up once per id on the sender and once per name on
  the receiver. Privilege rules are unchanged: the owner is applied only as
  root, the group only as a member.
- FIFOs, devices and sockets with the same `rdev` on both sides get the
  same metadata decision as directories: `MetaOnly` when mode, time or
  ownership differ under the preserve flags, `Skip` otherwise, and
  `Special` (recreate) only when `rdev` differs. Before 2026-10-21 only
  `rdev` and mode were compared, so a FIFO's mtime never updated
  (found by irsync-stress).
- **R23 — Hard links are applied in a final pass after all regular files are
  finalized.** The sender marks the first path (listing order) `REGULAR` and
  later paths `Kind::HARDLINK` (7) with `link_target`. Sockets are skipped
  with a warning like rsync. Device nodes needing privilege yield a per-file
  error.
- Ownership: uid is applied only as root; gid is applied when root, equal to
  the effective gid, or a member of the process's supplementary groups.

### 1.8 CLI, output and reporting

- **R24 — Itemize direction is `>` for pull and `<` for push**, from the
  client's point of view; hard links count under "created".
- **R26 — Itemize attribute letters appear only for attributes being
  preserved**, matching rsync.
- **R27 — Progress percentage and to-check counts come from a `Total` event**
  emitted once the sender's listing completes (or the peer's `ListingEnd`
  arrives); until then the percentage is blank.
- `--info=progress1|progress2` only; `-v`, `-i`, `--stats` follow rsync's
  shapes.
- Client endpoints do not publish their address (`publish: false`); daemons
  do.

### 1.9 Garbage collection

- **R28 — GC removes only `.irsync.*.part` *regular* files** (checked with
  `symlink_metadata`) that are older than the timeout and have no partials
  row or a stale unreferenced one. Live sessions are protected by plan
  references and fresh rows.
- **R32 — A root is walked at most once per 24 h** (timestamp in index
  meta); the hourly daemon tick runs only when stale partial rows exist or
  24 h have elapsed. A full walk before every sync would violate the
  "no slower than rsync" anti-goal.
- Thresholds: client 7 days (and 1 h for stale partial rows), daemon
  `dormant_session_timeout`.

### 1.10 Tests and CI

- **R29 — Tests that create non-UTF-8 filenames skip when the filesystem
  refuses the name** (e.g. APFS) and stay mandatory on Linux. With `CI` set,
  the GNU rsync comparison panics instead of skipping if rsync is unusable.
- CI runs Linux and macOS (fmt, clippy `-D warnings`, full test suite). macOS
  was never compiled locally during v1 development.

### 1.11 Process rulings

- **R31 — Unbounded control/listing channels stay in v1**, with a cap of 4096
  planned files in flight per session. Bounded channels were deliberately
  avoided to prevent the reader/writer deadlock cycle; redesigning that in a
  late fix wave was judged riskier than memory growth from an *authorised*
  peer. Tracked follow-up (see below). Amended 2026-10-25: a file in flight
  holds its `.part` open (and the old file for a `DELTA`) from its chunk
  list to its finalize, while the sender streams only 32 files at a time,
  so thousands of descriptors were held and a receiver under the common
  soft limit of 1024 failed files with `EMFILE`. The process now raises its
  soft `RLIMIT_NOFILE` to the hard limit at startup (`fs::rlimit`), and the
  window is the smaller of 4096 and what the limit leaves after a reserve
  of 256, at two descriptors per file (`planner::inflight_for_limit`).
  Also, a fresh `.part` (nothing to adopt, nothing to clone) is created
  and locked by its first write rather than when the chunk list arrives
  (`FileWriter::ensure_open`), so a file waiting for the sender's stream
  holds no descriptor; the window's descriptor cost falls on resumed and
  `DELTA` files only. Amended 2026-10-26: a file open that still hits a
  momentary shortage (`EMFILE`, `ENFILE`, `EAGAIN`, `ENOMEM`, `ENOBUFS`)
  is tried again on a bounded schedule of six waits doubling from 15 ms,
  about a second in all (`fs::transient`), at both open primitives: the
  `.part` open and `safe::open_regular_nofollow` (sources, old files,
  local copies, both roles). Errors that will not fix themselves fail the
  file at once, as before; a failed file is otherwise retried only by a
  reconnect's resume or by the next run. Amended 2026-11-07: the follow-up
  landed (see 2.1(1)); the listing has its own stream, the queues between
  the network and a consumer are bounded, and each side enforces the
  protocol's limits on the other.
- **R34 (process) — One narrowly scoped follow-up fix was dispatched after
  the single final fix wave**, deviating from the "one fix wave" rule, because
  the fix wave itself introduced the idle-timeout regression and it hit the
  core use case.

---

## 2. Open Points

### 2.1 Tracked follow-ups (should land before relying on irsync at scale)

1. **Bounded channels / backpressure (R31).** **Done 2026-11-07.** No
   queue in a session grows because one side is faster than the other, and
   each side bounds what the other can make it remember or spawn. Design:
   `docs/superpowers/specs/2026-11-06-listing-backpressure-design.md`.
   - *Own listing stream.* Each side that lists sends listing entries,
     gaps and `ListingEnd` on a dedicated QUIC stream, off the control
     stream. A listing reader feeds the planner through a bounded queue
     (`LISTING_QUEUE`, 1024); when the planner stalls on its in-flight
     slots the reader stops, QUIC flow control backs up the peer's walk,
     and the control stream keeps being read, so the reader/writer
     deadlock R31 avoided cannot form.
   - *Bounded peer queues.* The events queue is bounded
     (`PEER_EVENTS`, 1024); a full queue stops the control reader, so a
     flooding peer waits in its own writer.
   - *Byte-paced frames.* Per-path and per-chunk frames (`Itemized`,
     `ChunkBasis`) go through `Out::send_paced`, which waits while
     `OUT_BUDGET` (4 MiB) is queued for the writer; the main loop never
     waits on the peer.
   - *Batched bases.* The receiver sends `NeedChunks` with each batch of
     bases (`BASIS_BATCH`, 64), so the sender uses and drops them; the
     sender fails a session that holds more than `BASIS_BUDGET` (32 MiB).
   - *Granted list room.* The receiver holds chunk lists under a
     session-wide budget (`LIST_BUDGET` chunk records) and grants room per
     file; a file whose list does not fit waits. A list part sent without
     room is a protocol error. The budget is 2^24 records (one maximal
     list), at about 50 bytes a record: about 800 MB per session on the
     receiver (its assembled lists), up to about twice that with the
     obsolete-list allowance of (a) below, and as much again on the sender
     (lists queued for its control writer and not yet written, see (h)).
     A daemon's bound is that times its concurrent sessions.
   - *Enforced rules.* Each side fails the session, with an `Error` frame,
     for a peer that breaks a limit: more than `MAX_INFLIGHT` files in
     flight, unknown or unlisted paths or a path planned twice, bases for
     files not in flight, list parts beyond the per-file cap or the
     budget, more list room granted than a connection starts with, more
     chunk lists started for a file than retries allow, listing paths out
     of order, a `ListingEnd` whose count differs from the entries
     received, a missing or repeated `ListingEnd`, an undecodable frame,
     or a peer that does not read its control stream.
   - *Settled along the way.*
     (a) A receiver may briefly hold an obsolete chunk list beyond its
     counted room, so the bound is about 2x `LIST_BUDGET`, not 1x.
     (b) Bases parked in a file's pending request sit outside the
     sender's basis budget; they are bounded by the receiver's batches and
     the files in flight.
     (c) At session end the daemon waits for its control writer and then
     for the client to close, while the connection makes progress, so a
     slow client can finish reading. It gives up after 30 s of no progress
     or at the idle timeout (aborting the writer if it still runs), and
     the wait is unbounded when the idle timeout is 0.
     (d) `LIST_BUDGET` (= `MAX_LIST_CHUNKS`) is an implicit protocol
     constant shared by both sides: the sender starts with that much room
     and the receiver grants against the same number, so the two must
     agree.
     (e) With a listing of the connection, a receiver plans each listed
     regular file once (a retry keeps its file id and sends no new plan;
     `-H` names after the first are linked on the receiver, never
     planned), so a `PlanItem` for a path the sender did not list as a
     regular file, or one already planned, is a protocol error. A sender
     resumed from the receiver's stored plan has no listing to check
     against: it checks each path as its walk would have admitted it, and
     a path gone since is an honest source error. There, what a hostile
     receiver's plans leave behind (a stat each, and per source error an
     entry in the outcome and the session row) grows with the plans it
     sends, at most `MAX_INFLIGHT` at a time, and the unread replies are
     bounded by (h).
     (f) The sender's list room never exceeds what the connection
     started with: the receiver frees only what the sender's lists took,
     so a `ListRoom` that would raise it higher is a protocol error.
     (g) A sender starts at most 1 + 2 x `MAX_RETRIES` (3) chunk lists per
     file and connection: the first, and per retry an empty first part
     withdrawing the old list and the new list. More is a protocol error,
     since each would make the receiver grant room again. Room freed by
     one main-loop event goes back in one `ListRoom`.
     (h) Every byte queued for the control writer, paced or not, counts
     against a ceiling of twice what an honest session can queue
     (`limits::out_ceiling`, derived in `limits.rs`: about 2.0 GB for a
     sender, nearly all of it chunk lists within the granted room, and
     850 MB for a receiver, mostly `NeedChunks`). Past it the session
     fails with "the peer is not reading its control stream". This stops
     a peer that frees slots or room with frames of its own while never
     reading what we send, which no queue bound stops (main loops queue
     replies without waiting) and which the idle timeout misses (its own
     frames count as activity). The earlier reasoning that the queued
     `PlanItem`s are bounded by `MAX_INFLIGHT` holds only for a peer that
     reads: one that acks files it never read the plan of frees slots at
     once.
     (i) A file's digest task ends with the file, as its send task does,
     and a retired `offer_bases` task stops before signing another basis.
     (j) A listing half that ends without `ListingEnd` (nothing listed, or
     a lister that stopped short) is held back until the role ends, so a
     side failing mid-listing reports its `Error` first rather than a cut
     listing, which the peer would take as a transport error and retry.
     Likewise an undecodable frame on either stream is a protocol error,
     not a transport error.
     (k) A failed session aborts its control writer after the 5 s drain
     rather than detaching it, so the backlog (h) stops and the
     connection it held is freed. A digest's hasher permit travels with
     its blocking read, so aborting the task does not free a worker
     early. The sender never sends the root as a file, even when it is
     one and was listed.
     (l) A request for more than `NEED_PIECE` (2^18) chunks goes out as
     several `NeedChunks` of at most that many (about 1 MiB each); before
     2026-11-07 a file of more than about 4M chunks (about 1 TB) asked in
     one frame above `MAX_FRAME`, which failed the control writer. Since
     each request takes every basis the sender holds for the file, in
     place each piece's bases go out just before it (signed once the
     earlier pieces are requested; the ordered writes have not reached
     the piece yet, and a basis region starts at its chunk).
     (m) The client's success path waits for its own control writer
     under the same progress bound as the daemon (30 s in which nothing
     moves, at the latest the idle timeout), then aborts it and closes:
     until 2026-11-07 a daemon that stopped reading while staying
     connected kept a finished session from returning.
     (n) The sender digests ahead of the room only as much as the room
     holds: a digest stats its file, then takes one permit per chunk
     record the file can have (`size / CHUNK_PARAMS.min + 1`, at most the
     room) before it reads it, and its list keeps them until it goes out
     under the room (`parts::ListCredit`). Digests running and lists
     waiting for room so hold at most one room of records; with the lists
     sent (within the room) the sender holds at most two rooms of digests
     however many files are in flight, and a file that needs the whole
     room is digested alone. Until 2026-11-07 every planned file was
     digested at once and its list held until room came, which the room
     did not bound (`the_sender_digests_ahead_of_the_room_only_a_room`:
     126 records held ahead of a room of 24).
   - *What it cost before the fix below.* Push of a generated tree of
     31,531 files (316 MiB,
     `filegen run --seed 1 --steps 400000 --max-total-size 600M
     --max-file-size 16K`) to a local daemon on tmpfs, `-a --no-resume`,
     release builds, six interleaved runs per build, 32 cores, busy
     machine (load 5-8). Wall time `main` 2544-3877 ms, median 2973 ms;
     branch 3899-5334 ms, median 4536 ms (+53%; the ranges barely
     separate). Daemon CPU is nearly the same: `main` 14.0-14.8 s, median
     14.4 s; branch 14.8-15.7 s, median 15.1 s (+5%). So the branch does
     the same work with less parallelism: the daemon's mean per-file time
     (first request to DONE) goes from 0.40 s to 0.66 s, and its `recv`
     activity (waiting for the sender's data streams) from 1.9 s to 4.2 s
     of wall time, while `write` and `place` are unchanged. The sender
     (`send` busy 1.3 s) is not busier.
   - *The stall, found 2026-11-07.* A data stream holds one of the
     sender's `DEFAULT_PARALLEL` (32) send permits until `stopped()`
     resolves, which is when the peer has QUIC-ACKed every byte. The
     receiver ACKs every second packet or after 25 ms, so when all 32
     permits wait on `stopped()` and nothing else is sent, all of them sit
     idle for about 25 ms. Per run, the number of `stopped()` waits of 20
     ms or more tracks wall time: `main` has 500-2200, the branch
     2100-3700. This exposure is already on `main`; the dedicated listing
     stream and the held-open stream tasks make the stalls more frequent
     (which of the two is not isolated). Not the cause: the close wait,
     `PEER_EVENTS`, `OUT_BUDGET`, `BASIS_BUDGET`, list room (no budget
     waits, no QUIC loss). Releasing the permit before `stopped()` made
     it slower (the 256-stream limit binds).
   - *Transport fix.* `transport_config()` asks the peer for immediate
     ACKs (`ack_eliciting_threshold` 0, `max_ack_delay` 1 ms), on both
     endpoints. No queue, budget or protocol rule changes. With it the
     branch's median fell from 4.4-4.5 s to about 2.0 s. (A first
     comparison against `main` without the fix, 3293 ms against 2022 ms,
     credited the branch with removing `main`'s own stall.)
   - *What it costs, with the fix on both.* `main` with the same ACK
     setting (ffea8b1 plus ac13135) against the branch (d89c63e, with the
     final review's fixes): same workload and method, separate release
     builds in separate target directories (binaries checked distinct),
     one warm-up run per build, then two batches of ten interleaved runs
     each, every destination removed after its run; 32 cores, 1-minute
     load 3.8 at the start and 6-14 during (the benchmark itself takes
     about five cores). Wall time, all 20 runs: `main`+ACKs median
     1973 ms (quartiles 1937-2223, range 1907-3257); branch median
     2018 ms (quartiles 1982-2241, range 1960-2364). The branch is about
     45 ms (2%) slower on the median, in both batches (1958 against 2006,
     1986 against 2034), which is within the run-to-run spread: the
     quartile ranges overlap, and the noise floor of the median measured
     during the investigation is about 150 ms. Daemon CPU is about 3%
     higher on the branch, in both batches (median 14.2 s against
     13.8 s).
   - *Still open.* The effect of immediate ACKs on a real long-RTT link
     is untested (all runs were on loopback); the gain is expected to be
     smaller there, and the extra ACK traffic is unmeasured. Threshold 1
     (the default) with `max_ack_delay` 1 ms is a possible gentler
     setting, since the stalls come from the delay timer; it was not
     measured. *Measured 2026-11-08* with irsync-netstress (see
     "ACK settings on a long-delay link" in `docs/NETSTRESS_DECISIONS.md`):
     on a 90 ms round-trip link the three settings took the same time; on
     the slow `dsl-video` uplink the peer's default was slower on pulls,
     and the 10 s idle drops on pushes came from a large `SessionDone`
     (fixed 2026-11-09), which immediate ACKs only kept under the limit.
     The immediate setting stays. The occasional multi-minute syncs on the
     90 ms link were not an ACK effect but a bug in noq's BBR after a quiet
     spell (2026-11-09, "The stalls on a long-delay pull" in the same file).
2. **Application heartbeat (R35).** One peer's long local work (e.g. hashing
   a multi-GB file on a slow disk) is invisible to the other peer's idle
   watchdog. Mitigated by the 30-min daemon default and `idle_timeout = 0`.
   Plan: a `Control.Heartbeat` frame, bundled with (1) as one protocol
   revision. **Done 2026-10-21**, after irsync-stress reproduced it (a push
   with `-c` and multi-gigabyte files, client `--timeout 10`): `Heartbeat`
   is field 10 of `Control`, sent by either side every 500 ms in which its
   activity marker moved and swallowed by the reader (which already counts
   every frame as activity). It needed no protocol revision: an older peer
   decodes an unknown oneof member as an empty `Control`, which the reader
   has always ignored. `--timeout` values below a few seconds remain
   unreliable by construction (`engine::HEARTBEAT_INTERVAL`).
3. **Daemon-side filters (R10/R22).** Done (R38): the rules travel in
   `Hello`.
4. **Transmitting remaining client options.** Done: the preserve flags
   (R36), the filters (R38) and a relative `--partial-dir` (R39); an
   absolute one stays client-side and a push with it is refused.
5. **Durability of received files: one fsync per file** (raised
   2026-11-05; decided 2026-11-08: (D), with (B) as the default). The
   receiver flushes nothing by default, as rsync does; `--fsync` (rsync's
   flag, carried to a receiving daemon as `Hello.fsync`) and an export's
   `fsync = true` (for pushes into it) restore the per-file `sync_all` of
   (A). (C1) and (C2) were not built. What follows is the state before the
   decision: `FileWriter::place` (`recv.rs`) writes the
   `.part`, calls `sync_all`, applies mode, owner and times, then renames.
   The parent directory is not synced after the rename (see 2.3), and the
   index runs with `synchronous=NORMAL`.
   - *What the fsync protects.* Only a power loss or kernel crash loses
     data; a killed process does not (the page cache survives it).
     Without the fsync, such a crash within the seconds before writeback
     can leave a file at its name with the right size and mtime (journaled
     metadata) but zeros or stale blocks inside (delayed allocation on
     ext4 and XFS). The quick check compares only size and mtime, so the
     next run passes it: silent corruption until a `-c` run. A lost rename
     is harmless (the file is old or absent and is sent again), and local
     chunk reuse verifies every chunk's hash, so a corrupt file never
     spreads into other files.
   - *What it costs.* Unmeasured. The push benchmarks ran on tmpfs, where
     fsync does nothing, and the development machine has no real disk.
     Expected: roughly 0.1-1 ms per file on NVMe, 5-20 ms on a spinning
     disk (minutes for a 33k-file push), softened because files are synced
     from many blocking threads at once and ext4 and XFS can merge the
     syncs into fewer journal commits.
   - *Options.*
     (A) Keep the per-file fsync: every file at its name has its content.
     (B) Drop it, as rsync does by default (rsync 3.2.4+ has an opt-in
     `--fsync`): no guarantee for the last seconds of writes.
     (C1) One `syncfs` per session before reporting success: a successful
     session is durable, a crash mid-session can leave silently corrupt
     files as in (B); `syncfs` also flushes other processes' dirty data on
     that filesystem, and it is Linux-only.
     (C2) Write a batch of `.part` files, sync them, then rename the batch:
     the guarantee of (A) at one flush per batch, but `place` splits into
     a write and a rename step and files appear in batches.
     (D) A flag choosing among these.
   - *Independent of the choice.* `sync_data` (`fdatasync`) would do
     instead of `sync_all`: mode and times are applied after the sync, so
     the extra metadata flush of `fsync` protects nothing; the new size is
     flushed either way. On macOS `fsync` does not flush the drive's write
     cache (only `F_FULLFSYNC` does), so (A) is weaker there than on Linux.
   - *Next step.* Measure a many-small-files push to ext4 or XFS on a real
     disk with the fsync on and off. If it matters, the likely choice is
     (D) with (C1) as the default and per-file syncing as the flag; (C2)
     only if (C1) proves insufficient.
   - *Measured 2026-11-07, on btrfs, not ext4 or XFS.* The 31,531-file
     push (see item 1's workload) from tmpfs to a daemon whose destination
     and index are on btrfs RAID1 over two WD20EFRX disks, 8 cores, a
     throwaway build switching the sync by environment, fresh destination
     and index every run, four interleaved runs each (medians); "+ sync" adds
     a `sync -f` of the destination after the client returns, which is what
     (C1) would cost at the end of a session: `sync_all` 22.5 s, + sync
     26.5 s; `sync_data` 20.8 s, + sync 24.8 s; no sync 10.4 s, + sync
     18.6 s. So on this disk (C1) saves about 30% of a push, (B) about 60%,
     and `sync_data` is within the noise. btrfs merges concurrent fsyncs
     into one log commit (about 500 threads waited in `wait_log_commit` at
     a time), which is why the per-file cost is about 0.25-0.4 ms, not 5-20 ms.
     Two of eight runs with a sync took 187-200 s; that was the index
     feedback loop fixed in f80a77e (`recv.rs`, `PERSIST_EVERY`), not the
     sync. ext4 and XFS, whose fsync behaves differently, are unmeasured.
6. **Daemon blocking pool: thread count and futex handoffs** (measured
   2026-11-06, left open). On the 33k-file push to tmpfs (32 cores), the
   daemon uses about 12.2 s of CPU for a push of about 1 s wall, 62% of
   it in the kernel. Futex is 21% of daemon CPU, and 80% of that is
   tokio's blocking-pool threads sleeping and being woken between tasks:
   with up to 4,096 files in flight and several blocking calls per file,
   the pool fills to tokio's default ceiling of 512 threads (544 runtime
   threads with the 32 async workers).
   - *Where the daemon blocks* (288k context switches, recorded with
     `perf record -e context-switches -c 1 -g` on a build with
     `-C force-frame-pointers=yes`; DWARF unwinding stops at
     `parking_lot`): idle blocking-pool threads 43%, async workers
     parking 29%, the index writer waiting for rows 4%, lock waits about
     20% (the pool's own mutex 3.7%, QUIC stream locks about 3%, the
     index reader pool 1.5%, malloc 1.2%), sleeps inside syscalls about 3%
     (directory locks in `rename` and `statx`).
   - *Capping the pool* (`max_blocking_threads` in a throwaway build,
     three interleaved runs per cap; wall ms / daemon CPU s / context
     switches): default 977-1033 / 11.7-12.5 / ~303k; 64: 957-1006 /
     11.6-11.7 / ~285k; 16: 943-974 / 10.9-11.1 / ~242k; 8: 948-990 /
     9.0-9.1 / ~190k; 4: 1215-1631 / 7.4-7.8 / ~182k. The handoffs cost
     CPU, not wall time: a cap of 8 saves about 25% of daemon CPU and
     37% of context switches at the same wall time, and 4 is too few.
   - *Why no cap yet.* On tmpfs a blocking call takes microseconds; on a
     real disk `fsync` (5) and metadata writes take milliseconds, and the
     threads in flight then buy I/O parallelism, so a cap of 8 could slow
     real-disk pushes. All concurrent sessions share the one pool, and
     long-lived blocking tasks (the walk's `blocking_send`, the planner)
     hold a thread for a whole session and need headroom under any cap.
   - *Next step.* Measure with (5), on a real disk: if a cap costs no wall
     time there, set `max_blocking_threads` in `run_daemon`, sized from
     the core count plus the long-lived tasks per session. The wall time
     of the tmpfs push is set elsewhere; the next profile is of the
     client and the pipeline between the two sides.
   - *Measured 2026-11-07 on the disk of (5): no cap while files are synced
     one by one.* Medians of four runs, wall / daemon CPU: with `sync_all`,
     default 22.5 s / 31 s, cap 64 229 s / 111 s, cap 16 373 s / 130 s,
     cap 8 520 s / 136 s (`sync_data` with cap 16: 381 s). Each blocking
     thread waits in `fsync` for the next btrfs log commit, so with eight
     threads each commit carries about eight files. The index feedback loop
     (f80a77e) made it worse; with that fixed, cap 8 is 334 s and cap 16
     210 s, still ten times slower. Without per-file syncing a cap costs
     nothing at 16 (7.8 s against 10.4 s) but varies at 8 (8.4-61.8 s).
     A cap becomes possible only together with (C1) or (B).
   - *Measured 2026-11-08 without the sync ((B), now the default): a cap
     buys little, and deadlocked the daemon.* Six interleaved runs per
     cap, medians, wall / daemon CPU / context switches: default 6.9 s /
     22.1 s / 790k; 64: 7.7 / 21.8 / 753k; 32: 8.4 / 21.3 / 719k; 16: 7.6
     / 20.6 / 630k; 12: 8.8 / 21.6 / 575k; 8: 8.7 / 21.7 / 503k, every
     wall range 5-15 s. On this 8-core machine the saving is 2-7% of CPU
     (25% on the 32-core tmpfs runs above). With a cap of 8, four
     concurrent pulls of the 31,531-file tree hung (1,063 files in 240 s,
     the daemon idle), three finished: every pool thread was a session's
     walk (`scan::walk`) or its listing loop (`control::spawn_listing`),
     each blocked on a full bounded channel for the whole walk, while the
     per-file reads that would let the peer drain them queued behind.
     With tokio's default limit of 512 that is 256 concurrent sessions, a
     number one client can open. Fixed by running both on threads of
     their own (`scan::spawn_thread`); `tests/blocking_pool.rs` pins it.
     With the fix and the cap of 8, the four pulls finished in 267-271 s
     and six in 640-645 s, against 193-204 s for six uncapped: cold reads
     gain from many in flight. The pool itself stays uncapped: that, and
     an export's `fsync` or a client's `--fsync`, which would make a
     capped pool ten times slower for that session.
7. **Reading a cold source from a spinning disk** (measured 2026-11-09).
   **Done.** A push from a
   source on the btrfs RAID1 pair of item 5, caches dropped before every
   run, to a daemon on tmpfs, so only the source's reads count; medians
   of three. Trees: item 1's 31,531 files of at most 16 KiB (321 MB),
   2,048 of 256 KiB, 512 of 1 MiB and 128 of 4 MiB (512 MiB each), and 16
   of 256 MiB. `tar cf - | cat` reads them in 12.8, 16.3, 8.4, 5.5 and
   32.7 s.
   - *A first push reads every file twice:* once to hash it (the client's
     index has no row) and once to send it, which then mostly hits the
     page cache. The hashing ran `HASH_WORKERS` = 4 files at a time, and
     that set the pace: 33.4 s for the small tree, against 24.3 s for the
     same push with the index filled. Hash workers, small tree: 4: 33.4 s;
     8: 30.1; 16: 26.1; 32: 21.3; 64: 19.1; 128: 15.0. Walk threads and
     `--parallel` made no difference while hashing was the limit.
   - *More is not better for large files.* 256 MiB files: 1 worker 35.8
     s; 2: 28.3; 4: 26.2; 8: 28.0; 16: 34.3; 128: 34.5. With a few
     readers btrfs serves both mirrors (irsync beats `tar`); with many
     long sequential reads at once the disks seek between them. 256 KiB:
     4: 9.3 s, 16: 7.4, 128: 7.0; 1 MiB: 5.5, 4.1, 4.1; 4 MiB: 4.3, 4.2,
     4.1: up to 1 MiB more helps, at 4 MiB the count no longer matters.
   - *Decision:* `HASH_WORKERS` = 128, and a file of at least
     `hasher::LARGE_FILE` (4 MiB) also takes one of
     `hasher::LARGE_WORKERS` (4) slots before its worker, so long reads
     stay four at a time while small files fill the rest. Release builds
     before and after, alternating, three runs each (index deleted unless
     said): small tree 33.8 s → 15.1 s, client CPU 45 → 27 s; with the
     index filled 22.8 → 23.2 s; 1 MiB 5.4 → 4.0 s; 4 MiB 4.3 → 4.4 s;
     256 MiB 27.1 → 24.8 s. The cost is memory: each worker holds a
     1 MiB chunking buffer and a chunk, so peak client RSS rose 60 → 214
     MB on the 1 MiB tree and 271 → 399 MB on the small one, and on
     tmpfs, where hashing is CPU-bound, 128 threads on 8 cores made the
     1 MiB tree 8% slower (2.32 → 2.51 s, five runs each; the small tree
     went 6.55 → 5.81 s). The daemon builds a hasher per session, so four
     sessions hashing at once fill tokio's 512-thread blocking pool;
     hash tasks never wait on each other, so other blocking work queues
     behind them instead of hanging as in item 6.
   - *Walk threads and `--parallel`.* With the index filled, so only the
     sends read (build before the hashing change): 4 walk threads and
     `--parallel` 32, the old defaults, 24.3 s; `--parallel` 128: 20.3,
     256: 20.4, 512: 21.5; 16 walk threads: 21.5; both: 15.9; 1 walk
     thread: 27.6; `--parallel` 1: 47.9. After the hashing change the same
     four combinations took 23.9 / 20.0 / 22.1 / 15.2 s with the index
     filled, and 15.3 / 15.5 / - / 14.9 s with it deleted (hashing then
     reads ahead of the sends).
   - *`--parallel` on a long link.* A stream holds its slot until the
     peer has acknowledged its last byte, so it bounds files per round
     trip, not only reads in flight. The small tree from tmpfs to tmpfs
     through the netsim profile `test-wan` (100 Mbit/s each way, 80 ms
     round trip, added for this): `--parallel` 32: 79.2 s (about 400
     files a second, 32 per round trip); 64: 40.9; 128: 28.2; 256: 28.7,
     against about 25 s for the bytes at the link's rate. Client RSS
     209 MB at 32, 218 at 128; client CPU 72 s at 32, 32 s at 128.
   - *What 128 streams cost.* In flight the connection is bounded by its
     64 MiB send and receive windows whatever the stream count, and the
     receiver accepts at most 256 streams. On the disk, 16 files of 256
     MiB (four 64 MiB ranges each) took 23.4 s at 32 and 25.0 s at 128
     (22.3 / 24.6 in an earlier set): 7-10% for files that large. Holding
     ranges of at least 4 MiB to four at a time, as the hasher does, made
     it worse (28.2 s at 32, 25.5 at 128; 128 x 4 MiB 4.2 -> 5.7 s) and
     changed nothing over `test-wan` (48.9 vs 48.6 s): a send reads one
     chunk at a time, paced by the network, so its reads do not thrash
     as the hasher's long ones do. No such limit.
   - *Walk threads alone: a no-change push.* 200,000 files of at most
     512 bytes in 20,000 directories, filled once, then pushed again
     with nothing to send, both trees on the HDD, caches dropped: 1
     thread 35.6 s; 4: 26.9; 8: 25.7; 16: 23.9; 32: 24.5. Both on tmpfs,
     warm, five runs: 3.6-3.85 s at every count from 1 to 32. `fileset`
     saw 8 threads slower than 4 on a warm APFS SSD (kernel metadata
     serialisation); that does not show here, on Linux.
   - *Decision:* `scan::WALK_THREADS` = 16 (the session walk and index
     warm-up, which had 4 each), `send::DEFAULT_PARALLEL` = 128 (a push's
     default and what a daemon sends a pull with). Each walk starts its
     own pool of that many threads, so a daemon walking for many sessions
     at once holds four times the threads it did.
   - *Not the cause: on-disk order.* The trees were written by `cp -a`
     in directory order, irsync reads in sorted order. A copy laid out
     in sorted order by rsync made `tar` faster (10.3 s) but not irsync
     (19-23 s against 16-26 s): the gap to `tar` is the number of reads
     in flight, not their order.

### 2.2 Parked after final review (real, judged not load-bearing)

- A hostile receiver can still make a sender read the export's largest
  files (the sender checks that a `Plan` names a listed path and that no
  more than `MAX_INFLIGHT` are in flight, not which ones). The digests it
  holds are bounded by the list room (2.1 (1) (n)), so this costs reads,
  not memory.
- A `PermissionDenied`/`restore_owner_rw` window can leave a placed,
  verified file with extra `u+rw` bits; fixed by the next `-p` run.
- Activity bumps in `digest_held`, the hasher, the delete pass and the
  daemon-side wiring have no dedicated tests.
- A file whose orphaned finalize succeeded during a reconnect is
  re-transferred in full after the 60 s wait (correct, costly).
- Two concurrent sessions on one destination pay up to 60 s per overlapping
  file before `PART_BUSY`.
- Rename after the dev+ino check is still by path (parent-directory swap
  window; local attacker only).
- `SessionKey` changed in the final fix wave, so sessions persisted by
  pre-release builds do not resume (acceptable pre-release).
- Missing tests: `Parallel` from config, `HelloAck` timeout, daemon
  watchdog, `--max-delete` across a reconnect (budget may reset after a lost
  `SessionDone`).

### 2.3 Deferred minors by area

Worked through on 2026-11-07. Fixed then, among others: deletions and GC's
`remove_part` remove relative to a parent opened with `O_NOFOLLOW` per
component (`unlinkat`), and a swapped parent is a delete error; the index
takes its write lock up front, refuses a root that does not resolve and
forgets the rows of deleted paths; `**` patterns match the full path and
pattern files are bytes (`FilterRule.pattern` is `bytes`); a missing root is
one error and an unnameable entry a gap; `gc_due` treats a last walk in the
future as due and walks again only for newly stale rows; `--bwlimit` follows
the rsync man page, `--timeout=0` is none, `--parallel 0` is refused; `id
show --daemon` no longer creates an identity; a dry run no longer counts
listed files as transferred; a host entry's `Address` with `--relay` is
refused; `host:+80` is rejected; the sender's burst is at most one second
of rate; golden wire bytes pin `Control` and `DataStreamHeader`. What is
left:

**Config**
- Two exports sharing one path share one SQLite file via two handles
  (works under WAL; intent undecided). Host aliases match exactly
  (case-sensitive).

**CLI**
- CLI paths are `String` (non-UTF-8 arguments rejected); consider `OsString`.
- A local source literally named `gc` or `daemon` parses as a subcommand.
- Push `-v -n` without `-i` lists nothing (daemon sends `Itemized` only when
  `Hello.itemize`); "Number of files" is approximate (hard-link
  classification); stats omit "File list generation/transfer time".

**Index**
- Setting `busy_timeout` before `journal_mode` has no test (the race is not
  reproducible).

**Scan / filter / hasher**
- jwalk read-ahead is unbounded (the bounded channel only throttles
  lstat+send; jwalk 0.9 has no knob). An unnameable entry's gap has no test
  (Linux cannot create one).

**Planner / receiver / sender**
- No parent-directory fsync after rename (part of 2.1 item 5).
- Stale `FileEntry` (mtime/mode) is applied after a changed-source resume
  (repaired on the next run); a retried file gets the original listing's
  mtime/mode. Fixing it needs the hashed file's stat in `ChunkListPart`,
  carried through the three digest paths, and a decision on which
  attributes it may override.
- The `HASH_MISMATCH` retry path is only lightly exercised;
  `parallel_bound_respected` depends on the receiver holding streams open.
- `Outcome` counts only the final connection; cumulative figures live in
  `SessionProgress`. Stored session errors are re-encoded every 2 s
  (persister rewrites the whole plan blob, O(N)).
- Single-file sources are refused in v1 (directories only).

**GC**
- `older_than` is one age for parts and sessions on each side (on purpose:
  a part never outlives a session that could resume it); a non-regular file
  at a part path leaves its row forever (conservative).

**Tests / platform**
- chown paths are untested (need root); macOS compiled only in CI.

### 2.4 Out of scope for v1 (by design)

- Transparent daemon spawn over SSH (mosh-style) — planned for a later
  version.
- Filesystem-specific extended attributes and APFS/ext4/ZFS-aware copying.
- rsync's full filter grammar beyond include/exclude and `*-from` files.
