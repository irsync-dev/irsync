# Finding irsync bugs with irsync-stress

A strategy for turning the stress harness from a loop that passes into a
loop that finds things. It builds on what the first runs taught us
(`IRSYNC_HARNESS_DECISIONS.md`), on the knobs filegen, fileset and the irsync
client already expose, and on irsync's own list of open points
(`IRSYNC_DECISIONS.md`, section 2), which is a map of where the thin ice is.

Network faults (loss, delay, dead zones that force a reconnect and resume)
are covered by a separate rig, `irsync-netstress`; see
[crates/netstress/README.md](../crates/netstress/README.md).

Written 2026-10-21. Commands assume a release build in `target/release` and
roots on a filesystem with room for twice the size cap.

## 1. What the loop can and cannot see

Every bug the harness finds must pass through three filters. Knowing them
tells you where to push.

1. **filegen must provoke it.** Only the operations with non-zero weight
   happen; `set_xattr`, `chown` and `mknod` are off by default, `mkfifo`
   and `punch_hole` are rare (0.5 and ~1 per 100). A bug in hard-link
   handling needs hard links in the tree, and at the default weight of ~1%
   a 10-iteration run makes about 20 of them.
2. **fileset check must observe it.** The harness runs the default tier:
   type, content, symlink target, device numbers, mode, mtime, size. It does
   **not** compare `nlink`, `uid`, `gid` or `xattr`, so a hard-link group
   that arrives as independent copies with the right bytes passes. Disk
   allocation (sparseness) is not recorded at all. Nothing about the
   destination's `.part` leftovers, index rows or session files is checked.
3. **The known bugs must not drown it.** Both known bugs compound: once a
   long name or a time-stamped FIFO exists in the source, every later
   iteration fails for the same reason. In the acceptance run all 1083
   failures were those two bugs, and anything else would have had to be
   found by hand underneath them.

The harness never deletes a root, and both manifests survive, so the
observation filter can be widened **after** a run at no cost:

```
fileset check -q --paranoid --manifest ROOT/SRC/data/.fileset.sqlite ROOT/DST/data
fileset check -q --relaxed --require nlink --manifest ROOT/SRC/data/.fileset.sqlite ROOT/DST/data
du -s ROOT/SRC/data ROOT/DST/data          # allocated blocks: sparse preservation
```

Do this on every finished run. On today's seeded push run (seed
17746108573265623486, 10 iterations, 2G cap) paranoid passed and the
destination used fewer blocks than the source, which is irsync's chunk-level
zero skipping (R3) and not a fault; a destination using **more** blocks with
`-S` passed would be one.

## 2. Phase 0: clear the two known bugs

Done on 2026-10-21: both NAME_MAX staging and FIFO mtime are fixed (see
`IRSYNC_HARNESS_DECISIONS.md`), and a default run passes without
`--filegen-set names.max_len=200`. The reasoning stands for the next bug
the harness finds: fix or neutralise anything that compounds before
investing in campaigns, because a harness whose default run fails 90% of
iterations on a known cause gives no signal on anything new.

The per-run neutralisation, should a compounding bug reappear, looks like
this (the `FG` array is what the commands below refer to; with the fixes
in, it can be empty):

```
# long names off
--filegen-set names.max_len=200
# FIFOs off: mkfifo must be zero in every row of the transition matrix
for k in create_file append truncate overwrite_range rewrite punch_hole delete_file \
         mkdir rmdir remove_tree rename rename_over symlink repoint_symlink \
         delete_symlink hardlink mkfifo chmod set_mtime set_xattr chown mknod; do
  FG+=(--filegen-set "weights.$k.mkfifo=0")
done
irsync-stress --direction push "${FG[@]}" --filegen-set names.max_len=200 ROOT
```

Keep such a function around; the commands below pass `"${FG[@]}"` so a
workaround can be slotted in without editing them.

## 3. Breadth before depth: many seeds, short runs

Because failures compound, a 300-iteration run mostly re-reports its first
bug. Distinct bugs per machine-hour is higher with many short runs:

```
crates/stress/tools/breadth.sh "${FG[@]}"      # 50 seeds x both directions, 8 at a time
grep -v 'exit=0' ~/work/irsync-breadth/summary.txt   # runs with a failure or error
```

The script takes `BIN`, `OUT`, `SEEDS`, `JOBS` and the run size from the
environment, passes extra arguments to `irsync-stress`, deletes the root of
every passing run and keeps the root of every failing one. The first sweep
(2026-10-21, right after the two fixes) passed 100 of 100 runs; about 160 s
per run on a 32-core box, 23 to 26 GB of disk for the eight live roots.

It runs the same seeds in both directions. The same seed produces the same
source tree in both, so a seed that fails on push and passes on pull puts
the bug in the daemon-side receiver, and the reverse puts it in the client
receiver. That is the cheapest localisation the harness offers.

Then go deep on a few seeds: 300 to 1000 iterations with `--keep-going`,
looking for bugs that need accumulation: index growth (deleted paths are
never purged from the index), session and GC state, `.part` files left
behind, memory of the daemon over time (`ps` it hourly; the control
channels are unbounded, R31).

## 4. The campaign matrix

Each row targets a part of irsync that the default run barely touches. Run
each as a breadth sweep (section 3), in both directions. `--filegen-set`
takes dotted YAML paths; weights are per transition row, so biasing an
operation means setting its column in every row (the loop in section 2 shows
the pattern).

| Target in irsync | How to aim the harness | What a failure looks like |
| --- | --- | --- |
| Hard-link final pass (R23); stale target content when the target failed at planning time; link groups across directories | `hardlink` weight 10 in every row, `rename` and `rename_over` 8; post-run `--require nlink` | `nlink=` differences in paranoid, or a short-named member with old content for one iteration |
| Rename detection and content reuse | `rename` 15, `rename_over` 10, `overwrite_range` 5; run the same seed with and without `-- --no-rename-detection` | A failure only with detection on is a reuse bug; only with it off, a plain transfer bug |
| Resume and chunk adoption of `.part` files | `-- --timeout 5` with `max_file_size` 2G and `--bwlimit 20m` so transfers outlive the timeout; or kill the client mid-sync (section 6) and re-run the same invocation | Client fails (harness exit 2) or content differs after the adopted chunks |
| Sparse files (R3, `-S`) | `punch_hole` 10, `content.pattern_fraction=0.5` (zeros blocks), `-- -S`; compare `du -s` of both trees | Destination allocates more than the source, or holes become data |
| Metadata: special mode bits, read-only files, `u+w` restore path | `chmod.special_weight=0.5`, `chmod` weight 10; modes `0444`/`0555` are already in the list | `mode=` lines; a `.part` stranded read-only in the destination (`find DST -name '.irsync.*.part'`) |
| Timestamps: future, pre-epoch, sub-second | `mtime: {future: 0.5, future_years: 50}` and `distant_years: 70` (1956, before the epoch) | `mtime=` lines on regular files or directories |
| Directory mtime after children change | default mix; add `--require` nothing, mtime is default. Look for `CHG <dir> mtime=` | Directories whose mtime was reset by a child write after metadata was applied |
| Symlinks: dangling, absolute, pointing at directories, repointed | `symlink: {dangling: 0.5, absolute: 0.5}`, `repoint_symlink` 10 | `target=` lines; a symlink followed instead of recreated (a file where a link should be) |
| Deletion passes and `--max-delete` | `remove_tree` 5 with `remove_tree_max_entries=5000`; `-- --delete-before`; separately `-- --max-delete 50` | Exit 25 is reported as harness exit 2 today (section 7); with `--delete-before` watch daemon memory |
| Listing size, ordering, out-of-order drops (R7) | `--steps 5000`, `size_distribution.small.weight=0.95`, `max_depth=64`, `names.classes.unicode=30, awkward=30` | `ADD`/`DEL` pairs on names that sort differently under byte order and NFC |
| Long paths once NAME_MAX is fixed | `names.max_len=255`, `max_path_len=3900`, `max_depth=64` | `File name too long` or `ENAMETOOLONG` on index paths rather than file names |
| Concurrency and backpressure (R31, unbounded channels) | `-- --parallel 1` (serialises; finds ordering bugs) and `-- --parallel 512` with 5000-step iterations of small files | Daemon RSS growth across iterations; a hang that `--timeout` turns into exit 2 |
| Idle watchdog versus long local work (R35) | one 8G file (`max_file_size=8G`, `large_weight=1`), `-- --timeout 10`, `-c` so the daemon hashes before sending | A spurious reconnect, visible as a failed client with `--reconnect 0` |
| Quick-check versus checksum | run the same seed with and without `-- -c`; a difference means the size-plus-mtime shortcut skipped a changed file | Pass with `-c`, fail without |
| Minimal deltas | `--steps 1 --iterations 500` | Each failing iteration names exactly one filegen operation in `filegen/filegen-<seed>.log` |
| Idle syncs | `--steps 0` runs but also empties the fill, so the tree is empty; a populated idle sync needs a separate fill step count (section 6) | A sync of an unchanged tree that transfers anything (check `-- --stats`) is an index bug |
| xattrs | `set_xattr` weight 5, post-run `--require xattr` | Expected to fail: irsync has no `-X`. Run only to document the gap, not to hunt |

Two differential patterns recur above and are worth stating on their own:
**same seed, one flag flipped** (`-c`, `--no-rename-detection`,
`--no-resume`, `-S`, `--delete-before`, `--parallel 1`) and **same seed, other
direction**. A bug that appears on exactly one side of such a pair is half
diagnosed before anyone reads a log.

## 5. Size and pressure

filegen damps growth above 80% of `max_total_size`, so a long run sits at
the cap with deletions boosted. Choose the cap for the regime you want:

- **Small cap, many files** (`--max-total-size 512M --max-file-size 8M`):
  metadata, naming, listing and deletion bugs; fast iterations, hundreds of
  seeds per hour.
- **Large cap, few files** (`--max-total-size 50G --max-file-size 8G`):
  chunking, resume, sparse handling, hashing time, watchdogs.
- **Default** (25G/256M): the steady state the acceptance run measured,
  about 3 s per sync and 30 minutes for 300 iterations on a disk-backed root.

A root on tmpfs (today's /tmp) is fast but competes with RAM; cap at a
quarter of free tmpfs per concurrent run.

## 6. What needs harness changes

Ordered by bugs-per-effort. Each is a small addition to the `stress` crate
and should land with its own end-to-end test like the existing ones.

1. **`--check-arg` passthrough** (or `--paranoid`) so `fileset check` runs
   with `--require nlink` and friends inside the loop, not after it. Today
   the harness hard-codes `check -q`.
2. **Treat client exit 25 like 23.** `--max-delete` cannot be stress-tested
   while exit 25 is a harness error.
3. **Kill-and-resume.** `--kill-after <secs>` sends SIGKILL to the client
   once per iteration after that delay, then re-runs the identical command
   and verifies. This is the spec's fault-injection non-goal and the single
   largest untested surface (the network half, a dead zone that forces a
   reconnect and resume, is now covered by `irsync-netstress`; killing the
   client is the second project): resume, `.part` adoption, hash-verified chunk
   reuse, orphaned finalize during reconnect. The acceptance run saw none
   of it because no sync was ever interrupted.
4. **Destination drift.** Before each sync, run a second filegen (own seed,
   own state) on `DST/data` with a small step count. irsync must then repair
   the destination; `fileset check` still expects the source. This
   exercises the planner's changed/equal decisions on files irsync did not
   write, including same-size-different-content and the mtime shortcut
   (compare with and without `-c`).
5. **Two clients, one export.** Run the same push from two client setups at
   once. Known cost is 60 s per overlapping file before `PART_BUSY`; unknown
   is whether the result is ever wrong.
6. **Split roots** (`--dst-root`) so source and destination can be on
   different filesystems: ext4 to tmpfs, to xfs, to btrfs, and on macOS to a
   case-insensitive APFS volume. Timestamp granularity, case folding and
   NFC normalisation are all filesystem-dependent, and filegen already draws
   names that are unique under folding and NFC for exactly this reason.
7. **Disk full.** A loop-mounted image of a fixed size as the destination;
   the run should fail cleanly with exit 23 and recover once space returns,
   never leave a torn file that passes the next check.
8. **Negative checks after a passing run.** Assert no `.irsync.*.part`
   remains in the destination, and that the index and session directories
   do not grow without bound across iterations.

## 7. From a failed iteration to a bug report

1. `run.toml` gives seed, direction, sizes and extra arguments. Re-run with
   the same seed and `--iterations N` where N is the failing iteration; the
   tree is deterministic, so it fails the same way.
2. Shrink: same seed, `--steps 1`, iterations multiplied accordingly, until
   one iteration holds one filegen operation. Its `step=` line in
   `filegen/filegen-<seed>.log` is the trigger.
3. Localise: flip the direction, then flip one client flag at a time
   (section 4). Record which side and which flag.
4. Read `logs/iter-NNNN.client.log` and the `=== iteration N ===` section of
   `logs/daemon.log` (both coloured; `sed 's/\x1b\[[0-9;]*m//g'` strips it
   for grep).
5. Write the finding into `IRSYNC_HARNESS_DECISIONS.md` under "Known irsync
   bugs" in the existing form: what filegen did, what the check reported,
   the hypothesis marked as such, the decision. Add a regression test to the
   `irsync` crate before fixing.

The acceptance run attributed every failure to a known cause with a
classifier script that was not committed. Write one, keep it under
`crates/stress/tools/`, and extend it with each new signature. A campaign
is only readable when known failures are subtracted automatically.

## 8. Cadence

- **Every merge to irsync:** the CI end-to-end test, as now.
- **Nightly:** the breadth sweep of section 3 in both directions with the
  current matrix row of the week, 50 seeds, 20 iterations, small cap.
- **Weekly:** two deep runs (push and pull, 1000 iterations, default cap)
  plus one large-file run, with daemon RSS sampled per iteration.
- **Per irsync fix:** the seed that found the bug, re-run as part of the
  fix's PR, and its row in the matrix promoted into the CI test's
  `--filegen-set` list if cheap enough.

The measure is distinct root causes found per machine-hour, and the ledger
is the "Known irsync bugs" section. When a nightly sweep has run clean for
two weeks on a matrix row, retire that row to weekly and bring in the next
harness extension from section 6.
