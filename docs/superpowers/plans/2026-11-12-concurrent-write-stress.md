# Concurrent-write stress mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `--concurrent` mode for `irsync-stress` and `irsync-netstress` in which filegen writes to the source while irsync syncs it, and every destination entry is checked (content and metadata) against the states the source really held.

**Architecture:** filegen gains a pace option and logs each touched path's stat after every step. The harness starts filegen in the background, syncs, and stops filegen between two steps. It then replays the round with filegen's own `Runner` on a reflink copy of the round-start tree, keeping each step's old file so it can judge in-between states. irsync gains a machine-readable report of retried and given-up files (env `IRSYNC_FILE_REPORT`). A quiet sync then requires an exact match, as today. netstress reuses the round function with its fault-layer syncs.

**Tech Stack:** Rust (workspace crates `filegen`, `stress`, `netstress`, `irsync`), `blake3`, `rand_chacha`, `serde_json`, bash campaign scripts.

**Spec:** `docs/superpowers/specs/2026-11-12-concurrent-write-stress-design.md`

## Global Constraints

- Additive only: existing stress/netstress modes, sweeps, matrix rows and tests keep their exact behaviour (including 23 = failed iteration, 24 = harness error outside `--concurrent`).
- "No destination file is ever a state the source never held": the faithful rule. In-between states of the running step count as held. When in doubt, accept what the operation really does: a false alarm is an anti-goal.
- Bytes from two different steps never mix in an accepted file.
- In the new mode, exit 0 and 24 are fine. Exit 23 is fine only if every path irsync gave up on was touched by filegen during that sync. Any other exit, a crash or a hang fails the round. The quiet sync must exit 0.
- The seed fixes every random choice. Each round's pace and real step count are logged, and a rerun replays them.
- The irsync report only adds output; nothing else in irsync's behaviour changes.
- Run the full regression before pushing (AGENTS.md): `touch crates/irsync/build.rs`, `cargo test -q --workspace --all-features --no-fail-fast -- --include-ignored`, `cargo fmt --all --check`, both clippy runs with `-D warnings`.
- Mark any new test that syncs through the fault layer `#[ignore = "slow: netsim; run with --include-ignored"]`.
- Commit messages: short summary line, blank line, brief why. No attribution trailers of any kind.

## Review Focus

1. **Replay diverging from the live run** (inode numbers differ, a `.irsync-clones-<uid>` folder or `.irsync-stress-ping` inside the tree, xattrs not copied). Expected: a harness error (exit 2) that names the first step whose op, path, to or bytes differ, never a false "torn file" failure. Pinned in Task 7 (`replay_detects_divergence`).
2. **Hard links**: a write to one link changes every path of its group. Expected: every path in the group gets the new state and the in-flight window, not only the logged path. Pinned in Task 7 (`replay_tracks_hard_link_groups`).
3. **A directory renamed or removed mid-round**: every path below it changes. Expected: descendants get "absent" states at the old place and moved states at the new place. Pinned in Task 7 (`replay_tracks_renamed_directories`).
4. **Non-UTF-8 and awkward file names** (filegen's `names.classes.awkward`) in the report, the log and the judge. Expected: matched byte for byte. Pinned in Task 3 (`report_paths_are_bytes`) and Task 2 (`step_line_round_trips_awkward_names`).
5. **filegen still in the middle of a step when the sync ends**: SIGTERM only stops it between steps. Expected: the harness waits for filegen to exit (bounded at 60 s, then a harness error) before replaying. Pinned in Task 8 (`stop_waits_for_the_step_to_finish`).

---

### Task 1: filegen `--pace`

**Files:**
- Create: `crates/filegen/src/pace.rs`
- Modify: `crates/filegen/src/lib.rs`, `crates/filegen/src/cli.rs` (RunArgs), `crates/filegen/src/run.rs` (RunOpts, `run` loop)
- Test: unit tests in `pace.rs` and `run.rs`

**Interfaces:**
- Produces: `filegen::pace::Pace` with `Fast`, `Steady { per_sec: u32 }`, `Bursts { on_ms: u32, off_ms: u32 }`; `impl FromStr` and `impl Display`. The text forms are `fast`, `steady:<N>` and `bursts:<ON_MS>:<OFF_MS>`, and Display is the exact inverse. `RunArgs.pace: Pace` (`--pace`, default `fast`). `RunOpts.pace: Pace`.

- [ ] **Step 1: Write failing tests** in `pace.rs`:
  - `pace_parses_and_prints`: `"fast"`, `"steady:50"` and `"bursts:200:800"` round-trip through `parse` and `to_string`. `"steady:0"`, `"bursts:0:10"`, `"slow"` and `"steady:x"` are errors.

  In `run.rs` tests:
  - `steady_pace_spaces_steps`: 10 steps at `steady:100` take at least 90 ms.
  - `pace_does_not_change_the_tree`: the same seed with `fast` and with `steady:1000` gives the same step lines (op, path, to, bytes).
  - `pace_sleep_ends_on_signal`: a run at `steady:1` with 100 steps, sent SIGTERM after 200 ms, returns `Outcome::Interrupted` within 500 ms.
- [ ] **Step 2: Run `cargo test -p filegen pace`.** Expected: compile failure (no `pace` module).
- [ ] **Step 3: Implement.**
  - Before every step except the first, `run` waits as the pace demands, sleeping in slices of at most 50 ms and checking the signal flag between slices.
  - Steady: step n starts no earlier than `start + n / per_sec`.
  - Bursts: steps run freely during each `on_ms` window and wait out each `off_ms` window, timed from the run's start.
  - Pace never touches the rng.
- [ ] **Step 4: Run `cargo test -p filegen`.** Expected: all pass.
- [ ] **Step 5: Commit** "Add a --pace option to filegen".

### Task 2: filegen logs post-step stats; parse step lines; opts from args

**Files:**
- Modify: `crates/filegen/src/log.rs` (step line and parser), `crates/filegen/src/run.rs` (`commit` stats the touched paths; new `opts_from_args`), `crates/filegen/src/main.rs` (uses `opts_from_args`), `crates/filegen/src/escape.rs` (only if `unescape` cannot invert `quoted`)
- Test: unit tests in `log.rs` and `run.rs`

**Interfaces:**
- Produces:
  - `filegen::log::PostStat { kind: char /* f d l p c b s */, size: u64, mode: u32 /* permission bits */, mtime_ns: i128 }`.
  - `filegen::log::StepLine { step: u64, op: OpKind, path: Vec<u8>, to: Option<Vec<u8>>, bytes: u64, details: BTreeMap<String, String>, post: Option<Option<PostStat>>, post_to: Option<Option<PostStat>> }`. The outer `None` means not logged (dry run); `Some(None)` means the path was absent after the step.
  - `filegen::log::parse_step_line(line: &str) -> Option<StepLine>` returns `None` for `#` lines and failure lines.
  - `filegen::run::opts_from_args(args: RunArgs) -> Result<RunOpts, String>`, the conversion `main.rs` does today.

- [ ] **Step 1: Write failing tests:**
  - `step_line_round_trips_awkward_names`: a record whose path holds bytes `b"a\tb\xff\n"` and whose `to` is `b"c d"`, written by `Log::step` and read back by `parse_step_line`, gives the same path, to, bytes and details.
  - `step_lines_carry_post_stats`: a 20-step run on a temp tree. For every line, `post` matches `lstat` of `path` taken right after the step, for kind, size, mode and mtime_ns. A file deleted by `delete_file` logs `post=-`.
  - `opts_from_args_matches_main`: parsing `["filegen","run","--seed","7","--steps","3","--pace","steady:5","/t"]` gives `steps == 3`, `Start::Seed(7)` and `pace == Steady{per_sec:5}`.
- [ ] **Step 2: Run `cargo test -p filegen log run`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
  - After each non-dry step, `Runner::commit` lstats `root/path` and, if present, `root/to`.
  - It appends ` post=<stat>` and ` post_to=<stat>`, where `<stat>` is `kind:size:mode_octal:mtime_ns`, or `-` when the path is absent.
  - `main.rs` calls `opts_from_args`.
- [ ] **Step 4: Run `cargo test -p filegen`.** Expected: all pass.
- [ ] **Step 5: Commit** "Log each touched path's stat after every filegen step".

### Task 3: irsync file report (`IRSYNC_FILE_REPORT`)

**Files:**
- Create: `crates/irsync/src/file_report.rs`
- Modify: `crates/irsync/src/lib.rs`; `crates/irsync/src/session/sender_role.rs` (`retry`, at the "re-sending file" log line); `crates/irsync/src/main.rs` (after the client's sessions end, near the `combine_exit_codes` call at :239)
- Test: `crates/irsync/tests/file_report.rs`

**Interfaces:**
- Produces:
  - `irsync::file_report::ENV: &str = "IRSYNC_FILE_REPORT"`.
  - `enum ReportEvent { Retried, GaveUp, Vanished }`, written as `"retried"`, `"gave_up"` and `"vanished"`.
  - `fn record(event: ReportEvent, path: &RelPath, reason: &str)`, a no-op when the variable is unset.
  - `fn record_outcome(o: &Outcome)`, which writes one `gave_up` line per `o.errors` entry and one `vanished` line per `o.vanished` entry.
  - Line format: one JSON object per line, `{"event":..,"path_hex":..,"path":..(lossy),"reason":..}`, appended through one `write_all` per line to a file opened with `O_APPEND`.
  - The sending side writes `retried` lines: the client on a push, the daemon on a pull. The client writes `gave_up` and `vanished` lines from its final `Outcome`, in both directions.

- [ ] **Step 1: Write failing tests:**
  - `report_lists_retry_and_give_up_push`: a push with the existing torn-hash hook firing twice (see `crates/irsync/tests/change_detection.rs`, `torn_hash_twice_fails_for`), with the report path set for the client. The report has exactly one `retried` line and one `gave_up` line for the file, and the reason contains "changed while it was hashed".
  - `report_lists_give_up_pull`: the same hook on a pull. The client's report has the `gave_up` line; the daemon's has the `retried` line.
  - `report_paths_are_bytes`: `record` on a path with bytes `\xff\n` writes a `path_hex` that decodes to those bytes.
  - `report_off_writes_nothing`: with the variable unset, no file appears.

  If the in-process test harness cannot set the variable per side, give `record` an injectable sink under `test-hooks` (`SideHooks.file_report: Option<PathBuf>`); the env var still drives the binary.
- [ ] **Step 2: Run `cargo test -p irsync --all-features --test file_report`.** Expected: FAIL.
- [ ] **Step 3: Implement** `file_report.rs` and the two call sites. Open the file once (`OnceLock`) and guard writes with a `Mutex`.
- [ ] **Step 4: Run `cargo test -p irsync --all-features`.** Expected: all pass.
- [ ] **Step 5: Commit** "Add a machine-readable report of retried and given-up files".

### Task 4: test-hooks switch that turns off the change checks

**Files:**
- Modify: `crates/irsync/src/session/mod.rs` (`SideHooks.no_change_checks: bool` under `test-hooks`); `crates/irsync/src/hasher.rs` (the post-read stat check near :183); `crates/irsync/src/session/sender_role.rs` (the MayPlace verdict near :1385-1445; clone creation; the final re-check); `crates/irsync/src/main.rs` (under `test-hooks`, set the flag from env `IRSYNC_TEST_NO_CHANGE_CHECKS=1` for client and daemon)
- Test: `crates/irsync/tests/change_detection.rs`

**Interfaces:**
- Produces: `IRSYNC_TEST_NO_CHANGE_CHECKS=1`, which in a `--features test-hooks` build turns off all of these:
  - the stat after hashing;
  - the ask before placing (always `Verdict::Place`);
  - `--paranoid` cloning (treated as unsupported);
  - the second hash;
  - the final re-check.

  It does nothing in a normal build.

- [ ] **Step 1: Write failing tests** in `change_detection.rs`:
  - `torn_hash_lands_torn_with_checks_off`: the existing `run_torn` setup with `no_change_checks = true`. The sync exits 0, and the destination equals neither the old nor the new source content. This is the original bug.
  - `paranoid_checks_off_skips_clone_and_recheck`: `--paranoid` with the switch on. No `.irsync-clones-*` folder is created, and a file deleted after the walk does not give exit 24.
- [ ] **Step 2: Run `cargo test -p irsync --all-features --test change_detection`.** Expected: FAIL.
- [ ] **Step 3: Implement** the flag at each check site.
- [ ] **Step 4: Run `cargo test -p irsync --all-features`.** Expected: all pass, including the existing change-detection tests (switch off).
- [ ] **Step 5: Commit** "Add a test switch that turns off the change checks".

### Task 5: stress — tree snapshot and copy

**Files:**
- Create: `crates/stress/src/snapshot.rs`
- Modify: `crates/stress/src/lib.rs`, `crates/stress/Cargo.toml` (add `filegen = { path = "../filegen" }`, `rand_chacha`, `serde_json`, and `xattr` if not already a workspace dependency)
- Test: unit tests in `snapshot.rs`

**Interfaces:**
- Produces:
  - `enum EntryKind { File, Dir, Symlink, Fifo, Other }`.
  - `struct Meta { kind: EntryKind, size: u64, mode: u32, mtime_ns: i128, target: Option<Vec<u8>>, ino: u64 }`.
  - `fn walk_meta(root: &Path) -> Result<BTreeMap<Vec<u8>, Meta>>`: keys are bytewise relative paths, the root is excluded, and symlinks are not followed.
  - `enum CopyMode { Reflink, Plain }`.
  - `fn copy_tree(from: &Path, to: &Path) -> Result<CopyMode>`: copies types, content, permissions, mtimes (to the nanosecond), symlink targets, fifos, hard-link groups and `user.*` xattrs. It clones each file where the filesystem allows and copies plainly otherwise; the result is `Reflink` only if every file was cloned. A device node gives an error naming the path unless running as root.
  - `fn clone_or_copy_file(from: &Path, to: &Path) -> Result<bool>` (true = cloned).

- [ ] **Step 1: Write failing tests:**
  - `copy_tree_preserves_what_filegen_reads`: copy a 200-step filegen tree. `walk_meta` of both sides is equal except for `ino`, and hard-link groups map one to one.
  - `copy_tree_then_filegen_resume_is_identical`: `filegen run --resume` for 50 steps on the original and on the copy, each from its own copy of the state file, gives identical step lines (op, path, to, bytes) and identical trees, mtimes aside. This is the determinism the replay depends on.
  - `walk_meta_sees_awkward_names`: a file named `b"x\xff"` appears under its exact bytes.
- [ ] **Step 2: Run `cargo test -p stress snapshot`.** Expected: FAIL.
- [ ] **Step 3: Implement.** Linux reflink: `FICLONE` (reuse the approach in `crates/irsync/src/recv.rs`). macOS: `clonefile`. Fall back to `std::io::copy`.
- [ ] **Step 4: Run `cargo build --workspace && cargo test -p stress`.** Expected: all pass.
- [ ] **Step 5: Commit** "Add tree snapshot and copy helpers to the stress harness".

### Task 6: stress — the judge (pure rules)

**Files:**
- Create: `crates/stress/src/judge.rs`
- Test: unit tests in `judge.rs`

**Interfaces:**
- Consumes: `EntryKind` (Task 5) and `filegen::kind::OpKind`.
- Produces:
  - `struct StateEntry { kind: EntryKind, size: u64, mode: u32, mtime_ns: i128, hash: Option<blake3::Hash>, target: Option<Vec<u8>> }`.
  - `struct State { step: u64 /* 0 = round start */, entry: Option<StateEntry> }`.
  - `struct InFlight { step: u64, op: OpKind, pre: Option<StateEntry>, post: Option<StateEntry>, content_ok: bool }`.
  - `type DestEntry = StateEntry`.
  - `enum Verdict { Held(String /* which state or step */), NotHeld(String /* rule broken */) }`.
  - `fn judge(dest: Option<&DestEntry>, states: &[State], in_flight: &[InFlight], is_root: bool) -> Verdict`.
  - `struct Write { truncate_first: bool, offset: u64, len: u64 }`.
  - `fn write_of(op: OpKind, details: &BTreeMap<String, String>, old_size: Option<u64>) -> Option<Write>`: `None` for steps that are atomic for a reader (truncate, chmod, the `set_len` and fallocate modes of `punch_hole`, renames).
  - `fn in_flight_content_ok(dest: &Path, old: Option<&Path>, new: &Path, w: Option<&Write>) -> io::Result<bool>`.

The rules `judge` applies, in order:

1. **Absent destination:** held if any state, or any in-flight `pre` or `post`, is absent.
2. **Directory:** held if some state is a directory with the same mode. mtime is ignored.
3. **Symlink:** held if some state is a symlink with the same target.
4. **Fifo and others:** held if some state has the same kind and mode.
5. **Regular file, complete state:** if `dest.size` and `dest.mtime_ns` equal those of one or more complete file states, it is held only if one of those also has the same mode and hash. Otherwise it fails with `content and times disagree`.
6. **Regular file, in flight:** held if some in-flight step has `content_ok`, a mode equal to `pre` or `post`, and `pre.mtime ≤ dest.mtime ≤ post.mtime`. A missing `pre` counts as no lower bound.
7. Otherwise: `NotHeld("no state the source held")`.

Owner and group are checked only when `is_root`.

`in_flight_content_ok` with a write `w`:
- The base B is the old file, or empty if `truncate_first` or there is no old file.
- The destination D passes if it equals B with the new file's bytes `[offset, offset+k)` written over it, for some `k` in `0..=len`. The length is `max(len(B), offset+k)`.
- With `w == None`, D passes if it equals the old or the new file.
- Compare in 1 MiB chunks. Never load a whole file.

- [ ] **Step 1: Write failing tests**, one per rule. The exact cases:
  - `rewrite_prefix_is_held`: old `"AAAA"`, new `"BBBBBB"`, rewrite (`truncate_first`, offset 0, len 6). D `"BBB"` is held, D `""` is held, and D `"BBBA"` is not held.
  - `append_partial_is_held`: old `"AAAA"`, new `"AAAABBBB"`. D `"AAAABB"` is held; D `"AAABBB"` is not.
  - `overwrite_front_part_is_held`: old `"AAAAAAAA"`, write offset 2 len 4, new `"AABBBBAA"`. D `"AABBAAAA"` is held; D `"AAAABBAA"` is not (back part written first).
  - `two_steps_never_mix`: states s0 `"AAAA"`, s1 `"BBBB"`, s2 `"CCCC"` (rewrites). D `"BBCC"` is not held.
  - `times_say_current_bytes_old_fails`: D has s2's size and mtime but s1's hash. Expect `NotHeld` containing "content and times disagree".
  - `mtime_outside_window_fails`: in-flight content ok, but D.mtime is greater than post.mtime. Not held.
  - `absent_held_when_created_mid_round`: states `[absent at 0, file at 3]`, D absent. Held.
  - `dir_mode_must_have_been_held`: dir states with modes 755 and 700. D 711 is not held.
- [ ] **Step 2: Run `cargo test -p stress judge`.** Expected: FAIL.
- [ ] **Step 3: Implement** `judge.rs`.
- [ ] **Step 4: Run `cargo test -p stress judge`.** Expected: PASS.
- [ ] **Step 5: Commit** "Add the judge that checks a destination entry against source history".

### Task 7: stress — replay a round and judge the destination

**Files:**
- Create: `crates/stress/src/replay.rs`
- Test: unit tests in `replay.rs`

**Interfaces:**
- Consumes:
  - `filegen::run::{opts_from_args, Runner, Start}` and `filegen::log::{StepLine, parse_step_line}` (Task 2);
  - `snapshot::{walk_meta, copy_tree, clone_or_copy_file, Meta}` (Task 5);
  - `judge::*` (Task 6).
- Produces:
  - `struct RoundInput<'a> { start_meta: &'a BTreeMap<Vec<u8>, Meta>, start_copy: &'a Path, work: &'a Path, state: &'a Path, filegen_args: &'a [OsString], live_steps: &'a [StepLine], live_src: &'a Path, dest: &'a Path, is_root: bool }`.
  - `struct RoundJudgement { judged: u64, failures: Vec<(Vec<u8>, String)>, touched: BTreeSet<Vec<u8>>, changed_and_held: u64 }`.
  - `fn replay_and_judge(input: &RoundInput) -> Result<RoundJudgement>`. `Err` is a harness error.

Behaviour:
1. `copy_tree(start_copy, work)`.
2. Build a `Runner` from `opts_from_args(filegen_args)`, with `root = work`, `Start::Resume(state)` and the steps set to `live_steps.len()`.
3. Read every destination entry (lstat, plus a blake3 hash for regular files) before replaying.
4. For each live step k:
   - Before the step, keep a clone of every regular file in the link group of `path`, but only if the destination holds a regular file at one of those paths.
   - Call `runner.step()`. Compare the replayed log line's op, path, to and bytes with `live_steps[k]`. A difference is `Err("replay diverged at step N: …")`.
   - Then re-scan the touched set: `path`, `to`, their link-group members, and every path under them (for removed or renamed directories, through the current index's prefix range). Push new `State`s for changed paths and an `InFlight` for the step, with `content_ok` computed against the kept clone.
   - Metadata comes from the live line's `post`/`post_to`. Link-group members share it. Paths moved under a renamed directory carry their previous metadata.
5. After the last step, the replayed tree must equal `live_src` in kinds, sizes, modes, symlink targets and content hashes (mtimes ignored). A difference is `Err("replay diverged from the live source: <first path>")`.
6. Judge every path in the union of destination paths and paths that have any state. Ignore names `.irsync-stress-ping`, `.irsync-clones-*` and `.irsync.*.part`.
7. `changed_and_held` counts touched paths whose destination is held and differs from their round-start state.

- [ ] **Step 1: Write failing tests**, built on hand-made temp trees and `filegen run` output:
  - `replay_accepts_the_end_state`: start tree, 30 live steps, destination = live end tree. No failures, and `judged` equals the entry count.
  - `replay_accepts_the_start_state`: destination = start tree. No failures.
  - `replay_rejects_a_torn_file`: the destination holds a file whose first half comes from one rewrite step and second half from a later one. One failure, on that path.
  - `replay_detects_divergence`: tamper with one live step's `path`. Expect `Err` containing "replay diverged at step".
  - `replay_tracks_hard_link_groups`: a live run with `weights.*.hardlink=10` and `weights.*.append=10`, destination = end tree. No failures. A destination link member holding its pre-append content with post-append times fails.
  - `replay_tracks_renamed_directories`: a run heavy on `rename` of directories (`--set weights.*.rename=15`), destination = start tree and = end tree. No failures either way.
- [ ] **Step 2: Run `cargo build --workspace && cargo test -p stress replay`.** Expected: FAIL.
- [ ] **Step 3: Implement** `replay.rs`.
- [ ] **Step 4: Run `cargo test -p stress replay`.** Expected: PASS.
- [ ] **Step 5: Commit** "Replay a filegen round and judge the destination against it".

### Task 8: irsync-stress `--concurrent`

**Files:**
- Create: `crates/stress/src/concurrent.rs`
- Modify:
  - `crates/stress/src/cli.rs`: `--concurrent`; `--pace <PACE>`, which needs `--concurrent`; `--replay-rounds <FILE>`, which needs `--concurrent`.
  - `crates/stress/src/filegen.rs`: `spawn` and `stop`.
  - `crates/stress/src/layout.rs`: the new paths below.
  - `crates/stress/src/run.rs`: branch to the concurrent loop. The existing loop is untouched.
  - `crates/stress/src/report.rs`: `ConcurrentRound` and the run exit code.
  - `crates/stress/src/run_file.rs`: record `concurrent`, a fixed pace if any, and the replay file.
- Test: unit tests plus `crates/stress/tests/e2e.rs`

**Interfaces:**
- Consumes: `replay_and_judge` (Task 7), `walk_meta`/`copy_tree` (Task 5), `Pace` (Task 1), the report format (Task 3).
- Produces (netstress uses these):
  - `fn draw_pace(seed: u64, round: u64) -> Pace`:
    - `ChaCha8Rng::seed_from_u64(seed ^ 0x636f_6e63_7572_7265)` with `set_stream(round)`.
    - It picks `fast`, `steady` or `bursts` with probability 1/3 each.
    - `steady.per_sec` is log-uniform in `[5, 500]`. `bursts.on_ms` and `off_ms` are each uniform in `[50, 2000]`.
  - `struct RoundRecord { round: u64, pace: Pace, steps: u64 }`.
  - `fn append_round(path: &Path, r: &RoundRecord) -> Result<()>` and `fn read_rounds(path: &Path) -> Result<Vec<RoundRecord>>`. The file is `logs/rounds.tsv`, one `round\tpace\tsteps` line per round.
  - `struct ReportLine { event: String, path: Vec<u8>, reason: String }`.
  - `fn read_report(path: &Path, from: u64) -> Result<(Vec<ReportLine>, u64 /* new offset */)>`. A missing file means no lines.
  - `struct RoundEnv<'a> { cli: &'a Cli, layout: &'a Layout, tools: &'a Tools, seed: u64, filegen_opts: FilegenOpts<'a> }`.
  - `fn round(env: &RoundEnv, i: u64, fixed: Option<&RoundRecord>, sync: &dyn Fn(&Path) -> Result<SyncResult>, quiet: &dyn Fn(&Path) -> Result<SyncResult>, daemon_report_offset: &mut u64) -> Result<ConcurrentRound>`.
  - `struct ConcurrentRound { number: u64, total: u64, pace: Pace, steps: u64, sync: SyncResult, retried: u64, gave_up: Vec<Vec<u8>>, gave_up_untouched: Vec<Vec<u8>>, judgement: RoundJudgement, quiet: Iteration }`, with:
    - `passed()`: the sync exit is `Ok`, `Other(24)`, or `Partial` with `gave_up_untouched` empty; `judgement.failures` is empty; and `quiet.passed()`.
    - `line()`.
  - `fn concurrent_exit_code(rounds: &[ConcurrentRound]) -> i32`: 1 if any round failed, else 4 if the sum of `changed_and_held` is 0, else 0.
  - In `filegen.rs`:
    - `fn spawn(filegen: &Path, args: &[OsString], stderr_log: &Path) -> Result<RunningFilegen>`. `RunningFilegen`'s `Drop` sends SIGTERM and then SIGKILL after 5 s.
    - `fn stop(f: RunningFilegen, wait_only: bool) -> Result<()>`: SIGTERM unless `wait_only`, then wait at most 60 s. Exit 0 or 143 is fine; anything else is an error naming the stderr log.
  - Layout:
    - `round_dir()` → `ROOT/round`, holding `start/`, `work/`, `old/` and `state.yaml`;
    - `rounds_file()` → `logs/rounds.tsv`;
    - `client_report(i)` → `logs/iter-NNNN.client.report.jsonl`;
    - `daemon_report()` → `logs/daemon.report.jsonl`;
    - `quiet_client_log(i)` → `logs/iter-NNNN.quiet.client.log`;
    - `judge_report(i)` → `logs/iter-NNNN.judge.txt`.

How a run goes:
1. Fill and `fileset init`, as today.
2. **Round 0:** one quiet sync plus today's manifest comparison and fileset check. It must pass, or the run stops with a harness error.
3. The daemon starts with `IRSYNC_FILE_REPORT=<daemon_report()>`. Each concurrent client sync gets `IRSYNC_FILE_REPORT=<client_report(i)>`.
4. **Round i:**
   1. Pick the pace (`fixed`, else `--pace`, else `draw_pace`).
   2. Take the snapshot: `walk_meta` and `copy_tree` from `src_data` into `start/`, and copy the filegen state into `state.yaml`.
   3. Note the filegen log's length.
   4. Spawn filegen with `mutate_args` plus `--pace`. Steps: `fixed.steps`, else `u64::MAX`.
   5. Concurrent sync.
   6. `stop` (`wait_only` when `fixed`).
   7. Parse the new step lines and append the round record.
   8. Read both reports: the client's whole file, and the daemon's from the offset.
   9. `replay_and_judge`. Write the failures to `judge_report(i)`.
   10. `fileset update`.
   11. Quiet sync, then today's checks.
5. **Exit codes of the concurrent sync:**
   - `Other(c)` with c not 24, a signal, or the daemon dying: a harness error, as today.
   - `Partial`: compare the `gave_up` paths with `judgement.touched`. Paths are relative to `src_data`, and the root path `""` is never touched.
6. **On failure:**
   - Print the failing paths with their rule (the first 20, then the count).
   - Print the judge report path.
   - Print the rerun command: the original arguments with a fresh root plus `--replay-rounds <rounds_file>`.
   - With `--keep-going`, carry on; otherwise stop and keep the root.
7. The header gains `clones:   yes` or `no`, from the first snapshot's `CopyMode`: whether this filesystem can clone, and so whether `--paranoid` runs take the clone path or the second-hash path.
8. A run that ends with exit 4 prints `concurrent writes did not bite: no file changed during a sync was placed`.

- [ ] **Step 1: Write failing tests.** Unit tests:
  - `draw_pace_is_repeatable`: the same (seed, round) gives the same pace; 300 draws hit all three kinds.
  - `rounds_file_round_trips`.
  - `read_report_from_offset`: lines appended after the offset are the only ones returned.
  - `partial_ok_only_for_touched_paths`: a `ConcurrentRound` with `gave_up_untouched = [b"x"]` fails; with it empty, it passes.
  - `stop_waits_for_the_step_to_finish`: a fake filegen script that traps TERM, sleeps 1 s and exits 143. `stop` returns Ok after at least 1 s.
  - `concurrent_flags_need_concurrent`: `--pace fast` without `--concurrent` is a parse error.

  End-to-end (`crates/stress/tests/e2e.rs`):
  - `concurrent_push_and_pull_run`: each direction with `--concurrent --iterations 3 --steps 50 --max-total-size 8M`. Exit is 0 or 4, `rounds.tsv` has 3 lines, and three judge reports exist.
  - `replay_rounds_reproduces_steps`: rerun the first run with `--replay-rounds`. Each round's step lines (op, path, to, bytes) are identical to the first run's.
  - `existing_mode_unchanged`: the existing e2e tests pass unmodified.
- [ ] **Step 2: Run `cargo build --workspace && cargo test -p stress`.** Expected: FAIL.
- [ ] **Step 3: Implement** `concurrent.rs`, the CLI, layout and report changes, and the `run.rs` branch.
- [ ] **Step 4: Run `cargo build --workspace && cargo test -p stress`.** Expected: PASS.
- [ ] **Step 5: Commit** "Add --concurrent to irsync-stress: write to the source while syncing".

### Task 9: irsync-netstress `--concurrent`

**Files:**
- Modify:
  - `crates/netstress/src/cli.rs`: the same three flags, mapped in `to_stress`.
  - `crates/netstress/src/run.rs`: a concurrent loop calling `stress::concurrent::round`. The existing loop is untouched.
  - `crates/netstress/src/report.rs`: `NetConcurrentRound` and the exit code.
- Test: `crates/netstress/tests/e2e.rs`

**Interfaces:**
- Consumes: everything Task 8 lists under "Produces".
- Produces:
  - `NetConcurrentRound { base: ConcurrentRound, events: NetEvents, verdict: Option<String> }`. `passed()` requires `base.passed()` and `verdict.is_none()`.
  - The concurrent sync runs as stream `i`, and the quiet sync as stream `i + (1 << 32)`. Both go through the fault layer.
  - The netlog verdict comes from the concurrent sync's client log.
  - Exit code: 1 if any round failed; else 3 if the profile has outages and none happened; else 4 if no changed file was placed; else 0.

- [ ] **Step 1: Write the failing test** `concurrent_netstress_runs`, marked `#[ignore = "slow: netsim; run with --include-ignored"]`: the `test-blackout` profile, `--concurrent --iterations 2`. Exit is in {0, 3, 4} and `rounds.tsv` has 2 lines.
- [ ] **Step 2: Run `cargo test -p netstress -- --include-ignored concurrent`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run `cargo test -p netstress -- --include-ignored`.** Expected: PASS, the existing tests included.
- [ ] **Step 5: Commit** "Add --concurrent to irsync-netstress".

### Task 10: campaign rows, time budget, docs

**Files:**
- Modify:
  - `crates/stress/tools/breadth.sh` and `crates/netstress/tools/sweep.sh`: optional `DURATION` in seconds. After that, `run` starts no new runs and writes no summary line for skipped ones.
  - `crates/stress/tools/matrix.sh`: four new rows and their header lines.
  - `book/src/contributing/bug-hunting.md`; `crates/netstress/README.md` if it lists modes; `book/src/explanation/correctness.md` (the list of how correctness is tested); `TODO.md` (close the stress-mode item, pointing at the spec).

**Interfaces:** the rows, appended to `ALL`:
- `concurrent`: `CLIENT='--timeout 300' DURATION=3600 SEEDS='1 1000' ITER=10 TOTAL=1G FILE=64M|--concurrent`
- `concurrent-paranoid`: as above with `CLIENT='--timeout 300 --paranoid'`
- `net-concurrent` and `net-concurrent-paranoid`: run `crates/netstress/tools/sweep.sh --concurrent` with `DURATION=3600`, `BIN` defaulting to `target/netsim/release`. If `$BIN/irsync-netstress` is missing, refuse up front with the build command.

The filegen dry-run check at the top of `matrix.sh` must accept these rows. Their filegen sets are empty.

- [ ] **Step 1: Check:** `DRY=1 crates/stress/tools/matrix.sh concurrent concurrent-paranoid net-concurrent net-concurrent-paranoid` prints `settings ok for:` with all four rows, and the default `ALL` list still contains every old row.
- [ ] **Step 2: Docs.**
  - `bug-hunting.md` gets:
    - the four rows in the campaign-matrix table;
    - a section "Writing during the sync (`--concurrent`)", covering what a round does, the rules, the exit codes 1, 2, 3 and 4, the files kept, and the rerun with `--replay-rounds`;
    - the self-test: `cargo build --release -p irsync --features test-hooks --target-dir target/selftest`, then `IRSYNC_TEST_NO_CHANGE_CHECKS=1 target/release/irsync-stress --concurrent --irsync target/selftest/release/irsync --direction push --iterations 20 /tmp/x`, which must exit 1.
  - Mention `IRSYNC_FILE_REPORT` there as a harness-only variable. It is not in `command-line.md`.
- [ ] **Step 3: Run a short real campaign**, both directions, with and without `--paranoid`. Use `--iterations 5` in release, on this machine's ZFS. Then run the self-test. Record the four exit codes and the self-test's exit in the commit message body. The self-test must exit 1. If it does not, stop and report: the checker is not biting.
- [ ] **Step 4: Full regression** (Global Constraints) and the site check: `site/build.sh && python3 site/tools/check.py --irsync target/debug/irsync target/site`.
- [ ] **Step 5: Commit** "Add concurrent-write campaign rows and document the mode".
