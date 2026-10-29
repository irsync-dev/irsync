# fileset — deferred findings from per-task and final reviews

Triage by the final whole-branch reviewer: everything marked "FIX BEFORE MERGE" was fixed in the final fix wave; the rest is deferred. One line per finding, as recorded in the implementation ledger.

Closed 2026-10-29: every finding below is fixed or covered by a test, except the two in `TODO.md` (a mid-listing `readdir` failure, and a SQLite migration from an older schema), which wait for fault injection and a second schema version.

Ruling: WalkConfig derives Clone and is Send; engines clone it into the producer closure — needed by pipeline::run's 'static producer — cost if wrong: trivial refactor.
Ruling: engines hold the store in a std::cell::RefCell<Box<dyn Store>> so decide (borrow) and sink (borrow_mut) can both use it; both closures run on the calling thread so no overlap — cost if wrong: a runtime BorrowMutError in tests, easy to see.
Ruling: Task 10 creates progress.rs with the Progress trait and NoProgress only; Task 12 adds TermProgress — avoids a half-built type across two tasks — cost if wrong: none.
Task 1: minor (deferred): tests/cli.rs relaxed/paranoid test only asserts exit 2, not the conflict message; no test that other flags parse
Task 1: minor (deferred): tests/cli.rs one-line fn bin() not rustfmt style (Task 12 runs cargo fmt)
Task 1: minor (deferred): -j 0 accepted unvalidated (Task 12 wires jobs; use range(1..))
Task 2: minor (deferred): model.rs:188 ENOTSUP mid-way through xattr::get discards already-collected xattrs
Task 2: minor (deferred): no tests for suid/sgid/sticky mask, fifo/socket/device kinds, non-UTF-8 path (later tasks cover some)
Task 3: minor (deferred): report.rs:120 out-of-range fmt_mtime fallback lacks Z / spec shape (unreachable for stat values)
Task 3: minor (deferred): escape.rs only escapes C0 and DEL; C1 controls and bidi overrides pass through (brief-compliant)
Task 3: minor (deferred): line_chg writes diff values unescaped; callers pre-escape — add doc comment
Task 4: minor (deferred): sqlite.rs:392 wal_checkpoint(TRUNCATE) busy flag discarded; close reports success even if blocked
Task 4: minor (deferred): missing tests: ReadWrite migrating older schema, ReadWrite on newer schema, Create on existing manifest, -journal sidecar after close
Task 4: minor (deferred): Create failing after connection open leaves a partial file
Task 4: minor (deferred): ReadOnly open of a WAL-mode db left by a crash may need -shm write access
Task 4: minor (deferred): crash during Create before commit leaves an empty .fileset.sqlite that Create then refuses (needs manual deletion)
Task 5: minor (deferred): fjall.rs:122 `p.kind as u8` truncates a corrupt u32 kind to 0 silently; use u8::try_from
Task 5: minor (deferred): no fjall test for ReadOnly+older and ReadWrite auto-bump branches
Task 5: minor (deferred): fjall.rs:183 ReadWrite on a non-manifest directory creates keyspaces before failing on missing meta
Task 5: minor (deferred): fjall.rs:240 sweep buffers all doomed keys before one batch; memory grows with deletions (spec anti-goal: memory ∝ entries) — final review to triage
Task 6: Ruling: --store mismatch check also applies when the manifest is found via --manifest — spec says "if one exists and --store names a different backend, that is an error", brief's rule list omitted it — cost if wrong: a spurious error only when a user passes both flags inconsistently.
Task 6: minor (deferred): store_path_inside falls back to the raw path when the parent cannot be canonicalized (relative manifest with missing parent returns None); undocumented
Task 6: minor (deferred): missing tests: relative --manifest, manifest inside root returning Some, /a/b vs /a/bc prefix, file → Sqlite via --manifest, symlinked manifest, missing --manifest on update with/without requested
Task 7: minor (deferred): no tests for mount-point non-crossing, cannot-stat, cannot-read-directory (incl. once-only), sort:false branches
Task 7: minor (deferred): "cannot read directory" format string duplicated at walk.rs:112 and :124; tidiness (import Kind, test re-imports); recursion-depth comment
Task 8: Ruling: reorder buffer is a BTreeMap instead of the plan's BinaryHeap, and a reorder window of JOB_CAPACITY + jobs caps undelivered events — functionally equivalent, and the window is needed for the spec's memory bound — cost if wrong: none (reviewer traced it deadlock-free).
Task 8: minor (deferred): pipeline.rs:65-79 a panicking hasher thread hangs run forever instead of erroring (catch_unwind or drop guard) — final review to triage
Task 8: minor (deferred): sink-error test does not guarantee the producer is blocked on a full channel; no tests for the reorder window, producer error with jobs in flight, jobs == 0
Task 9: minor (deferred): no direct tests for target, rdev, nlink, non-dir size, gid, blake3-under-Relaxed branches
Task 9: minor (deferred): compare.rs:71 idx() and hard-coded array length depend on enum order/count; xattr merge-walk assumes sorted lists (add debug_assert + test); comment for unhashed-observed case
Task 10: minor (deferred): update.rs:158 pop_front().unwrap_or(false) hides a decide/sink order mismatch; use expect/debug_assert
Task 10: minor (deferred): update.rs:158-161 unreadable files counted as hashed and progress bytes reported; count only when hash_error is None or document "attempted"
Task 10: minor (deferred): update.rs:175-184 error paths skip close() and progress.finish(); relies on backend Drop safety (fjall journal) — final review to triage
Task 10: minor (deferred): unix_now maps pre-epoch clock to 0 silently; 2000-entry batch boundary untested; root-skip detection via readability rather than euid
Task 11: Ruling: in check_reports_add_del_chg the brief's expected order "ADD n" before "CHG m" is impossible under sorted walk order (m < n); the test asserts CHG m, ADD n, CHG x, DEL d/f — spec's sorted DFS order is binding — cost if wrong: none.
Task 11: Ruling: deleting d/f changes d's mtime, which is a legitimate CHG under the default tier; the test restores d's mtime rather than weakening the comparator — cost if wrong: none.
Task 11: minor (deferred): check.rs:93 stored entries under a parent that became a symlink to an outside dir are not reported DEL (follows from spec's one-lstat-per-entry)
Task 11: minor (deferred): check.rs:63-69 "no stored checksum" warning also fires when the observed object is no longer a file or when blake3 is ignored; with --fail-on-warnings this flips the outcome — final review to triage (controller view: should be fixed before merge)
Task 11: minor (deferred): two store lookups per tracked file (decide + sink), same as update; setup block duplicated from update.rs (~15 lines) — shared helper candidate
Task 11: minor (deferred): tests: require-uid test proves only cleanliness; missing-manifest test ignores backend; CHG x relies on nanosecond mtime resolution
Task 12: minor (deferred): no CLI test for -j 0 (range message) or for valid --ignore/--require values; fail() called while BufWriter holds stdout lock (harmless, comment); TermProgress throttle untestable without tty
Task 13: minor (deferred): README exit-code wording: "1: differences found" applies to check; update returns 1 only with --fail-on-warnings; --paranoid prose could name the concrete list
Task 13: Ruling: spec line "check with the manifest inside a read-only tree may fail with exit 2 and a message suggesting --manifest" has no implementation (no engine/store error mentions --manifest on a failed read-only open) — confirmed cross-task gap, carried into the final-review fix wave rather than reopening Task 13 — cost if wrong: a less helpful error on read-only media.
