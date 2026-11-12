# Hunting bugs

Part of [Testing](testing.md): campaigns, the slowest layer.

A strategy for turning [irsync-stress](stress.md) from a loop that passes into
a loop that finds bugs. It builds on what the harness has found so far
([IRSYNC_HARNESS_DECISIONS.md](https://github.com/irsync-dev/irsync/blob/main/docs/IRSYNC_HARNESS_DECISIONS.md)),
on the knobs that filegen, fileset and the irsync client expose, and on
irsync's list of open points (section 2 of
[IRSYNC_DECISIONS.md](https://github.com/irsync-dev/irsync/blob/main/docs/IRSYNC_DECISIONS.md)),
which is a map of where the ice is thin.

Network faults (loss, delay, dead zones that force a reconnect and resume)
are covered by a separate rig, [irsync-netstress](netstress.md).

The commands assume a release build in `target/release` and roots on a
filesystem with room for twice the size cap.

## What the loop can and cannot see

Every bug the harness finds has to pass three filters. Knowing them tells
you where to push.

1. **filegen must provoke it.** Only operations with a non-zero weight
   happen. `set_xattr`, `chown` and `mknod` are off by default; `mkfifo`
   and `punch_hole` are rare (weights 0.5 and up to 1 in rows that sum to
   about 100). A bug in hard-link handling needs hard links in the tree,
   and at the default weight of about 1% a default 10-iteration run makes
   about 10 of them.
2. **fileset check must observe it.** The harness runs the default tier:
   type, content, symlink target, device numbers, mode, mtime, size. It does
   **not** compare `nlink`, `uid`, `gid` or `xattr`, so a hard-link group
   that arrives as independent copies with the right bytes passes. Disk
   allocation (sparseness) is not recorded at all. Nothing checks the
   destination's `.part` leftovers, index rows or session files.
3. **A known bug must not drown it.** Some bugs compound: once their trigger
   exists in the source, every later iteration fails for the same reason,
   and anything else has to be found by hand underneath them. In the
   harness's acceptance run all 1083 failed iterations came from two such
   bugs. Fix or neutralise a compounding bug before investing in
   campaigns: a harness whose default run fails most iterations on a known
   cause gives no signal on anything new.

A compounding bug is neutralised with `--filegen-set`, by keeping filegen
from making its trigger. Long names off: `--filegen-set names.max_len=200`.
An operation off: its column must be zero in every row of filegen's
transition matrix:

```sh
FG=()
for k in create_file append truncate overwrite_range rewrite punch_hole delete_file \
         mkdir rmdir remove_tree rename rename_over symlink repoint_symlink \
         delete_symlink hardlink mkfifo chmod set_mtime set_xattr chown mknod; do
  FG+=(--filegen-set "weights.$k.mkfifo=0")
done
irsync-stress --direction push "${FG[@]}" ROOT
```

The same loop with another weight biases an operation instead of switching
it off; `matrix.sh` does this with its `col` function.

The harness never deletes a root, and both manifests survive, so the
observation filter can be widened **after** a run at no cost:

```sh
fileset check -q --paranoid --manifest ROOT/SRC/data/.fileset.sqlite ROOT/DST/data
fileset check -q --relaxed --require nlink --manifest ROOT/SRC/data/.fileset.sqlite ROOT/DST/data
du -s ROOT/SRC/data ROOT/DST/data          # allocated blocks: sparse preservation
```

Do this on every finished run; `crates/stress/tools/paranoid-check.sh ROOT`
runs the first. A destination that uses fewer blocks than the source is
irsync skipping all-zero chunks (R3), not a fault; a destination that uses
**more** blocks with `-S` passed would be one.

## Breadth before depth: many seeds, short runs

Because failures compound, a 300-iteration run mostly re-reports its first
bug. Many short runs find more distinct bugs per machine-hour:

```sh
crates/stress/tools/breadth.sh                       # seeds 1 to 50, both directions, 8 at a time
grep -v 'exit=0' ~/work/irsync-breadth/summary.txt   # runs with a failure or error
```

The script takes `BIN`, `OUT`, `SEEDS`, `JOBS`, the run size and the client
arguments from the environment, passes extra arguments to `irsync-stress`,
deletes the root of every passing run and keeps the root of every failing
one ([Campaign tools](stress.md#campaign-tools) lists the variables). At the
defaults, a run took about 160 s on a 32-core machine, and the eight live
roots used 23 to 26 GB of disk.

It runs every seed in both directions. The same seed produces the same
source tree in both, so a seed that fails on push and passes on pull puts
the bug in the daemon-side receiver, and the reverse puts it in the client
receiver. That is the cheapest localisation the harness offers.

Then go deep on a few seeds: 300 to 1000 iterations with `--keep-going`,
looking for bugs that need accumulation: index growth, session and GC
state, `.part` files left behind, and the daemon's memory over time (`ps`
it hourly).

## The campaign matrix

Each row aims the harness at a part of irsync that the default run barely
touches. Run each as a breadth sweep, in both directions.
`crates/stress/tools/matrix.sh` runs the rows it names in the last column
([Campaign tools](stress.md#campaign-tools)). `--filegen-set` takes dotted
YAML paths; weights are per transition row, so biasing an operation means
setting its column in every row, as above.

| Target in irsync | How to aim the harness | What a failure looks like | `matrix.sh` row |
| --- | --- | --- | --- |
| Hard-link final pass (R23); stale target content when the target failed at planning time; link groups across directories | `hardlink` weight 10 in every row, `rename` and `rename_over` 8; post-run `--require nlink` | `nlink=` differences in paranoid, or a short-named member with old content for one iteration | `hardlink` |
| Rename detection and content reuse | `rename` 15, `rename_over` 10, `overwrite_range` 5; run the same seed with and without `-- --no-rename-detection` | A failure only with detection on is a reuse bug; only with it off, a plain transfer bug | `rename`, `rename-off` |
| Resume and chunk adoption of `.part` files | `-- --timeout 5` with `max_file_size` 2G and `--bwlimit 20m`, so transfers outlive the timeout; or kill the client mid-sync and re-run the same invocation | Client fails (harness exit 2), or content differs after the adopted chunks | none |
| Sparse files (R3, `-S`) | `punch_hole` 10, `content.pattern_fraction=0.5` (zero blocks), `-- -S`; compare `du -s` of both trees | Destination allocates more than the source, or holes become data | `sparse` |
| Metadata: special mode bits, read-only files, the `u+w` restore path | `chmod.special_weight=0.5`, `chmod` weight 10; modes `0444` and `0555` are already in the list | `mode=` lines; a `.part` stranded read-only in the destination (`find DST -name '.irsync.*.part'`) | `metadata` |
| Timestamps: future, pre-epoch, sub-second | `mtime: {future: 0.5, future_years: 50}` and `distant_years: 70` (1956, before the epoch) | `mtime=` lines on regular files or directories | `timestamps` |
| Directory mtime after children change | The default mix; mtime is in the default tier. Look for `CHG <dir> mtime=` | Directories whose mtime a child write reset after metadata was applied | none |
| Symlinks: dangling, absolute, pointing at directories, repointed | `symlink: {dangling: 0.5, absolute: 0.5}`, `repoint_symlink` 10 | `target=` lines; a symlink followed instead of recreated (a file where a link should be) | `symlinks` |
| Deletion passes and `--max-delete` | `remove_tree` 5 with `remove_tree_max_entries=5000`; `-- --delete-before`; separately `-- --max-delete 50` | With `--delete-before`, watch daemon memory. Client exit 25 is a harness error (exit 2), so `--max-delete` needs a harness change first | `deletion` (`--delete-before` only) |
| Listing size, ordering, out-of-order drops (R7) | `--steps 5000`, `size_distribution.small.weight=0.95`, `max_depth=64`, `names.classes.unicode=30`, `names.classes.awkward=30` | `ADD`/`DEL` pairs on names that sort differently under byte order and NFC | `listing` |
| Long names and paths | `names.classes.long=30`, `max_depth=64`; further `names.max_len=255`, `max_path_len=3900` | `File name too long` or `ENAMETOOLONG`, on index paths rather than file names | `longnames` |
| Concurrency and backpressure (R31) | `-- --parallel 1` (serialises; finds ordering bugs) and `-- --parallel 512` with 5000-step iterations of small files | Daemon memory growth across iterations; a hang that `--timeout` turns into exit 2 | `parallel1`, `parallel512` |
| Inactivity watchdog against long local work (R35) | Large files (`max_file_size=8G`, `size_distribution.large_weight=1`), `-- -c --timeout 10`, so the receiver hashes before answering | A spurious reconnect, visible as a failed client with `--reconnect 0` | `bigfile` |
| Quick check against checksum | Run the same seed with and without `-- -c`; a difference means the size-and-mtime shortcut skipped a changed file | Pass with `-c`, fail without | `checksum` |
| The `--paranoid` code paths (clones, second hash, final re-check) on a source that nothing writes to during the sync; the harness changes the tree only between syncs, so this does not test catching a change | `-- --paranoid` with the default mix; run on a release build to test clones, on a `test-hooks` build to test the second hash | A failure the same seed does not show without `--paranoid`; a `.irsync-clones-*` folder left in the source tree | `paranoid` |
| Source files that change **during** the sync: the change checks (stat after hashing, the ask before placing, `--paranoid`'s clone, second hash and final re-check) | `--concurrent`: filegen writes to the source while irsync syncs it, and every destination path is judged against the states the source held (see [below](#writing-during-the-sync---concurrent)); push and pull, with and without `-- --paranoid` | A path named with the rule it broke: `no state the source held`, or `content and times disagree` | `concurrent`, `concurrent-paranoid` |
| The same, over a bad network | `irsync-netstress --concurrent` through `crates/netstress/tools/sweep.sh`, on the fault build | As above; also exit 3 when the faults never bit | `net-concurrent`, `net-concurrent-paranoid` |
| Minimal deltas | `--steps 1 --iterations 500` | Each failing iteration names exactly one filegen operation in `filegen/filegen-<seed>.log` | `minimal` |
| Idle syncs | `--steps 0` runs, but also empties the fill, so the tree is empty; a populated idle sync needs a separate fill step count (a harness change) | A sync of an unchanged tree that transfers anything (check `-- --stats`) is an index bug | none |
| xattrs | `set_xattr` weight 5, post-run `--require xattr` | Expected to fail: irsync has no `-X`. Run only to document the gap | none |

Two differential patterns recur in the table and are worth stating on
their own: **same seed, one flag flipped** (`-c`, `--no-rename-detection`,
`--no-resume`, `-S`, `--delete-before`, `--parallel 1`) and **same seed,
other direction**. A bug that appears on exactly one side of such a pair is
half diagnosed before anyone reads a log.

## Writing during the sync (`--concurrent`)

The loop above changes the source and syncs it one after the other.
`--concurrent`, on `irsync-stress` and on `irsync-netstress`, makes filegen
write to the source **while** irsync syncs it. The goal: no destination file
is ever a state the source never held. On a pull, filegen writes to the
server's tree while the daemon sends it.

### What a round does

1. Pick a pace for filegen from the seed: `fast` (never pauses),
   `steady:N` (N steps a second) or `bursts:ON:OFF` (milliseconds of
   writing, then of quiet). `--pace` fixes it for the whole run.
2. Copy the source tree (a reflink copy where the filesystem can).
3. Start filegen in the background, then start the sync.
4. When irsync exits, stop filegen between two steps.
5. Rebuild the source's history from the copy and filegen's log (its
   `post=` and `post_to=` fields give the stat after each step), and judge
   every destination path against it.
6. Run one more sync with nothing writing. It must exit 0 and leave an exact
   copy.

The seed fixes the random choices. How many steps filegen gets through
before irsync exits depends on timing, so each round's pace and step count
go to `logs/rounds.tsv`. The race itself cannot be replayed: a rerun may
not fail again. The kept files are the evidence.

### The rules

A destination path passes when it is a state the source really held:

- its state at the start of the round, or after any complete filegen step;
- a state in the middle of a step, as filegen writes it: a truncated file,
  or the old bytes followed by a prefix of the new ones.

A path fails when:

- it matches no such state (`no state the source held`). Bytes from two
  steps never mix: that is a torn file;
- its size and modification time match a state but its content or mode do
  not (`content and times disagree`). The next normal sync would skip it
  forever;
- it exists but the source never held it.

Mode and owner (when run as root) are judged too. Directories are judged
on existence and permissions only; hard-link groups are left to the quiet
sync. Exit 23 is accepted only when every file irsync gave up on is one
filegen touched during that sync. The harness learns this from
`IRSYNC_FILE_REPORT`, a variable it sets so irsync writes a small report of
the files it retried and gave up on. It is for the harness only, so it is
not in the [command-line reference](../reference/command-line.md).

### Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Every round passed. |
| `1` | A round failed. |
| `2` | The harness could not do its job. |
| `3` | `irsync-netstress` only: the faults did not bite. |
| `4` | The concurrent writes did not bite: no file changed during a sync was ever placed. The run tested nothing. |

### Files kept, and the rerun

A failed round is kept as `ROOT/round-NNNN/`: `start/` (the source at the
start of the round), `dest/` (the destination as judged, before the quiet
sync repaired it) and filegen's state, beside `ROOT/logs`. The output names
each failing path with its rule (the first 20), then `judge report:`
(`logs/iter-NNNN.judge.txt`, one `path<TAB>rule` line per failure),
`round kept in:` and `rerun:`. The rerun command uses a fresh root and `--replay-rounds ROOT/logs/rounds.tsv`, so
every round starts from the same source tree with the same paces and step
counts. `--keep-going` carries on past a failed round.

The command looks like this (the new root is the old one with `.rerun`
added):

```sh
irsync-stress --direction pull --iterations 5 --seed 99 --steps 100 \
  --concurrent --pace fast --replay-rounds ~/work/run/logs/rounds.tsv \
  ~/work/run.rerun -- --paranoid
```

### Known failures while two irsync bugs are open

The first full-pace runs found two real irsync bugs, listed in `TODO.md`:
a mode (or owner) taken from the walk instead of the file that was hashed,
and a hard link made to a name that was renamed away. Until they are fixed,
the concurrent rows report failures of these two kinds: a path with the
right content and times but a mode from an earlier state, and a path that is
a hard link. Judge a campaign by failures of any other kind.

### The self-test: does the checker bite?

A checker that cannot fail is worth nothing. A `test-hooks` build of irsync
has a switch, `IRSYNC_TEST_NO_CHANGE_CHECKS=1`, that turns all the change
checks off. With it on, a concurrent run must produce a torn file and fail:

Build the release tools first (`cargo build --release --workspace`), then:

```sh
cargo build --release -p irsync --features test-hooks --target-dir target/selftest
IRSYNC_TEST_NO_CHANGE_CHECKS=1 target/release/irsync-stress --concurrent \
  --irsync target/selftest/release/irsync --direction push --iterations 20 ~/work/selftest
```

It must exit 1. Look at the report: the failure that proves the checker
bites is on a path that is not hard-linked and is not a mode-only mismatch.
The two known bugs can fail a run without that proof, so try more seeds
(`--seed`) until a torn file shows up.
If the run passes, the checker is not catching what it should: stop and
find out why. It depends on timing, so it is a manual step and not a
`cargo test`; the checker's own rules have deterministic tests.

## Size and pressure

filegen damps growth above 80% of `max_total_size`, so a long run sits at
the cap with deletions boosted. Choose the cap for the regime you want:

- **Small cap, many files** (`--max-total-size 512M --max-file-size 8M`):
  metadata, naming, listing and deletion bugs; fast iterations, hundreds of
  seeds per hour.
- **Large cap, few files** (`--max-total-size 50G --max-file-size 8G`):
  chunking, resume, sparse handling, hashing time, watchdogs.
- **Default** (25G and 256M): the steady state of the acceptance run, about
  3 s per sync and 30 minutes for 300 iterations on a disk-backed root.

A root on tmpfs is fast but competes with RAM; cap each concurrent run at a
quarter of the free tmpfs space.

## Harness extensions

These need changes to the `stress` crate, ordered by bugs per effort. Each
should come with its own end-to-end test, like the existing one.

1. **A `--check-arg` passthrough** (or `--paranoid`), so `fileset check`
   runs with `--require nlink` and the like inside the loop, not after it.
   The harness runs `fileset check -q`; `breadth.sh`'s `CHECK` hook with
   `paranoid-check.sh` covers only the final state of passing runs.
2. **Treat client exit 25 like 23.** `--max-delete` cannot be stress-tested
   while exit 25 is a harness error.
3. **Kill and resume.** `--kill-after <secs>` sends SIGKILL to the client
   once per iteration after that delay, then re-runs the identical command
   and verifies. This is the largest untested surface: resume, `.part`
   adoption, hash-verified chunk reuse, an orphaned finalize during a
   reconnect. irsync-netstress covers the network half (a dead zone that
   forces a reconnect and resume), not a killed client.
4. **Destination drift.** Before each sync, run a second filegen (its own
   seed and state) on `DST/data` with a small step count. irsync must then
   repair the destination, and `fileset check` still expects the source.
   This exercises the planner's changed-or-equal decisions on files irsync
   did not write, including same size with different content and the mtime
   shortcut (compare with and without `-c`).
5. **Two clients, one export.** Run the same push from two client setups at
   once. The known cost is 60 s per overlapping file before `PART_BUSY`;
   unknown is whether the result is ever wrong.
6. **Split roots** (`--dst-root`), so source and destination can be on
   different filesystems: ext4 to tmpfs, to xfs, to btrfs, and on macOS to
   a case-insensitive APFS volume. Timestamp granularity, case folding and
   NFC normalisation depend on the filesystem, and filegen draws names that
   are unique under folding and NFC for this reason.
7. **Disk full.** A loop-mounted image of a fixed size as the destination.
   The run should fail cleanly with exit 23 and recover once space returns,
   and never leave a torn file that passes the next check.
8. **Negative checks after every iteration.** No `.irsync.*.part` remains in
   the destination, and the index and session directories do not grow
   without bound across iterations. `clean-check.sh` checks the first after
   a passing run.

## From a failed iteration to a bug report

1. `run.toml` gives the seed, direction, sizes and extra arguments. Re-run
   with the same seed and `--iterations N`, where N is the failing
   iteration; the tree is deterministic, so it fails the same way.
2. Shrink: same seed, `--steps 1`, iterations multiplied accordingly, until
   one iteration holds one filegen operation. Its `step=` line in
   `filegen/filegen-<seed>.log` is the trigger.
3. Localise: flip the direction, then flip one client flag at a time (see
   [the campaign matrix](#the-campaign-matrix)). Record which side and which
   flag.
4. Read `logs/iter-NNNN.client.log` and the `=== iteration N ===` section of
   `logs/daemon.log`. Both carry colour codes;
   `sed 's/\x1b\[[0-9;]*m//g'` strips them for `grep`.
5. Write the finding into
   [IRSYNC_HARNESS_DECISIONS.md](https://github.com/irsync-dev/irsync/blob/main/docs/IRSYNC_HARNESS_DECISIONS.md)
   under "Known irsync bugs found by the harness", in the form of the
   entries there: what filegen did, what the check reported, the hypothesis
   marked as such, the decision. Add a regression test to the `irsync`
   crate before fixing.

A campaign is only readable when known failures are subtracted
automatically. That takes a classifier script that attributes each failure
to a known cause by its signature; the repository has none. Keep it
under `crates/stress/tools/` and extend it with each new signature.

## Cadence

- **Every change to irsync:** CI runs the full regression
  (`cargo test --workspace --all-features -- --include-ignored`), which
  includes the harness's end-to-end test.
- **Nightly:** a breadth sweep in both directions with the matrix row of the
  week, 50 seeds, 20 iterations, small cap.
- **Weekly:** two deep runs (push and pull, 1000 iterations, default cap)
  and one large-file run, with the daemon's memory sampled per iteration.
- **Per irsync fix:** the seed that found the bug, re-run as part of the
  fix, and its matrix row's settings added to the harness's end-to-end
  test (`crates/stress/tests/e2e.rs`) if they are cheap enough.

The measure is distinct root causes found per machine-hour, and the ledger
is the "Known irsync bugs found by the harness" section. When a nightly
sweep has run clean on a matrix row for two weeks, move that row to weekly
and bring in the next harness extension.
