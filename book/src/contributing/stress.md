# Stress testing

Part of [Testing](testing.md): the loopback stress loop.

`irsync-stress` runs the loop that filegen, fileset and irsync were built
for. It starts an irsync daemon on loopback, fills a source tree with
[filegen](filegen.md), records it with `fileset init`, then repeats: sync
with the irsync client, compare the two manifests, run `fileset check` on
the destination, mutate the source with filegen and update its manifest. An
iteration where the sync, the manifest comparison or the check fails is
reported and, unless `--keep-going` is given, ends the run. It touches no
network beyond loopback; [irsync-netstress](netstress.md) runs the same loop
over a simulated bad network.

The design is in
[2026-10-21-stress-harness-design.md](https://github.com/irsync-dev/irsync/blob/main/docs/superpowers/specs/2026-10-21-stress-harness-design.md),
the calls made while building it and the bugs it found in
[IRSYNC_HARNESS_DECISIONS.md](https://github.com/irsync-dev/irsync/blob/main/docs/IRSYNC_HARNESS_DECISIONS.md).
[Hunting bugs](bug-hunting.md) is the strategy for making it find more.

## Run the loop

Build the workspace, so that `irsync`, `filegen` and `fileset` sit next to
`irsync-stress`:

```sh
cargo build --release --workspace
```

Then run it with a root directory that does not exist yet, or is empty:

```sh
target/release/irsync-stress --direction push --iterations 20 /tmp/stress-push
target/release/irsync-stress --direction pull --seed 7 --keep-going /tmp/stress-pull -- --parallel 4
target/release/irsync-stress --direction push --max-total-size 8M --max-file-size 1M /tmp/small
```

The root is never deleted, on success or failure. A run without `--seed`
draws one and prints it in the header; `run.toml` in the root records it
with every other setting.

### After a failure

Look at, in this order:

1. `ROOT/run.toml`: seed, direction, sizes, extra client arguments, tool
   versions, and the daemon's `EndpointId`, address and pid. It is enough
   to repeat the run.
2. The two manifests. `SRC/data/.fileset.sqlite` is what the destination
   should have matched; `DST/data/.fileset.sqlite` is what was checked.
   Inspect them with `fileset`, or compare `logs/iter-NNNN.manifest.blake3`.
3. `logs/iter-NNNN.check.txt`: what `fileset check` found (`ADD`, `DEL`,
   `CHG` lines and a summary).
4. `logs/iter-NNNN.client.log`: the client's output for that iteration. In
   `logs/daemon.log` the line `=== iteration N ===` marks where that
   iteration starts, so the daemon's side of the same sync can be read.

The daemon and client logs contain ANSI colour codes from irsync's tracing
output. Read them with `less -R`, or strip them with
`sed 's/\x1b\[[0-9;]*m//g'` before searching.

[From a failed iteration to a bug report](bug-hunting.md#from-a-failed-iteration-to-a-bug-report)
continues from here.

### Campaign tools

`crates/stress/tools/breadth.sh` runs a breadth sweep: many seeds, short
runs, both directions, several runs at a time. It deletes the root of every
passing run and keeps the root of every failing one. Its environment
variables, with their defaults:

| Variable | Meaning | Default |
| --- | --- | --- |
| `BIN` | Directory with `irsync-stress`, `irsync`, `filegen` and `fileset`. | `target/release` |
| `OUT` | Where roots, per-run output and `summary.txt` go. | `~/work/irsync-breadth` |
| `SEEDS` | Seeds to run, as arguments to `seq`. | `1 50` |
| `JOBS` | Runs at the same time. | `8` |
| `ITER`, `STEPS`, `TOTAL`, `FILE` | Iterations, filegen steps, `--max-total-size` and `--max-file-size` of each run. | `20`, `200`, `4G`, `256M` |
| `CLIENT` | Arguments for the irsync client, after `--`. | `--timeout 300` |
| `CHECK` | A command run as `$CHECK <root>` after each passing run; if it fails, the run counts as failed (`exit=3`) and its root is kept. | none |

Extra arguments go to every `irsync-stress` run, before the root. Each run
needs up to twice `TOTAL` on disk while it runs.

`crates/stress/tools/matrix.sh` runs the [campaign
matrix](bug-hunting.md#the-campaign-matrix), one breadth sweep per row. The
rows are `hardlink`, `rename`, `rename-off`, `sparse`, `metadata`,
`timestamps`, `symlinks`, `deletion`, `listing`, `longnames`, `parallel1`,
`parallel512`, `checksum`, `minimal`, `bigfile` and `paranoid`. `matrix.sh hardlink
rename` runs only those rows; with `DRY=1` it only checks that filegen
accepts every row's settings. Each passing run is checked with
`tools/clean-check.sh`, which runs `tools/paranoid-check.sh` (a
`fileset check --paranoid` of the final destination against the source
manifest, link counts included) and then asserts that no `.irsync.*.part`
is left in either tree. All rows take about three hours on a 16-core Mac.
The `paranoid` row passes `--paranoid`. Binaries built by `cargo test
--workspace` have the `test-hooks` feature, and there `--paranoid` never
clones, so the row tests the second-hash path; `cargo build --release`
binaries clone where the filesystem can.

## Reference

### Command

```text
irsync-stress [OPTIONS] --direction <DIRECTION> <ROOT> [-- <CLIENT_ARGS>...]
```

`<ROOT>` is created if missing; it must be empty if it exists.

| Option | Meaning | Default |
| --- | --- | --- |
| `--direction <push\|pull>` | Which way the client moves data (required). On a push the source tree is `ROOT/SRC/data` and the daemon serves `ROOT/DST/data`; on a pull the roles are swapped. | required |
| `--iterations <N>` | Sync-and-verify rounds. | `10` |
| `--seed <S>` | Seed for filegen, printed in the header. | random |
| `--steps <N>` | filegen operations per fill or mutation. | `100` |
| `--max-total-size <SIZE>`, `--max-file-size <SIZE>` | Passed to filegen verbatim. filegen refuses a per-file limit above the total, so a small `--max-total-size` needs a `--max-file-size` as well. | filegen's: `25G`, `256M` |
| `--filegen-set <KEY=VALUE>` | An extra `--set` for every filegen run; repeatable. Example: `--filegen-set names.max_len=200`. | none |
| `--log-level <LEVEL>` | Log level for the daemon and the client. | `debug` |
| `--keep-going` | Carry on after a failed iteration instead of stopping. | off |
| `--irsync <PATH>`, `--filegen <PATH>`, `--fileset <PATH>` | Use this binary instead of looking one up. | lookup |
| `-- <ARGS>...` | Appended to every irsync client command line. | none |

The tools are looked up next to the `irsync-stress` binary first, then on
`PATH`. `run.toml` records the path and `--version` output of each.

The client runs with `-a -H --delete --reconnect 0`, `--address` set to the
daemon's loopback address, and its own `--identity` and an empty
`--config`, then the arguments after `--`. The harness sets no inactivity
timeout, so a hung sync hangs the run; `-- --timeout N` makes such a client
fail instead, and the harness then exits 2.

### Output

A header, one line per iteration, a footer:

```text
irsync-stress
  root:      /path/to/run
  seed:      3
  direction: push
  endpoint:  716a53cc4cea6705494da4d3b72cae40cfa658440e2e1fe4afba1f1bfbc2a5bb
  address:   127.0.0.1:50357
iter 0001/0003  sync 0.2s  exit 0    manifest ok  check ok
iter 0002/0003  sync 0.3s  exit 0    manifest ok  check ok
iter 0003/0003  sync 0.2s  exit 0    manifest ok  check ok
passed=3 failed=0 elapsed=1.1s
```

A failed iteration looks like this, followed by the path of its check
report:

```text
iter 0002/0015  sync 1.3s  exit 23   manifest ok  check FAILED (added=0 deleted=1 changed=0)
```

### Layout

```text
ROOT/
  run.toml                    parameters of the run, enough to repeat it
  SRC/
    config/                   irsync.toml
    data/                     the tree filegen mutates; its manifest is data/.fileset.sqlite
  DST/
    config/                   the other side's config files
    data/                     where the tree arrives; checked here
  filegen/                    filegen's log and state file (outside both trees)
  logs/
    daemon.log                daemon output for the whole run, with iteration markers
    ping.client.log           client log of the sentinel round
    filegen.stderr.log        filegen's stderr, appended by every filegen run
    iter-0001.client.log      client log per iteration
    iter-0001.check.txt       fileset check output per iteration
    iter-0001.manifest.blake3 hashes of both manifests per iteration
```

### Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Every iteration passed. |
| `1` | At least one iteration failed: client exit 23, manifests differ, or `fileset check` found differences. |
| `2` | The harness could not do its job: bad arguments, a root that is not empty, a missing tool, a daemon that did not start, a tool failing, or the client exiting with anything other than 0 or 23. |
| `130` | Interrupted by SIGINT or SIGTERM. The client, filegen or fileset run in progress, and the daemon, are sent SIGINT first. |

A client exit of 23 (partial transfer) is a failed iteration, not a harness
error; the manifest comparison and the check still run for it. Any other
client exit, such as 25 from `--max-delete`, is a harness error.

### Bugs the harness has found

Each is fixed, with a regression test in the `irsync` crate.
[IRSYNC_HARNESS_DECISIONS.md](https://github.com/irsync-dev/irsync/blob/main/docs/IRSYNC_HARNESS_DECISIONS.md)
has the full record of each.

| Bug | Found by | Regression tests |
| --- | --- | --- |
| A file name longer than about 242 bytes could not be transferred: its staging name `.irsync.<name>.part` exceeded `NAME_MAX`. | a default run | `tests/posix_extras.rs` `name_max_entries_transfer`, `tests/gc.rs` `gc_handles_shortened_part_names`, the `temp_names` tests in `tests/recv.rs` |
| A FIFO whose only change was its modification time was never updated. | the acceptance run | `tests/posix_extras.rs` `fifo_mtime_is_applied_and_updated`, `tests/plan.rs` `special_metadata_only_difference_is_meta_only` |
| One side's long local work, such as the daemon hashing with `-c`, was invisible to the other side's inactivity watchdog. | the `bigfile` matrix row | `tests/resume.rs` `peer_local_progress_resets_idle_watchdog` |
| Metadata applied to a destination file with several links reached the names the source had unlinked. | the `hardlink` matrix row | `tests/posix_extras.rs` `link_broken_at_source_with_equal_content_is_broken_at_destination`, `mode_change_does_not_reach_a_destination_link_outside_the_tree` |

### Tests

`cargo test -p stress` runs unit tests and an end-to-end test that starts a
real daemon and the real tools. That test needs the other binaries, which
`cargo test -p stress` alone does not build: run `cargo build --workspace`
first, or use `cargo test --workspace`.
