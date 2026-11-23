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
    wrong: none. (Since 33 the sweep goes by what the walk saw, not by
    generation; the claim still gives every run's writes their own number.)

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
    the earliest pending one. At most `1024 × threads` listings may be held
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

## Follow-up: unchanged rows are not rewritten (2026-10-29)

33. **`update` writes only the rows that differ, and sweeps what the walk
    did not see.** Every entry is looked up, directories and symlinks too,
    and an entry equal to its row but for the generation is left alone; a
    row's generation now says which run last wrote it. Without a fresh
    generation on every row the sweep cannot go by generation, so the engine
    keeps the set of paths the walk saw and the store deletes every other
    key. A no-change `update` of the 1.7M-entry volume wrote about 2 GB
    (every row and every xattr rewritten, then checkpointed) and now writes
    next to nothing; its wall time hardly moves, since the walk sets it.

34. **The seen set holds 128-bit BLAKE3 prefixes of the paths, not the
    paths.** About 16 bytes an entry (some 40 MB for 1.7M entries) instead
    of the paths themselves. Cost if wrong: a vanished path whose prefix
    equals a seen path's would survive the sweep; with 2^-128 per pair that
    is not a practical risk.

35. **The SQLite sweep pages through the keys, 2000 at a time, inside one
    transaction.** It deletes between pages rather than under an open
    cursor on the same table. The `entries_generation` index no longer
    serves a query but stays, to avoid a schema migration; it costs only on
    the rows that are written.

## Follow-up: xattrs are not re-read while ctime is unchanged (2026-10-29)

36. **`update` reuses the manifest's xattrs for an entry whose kind, inode,
    size, mtime and ctime match.** Setting, changing and removing an xattr
    each update the ctime (checked on APFS). On the 1.7M-entry volume this
    drops about 3.3M `listxattr`/`getxattr` calls: a no-change `update`
    goes from 21.6 s to 17.5 s and its system time halves (57 s to 26.5 s).
    `--force-xattr-reads` (update only, like `--rehash`) reads them all;
    `init` has nothing to reuse and `check` always reads, since verifying is
    its job.

37. **The walk threads do the manifest lookups.** The reuse decision is made
    right after the `lstat`, so each walk thread asks the manifest through a
    `Reader` (SQLite: read-only connections of its own, which WAL lets read
    while the store writes; fjall: a shared keyspace handle) and the entry
    carries what it found to the engine, which no longer looks it up again.
    A row is looked up before the engine can write it, since entries are
    written only after they are emitted. Cost: with `--force-xattr-reads`
    the walk threads carry both loads, and the run is slower than before
    this change (26 s against 21.6 s on the volume above).

38. **An entry whose xattrs could not be read in full is flagged
    `XATTRS_PARTIAL` and read again on every update.** Without the flag the
    partial set would be reused silently and its warning would show only
    once; with it, `--fail-on-warnings` keeps failing until the attributes
    can be read. The flag lives in the existing `flags` column, so no
    migration.

39. **The walk-thread default stays 4.** Without the xattr reads the walk
    scales a little further (8 threads: 16.9 s against 17.9 s) at more than
    twice the system time, so the gain does not justify a new default.

## Follow-up: a wider walk window (2026-10-29)

40. **Each walk thread may finish 1024 listings ahead of the emitter, up
    from 64.** A profile showed the walk threads idle a quarter of the time:
    with one slow directory holding up the emitter, 256 listings filled the
    window and the threads waited. On the 1.7M-entry volume a no-change
    `update` goes from 15–18 s to about 13 s; peak memory stays around
    150–175 MB, since listings average under five entries. 16384 was no
    faster.

## Follow-up: cheaper manifest lookups (2026-10-29)

41. **A lookup is one query, and a directory's lookups share one read
    transaction.** The row and its xattrs come back from one `LEFT JOIN`,
    and `Reader::get_many` reads all the names of a listing in one
    transaction (SQLite; the default loops over `get`). In the walk threads
    lookups fell from 25% to 18% of the time, the per-query read-lock setup
    from about 7% to under 1%, and waits on SQLite's internal mutexes from
    about 5% to 2%. A no-change `update` of the 1.7M-entry volume goes from
    13.1 s to 12.0 s, user time from 9.3 s to 7.0 s.

42. **No mmap or larger page cache on the reader connections.** Most of the
    remaining lookup time is `pread` from the OS cache. A 64 MiB page cache
    per connection gained nothing and cost 300 MB; `mmap_size` saved about
    0.7 s but counts the mapped manifest as resident (2.2 GB here). Turning
    off SQLite's memory statistics was not tried: what remains of the mutex
    wait is about 2%, and the switch is process-wide.

43. **`update` stops at the first lookup that fails.** It used to note the
    error, treat the entry as new and carry on, failing only at the end: a
    manifest that could not be read made every file look new, and the run
    started re-hashing the whole 4.2 TB volume before it could report. Now
    the entry is not sent to be hashed and the sink fails when its turn
    comes, which stops the walk and the hashers. Entries ahead of it in walk
    order are still recorded, as in any interrupted run.

## Follow-up: fjall keeps filter and index blocks in memory (2026-11-21)

Issue #188. On a fjall manifest of a million entries that had been closed
and reopened, a lookup took about 71 µs instead of 4.

- **Cause.** fjall keeps the filter block only of its newest tables in
  memory and reads the others through its block cache (32 MiB). That cache
  is split into four shards per CPU, and a block larger than one shard is
  never kept. With eight CPUs a shard is 1 MiB; the filter of a table of a
  million entries is larger, so every lookup read and checksummed it again.
  The same run limited to two CPUs (16 MiB shards) was fast, which is what
  confirmed it.
- **Fix.** The `entries` keyspace is created with the filter and the index
  block of every table pinned (`entries_options` in `store/fjall.rs`). The
  index block is pinned too because it would hit the same limit on a
  machine with more CPUs.
- **Why not a larger cache.** 64 MiB also fixed it here, but only because
  it makes a shard 2 MiB on eight CPUs. On sixteen it would be 1 MiB again.
- **Cost.** About 2 bytes of memory per entry.
- **Existing manifests keep the old behaviour.** fjall stores a keyspace's
  options when it is created. A manifest made before this change stays
  slow until it is made again with `init`.
- **Measured** with `benches/lookup.rs`, 8 CPUs, microseconds per lookup on
  a reopened manifest: 1,000,000 entries 71 before, 3.8 after; 4,000,000
  entries 4.7 after.

## Follow-up: `check` looks entries up through the walk (2026-11-21)

Issue #189. `check` asked the store for every entry it walked, one lookup
at a time, and a second time for every regular file to decide whether to
hash it. On a SQLite manifest opened read-only each lone lookup is its own
read transaction in rollback-journal mode: file locks, a check for a
leftover journal and a re-read of the file header, about 11 µs against 4
in a batch.

- **Fix.** `check` hands the walk a reader, as `update` does. The walk
  threads look the entries up in batches and each `Observed` carries what
  the manifest holds. One lookup per entry instead of up to two.
- **xattrs are still read from the tree.** With a reader the walk takes
  xattrs the manifest holds as current from the manifest. `check` sets
  `force_xattr_reads`, so it reads them as before.
- **A failed lookup** stops the run when the entry's turn comes, with the
  message `update` uses.
- **Measured.** 200,000 empty files, SQLite manifest, 8 CPUs: 5.8 s before,
  1.26 s after, the same in every tier. The reports of the old and the new
  binary on a tree with added, deleted and changed entries were identical.

## Follow-up: a redb store (2026-11-21)

Issue #192. A third store, `--store redb`, for a manifest that travels with
its tree: one file, as small as it gets, complete on disk when the command
returns, and readable where nothing can be written. fjall cannot open
read-only at all, and a SQLite manifest opens on such media only because
`close` folds its log in.

What was checked before building, and what came of it:

- **Reading is only reading.** `check` and `list` open the file with
  `open_read_only`. Under `strace` a `check` opens it `O_RDONLY`, takes read
  locks on byte ranges of it, and creates, writes and renames nothing. A
  test runs `check` and `list` on a manifest and a tree with the write
  permission taken away. A real read-only mount was not tried then: it
  needs root. It was later (issue #206): a test runs `check`, `list` and
  `update` with the tree bound read-only in a mount namespace of its own,
  made with Bubblewrap, an unprivileged `unshare`, or `sudo` on a CI runner.
  On macOS a second test does the same on a disk image attached for
  reading only (issue #246), which needs no root. On FreeBSD, where redb
  locks the whole file instead of ranges, a third test mounts the tree
  read-only somewhere else with nullfs. An ordinary user may do that once
  root sets `vfs.usermount=1` and loads nullfs, which the FreeBSD workflow
  does.
- **Readers beside the writer** are read transactions on the one open
  database. A second, read-only open is refused while it is open for
  writing.
- **Compaction.** redb never gives freed pages back, and it grows its file
  in large steps: after an `update` that changed 1,000 of a million entries
  the file was twice the size. `Store::compact` runs after the last write
  of `init` and `update`, and `--no-compaction` skips it. Of redb's two
  modes, `Maximum` is used: after an update of a tenth of a million
  entries it left 101.3 MB where a new `init` gives 101.4 MB, in under a
  second. `Balanced` was about half a percent larger and three times as
  fast, which is not worth the size here.
- **Size** of a million entries: redb 102 MB, SQLite 115 MB, fjall 245 MB
  (104 MB once its journal is folded in).
- **Speed.** A lookup takes about 2 µs, written or reopened, single or in
  batches (SQLite 3.5 to 11, fjall 3.8). On a million empty files `init`
  takes 6.2 s (SQLite 7.8) and `check` 5.7 s (6.4). An `update` that
  changes a tenth of them takes 3.3 s (2.8): the integrity check on open
  and the compaction are the difference.
- **A killed run.** Batches of entries are committed without forcing them
  to disk, except once a second, and the meta rows that end a run force
  everything. So a killed `update` loses at most about a second of work,
  not all of it, and does not pay for an `fsync` per batch. The file is
  then left in need of a repair, which only an open for writing makes:
  `check` and `list` say so, and the next `update` repairs it.
  `unfinished_manifest` opens a redb manifest for writing for this reason.
- **The sweep does not use `retain`.** It first did. Dropping a thousandth
  of a million entries then doubled the file. It now finds the paths to
  drop a page of keys at a time and removes them one by one, which left
  the file the size it was.
- **Damaged and crafted files.** This is where redb is weakest. It verifies
  page checksums when it repairs a file or checks it on an open for
  writing, never on a read. Of 4,000 random corruptions of a manifest, a
  read-only open handed back altered entries in about one in ten, and redb
  panicked in about 170. So:
  - every entry is stored with four bytes of the BLAKE3 hash of its path
    and content, checked on every read (4 bytes an entry, 4 MB a million);
  - an open for writing turns on redb's integrity check, which reads the
    whole file;
  - every read through redb runs under `catch_unwind`, and a panic is
    reported as a damaged manifest. redb's own panic message is kept off
    stderr (issue #206): a panic hook drops it when the panic comes from
    redb's source while such a read runs on that thread, and prints every
    other panic as before;
  - `for_each` refuses paths that do not rise, as the SQLite store does;
  - the meta table holds a digest of all entries (issue #206): 16 more
    bytes of each entry's hash, combined with XOR. Every write updates it in
    the transaction that changes the entries, and a read of all entries, as
    `check` and `list` make, compares it. It finds an entry that is gone, or
    is the one an earlier run stored, in a file that is otherwise sound. It
    is no defence against a file made on purpose, whose maker can write a
    digest to match. A manifest without the digest is refused: there is no
    migration.

  With these, the same 4,000 corruptions gave an error or the stored
  entries, every time. A smaller run of them is a test. What is not
  covered: a file crafted to make redb loop. Nothing bounds a read, and
  none has been seen to loop: the fuzz target below fails on an input that
  takes over 10 seconds.

  There was no fuzz target at first, because under the fuzzer a panic
  inside redb is a crash whatever `fileset` does with it. The panic hook
  above changed that (issue #206): it also keeps redb's caught panics from
  libFuzzer's hook, which is the one that aborts. The `redb_manifest`
  target opens altered redb files as `check` and `list` do, and fails only
  on a panic of `fileset`'s own or one outside a guarded read. Its first
  180,000 runs found nothing.
- **Record format.** The protobuf encoding of the fjall store, reused, with
  the checksum after it.
- **`locate`.** A file named by `--manifest` is redb if it starts with
  redb's magic bytes, and SQLite otherwise.

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
  noted in `book/src/reference/fileset.md` for the planned sqlite-vs-fjall
  benchmark, since on-disk size is one of its measures.
