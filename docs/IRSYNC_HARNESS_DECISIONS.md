# irsync-stress: implementation decisions

This file records the rulings made while implementing the `irsync-stress`
crate from `docs/superpowers/specs/2026-10-21-stress-harness-design.md` and
`docs/superpowers/plans/2026-10-21-stress-harness.md`, the irsync bugs the
harness has found, and the outcome of its acceptance run. Design-level
decisions live in the spec; this file covers the calls made during execution
that the spec and plan did not settle, or where the shipped code differs from
the spec. Each entry names what was decided, why, and what it costs if the
ruling turns out to be wrong. The spec itself was not amended; where it and
the code disagree, the code is what shipped and the entry says so.

## The spec's "left to the builder" items

1. **Timeouts and bounds.** The daemon has 15 s to print its startup lines.
   Stopping it sends SIGINT and waits up to 10 s, then SIGKILL. Joining a
   log-copier thread, and draining the daemon's stdout after it has exited,
   are each bounded at 1 s. `DaemonLog::tail`, used in error messages, reads
   the last 64 KiB of the log. Why: every wait must end, so that a wedged
   child cannot hang the harness (or CI), and a 64 KiB tail bounds memory on
   a long run. Cost if wrong: a slow machine could see a spurious "daemon did
   not start" (raise the 15 s), or a daemon that needs longer than 10 s to
   shut down is killed rather than stopped cleanly.

2. **Sentinel file.** The first sync carries `.irsync-stress-ping` in the
   source tree, with content `seed=<seed> started=<timestamp>\n`. Why: the
   sentinel round proves the daemon, keys and module work before any filegen
   output is involved, so a setup fault is not confused with a transfer
   fault; the content makes the file unique per run. The file stays in the
   tree and filegen may change or delete it. Cost if wrong: a name clash
   with a generated file is possible in principle; the dot-prefix makes it
   unlikely.

3. **The daemon pid is in `run.toml`** (`[daemon] pid`), next to its endpoint
   id and address. Why: the SIGINT end-to-end test needs it to check that
   the daemon is gone after the harness is signalled, and it is useful to
   anyone looking at a leftover process. Cost if wrong: none beyond an extra
   field; a stale pid is only informative.

4. **Signal handling.** A SIGINT/SIGTERM handler sends SIGINT to the
   current foreground child (the client, filegen or fileset run the harness
   is waiting for, if any), then to the daemon, and calls `_exit(130)`. Each
   pid sits in its own atomic slot: `process::status` and `process::output`
   store the child's pid between spawn and wait and clear it afterwards;
   `run` stores the daemon's pid once it is up and clears it after stopping
   it. Both signals are blocked on the spawning thread from before each
   spawn until the pid is stored, so a signal arriving in between is
   delivered afterwards, with the pid set. Because std does not reset a
   child's signal mask, `process::spawn` unblocks both signals in every
   child with `pre_exec`; without it the daemon would inherit the block and
   never see the SIGINT sent to it. Why: the handler can do only
   async-signal-safe work (atomic loads, `kill`, `_exit`), and the harness
   runs at most one foreground child at a time, so two fixed slots cover
   every child; a `kill` of the harness from outside the terminal then
   stops its children too, not only a terminal Ctrl-C. Cost if wrong: a
   child that ignores SIGINT can still outlive the harness, which does not
   wait for it; and in the instant between a child being reaped and its
   slot being cleared, a signal could reach a process that has reused the
   pid (vanishingly unlikely, and only SIGINT).

5. **`process::spawn` retries on ETXTBSY** (10 ms apart, up to 100
   attempts), and every child goes through it. Why: parallel unit tests
   write fake scripts and execute them, and on Linux a fork in another
   thread can briefly hold the script's write descriptor, so exec fails with
   ETXTBSY; this hit about one run in eight. Cost if wrong: a genuinely stuck
   ETXTBSY delays a failure by up to one second.

6. **The CI end-to-end test deviates from the spec's invocation.** It passes
   `--filegen-set names.max_len=200`, which the spec's CI invocation did not
   have, so the known NAME_MAX bug (below) cannot make it flaky; it uses a
   fixed seed; and it bounds each harness run to 300 s, with a guard that
   stops a stray harness on failure. Why: a test that fails because of a
   known, unfixed bug in the product is noise, and a hung child must not
   hang CI. Cost if wrong: the test does not exercise long names; once the
   bug is fixed the `--filegen-set` can be dropped. **Dropped on
   2026-10-21** with the NAME_MAX fix below; the test now runs filegen's
   default names.

7. **`--max-file-size` and `--filegen-set` exist.** The design review added
   them. Why: filegen refuses a total cap below its 256M per-file default,
   so a small `--max-total-size` is unusable without `--max-file-size`;
   `--filegen-set` is also the way to apply the NAME_MAX workaround. Cost if
   wrong: two more flags to maintain.

8. **Exit classification.** Client exit 23 is a failed iteration (harness
   exit 1); any other non-zero client exit is a harness error (exit 2). After
   exit 23 the manifest comparison and `fileset check` still run, because the
   check report is the main diagnostic. Currently, if the destination
   manifest is missing after an exit 23, that is exit 2 (in `verify.rs`)
   rather than a failed iteration. Why: 23 means the transfer was partial, a
   finding about irsync; other codes mean the invocation itself is wrong.
   Cost if wrong: the missing-manifest case is reported as a harness error
   when it should be a failed iteration; noted as a follow-up.

9. **Tool lookup.** Each tool is looked up next to the harness binary first,
   then on `PATH`; `--irsync`, `--filegen` and `--fileset` override. Why: a
   `cargo build --workspace` puts all binaries in one directory, so the
   freshly built tools are used rather than whatever is installed. Cost if
   wrong: an installed older tool on `PATH` is used when the sibling is
   missing; `run.toml` records the paths and versions so this is visible.
   `cargo test -p stress` alone does not build the other binaries, so
   `cargo build --workspace` is needed first.

10. **Log content.** The client's stdout is appended to the iteration log
    along with its stderr. filegen's stdout is discarded and its stderr is
    appended to `logs/filegen.stderr.log` (every filegen run of the
    harness, fill and mutations, in one file); a filegen failure names that
    file and filegen's own log `filegen/filegen-<seed>.log`. The daemon and
    client logs contain ANSI colour codes, because irsync's tracing output
    is coloured. Why: nothing the client prints should be lost, and a
    filegen error must survive an unattended run rather than scroll past on
    a terminal nobody watches. Cost if wrong: filegen's progress output no
    longer shows on the terminal; logs need `less -R` or an
    escape-stripping filter to grep cleanly.

11. **No client inactivity `--timeout`.** The harness passes no
    `--timeout` to the irsync client. Why: the spec left it open, and a
    hung client is itself a finding about irsync that should be visible as
    such, with the daemon and client logs showing where it stopped, rather
    than turned into an exit code. Cost if wrong: an overnight run can hang
    silently until someone looks. Workaround: pass `-- --timeout N`; with
    the harness's `--reconnect 0` the client then fails, and the harness
    exits 2 naming the client and daemon logs.

## Known irsync bugs found by the harness

### NAME_MAX: long file names cannot be transferred

filegen's `long` name class draws file names of 200 to 255 bytes. irsync's
receiver stages each incoming file as `.irsync.<name>.part` in the same
directory, so a name longer than about 242 bytes makes the staging name
exceed NAME_MAX and fails with `File name too long (os error 36)`; the
client exits 23. The bug was found while checking the design by running the
loop by hand, and confirmed by the first real runs.

What the first real runs showed: in a default-size push run, iteration 1
passed and every iteration from 2 to 15 exited 23 with `check FAILED
(added=0 deleted=1..3 changed=0)`. The client log shows `File name too long
(os error 36)`; the check report lists the 200+ byte names as `DEL`. Once
such a file exists in the source it never transfers, so the failure compounds
across all later iterations rather than appearing sporadically.

Decision at the time: not fixed in this project; the harness reports it as a
failed iteration like any other, and a run that wants to get past it uses
`--filegen-set names.max_len=200`.

**Fixed 2026-10-21.** Every temporary name irsync makes (`.part` for staged
files, `.tmp` for symlinks, specials and hard links) now comes from one
function, `recv::temp_name`. When `.irsync.<name><suffix>` would exceed 255
bytes, the name is cut (on a character boundary when it is UTF-8) and `~`
plus 16 hex digits of the name's BLAKE3 hash are put before the suffix, so
the result fits and names that share a long prefix still get distinct
temporaries. The GC can no longer invert a part name into its destination,
so it maps every recorded and referenced destination forward instead and
compares. Tests: `tests/recv.rs` (`temp_names`), `tests/gc.rs`
(`gc_handles_shortened_part_names`), `tests/posix_extras.rs`
(`name_max_entries_transfer`, every entry kind with a 255-byte name, both
directions, synced twice). Verified with the harness: seed 11, 40 iterations,
default names, both directions, 40 passes each and a clean `--paranoid`
check.

### FIFO mtime: a FIFO's modification time never arrives

Found by the acceptance run, in all four runs and both directions. A FIFO
that filegen created (`mkfifo`) and then gave a drawn modification time
(`set_mtime`) arrives on the destination with the time of its creation
there. The check reports `CHG <path> mtime=<source>→<destination>` with only `mtime`
differing, and the line persists across every later sync until filegen
deletes or renames the FIFO, so later `set_mtime` operations on the source
evidently do not reach it (inferred from that persistence). The number of such FIFOs grows over a run
(push-maxlen200 ended with changed=9, pull-maxlen200 with changed=7).
Every failed iteration was attributed to one of the two bugs, so no other
drift was observed; it was not measured separately.

Evidence: push-maxlen200 iteration 8's check report has two `CHG ... mtime=`
lines, both paths created by `mkfifo` and then `set_mtime` in
`filegen/filegen-11.log`; pull-maxlen200 iteration 7 (`mkfifo` at step 99,
`set_mtime` at steps 664, 812 and 1763).

Hypothesis at the time, not verified: the receive path creates special files
without applying the sent mtime, and the planner treats a FIFO as unchanged
when only its mtime differs.

**Fixed 2026-10-21.** Only the second half of the hypothesis was true: the
regression test's first assertion (a FIFO created with a drawn mtime arrives
with it) passed on the unfixed code, and the second (the mtime changes on
the source, the next sync carries it) failed. The planner's branch for
FIFOs, devices and sockets compared only `rdev` and mode and returned `Skip`
otherwise. It now returns `Special` when `rdev` differs and otherwise the
same `skip_or_meta` decision as directories, so a differing mode, time or
owner is fixed in place with `MetaOnly`. Tests: `tests/plan.rs`
(`special_metadata_only_difference_is_meta_only`), `tests/posix_extras.rs`
(`fifo_mtime_is_applied_and_updated`). Verified with the same harness runs
as the NAME_MAX fix: 50 `mkfifo` steps per run, no `CHG` lines.

## Acceptance run

Binaries: a release build of the tree at commit 8cb3102 for all four tools.
The harness code changed afterwards only in signal handling, tool lookup
order, absolute tool paths, filegen stderr capture and error wording
(703c180); the loop itself is unchanged. Runs were started from
`target/release` with `--iterations 300 --keep-going`, default size (25G
cap, 100 steps), concurrently on one 32-core Linux box, roots on a
disk-backed filesystem. The two `names.max_len=200` runs were added by the
controller (the spec has none) because the NAME_MAX bug compounds from its
first occurrence and would otherwise hide everything else.

| run | extra arguments | footer | elapsed | final SRC tree |
| --- | --- | --- | --- | --- |
| push | `--direction push --seed 1` | passed=9 failed=291 | 1772.8 s | 14 GB, 2106 files, 48 FIFOs |
| pull | `--direction pull --seed 2` | passed=29 failed=271 | 1575.5 s | 9.5 GB, 1546 files, 25 FIFOs |
| push-maxlen200 | `--direction push --seed 11 --filegen-set names.max_len=200` | passed=66 failed=234 | 1719.7 s | 13 GB, 2060 files, 60 FIFOs |
| pull-maxlen200 | `--direction pull --seed 12 --filegen-set names.max_len=200` | passed=13 failed=287 | 1766.4 s | 19 GB, 2887 files, 69 FIFOs |

All four harness exits were 1 (failed iterations). There was no harness
error (no exit 2), no client exit other than 0 or 23, and no manifest hash
mismatch in any iteration of any run. Sync wall time averaged 3.2 to 3.6 s,
maximum 13.1 s. An unattended default-size 300-iteration run takes about 30
minutes. The measured trees were 9.5-19 GB per side after 300 iterations;
the 25G cap bounds each side, so a run needs up to 2 x 25 GB = 50 GB on disk
(cap arithmetic, not measured).

Every one of the 1083 failed iterations is explained by exactly two irsync
bugs, classified by a script over the check reports, client logs and filegen
logs:

1. **NAME_MAX** (default-size runs only). Client exit 23 with `File name too
   long (os error 36)` in the client log; the check reports `DEL` for every
   file whose name exceeds about 242 bytes (2435 such DEL lines over the push
   run, 1735 over the pull run). Once such a file exists it never transfers,
   so from the first occurrence nearly every later iteration fails: 32
   push and 34 pull iterations failed with only this bug. On a pull,
   iterations can go clean again when filegen removes the long-named files
   (the pull run has 29 passes scattered through it).

   Second-order effect, seen twice (push iteration 264, pull iteration 94):
   a regular file that filegen had just hard-linked to a name over 242 bytes
   was skipped with `hard link: hard link target failed`. The likely
   explanation (not observed directly) is that irsync sent the long name as
   the link group's first name and creating it failed. The short name kept its previous content for one iteration (`CHG` with blake3,
   size and mtime differing) and was repaired by the next sync.

2. **FIFO mtime** (all four runs). This caused all 234 failures of
   push-maxlen200 and all 287 of pull-maxlen200, and co-occurred with
   NAME_MAX in 258 push and 236 pull iterations of the default-size runs
   (seeds 1 and 2). See the entry above.

Definition of done, from the spec: the harness runs to the end with
`--keep-going`, every failure it reports is explained, and the artefacts it
leaves behind are the ones the design promises. All three held. The
artefacts checked in the run directories: `run.toml` with its `[daemon]`
section, both manifests, `logs/iter-NNNN.check.txt`,
`logs/iter-NNNN.client.log`, and `logs/daemon.log` with the iteration
markers.
