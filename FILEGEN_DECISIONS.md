# filegen: implementation decisions

This file records the rulings made while implementing the `filegen` crate
from `docs/superpowers/specs/2026-10-20-filegen-design.md` and
`docs/superpowers/plans/2026-10-20-filegen.md`. Design-level decisions live
in the spec; this file covers the calls made during execution that the spec
and plan did not settle, or where the shipped code differs from the spec.
Each entry names what was decided, why, and what it costs if the ruling
turns out to be wrong. The spec itself was not amended; where it and the
code disagree, the code is what shipped and the entry says so.

## The spec's "left to the builder" items

1. **Targets are picked by a weighted descent over per-subtree kind
   counts.** Every directory in the model keeps, per node kind, how many
   such nodes lie strictly below it. A pick first draws a scope from
   `locality` (below the anchor, below its parent, or below the root,
   widening when the scope holds nothing of the kind), then walks down
   choosing each child with weight "is that kind, plus how many of that kind
   it holds", stopping at a match with probability `1 / (1 + matches below
   it)`. The result is uniform over the scope. Why: it costs one path from
   the scope to the target, never a list of candidates, and depends only on
   the generator and the sorted model. Cost if wrong: the locality is
   coarser than a real workload's; the `locality` weights tune it.

2. **The size-cap pressure is linear from 80% to 100% of
   `max_total_size`.** Below 80% it is 0; at the cap it is 1. Kinds that
   grow the tree (`create_file`, `append`, `rewrite`, `hardlink`) have their
   weight multiplied by `1 - pressure`, kinds that shrink it (`truncate`,
   `delete_file`, `remove_tree`) by `1 + 4 * pressure`; a row that ends up
   all zero is drawn unadjusted. The log shows `pressure=` on every step
   where it is above 0. Why: the simplest curve that keeps a long run
   churning, and the cap itself is enforced separately by the size draws.
   Cost if wrong: a run spends more or fewer steps near the cap than a real
   workload would; the cap is never exceeded either way.

3. **The log is `key=value` pairs in a fixed key order.** Step lines are
   `step ts op path [to] [dry] [bytes] [details...] [pressure] dur_us`;
   the header is `# run ...`, the footer `# end ...` followed by one
   `# stat ...` line per kind; a failure line ends in `error="..."`. Paths
   and detail values that are not plain are quoted and escaped. Why: easy to
   grep and to split in a shell, and the `#` lines are skippable by a future
   `replay` parser. Cost if wrong: the order is now a format; a reader that
   relies on it breaks when it changes.

4. **Content costs exactly two draws of the main generator.** `Content::draw`
   takes two `u64`s, one choosing random or patterned content and one
   seeding the per-operation sub-generator (or the pattern), whatever the
   length and whether or not the bytes are written. The size draws likewise
   consume a fixed number of draws per call. Why: a dry run and a real run
   stay on the same generator position step for step, and a dry run never
   has to produce the bytes. Cost if wrong: none to correctness; changing
   the count changes every seed's sequence.

5. **`rand 0.8` with `rand_chacha 0.3` (`ChaCha8Rng`, `serde1`), and
   `serde_yaml_ng 0.10`.** Why: `rand 0.8` matches the workspace's
   `rand_core 0.6`; ChaCha8 is fast enough to fill files and its state
   serializes into the state file as `seed`, `stream`, `word_pos`;
   `serde_yaml_ng` is the maintained fork of `serde_yaml`. Cost if wrong: a
   later upgrade of `rand`/`rand_chacha` may change the streams, so the same
   seed would produce a different run and old state files would not resume
   identically.

## Task 3 (configuration)

6. **Modes serialize as single-quoted strings (`- '0644'`).** The plan's
   test expected double quotes; `serde_yaml_ng` single-quotes strings made
   of digits. Why: the intent, a quoted octal string, is preserved, and the
   test was adjusted to the serializer. Cost if wrong: none.

## Task 4 (model and start-up walk)

7. **Entries the walk cannot stat or list become off limits.** A
   listable-but-unsearchable directory or an entry that vanishes during the
   walk originally aborted the start-up; now it is recorded as `OffLimits`,
   like a mount point or a socket. Why: a tree being worked on may contain
   such entries, and off-limits handling already keeps filegen away from
   them. Cost if wrong: an unreadable subtree is silently left alone rather
   than reported.

## Task 5 (chain)

8. **`cap_pressure` is computed as `(5t - 4m) / m`, clamped to `[0, 1]`.**
   This is the spec'd `(t/m - 0.8) / 0.2` rearranged so the endpoints come
   out exactly 0 and 1, with the casts to `f64` done first so it cannot
   overflow. Cost if wrong: sub-ulp drift above 2^50 bytes, harmless.

## Task 6 (names)

9. **The `spaces` name class joins words with spaces and puts one extension
   at the end.** Why: more natural than an extension per word, and no rule
   is violated. Cost if wrong: none.

## Task 7 (target selection)

10. **A freed model slot becomes an `OffLimits` placeholder, and a dead or
    non-directory anchor is normalised to its live directory parent, else
    the root.** Review found a stale anchor could be returned as a
    directory. `Model::is_live` was added, and every picker normalises the
    anchor first. Why: closes the stale-anchor bug and the earlier concern
    that the placeholder looked like the root. Cost if wrong: none.

11. **`pick_dir_or_root` may return the anchor itself, root included, in the
    `same_dir` scope.** Why: otherwise nothing could be created in the root
    once any directory exists. Cost if wrong: a slight bias toward the
    anchor.

## Task 9 (log and statistics)

12. **p99 is the element at `ceil((n - 1) * 99 / 100)` of the sorted
    durations.** The plan's floor contradicted its own test (which expects 5
    from `[1..5]`); ceil equals the nearest-rank percentile. Cost if wrong:
    p99 reads one rank high on tiny samples.

## Tasks 11 and 12 (operations)

13. **Content operations resize every node sharing the target's inode.**
    The model keeps an index from inode to the file nodes that share it, for
    inodes with two or more names; it is filled by `hardlink` and by the
    start-up walk (files with `nlink > 1`) and pruned on delete. Content
    operations resize all linked nodes and divide their growth budget by the
    link count. Why: without it the model's sizes drift from the disk on
    hard-linked files, and the total-size cap is counted against the wrong
    numbers. Cost if wrong: model/disk size drift on hard-linked files,
    visible as cap violations.

14. **`punch_hole`'s `emulated` and `bytes`, and `set_xattr`'s `action`, are
    observed outcomes.** Whether a hole was punched or written as zeros
    depends on the filesystem, and whether an xattr was set or replaced on
    what is on disk; a dry run cannot know either. They are logged, but are
    not part of the record a dry run must reproduce, like `dur_us`. Plain
    holes are aligned to 4 KiB, because macOS `F_PUNCHHOLE` needs block
    alignment. Cost if wrong: none to determinism; real holes become more
    common.

15. **Dry runs hand out synthetic inode numbers** from a counter starting at
    2^63 (`Model::synthetic_ino`). Nothing is created on disk in a dry
    run, so without them every new file would carry inode 0 and the
    hard-link index (entry 13) would treat them all as one inode. Cost if
    wrong: none.

16. **`rename_over` draws its destination from the whole tree.** It descends
    from the root, as the plan intended ("from the root scope"). Cost if
    wrong: locality does not apply to the replaced file.

17. **Directory depth checks count directories only.** A file may sit one
    level below a directory at `max_depth`. Why: matches `mkdir` and
    `create_file`. Cost if wrong: none.

## Task 13 (run loop)

18. **`set_mtime` draws relative to a fixed reference instant,
    2026-10-20T00:00:00Z, not the clock.** Why: the same seed must give the
    same records, which the log comparison in the determinism tests relies
    on. This differs from the spec, which says "recent past" and "future"
    relative to now. Cost if wrong: over the years the `future` class drifts
    into the past; the remedy is a `mtime.reference` config key (deferred).

19. **Signal handlers use `SA_RESTART`, and a failed step rewinds the
    generator and anchor.** Review found that without `SA_RESTART` a signal
    could abort the step in flight with `EINTR`, leaving the saved generator
    ahead of `steps_done`. Now the step finishes and the signal is seen
    between steps; a step that fails for any reason restores the generator
    to where it was before the step, so a state written afterwards is at the
    last logged step. Cost if wrong: none.

20. **The prefix may not be empty, `.`, `..`, or contain `/`.** It names
    files in the output directory. Cost if wrong: none.

21. **A resume without `--out-dir` or `out_dir` writes next to its state
    file, and a resume without a prefix takes it from the state file's
    name.** The spec's lookup order alone would send a resumed run's output
    wherever `$FILEGENTMP`/`$TMPDIR` point now. Why: a resume should append
    to the history it belongs to. Cost if wrong: none.

22. **`--force` truncates the old files only after the start-up walk has
    succeeded.** Why: a start that fails must not destroy the previous
    history. Cost if wrong: none.

## Differences from the spec not covered above

23. **`--fsync` takes a value (`--fsync true`), not a bare switch.** The
    flag mirrors an `Option<bool>` so that leaving it out keeps the config's
    value; a switch could only turn fsync on. Cost if wrong: a user who
    types `--fsync` alone gets a usage error naming the expected value.

24. **`last_run.outcome` has more values than the spec lists.** Besides
    `ok` and `failed: <message>` it can be `interrupted: SIGINT`,
    `interrupted: SIGTERM`, and `running` in a state file written at a
    checkpoint. Why: the state file must say why it was written. Cost if
    wrong: none.

## Task 15 (documentation)

25. **The README's default configuration is generated, not printed by a
    flag.** It was produced by a one-step dry run
    (`filegen run --seed 1 --steps 1 --dry-run`) and the `config:` block of
    its state file, with comments added and nested values put in YAML flow
    style to fit the README; that block was checked to load as `--config`
    and yield the same configuration. No `--print-default-config` flag was
    added. Why: the state file always carries the full effective
    configuration, so a dry run already documents the schema, and a flag
    would be one more thing to keep in sync. Cost if wrong: the README copy
    can go stale when a default changes; regenerating it is one command.

## Parked (real, deferred, not fixed)

26. **`--set prefix=123` fails with a type error.** `--set` values are
    parsed as YAML, so a numeric prefix becomes an integer; quote it
    (`--set "prefix='123'"`) or use `--prefix 123`.

27. **`chmod.modes` rejects a YAML integer.** `- 644` is a type error
    ("expected a string"). An unquoted `- 0644` happens to load, because
    the YAML parser reads a leading-zero number as a string, but quoting is
    the reliable form.

28. **Validation is incomplete.** It does not check that the size bands are
    in increasing order, that the name-class weights sum to more than 0,
    that lists are non-empty, or that `names.extensions` hold valid name
    characters (a `/` or NUL in one makes name drawing fail). A
    `size_distribution.small.max` of 0 yields 1-byte files, not empty ones.

29. **A size with a leading `+` (`+4K`) is accepted.**

30. **`repoint_symlink` is not atomic.** It unlinks and recreates the link,
    so an observer can see it missing, and the model keeps the old inode
    number for it.

31. **The log footer quotes the outcome** when it contains a space
    (`outcome="interrupted: SIGTERM"`), while `ok` stays bare. Consistent
    with the quoting rule, but easy to trip over when parsing.

32. **Some errors end the run without a failure line** (writing the log or
    a checkpoint after a step succeeded): only the footer's outcome names
    them. Failure lines used to carry an empty path too; entry 41 fixed
    that.

33. **No signal handlers during the start-up walk.** The plan installs them
    after the walk and the log header. SIGINT or SIGTERM before that kills
    the process with the default action: the log may hold a `# run` line
    with no footer and no state is written, which loses nothing because no
    step was taken.

34. **The progress line is hidden under `--verbose`**, whose echoed log
    lines would garble it.

35. **`pressure` below 0.005 prints as `pressure=0.00`.**

36. **A closed stderr makes filegen panic** when it has something to print
    there (the file paths at start, `--verbose` echo, errors), because it
    writes with `eprintln!`.

37. **The state file's rename is not followed by an fsync of its
    directory**, and its temporary name is unique only per process id.

38. **After a kill no handler can catch, the log may hold steps past the
    last checkpoint**, because it is buffered and flushed at checkpoints and
    when the buffer fills. The resumed run starts at the checkpoint's step
    after a new `# run` line, so those step numbers appear twice.

39. **The test helper `snapshot` ignores mtimes in the hour before it
    runs**, because those come from the clock and differ between two runs.
    While the current date is close to the `set_mtime` reference instant
    (2026-10-20), a drawn `recent` time can land in that hour and is then
    not compared by the determinism tests.

## Final fix wave (whole-branch review)

40. **Unless running as root, the walk puts off limits every entry filegen
    could not safely change**: a directory it cannot list, an entry not
    owned by the effective uid, a regular file without owner read and write
    (`mode & 0o600 != 0o600`), and a directory without owner read, write
    and search (`mode & 0o700 != 0o700`). Before, a tree with `0444` files
    (a git checkout) failed within a few steps with `Permission denied`,
    and since a failed step rewinds the generator, every `--resume`
    replayed the same operation on the same file and failed again; a `-wx`
    directory let filegen rename into it over entries it could not see; an
    unlistable directory was kept as an empty `Dir`. Why it stays
    deterministic: the rules read only ownership and owner permission bits,
    which filegen never removes (`chmod` keeps owner rw, rwx on
    directories; giving an entry to another uid needs root), so an entry
    filegen made is never put off limits by a later walk, and a resume
    rebuilds the same model. An unlistable directory is first inserted as a
    `Dir` and then replaced by an off-limits entry of the same name, which
    reuses its id, so ids are as if it had been classified so at once. As
    root nothing changes (root may write anything; a directory that
    vanished mid-walk still stays an empty `Dir`). Cost if wrong: an
    unprivileged run over a tree of foreign or read-only entries works on
    fewer targets, which is the point.

41. **A failed operation's error carries its paths.** The `syscalls`
    helper (and `new_ino`) wrap the OS error as
    `io::Error::new(kind, OpError { path, to, source })`, with both paths
    made relative to the root; `Runner::step` downcasts it and the failure
    line logs `path=` and, for a rename or hard link, `to=`. `OpError`
    displays as its source, so the error text, the footer's outcome and the
    exit code are unchanged. Why: the spec's failure line names the paths,
    and an empty `path=""` left the reader guessing which entry failed. Cost
    if wrong: `raw_os_error()` on the returned error is now `None` (nothing
    reads it; `kind()` is kept).

## Process notes

- Every commit on the branch ends with the trailer
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`; one
  implementer commit was amended to it after the fact.
