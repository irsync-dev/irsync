# Source change detection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A source file that changes during a sync never ends the run with
exit 0 and a destination the source never held. irsync retries the file
once, then exits 23. With `--paranoid` it also sends reflink clones, hashes
again where it cannot clone, and re-checks every listed file at the end.

**Architecture:** The work comes in six steps.
1. The hasher returns the stat its digest was taken under and fails when
   the file moved while it was being read.
2. The receiver asks the sender before it places a verified file.
3. Every chunk list carries the hash stat's size and mtime.
4. `--paranoid` adds a flag, a second hash before placement, clones in a
   per-filesystem hidden folder, and an end-of-session re-check that
   re-plans changed files. Files that vanished by then end the run with
   exit 24.
5. A test checks what clones do with memory-mapped writes.
6. The docs close the loop.

**Tech Stack:** Rust (tokio, prost via `crates/irsync-proto/build.rs`, nix), irsync's `test-hooks` integration harness, mdBook.

**Spec:** `docs/superpowers/specs/2026-11-10-source-change-detection-design.md`.
Read it before any task. The bug record with its reproduction is the last
section of `TODO.md`.

## Global Constraints

- **Retries.** One retry per file in total, shared by every check. This is
  the existing `MAX_RETRIES = 1` in `crates/irsync/src/session/sender_role.rs:49`.
  A file that changes again is given up with an error naming it, the others
  carry on, and the run exits 23.
- **What counts as a change.** A difference in any of `Freshness { size,
  mtime, ctime, inode }` (`hasher.rs:108`) between two stats of the same
  file.
- **Error texts.** Use these strings, with the existing ` (after retry)`
  suffix when giving up:
  - `changed while it was hashed`
  - `changed after it was hashed`
  - `changed after it was sent`
  - `vanished during the sync`
- **Exit codes.**
  - 24 is new: "some source files vanished", only from the `--paranoid`
    final re-check.
  - Ranking in `combine_exit_codes` (`main.rs:274`): 1 > 25 > 23 > 24 > 0.
  - `Outcome::exit_code`: 25 if max-delete was reached, else 23 if there
    are errors, else 24 if files vanished, else 0.
- **Protocol.** No backward compatibility. Bump `PROTOCOL_VERSION`
  (`crates/irsync-proto/src/lib.rs:12`) from 2 to 3.
  - New `Control.msg` oneof tags: `MayPlace = 15`, `PlaceVerdict = 16`,
    `Replan = 17`, `Settle = 18`, `Settled = 19`.
  - `Hello.paranoid = 30`.
  - `ChunkListPart.mtime_sec = 8`, `ChunkListPart.mtime_nsec = 9`.
  - Every new control message needs an arm in `msg_name`
    (`session/control.rs:912`). New per-file frames must fit the `FILE_WIRE`
    budget in `session/limits.rs:75-105`.
- **Clone folders.**
  - Name: `.irsync-clones-<uid>`, mode 0700.
  - Each session works in its own subfolder `<session_id>/`, which holds a
    `lock` file flock'ed for the session's life.
  - The walk prunes any directory whose name starts with `.irsync-clones-`,
    with everything below it.
- **The sending side runs every check.** On a pull that is the daemon, so
  `paranoid` travels in `Hello` and is honoured when the daemon does not
  receive (`RoleOpts::daemon`, `engine.rs:170-215`, gated on `!receives`).
- **Before every commit:** `touch crates/irsync/build.rs`, then run the four
  commands in `book/src/contributing/setup.md#before-pushing` (tests with
  `--all-features`, fmt, both clippy runs). All must pass.
- **Commit messages:** a short summary line, a blank line, and a short
  explanation of why. No attribution trailers of any kind.
- **Clone tests** skip with a printed reason where the test directory
  cannot reflink, like `tests/inplace.rs:54-67` `reflink_harness`. On this
  machine `CARGO_TARGET_TMPDIR` is on ZFS 2.4 with block cloning (it
  reflinks), and `/tmp` is tmpfs (it does not).

## Review Focus

1. **A normal run with nothing changing must not slow down or stall.**
   "Ask before placing" adds a round trip per file. With the planner capped
   at one file in flight (`SideHooks.max_inflight = Some(1)`), a 200-file
   push must still finish with exit 0. Task 2 adds
   `ask_before_placing_with_one_file_in_flight`.
2. **An interrupted run while a file waits for its verdict.** If the
   connection drops between `MayPlace` and `PlaceVerdict`, the next run must
   resume and place the file correctly. Task 2 adds
   `drop_while_waiting_for_verdict_resumes` (use `FaultInjector`, or
   `drop_after_parts`).
3. **`--inplace` and `--append` with a change.** These write into the
   destination file itself, so the run must still retry and then exit 23,
   and must not report success. Task 2 adds
   `inplace_change_after_hash_retries_then_fails`.
4. **A clone folder that cannot be created** (filesystem cannot reflink, or
   no writable directory) must fall back quietly to the second hash, with
   no error and exit 0. Task 5 adds `paranoid_without_reflink_falls_back`
   (tree under `std::env::temp_dir()`; skip if that directory can reflink).
5. **Clone leftovers after a crash** must be removed by the next
   `--paranoid` run that uses the same folder. A live session's folder must
   never be removed. Task 5 adds `stale_clone_session_is_swept` and
   `live_clone_session_is_kept`.

---

### Task 1: The hasher returns its stat and catches torn hashes

**Files:**
- Modify: `crates/irsync/src/hasher.rs`: `digest`, `digest_walked`,
  `digest_as` (:212-312) and `Ticking` (:75-101).
- Modify: `crates/irsync/src/send.rs`: `lookahead_digest`,
  `lookahead_digest_walked` (:212-220).
- Modify: `crates/irsync/src/session/sender_role.rs`: `Done::Digested`
  (:54-69), `spawn_digest` (:292-386), `on_digested` (:573-609),
  `tail_digest` (:979), and `SFile` (:127-163).
- Modify: `crates/irsync/src/session/mod.rs`: `SideHooks` (:614-752) and
  `DaemonHooks` (:441-461). Wire the hook through `engine.rs` where the
  hashers are built (client :699, daemon :1083).
- Test: `crates/irsync/tests/change_detection.rs` (new) and the unit tests
  in `hasher.rs`.

**Interfaces:**
- Produces:
  - `pub struct Hashed { pub digest: FileDigest, pub stat: Freshness }` in
    `hasher.rs`.
  - `Hasher::digest(&self, item: &ScanItem) -> io::Result<Hashed>` and
    `Hasher::digest_walked(&self, path: RelPath, walked: Freshness) ->
    io::Result<Hashed>`. On an index cache hit, `stat` is the `Freshness`
    that matched.
  - `pub type HashHook = Arc<dyn Fn(&RelPath, u64) + Send + Sync>`. For
    each hash pass over a file it is called once with `0` *before the file
    is opened*. It is then called after each read with the total bytes read
    so far. A call with `0` therefore marks the start of a pass.
    `Hasher::with_hash_hook(self, HashHook) -> Hasher`.
  - `SideHooks.during_hash: Option<HashHook>` and
    `DaemonHooks.during_hash: Option<HashHook>`.
  - `SFile.hash_stat: Option<Freshness>`, set from `Hashed.stat` in
    `on_digested`.
  - Error on a torn hash: `io::Error` of kind `InvalidData`, message
    `"{path}: changed while it was hashed"`.

- [ ] **Step 1: Write the failing tests** in `tests/change_detection.rs`,
  using `common::Harness`, `archive_opts()` and `Flow::Push`. Source and
  destination hold the same 8 MiB pseudo-random `x.bin`. Set the
  destination's mtime to 1e9 s so the planner picks a delta, as in the
  `TODO.md` reproduction. A `during_hash` hook on the source side, when
  bytes ≥ 4 MiB for `x.bin`, writes 256 KiB of other data at offset 0 and
  at offset size−256 KiB (with `write_all_at`).
  - `torn_hash_is_retried`: the hook fires only on the first pass. Expect
    `out.exit_code() == 0`, `out.errors` empty, the destination byte-equal
    to the source, and two hash passes over `x.bin`. Count the hook's calls
    with `0`.
  - `torn_hash_twice_fails_with_23`: the hook fires on every pass. Expect
    `out.exit_code() == 23`, one error for `x.bin` containing
    `changed while it was hashed (after retry)`, and the destination
    unchanged from before the run.
  - The same pair for `Flow::Pull`, with the hook on `daemon.hooks`:
    `torn_hash_is_retried_on_pull` and `torn_hash_twice_fails_with_23_on_pull`.
  - Unit test in `hasher.rs`, `digest_reports_the_stat_it_hashed_under`:
    `digest_walked` on an unchanged file returns `Hashed.stat` equal to the
    file's `Freshness`. A second call (cache hit) returns the same stat.
- [ ] **Step 2: Run them and see them fail.**
  `cargo test -p irsync --all-features --test change_detection` fails:
  `torn_hash_twice_fails_with_23` ends with exit 0. The unit test does not
  compile until `Hashed` exists.
- [ ] **Step 3: Implement.**
  - In the `spawn_blocking` of `digest_as`, keep the `File` borrowed (hash
    through `Ticking::new(&file, ..)`). After `digest_reader`, call
    `fs::meta::fstat_entry_with_ctime(&file, &path)` and build a
    `Freshness`. If it differs from the one taken at open
    (`open_regular_nofollow_stat`), return the `InvalidData` error, and
    write no index row.
  - `Ticking` calls the hook after each read.
  - `tail_digest` does the same fstat compare.
  - In `on_digested`, an `Err` of kind `InvalidData` goes to
    `self.retry(file_id, "changed while it was hashed".into())`. Other
    errors keep going to `source_error`.
  - Update the callers in `send.rs` and the sender to `Hashed`.
- [ ] **Step 4: Run the tests.** The `change_detection` tests pass, and
  `cargo test -p irsync --all-features` is all green.
- [ ] **Step 5: Commit.**
  `git commit -m "Catch source files that change while they are hashed" -m "A write during the hash pass could give a digest of a state the file never had, and the torn result landed with exit 0. The hasher now checks the open file again after reading and the sender retries once, then fails with exit 23."`

### Task 2: Ask the sender before placing a file

**Files:**
- Modify: `proto/irsync/v1/control.proto`. Add the messages and the oneof
  arms. Bump `crates/irsync-proto/src/lib.rs:12` to 3. Update
  `crates/irsync-proto/tests/roundtrip.rs` and `golden_bytes.rs` as needed.
- Modify: `crates/irsync/src/session/control.rs`: `msg_name` (:912-927).
- Modify: `crates/irsync/src/recv.rs`. Split `FileWriter::place`
  (:1094-1200) into `verify` and `commit`.
- Modify: `crates/irsync/src/session/recv_io.rs`: `finalize` (:1091),
  `settle_identical` (:835).
- Modify: `crates/irsync/src/session/receiver.rs`: `St` (~:130),
  `spawn_finalize` (:587-609), `on_finalized` (:898-978), `on_source_error`
  (:980-989), `handle` (:991-1023), `shut_down` (:1028-1063).
- Modify: `crates/irsync/src/session/sender_role.rs`: the main loop arms
  (:1151-1211), `on_ack`'s late-`Done` withdrawal (:874-884).
- Modify: `crates/irsync/src/session/limits.rs`: the `FILE_WIRE` doc and
  value, if the new frames need it.
- Test: `crates/irsync/tests/change_detection.rs`.

**Interfaces:**
- Consumes: `SFile.hash_stat` (Task 1).
- Produces:
  - `message MayPlace { uint64 file_id = 1; }`, sent receiver → sender.
  - `message PlaceVerdict { uint64 file_id = 1; bool changed = 2; }`, sent
    sender → receiver.
  - `FileWriter::verify(self, opts) -> Result<Verified, FinalizeError>`.
    This does `set_len` for in-place writes, the optional fsync, and the
    whole-hash check.
  - `Verified::commit(self, entry: &FileEntry, opts) -> Result<Placed,
    FinalizeError>`. This applies metadata, runs `still_same_file`, renames,
    and does the fstat.
  - `Verified::discard(self)`. This drops the `.part` for a changed answer.
    Keep it for local-chunk reuse only if that is free; otherwise remove it.
  - Receiver state `St::Confirming`, between `Finalizing` and done.
  - Sender hook point, so Task 4 can add the second hash:
    `fn verdict(&mut self, file_id) -> Verdict`, where
    `enum Verdict { Place, Changed(String), Later }`. It returns `Later`
    when the answer needs async work.

- [ ] **Step 1: Write the failing tests.**
  - `change_after_hash_is_retried`: use `on_first_chunk_sent` (as in
    `sync_e2e.rs:259-302`) to rewrite only the *first* 256 KiB of `x.bin`.
    This is a range the destination copies locally, so the sender never
    re-reads it. Expect exit 0, no errors, and the destination equal to the
    final source.
  - `change_after_hash_twice_fails_with_23`: the hook rewrites on every
    send. Expect exit 23 and an error containing
    `changed after it was hashed (after retry)`. The destination keeps its
    old content, with no `.irsync.x.bin.part` left.
  - `ask_before_placing_with_one_file_in_flight` (Review Focus 1).
  - `drop_while_waiting_for_verdict_resumes` (Review Focus 2).
  - `inplace_change_after_hash_retries_then_fails` (Review Focus 3), with
    `--inplace` and a hook that changes the source on every send: exit 23.
  - Pull variants of the first two.
- [ ] **Step 2: Run them and see them fail.**
  `cargo test -p irsync --all-features --test change_detection change_after`
  fails: the run ends with exit 0, and the destination holds the old first
  range.
- [ ] **Step 3: Implement.**
  - **Receiver:** after `verify`, set `St::Confirming` and send `MayPlace`.
    On `PlaceVerdict{changed:false}`, `commit`, then the existing Ok path
    (`persist.done` and `FileAck Done`). On `changed:true`, `discard` and
    wait for the new chunk list, as after a hash mismatch, without counting
    it as a mismatch. `settle_identical` also asks before applying metadata.
  - **Sender:** on `MayPlace`, lstat the source path. Compare it with
    `hash_stat`, using `fs::meta::lstat_entry_with_ctime` (`meta.rs:50`).
    If equal, send `PlaceVerdict{changed:false}`. Otherwise send
    `changed:true` and call `retry(file_id, "changed after it was hashed")`.
    Once the retry limit is reached, `retry` calls `source_error`, and the
    receiver's existing `on_source_error` path fails the file.
  - Remove the late-`Done` withdrawal in `on_ack` and the `Finalizing`
    branch of `on_source_error`. A file the sender gave up on can no longer
    be placed. Update `shut_down`: files in `Confirming` are not done.
- [ ] **Step 4: Run the tests.** The new tests pass. The whole irsync suite
  passes, including `sync_e2e.rs file_changed_during_transfer_is_resent`
  and the resume tests.
- [ ] **Step 5: Commit.**
  `git commit -m "Ask the sender before placing a received file" -m "A change after hashing in chunks the destination copied locally was never seen. The receiver now verifies a file, asks whether the source is unchanged, and only then renames it into place; a changed file is retried instead of placed."`

### Task 3: Send the hash stat's size and mtime with every chunk list

**Files:**
- Modify: `proto/irsync/v1/control.proto` (`ChunkListPart` :60-68).
- Modify: `crates/irsync/src/session/parts.rs`: `split` (:27) and
  `assemble` (:65).
- Modify: `crates/irsync/src/session/sender_role.rs` (`on_digested`).
- Modify: `crates/irsync/src/session/receiver.rs`: `on_chunk_list`
  (:550-585). Update `RFile.entry` before finalize.
- Test: `crates/irsync/tests/change_detection.rs`.

**Interfaces:**
- Consumes: `Hashed.stat` / `SFile.hash_stat` (Task 1).
- Produces: `ChunkListPart.mtime_sec = 8` (int64) and `mtime_nsec = 9`
  (uint32), set on the part with `first == 0`. Together with `size`
  (field 2) they override the walk entry's size and mtime for that send.
  `assemble` returns them with the digest.

- [ ] **Step 1: Write the failing test** `retry_places_the_hashed_mtime`.
  Change `x.bin` after the walk but before hashing, so the hash sees the
  new state cleanly. Use `during_hash`'s first call with `0`, which comes
  before the open, to append 1 KiB and set a new mtime. Expect exit 0. The destination's size and mtime equal
  the source's final stat, not the walk's. A second run reports nothing to
  transfer.
- [ ] **Step 2: Run it and see it fail.** The destination carries the
  walk's mtime, and the second run transfers again.
- [ ] **Step 3: Implement.** Fill the fields from `hash_stat` in
  `on_digested`'s `split`. In `on_chunk_list`, overwrite
  `f.entry.size/mtime_sec/mtime_nsec` from the assembled values. Also
  correct `on_finalized`'s reported size (`receiver.rs:915`).
- [ ] **Step 4: Run the tests.** Everything passes.
- [ ] **Step 5: Commit.**
  `git commit -m "Place files with the size and mtime their hash was taken under" -m "The receiver applied the walk's metadata to content hashed later, so a retried file could carry a time that does not belong to its bytes."`

### Task 4: `--paranoid`: the flag and a second hash before placement

**Files:**
- Modify: `crates/irsync/src/cli/mod.rs`. Add `#[arg(long)] pub paranoid:
  bool` in `SyncArgs` next to `fsync` (:802), and `SyncOptions.paranoid`
  plus `from_args` (:889, :960).
- Modify: `proto/irsync/v1/common.proto`: `Hello.paranoid = 30`.
- Modify: `crates/irsync/src/session/engine.rs`: `RoleOpts` (:98, :131,
  :170-215), `set_hello_options` (:227-273), `check_options` (:289).
- Modify: `crates/irsync/src/session/mod.rs`: the session key (client
  `field(.., b"paranoid", ..)` near :109; daemon `SessionKey::for_daemon`
  :139-170).
- Modify: `crates/irsync/src/session/sender_role.rs`: the `verdict` path
  from Task 2.
- Test: `crates/irsync/tests/change_detection.rs`, and a CLI parse test
  where the existing flag tests live.

**Interfaces:**
- Consumes: `verdict` / `Verdict::Later` (Task 2), and `SFile.digest`.
- Produces: `RoleOpts.paranoid: bool`, true on the sending side when
  requested. On a pull it is `!receives && hello.paranoid`.

- [ ] **Step 1: Write the failing tests.**
  - `paranoid_second_hash_catches_a_change_the_stat_misses`. From user
    space, no write can leave the change time alone, so add a test-only
    `SideHooks.skip_place_stat: bool` (and the same field on `DaemonHooks`).
    It makes `verdict` skip Task 2's stat compare, which leaves only the
    second hash. Run with `--paranoid` on a tree under
    `std::env::temp_dir()`, so no clones are made. A hook rewrites bytes in
    a range the destination copies locally, on every send. Expect exit 23
    and an error containing `changed after it was sent (after retry)`.
  - `paranoid_reaches_the_daemon_on_pull`: same, with `Flow::Pull` and the
    hook on `daemon.hooks`.
  - CLI: `--paranoid` parses, and `SyncOptions.paranoid` is true.
- [ ] **Step 2: Run them and see them fail.** They fail to compile or parse
  `--paranoid`.
- [ ] **Step 3: Implement.** Plumb the flag. In `verdict`, when `paranoid`
  is on and the file was not cloned, hash the source file again
  (`Hasher::digest` with force, in a task) and compare the whole hash with
  `SFile.digest`. Answer `PlaceVerdict` when the hash is done. A mismatch
  calls `retry(file_id, "changed after it was sent")`. `--paranoid` is
  allowed with every other option.
- [ ] **Step 4: Run the tests.** All pass.
- [ ] **Step 5: Commit.**
  `git commit -m "Add --paranoid with a second hash before placing" -m "Writers that do not update a file's times, such as memory-mapped databases, are invisible to stat checks. --paranoid hashes each sent file again before it is placed."`

### Task 5: `--paranoid`: send reflink clones

**Files:**
- Create: `crates/irsync/src/fs/clone.rs` (module `fs::clone`).
- Modify: `crates/irsync/src/fs/mod.rs`. Declare the module, and move
  `reflink` from `recv.rs:65-83` here, re-exported from `recv`, if both
  places use it.
- Modify: `crates/irsync/src/scan.rs`: `run_walk` `children.retain_mut`
  (~:252). Prune `.irsync-clones-` subtrees.
- Modify: `crates/irsync/src/session/sender_role.rs`: `spawn_digest`,
  `SFile`, `remove_file`, `verdict` (cloned files answer `Place`
  immediately), and session end.
- Modify: `crates/irsync/src/send.rs`. Send reads chunks from the clone's
  file when the `SFile` has one.
- Test: `crates/irsync/tests/paranoid_clone.rs` (new), and unit tests in
  `fs/clone.rs`.

**Interfaces:**
- Consumes: `RoleOpts.paranoid` (Task 4), the `ChunkListPart` mtime fields
  (Task 3), `Freshness`, and `Hashed`.
- Produces:
  - `pub const CLONE_DIR_PREFIX: &str = ".irsync-clones-";`
  - `pub struct Clones` with `Clones::new(session_id: &str) -> Clones`.
    It caches one area per `st_dev`, `None` when an area could not be made.
  - `Clones::clone_file(&self, full: &Path) -> io::Result<Option<Cloned>>`.
    `Ok(None)` means fall back.
  - `pub struct Cloned { pub file: File, pub path: PathBuf, pub stat:
    Freshness }`. `stat` is the source's stat at the moment of cloning.
    `Drop` unlinks `path`.
  - `fn area_dir(file: &Path, dev: u64) -> Option<PathBuf>`. It walks the
    file's ancestors that have `st_dev == dev`, from the topmost (the
    mount point) down, and returns the first one where
    `access(W_OK|X_OK)` succeeds.
  - `fn sweep_stale(clone_root: &Path) -> io::Result<usize>`. It removes
    every `<session>/` subfolder whose `lock` it can flock exclusively
    without blocking.
  - `Hasher::digest_file(&self, file: &File, path: &RelPath) ->
    io::Result<FileDigest>`, a hash with no index.
  - `SFile.cloned: Option<Arc<Cloned>>`.
- Algorithm for `clone_file`, where the code cannot settle it:
  1. Open the source without following symlinks and fstat it (s1).
  2. Create the destination with `O_EXCL` in the area.
  3. Clone. On Linux use FICLONE on the fds. On macOS use
     `clonefile(src_path, dst_path)`, then open the destination.
  4. fstat the source again (s2).
  5. If s1 ≠ s2, unlink and try once more. On a second mismatch return
     `Ok(None)`.
  6. `Unsupported`, `EXDEV`, `EOPNOTSUPP` and a missing area all return
     `Ok(None)`.

  The first `Clones` use of an area runs `sweep_stale` on its
  `.irsync-clones-<uid>` folder, then creates `<session_id>/` and its
  locked `lock` file. Dropping `Clones` at session end removes the
  session's subfolders.

- [ ] **Step 1: Write the failing tests.** Use the reflink skip pattern
  from Global Constraints.
  - `paranoid_sends_a_clone_even_if_the_source_changes`: under
    `--paranoid`, a `during_hash` hook rewrites the source on every pass.
    Expect exit 0, no errors, and the destination equal to the source's
    content at clone time (save a copy in the hook before the first
    rewrite). An info log, or a `SideHooks.events` entry, says the source
    changed after cloning.
  - `paranoid_places_the_clone_time_stat`: the destination's mtime equals
    the source's at clone time.
  - `clones_are_removed_after_the_run`, on success and on a failed file.
    No `.irsync-clones-<uid>/<session>` subfolder is left.
  - `stale_clone_session_is_swept` and `live_clone_session_is_kept`
    (Review Focus 5). These are unit tests in `fs/clone.rs`, with a
    subfolder whose lock is free and one whose lock is held.
  - `paranoid_without_reflink_falls_back` (Review Focus 4).
  - `walk_skips_clone_folders`: a `.irsync-clones-1000/x/y` inside the
    source tree is neither listed nor deleted at the destination.
  - Pull variant: `paranoid_sends_a_clone_on_pull`.
- [ ] **Step 2: Run them and see them fail.** `cargo test -p irsync
  --all-features --test paranoid_clone` fails: the source change is
  detected and the run exits 23.
- [ ] **Step 3: Implement** `fs::clone` and the walk prune. In
  `spawn_digest`, when `paranoid` is on, call `clone_file` first. On
  `Some`, hash the clone with `digest_file`, keep `Cloned` in the `SFile`,
  send its `stat` as the hash stat, and read chunks from `cloned.file`.
  Drop it in `remove_file`. When the source's stat differs from
  `cloned.stat` at `verdict` time, log at info level
  `"{path}: source changed after it was cloned; sent the clone"`.
- [ ] **Step 4: Run the tests.** All pass, and the whole suite is green.
- [ ] **Step 5: Commit.**
  `git commit -m "Send reflink clones under --paranoid" -m "A clone is frozen at one moment the source really held, without root and without disturbing the writer. Clones live in a hidden per-user folder as high up the file's filesystem as irsync can write, and are swept after a crash."`

### Task 6: `--paranoid`: re-check every listed file at the end

**Files:**
- Modify: `proto/irsync/v1/control.proto`. Add `Replan`, `Settle` and
  `Settled`, and their `msg_name` arms.
- Modify: `crates/irsync/src/session/sender_role.rs`. Keep a stat per
  listed file, run the final re-check, and accept re-planned paths in
  `on_plan` (:462-551).
- Modify: `crates/irsync/src/session/receiver.rs` and
  `session/planner.rs`. Send `Settle`, plan `Replan`s with fresh file ids,
  and fit them to `MAX_LIST_STARTS`.
- Modify: `crates/irsync/src/session/mod.rs`. Add `Outcome.vanished:
  Vec<RelPath>` and change `exit_code` (:1002).
- Modify: `crates/irsync/src/main.rs`. Change `combine_exit_codes` (:274)
  and its test (:1834).
- Test: `crates/irsync/tests/change_detection.rs`.

**Interfaces:**
- Consumes: `RoleOpts.paranoid`, `Hashed.stat`, and `Clones` (Task 5).
- Produces:
  - `message Replan { bytes path = 1; FileEntry entry = 2; }`, sent
    sender → receiver.
  - `message Settle {}`, sent receiver → sender.
  - `message Settled { repeated bytes vanished = 1; }`, sent
    sender → receiver after its `Replan`s.
  - `SenderRole.final_stats: HashMap<RelPath, Freshness>`. Every listed
    regular file's walk stat goes in, and each send's hash stat overwrites
    it.
  - `Outcome.vanished`.
- Protocol, when `paranoid` is on:
  1. The receiver has finished its plan and has nothing in flight. Instead
     of `SessionDone`, it sends `Settle` once.
  2. The sender lstats every path in `final_stats`.
     - Changed regular file: send `Replan` with a fresh `FileEntry`.
     - Gone, or not a regular file: add it to `vanished`.
     - Unreadable: add it to `errors`.
  3. The sender sends `Settled{vanished}`.
  4. The receiver plans each `Replan` through the normal planner decision.
     It gives each new item a new file id, above `Prior.max_id`, and
     persists it with `persist.plan`. Done files are never un-done.
  5. The receiver finishes, then sends `SessionDone`.

  The re-sent files go through all of Tasks 1-5's checks, so a second
  change ends in exit 23 (or is accepted from a clone).

  On a push the client is the sender and fills `vanished` itself. On a
  pull, the client fills it from `Settled`.

  A resumed run (`Listing::FromPlan`) has no walk stats. It re-checks only
  files sent in this run (Claude's call). The docs in Task 7 say so.

- [ ] **Step 1: Write the failing tests.**
  - `paranoid_recheck_resends_a_file_changed_after_the_walk`: `y.txt` is
    unchanged at the start (identical on both sides). A hook (for example
    `on_first_chunk_sent` for another file, `x.bin`) rewrites `y.txt`
    during the run. Expect exit 0, and the destination's `y.txt` equal to
    the final source.
  - `paranoid_recheck_vanished_file_exits_24`: the hook deletes `y.txt`.
    Expect exit 24, `out.vanished == ["y.txt"]`, no errors, and the
    destination keeps `y.txt`.
  - `vanished_and_failed_exits_23`: 23 outranks 24.
  - Without `--paranoid`, the first case ends with exit 0 and the old
    `y.txt` (no re-check). This pins that the check is opt-in.
  - The `combine_exit_codes` unit test gets 24 in its ranking.
  - Pull variant of the vanished case.
- [ ] **Step 2: Run them and see them fail.**
- [ ] **Step 3: Implement** to the protocol above. On the sender, re-planned
  paths are admitted in `on_plan` through a `replanned: HashSet<RelPath>`
  that `Replan` fills. The existing `listed` check stays for first plans.
- [ ] **Step 4: Run the tests.** All pass, and the whole suite is green.
- [ ] **Step 5: Commit.**
  `git commit -m "Re-check every listed file at the end of a --paranoid run" -m "Files that looked unchanged at the walk could change before the run ended. --paranoid stats them all again, re-sends the changed ones, and ends with exit 24 when some vanished, as rsync does."`

### Task 7: Clones and memory-mapped writes; docs; campaign row

**Files:**
- Modify: `crates/irsync/src/fs/clone.rs` (a unit test). Enable nix's
  `mman` feature in `crates/irsync/Cargo.toml` dev-dependencies if needed.
- Modify: `book/src/explanation/correctness.md`, "Files that change during
  a sync" (:131).
- Modify: `book/src/explanation/transfer-algorithm.md`, the "Change during
  transfer" row (:145) and the Integrity bullet (:180).
- Modify: `book/src/reference/command-line.md`. Add a `--paranoid` row,
  and exit code 24 in the table (:236-244).
- Modify: `TODO.md`. Close the torn-file entry (:133) with a pointer to the
  spec and these commits.
- Modify: `crates/stress/tools/matrix.sh`. Add a `paranoid` row passing
  `-- --paranoid`, and list it in `book/src/contributing/stress.md` where
  the rows are named.

- [ ] **Step 1: Write the test**
  `clone_includes_unflushed_mmap_writes`. `mmap` a file shared and
  writable, write bytes through the mapping without `msync`, clone it with
  `Clones::clone_file`, and read the clone. Assert that the clone holds the
  new bytes. Skip with a printed reason where reflink is unsupported.
- [ ] **Step 2: Run it on this machine (ZFS)** and record the result.
  - If it passes, the docs may say that clones include memory-mapped
    writes on filesystems where this test passed.
  - If it fails, keep the test as `#[ignore]` with a comment stating the
    observed result, and the docs say clones may miss memory-mapped writes
    that have not reached the disk.
- [ ] **Step 3: Update the docs.**
  - The torn case is caught (one retry, then exit 23).
  - "Ask before placing" covers changes after hashing.
  - `--paranoid`: clones on btrfs, xfs, ZFS 2.2+, bcachefs and APFS; the
    second hash elsewhere; the final re-check; exit 24.
  - `--inplace` and `--append` already hold written data when a change is
    found.
  - The limits from the spec's "Known limits", including resumed
    `--paranoid` runs re-checking only files sent in that run, and the
    mmap result from Step 2.
  - `--paranoid` costs, in the command-line row.
  - Run `cargo build -q -p irsync && site/build.sh && python3
    site/tools/check.py --irsync target/debug/irsync target/site`; it must
    exit 0.
- [ ] **Step 4: Check the campaign row.** `DRY=1
  crates/stress/tools/matrix.sh paranoid` passes.
- [ ] **Step 5: Commit.**
  `git commit -m "Document source change detection and --paranoid" -m "The correctness page said a file written during hashing could land torn; it is now caught. The docs describe the retry, --paranoid, exit 24 and the remaining limits."`
