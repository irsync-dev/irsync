# fileset: implementation decisions

This file records the rulings made while implementing the `fileset` crate
from `docs/superpowers/specs/2026-10-19-fileset-design.md` and
`docs/superpowers/plans/2026-10-19-fileset.md`. Design-level decisions live
in the spec; this file covers the calls made during execution that the spec
and plan did not settle, or that amended them. Each entry names what was
decided, why, and what it costs if the ruling turns out to be wrong.

Behavioral amendments to the spec are also recorded in the spec's
"Amendments made during implementation" section. Review findings that were
deferred rather than fixed are listed in
`docs/superpowers/plans/2026-10-19-fileset-deferred.md`.

## Pre-flight (before Task 1)

1. **`WalkConfig` is `Clone + Send`.** The engines clone it into the walker
   thread instead of sharing it by reference. Why: the producer runs on its
   own thread and the config is small. Cost if wrong: a trivial refactor.

2. **Engines hold the store in `RefCell<Box<dyn Store>>`.** The `decide` and
   `sink` closures handed to the pipeline both need the store, and both run
   on the calling thread. Why: avoids a `Mutex` for single-threaded access.
   Cost if wrong: a runtime borrow panic, which the tests would surface
   immediately.

3. **Task 10 created only the `Progress` trait and `NoProgress`.** The
   terminal spinner (`TermProgress`) came with Task 12. Why: Task 10's tests
   did not need a terminal. Cost if wrong: none.

## Task 6 (store location)

4. **The `--store` mismatch error also applies when the manifest is found
   via `--manifest`.** The implementer only checked the in-tree branch; the
   spec says the backend must match the manifest wherever it lives. Why:
   spec is binding. Cost if wrong: a spurious error only when the two flags
   disagree, which is a user mistake anyway.

## Task 8 (pipeline)

5. **The reorder buffer is a `BTreeMap` with a window cap of
   `JOB_CAPACITY + jobs`.** Hashed results arrive out of order and must be
   emitted in walk order. Why: the cap bounds memory regardless of tree
   size, which is a hard requirement; traced deadlock-free because the
   walker cannot be more than `JOB_CAPACITY` entries ahead of the slowest
   hasher plus one in-flight job per thread. Cost if wrong: none observed;
   a bug here would show as a hang under load.

## Task 11 (check engine)

6. **In the add/del/chg test, sorted walk order puts `CHG m` before
   `ADD n`.** The plan's expected output listed them the other way. Why:
   byte-wise directory sorting is the spec'd behavior and the plan's order
   was a transcription slip. Cost if wrong: none.

7. **Deleting `d/f` changes `d`'s mtime, and that is a legitimate change.**
   The test restores `d`'s mtime after the delete rather than loosening the
   comparator. Why: the comparator is correct; the test setup was
   incomplete. Cost if wrong: none.

## Task 13 (CLI wiring)

8. **The `--manifest` hint on a failed read-only open was unimplemented.**
   The spec promised that a missing manifest error suggests `--manifest`;
   the implementer missed it. Carried into the final fix wave rather than
   reopening Task 13. Cost if wrong: none now; it is implemented.

## Final review (spec amendments)

9. **Content `check` cannot observe is a difference, not a warning.** When
   the second pass finds a stored entry under a directory the walk could
   not descend into, or lstat fails for a reason other than not-found,
   `check` prints `CHG <path> kind=<kind>→unverified` and exits 1. Why: the
   original spec text let `check` exit 0 while a rotted directory hid its
   contents, which defeats the archival purpose. Cost if wrong: a stricter
   check than the spec first described; `--relaxed` does not soften this.

10. **The store's parent directory is mtime-exempt, like the root.** Writing
    the manifest updates its parent's mtime, so comparing it would always
    report a change. Why: unavoidable self-interference. Cost if wrong: one
    directory's mtime drift goes unreported; content and mode are still
    compared.

11. **No nested-manifest warning for direct children of the root.** The
    manifest at depth 0 is the tree's own and is not "nested". Why: the
    original walker warned on its own manifest. Cost if wrong: none.

12. **Robustness changes bundled into the final wave:**
    - fjall marker check (`version` file and `keyspaces/` directory) before
      opening, so a non-fjall directory errors cleanly instead of being
      created or corrupted;
    - fjall sweep processes stale entries in chunks of 2000 so memory stays
      bounded during `update`;
    - the walker thread gets a 256 MiB stack so deep trees do not overflow
      the recursive DFS (superseded by 30: the walk no longer recurses);
    - xattr and readlink failures on an entry produce a warning and keep the
      entry rather than dropping the subtree.
    Cost if wrong: small; each is conservative.

13. **`update` claims its generation number before the walk.** The new
    generation is written to meta first, so an interrupted run cannot leave
    entries that a later sweep mistakes for current. The regression test
    interrupts a real update after a committed batch rather than planting a
    ghost entry, because the fixed binary can no longer produce one. Cost if
    wrong: none.

14. **The deep-tree test uses depth 2000.** The plan said 2500, which
    exceeds `PATH_MAX` on Linux. Why: the test must create the tree it
    walks. Cost if wrong: none; 2000 is well past the default stack limit.

## Follow-up: `init` split from `update` (2026-10-20)

18. **`--store` moved to `init`; `--rehash` stayed on `update`.** The user
    asked for the verb split without saying where the flags go. Why: the
    backend is chosen at creation and detected afterwards, so `--store` on
    `update` could only ever trigger the mismatch error; `--rehash` has no
    effect on a first scan, which hashes everything anyway. Cost if wrong:
    adding a flag back to a verb is a few lines and breaks nothing.

19. **The split lives in the engine, not the CLI.** `UpdateOpts` carries a
    `Mode` enum and the engine refuses the wrong precondition after
    `locate` and before opening the store. Why: both the CLI and the test
    helpers construct the options, and the precondition belongs with the
    code that would otherwise create the store. Cost if wrong: none.

20. **The test helper picks the mode by whether the manifest exists.**
    `update_opts` returns `Init` on a fresh tree and `Update` on a tracked
    one, so the existing sixty-odd engine tests keep reading as a user
    would type them. Tests that need the other mode set it explicitly.
    Cost if wrong: a test could silently exercise `init` where `update` was
    meant; the dedicated precondition tests cover both directions.

## Follow-up: `init` refuses nested manifests (2026-10-20)

21. **The refusal fires when the walk reaches the nested manifest, not
    after a separate pre-scan.** The user asked `init` to refuse a tree
    with a manifest in any subdirectory. Why: a pre-scan would read every
    directory twice on every successful `init`, which is the common case
    on multi-million-file trees; aborting on discovery costs nothing when
    the tree is clean. Cost if wrong: on a dirty tree, everything sorted
    before the nested manifest gets hashed before the refusal, which can
    be hours on terabytes. A `--check-only` pre-scan would be the remedy.

22. **A failed `init` removes the manifest it created.** This covers the
    nested-manifest refusal and every other error after the store was
    created, SQLite sidecars included. Why: a half-built manifest has no
    value, and leaving it would make the retry fail with "already exists".
    A manifest that existed before the run is never touched, because it
    is opened read-write, not created. Cost if wrong: none; the removal
    is reported as a warning if it fails.

23. **`update` and `check` still only warn about nested manifests.** Why:
    manifests created before this rule may sit above nested ones, and
    `check` on an archive must keep working. Cost if wrong: a nested
    fileset keeps being tracked as ordinary content, as before.

## Follow-up: `init` refuses a root inside a fileset (2026-10-20)

24. **Ancestors are checked by name only, on the canonical root.** The
    user asked `init` to walk up from PATH to `/` and refuse if a parent
    holds a manifest. Why: one `lstat` per ancestor and name is the whole
    cost, so it runs before the store is created, and canonicalizing first
    means a symlinked PATH is judged by where it really lives. Cost if
    wrong: a fileset whose manifest is kept outside the tree with
    `--manifest` is not detected from below, and a dangling symlink named
    like a manifest counts as one. Both are consistent with how `locate`
    treats the root's own manifest.

25. **Only `init` looks upward.** `update` and `check` keep working inside
    an enclosing fileset. Why: they operate on a manifest that already
    exists, which this rule cannot retroactively forbid. Cost if wrong:
    none; a stricter rule can be added to `update` later.

## Follow-up: built-in excludes (2026-10-29)

26. **Excludes match by name, before the stat.** A file named `.Trashes` at a
    volume top is skipped as well as a directory; stat'ing the directory
    is what macOS refuses on some volumes, and the type would not change the
    answer.

27. **A volume top is the root or a directory whose device differs from its
    parent's.** Without `--cross-mount-points` the walk never enters such a
    directory, so in practice only the root's names are checked.

28. **`check` forgets stored entries at or below an exclude the walk
    skipped.** A stored exclude that has since vanished from disk is still
    reported deleted: the rule keys on what the walk saw, not on names.

29. **The `INFO` channel lives on `Warnings`.** It is uncounted, goes to
    stderr like `WARN`, and prints only with `-v`.

## Follow-up: parallel walk (2026-10-29)

30. **Directories are listed on a pool, events still come out in walk
    order.** A no-change `update` of a 1.7M-entry tree (370k directories,
    nearly every file with an xattr) spent 52 s with one thread making
    `lstat`, `listxattr` and `getxattr` calls one after another, and the
    database thread idle 69% of that time. Now `--walk-threads` threads each
    list, sort and stat one directory at a time; the walk thread emits.
    Same tree: 56 s with 1 thread, 24 s with 4, and the manifests are
    identical. The walk no longer recurses, so the walk thread's 256 MiB
    stack is gone.

31. **Listings run in walk order, behind a window.** Each directory is keyed
    by its path of child indices, whose order is walk order; the pool takes
    the earliest pending one. At most `64 × threads` listings may be held
    ahead of the emitter, except the one it waits for, which always runs:
    it is the earliest pending key, since it was queued when its parent
    finished. Cost if wrong: a deadlock, which the window exception rules
    out, or memory, which the window bounds.

32. **The default is 4 threads, not the CPU count.** On a 16-core M4 Max over
    an APFS SSD, a C walk of the same syscalls took 52 s on 1 thread, 16 s on
    4, 15 s on 5, 24 s on 8 and 78 s on 16: xattr reads contend in the kernel.
    The full `update` was fastest at 4 (24 s), then 5 (25 s), 6 (26 s) and
    8 (30 s). It is separate from `-j`, which sizes the hash pool, where
    reading file contents does scale. Cost if wrong: other filesystems may
    prefer another count, which the flag covers.

## Parked (real, deferred, not fixed)

15. **An unreadable fjall manifest directory reports "not a manifest"
    instead of "permission denied".** The marker check runs before the
    permission error is distinguishable. Misleading text only.

16. **A race between `check`'s two passes.** A file created or deleted
    between the walk and the store sweep can appear once too often or not
    at all in the output for that run. Harmless on quiescent trees, which is
    the intended use.

17. **One README sentence about mount points** is only accurate when
    `update` was run with `--cross-mount-points`. Documentation only.

## Process notes

- Every commit on the branch ends with the trailer
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`. Eleven
  commits were rewritten after the fact because subagents running on a
  different model used their own name in the trailer.
- The fjall backend preallocates a sparse 64 MiB journal on open. This is
  noted in `crates/fileset/README.md` for the planned sqlite-vs-fjall
  benchmark, since on-disk size is one of its measures.
