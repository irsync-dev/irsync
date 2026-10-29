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
  reconnect attempt; `Parallel` from the client config is applied; `-r` is
  implied (irsync always recurses; `--no-r` is refused).
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
- **R8 — `finalize` checks the destination kind before applying metadata**,
  and `FileWriter::open` restores `u+w` on irsync's own `.part` on
  `EACCES`, so a read-only temp file can never be stranded.
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
  mapping in v1.
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
  reconnect's resume or by the next run.
- **R34 (process) — One narrowly scoped follow-up fix was dispatched after
  the single final fix wave**, deviating from the "one fix wave" rule, because
  the fix wave itself introduced the idle-timeout regression and it hit the
  core use case.

---

## 2. Open Points

### 2.1 Tracked follow-ups (should land before relying on irsync at scale)

1. **Bounded channels / backpressure (R31).** Control and listing channels
   are unbounded; `--delete-before` buffers the whole listing. An authorised
   peer with a huge tree can grow memory until the follow-up lands. Needs a
   design that avoids the reader/writer deadlock cycle.
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

### 2.2 Parked after final review (real, judged not load-bearing)

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
- `gc_due` does not handle the clock stepping backwards; the client's 1 h
  stale-row threshold may cause more-than-daily walks until GC runs.
- Missing tests: `Parallel` from config, `HelloAck` timeout, daemon
  watchdog, `--max-delete` across a reconnect (budget may reset after a lost
  `SessionDone`), `irsync gc` end-to-end.

### 2.3 Deferred minors by area

**Build / proto**
- `build.rs` uses a cwd-relative `../../proto`; prefer `CARGO_MANIFEST_DIR`.
- No standalone assertion that `PROTOCOL_VERSION == 1`; no golden-bytes/tag
  assertions for `Control` and `DataStreamHeader` (roundtrip tests cannot
  catch field-number drift).
- Chunker golden vector lacks a forced-max-cut (low-entropy) case and a
  sub-minimum tail case; no tests for `ChunkRecord` conversion error paths.

**Identity / authz / config**
- Several `exports=` options on one line: last wins (OpenSSH: first);
  `Exports=` is case-sensitive; escaped quotes / whitespace inside quoted
  values are unsupported (fails closed). No test captures the warning for
  skipped non-Ed25519 lines.
- The v1 config-path environment variable and default-path branches of
  `ClientConfig::load` were untested; host matching was
  case-sensitive (ssh is insensitive); quoted values with spaces unsupported.
- `irsync id show --daemon` creates a new identity as a side effect if none
  exists. Two exports sharing one path share one SQLite file via two handles
  (works under WAL; intent undecided).

**CLI**
- `--bwlimit` decimals / `b` suffix / 1000-vs-1024 fidelity vs the rsync man
  page; `--timeout=0` should mean "none"; `--parallel 0` accepted.
- CLI paths are `String` (non-UTF-8 arguments rejected); consider `OsString`.
- A local source literally named `gc` or `daemon` parses as a subcommand.
- Push `-v -n` without `-i` lists nothing (daemon sends `Itemized` only when
  `Hello.itemize`); dry-run `FileDone(path, 0)` inflates `xfr#`/`to-chk`;
  "Number of files" is approximate (hard-link classification); stats omit
  "File list generation/transfer time".

**Index**
- `busy_timeout` is set after the `journal_mode` pragma; `save_session` uses
  a deferred transaction (read→write upgrade can hit `SQLITE_BUSY` under
  multi-process contention; use `Immediate`). No test for
  `SchemaVersionChanged`. Canonicalize failure silently hashes the
  non-canonical path. Index rows for deleted paths are not purged.

**Scan / filter / hasher**
- Entries whose `RelPath::join` fails are dropped silently rather than
  recorded as errors; a missing root yields two errors.
- `**` in a pattern without a slash matches the final component only (rsync:
  full path); pattern files are read as lossy UTF-8, so non-UTF-8 filenames
  in `--exclude-from` never match.
- `changed_content_same_mtime_rehashed` relies on ctime differing between two
  writes microseconds apart (could flake on coarse-ctime kernels).
- jwalk read-ahead is unbounded (the bounded channel only throttles
  lstat+send).

**Planner / receiver / sender**
- Duplicate-listing tests cover only duplicate-after-Equal; out-of-order test
  asserts only non-empty; `Special` mode mask untested.
- No test forces a failure after `apply_metadata` (the `u+w` restore path);
  no parent-directory fsync after rename; whole-file re-hash on adoption
  reopens by path rather than the held fd.
- Stale `FileEntry` (mtime/mode) is applied after a changed-source resume
  (repaired on the next run); a retried file gets the original listing's
  mtime/mode. Rename detection and `find_chunk` reuse under a non-empty
  export prefix lack an end-to-end test. `Decision::LocalCopy` and the
  `HASH_MISMATCH` retry path are only lightly exercised.
- A hard link can point at stale target content when the target failed only
  at planning time (only the receiver's `failed` set is consulted). Hard
  links are not counted in `files_transferred`. `may_chgrp` calls
  `getgroups` per file.
- Sender: token bucket can burst up to 1 MiB after idle at low limits;
  `parallel_bound_respected` depends on the receiver holding streams open.
- `Outcome` counts only the final connection; cumulative figures live in
  `SessionProgress`. Stored session errors are re-encoded every 2 s
  (persister rewrites the whole plan blob, O(N)).
- Single-file sources are refused in v1 (directories only).

**Deletions**
- Path-based removal has a TOCTOU window (parents checked, then remove);
  harden with `openat(O_NOFOLLOW|O_DIRECTORY)` + `unlinkat`. Skipped
  deletions under a swapped parent only warn. `ListingEnd.count` is not
  checked against entries received.

**GC**
- No test for a stale row with a fresh file (mtime re-check path);
  `older_than` conflates the client 7-day constant and the daemon
  `dormant_session_timeout`; a non-regular file at a part path leaves its
  row forever (conservative); a directory-component symlink swapped in after
  a row was written could redirect `remove_part`.

**Tests / platform**
- `try_create_non_utf8` skip branch is not platform-gated (a spurious
  `EINVAL` on Linux would skip silently); the rsync comparison header's "Not
  compared" list omits directory nlink; the CI-set panic path is untested.
- chown paths are untested (need root); macOS compiled only in CI.

**Isolated network mode** (design
`docs/superpowers/specs/2026-10-21-isolated-network-design.md`, final review
2026-10-21)
- `Address` in the client config together with `--relay` on the command
  line ignores `--relay` with only a debug log. The spec is silent on this
  pair; a warning, or refusing it like `--address` with `--relay`, would be
  friendlier.
- The end-to-end test's daemon spawn inherits the developer's environment
  (`IRSYNC_PASSPHRASE`, `RUST_LOG`), unlike its client run; the client run
  has no wall-clock bound beyond `--reconnect 0` and iroh's connect timeout.
- `HostPort` accepts a leading `+` in the port (`host:+80` parses and
  displays as `host:80`), because `u16::from_str` does.
- README: the `--address` row omits the `[ipv6]:port` form; the daemon
  example's `listen = ["0.0.0.0:0", "[::]:0"]` is captioned "pin sockets"
  but, with port 0 and the default addresses, pins nothing.

### 2.4 Out of scope for v1 (by design)

- Transparent daemon spawn over SSH (mosh-style) — planned for a later
  version.
- Filesystem-specific extended attributes and APFS/ext4/ZFS-aware copying.
- uid/gid name mapping (`--numeric-ids` is always on).
- rsync's full filter grammar beyond include/exclude and `*-from` files.
